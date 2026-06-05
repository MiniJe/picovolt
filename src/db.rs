//! The outer developer surface: [`Database`] plus the dev/prod lifecycle.
//!
//! This is the integration layer (spec §8, Phase 4). It composes the storage
//! engine (pages, CAS, VLE), the MVCC clock, the SQL front-end, the WASM runtime,
//! and the compliance hook into one object with a small, honest API:
//!
//! * [`Database::open_dev`] / [`pv_open_dev`] — open or create a `.pv/` workspace.
//! * [`Database::open_prod`] / [`pv_open_prod`] — mmap a baked `.pvdb` read-only.
//! * [`Database::query`] — run a SQL statement.
//! * [`Database::bake`] / [`pv_bake`] — compile the workspace into a monolith.
//!
//! ## Persistence model (a documented simplification)
//!
//! The authoritative state is held in memory; in development mode every mutation
//! re-serializes the affected tables back into row pages and rewrites the
//! manifest (`autocommit`). This keeps the integration small and the page / CAS /
//! VLE machinery genuinely exercised, at the cost of write amplification — a
//! production engine would persist incrementally and run the cold-columnar
//! conversion on a timer. Those hooks ([`crate::storage::page::ColumnarPage`])
//! exist and are tested independently.

use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::core::errors::{PvError, Result};
use crate::core::types::{RecordEnvelope, TxId, PAGE_HEADER_SIZE, PAGE_SIZE};
use crate::core::value::{Row, Value};
use crate::engine::compliance::{ComplianceMonitor, RuntimeMetrics};
use crate::engine::mvcc::{Snapshot, TxManager};
use crate::engine::query::{parse, Statement};
use crate::engine::wasm::WasmRuntime;
use crate::storage::cas::CasStore;
use crate::storage::page::{RowPage, SLOT_SIZE};
use crate::storage::record::{decode_record, encode_record};
use crate::storage::vle::{bake_monolith, Backend, DevStore, Monolith, PageBuf};

/// Manifest file name within a development workspace.
pub const MANIFEST_FILE: &str = "pv_manifest.json";

/// Largest record (envelope + body) that can fit in a fresh page.
const MAX_RECORD: usize = PAGE_SIZE - PAGE_HEADER_SIZE - SLOT_SIZE;

// ---------------------------------------------------------------------------
// In-memory table state
// ---------------------------------------------------------------------------

struct VersionRow {
    envelope: RecordEnvelope,
    values: Row,
}

struct TableData {
    columns: Vec<String>,
    rows: Vec<VersionRow>,
}

/// In-memory table catalog, keyed by table name.
type TableMap = BTreeMap<String, TableData>;

/// Per-table list of page ids holding that table's records.
type PageMap = BTreeMap<String, Vec<u64>>;

// ---------------------------------------------------------------------------
// Persisted manifest
// ---------------------------------------------------------------------------

#[derive(Serialize, Deserialize, Default)]
struct Manifest {
    page_count: u64,
    clock: u64,
    tables: Vec<TableMeta>,
    cas_hashes: Vec<String>,
    #[serde(default)]
    cas_dir: Vec<(u64, u64)>,
}

#[derive(Serialize, Deserialize)]
struct TableMeta {
    name: String,
    columns: Vec<String>,
    pages: Vec<u64>,
}

// ---------------------------------------------------------------------------
// Query result
// ---------------------------------------------------------------------------

/// The outcome of [`Database::query`].
#[derive(Debug, Clone, PartialEq)]
pub enum QueryResult {
    /// A `SELECT` result set.
    Rows {
        /// Column names.
        columns: Vec<String>,
        /// Visible rows.
        rows: Vec<Row>,
    },
    /// Number of rows affected by an `INSERT`/`DELETE`.
    Mutated(usize),
    /// A statement with no result set (e.g. `CREATE TABLE`).
    Done,
}

impl QueryResult {
    /// Borrow the row set, if this is a `SELECT` result.
    pub fn rows(&self) -> Option<&[Row]> {
        match self {
            QueryResult::Rows { rows, .. } => Some(rows),
            _ => None,
        }
    }
}

// ---------------------------------------------------------------------------
// Database
// ---------------------------------------------------------------------------

/// A PicoVolt database handle.
pub struct Database {
    backend: Backend,
    cas: CasStore,
    txm: TxManager,
    tables: BTreeMap<String, TableData>,
    table_pages: BTreeMap<String, Vec<u64>>,
    compliance: ComplianceMonitor,
    root: Option<PathBuf>,
    autocommit: bool,
}

impl Database {
    /// Open (or create) a development workspace rooted at `path`.
    pub fn open_dev(path: impl AsRef<Path>) -> Result<Self> {
        let root = path.as_ref().to_path_buf();
        fs::create_dir_all(&root)?;
        let manifest_path = root.join(MANIFEST_FILE);

        if manifest_path.exists() {
            let manifest: Manifest = serde_json::from_slice(&fs::read(&manifest_path)?)?;
            let dev = DevStore::open(&root, manifest.page_count)?;
            let cas = CasStore::load_dev(&root, &manifest.cas_hashes)?;
            let backend = Backend::Dev(dev);
            let (tables, table_pages) = load_tables(&backend, &cas, &manifest.tables)?;
            Ok(Self {
                backend,
                cas,
                txm: TxManager::with_clock(manifest.clock),
                tables,
                table_pages,
                compliance: ComplianceMonitor::new(),
                root: Some(root),
                autocommit: true,
            })
        } else {
            let dev = DevStore::create(&root)?;
            Ok(Self {
                backend: Backend::Dev(dev),
                cas: CasStore::new_dev(&root),
                txm: TxManager::new(),
                tables: BTreeMap::new(),
                table_pages: BTreeMap::new(),
                compliance: ComplianceMonitor::new(),
                root: Some(root),
                autocommit: true,
            })
        }
    }

    /// Open a baked `.pvdb` monolith, read-only, via mmap.
    pub fn open_prod(path: impl AsRef<Path>) -> Result<Self> {
        let mono = Monolith::open(path)?;
        let manifest: Manifest = serde_json::from_slice(mono.manifest_bytes())?;
        let cas = CasStore::from_mapped(
            mono.mmap(),
            mono.cas_offset(),
            &manifest.cas_dir,
            &manifest.cas_hashes,
        )?;
        let backend = Backend::Prod(mono);
        let (tables, table_pages) = load_tables(&backend, &cas, &manifest.tables)?;
        Ok(Self {
            backend,
            cas,
            txm: TxManager::with_clock(manifest.clock),
            tables,
            table_pages,
            compliance: ComplianceMonitor::new(),
            root: None,
            autocommit: false,
        })
    }

    /// Compile the current workspace into a `.pvdb` monolith at `out_path`.
    pub fn bake(&mut self, out_path: impl AsRef<Path>) -> Result<()> {
        self.flush()?;
        let pages = match &self.backend {
            Backend::Dev(dev) => dev.read_all_pages()?,
            Backend::Prod(_) => return Err(PvError::ReadOnly),
        };
        let (cas_pool, _dir) = self.cas.pack()?;
        let manifest = self.build_manifest(true)?;
        let json = serde_json::to_vec(&manifest)?;
        bake_monolith(out_path, &pages, &cas_pool, &json)
    }

    /// Execute a single SQL statement.
    pub fn query(&mut self, sql: &str) -> Result<QueryResult> {
        match parse(sql)? {
            Statement::CreateTable { name, columns } => {
                self.create_table(&name, columns)?;
                Ok(QueryResult::Done)
            }
            Statement::Insert { table, values } => {
                self.insert(&table, values)?;
                Ok(QueryResult::Mutated(1))
            }
            Statement::Select { table, before } => {
                let (columns, rows) = self.select(&table, before)?;
                Ok(QueryResult::Rows { columns, rows })
            }
            Statement::Delete {
                table,
                column,
                value,
            } => {
                let n = self.delete(&table, &column, &value)?;
                Ok(QueryResult::Mutated(n))
            }
        }
    }

    // --- programmatic API (used by `query`, and directly) ------------------

    /// Create a table with the given column names.
    pub fn create_table(&mut self, name: &str, columns: Vec<String>) -> Result<()> {
        self.ensure_writable()?;
        if self.tables.contains_key(name) {
            return Err(PvError::Schema(format!("table `{name}` already exists")));
        }
        self.tables.insert(
            name.to_string(),
            TableData {
                columns,
                rows: Vec::new(),
            },
        );
        self.table_pages.insert(name.to_string(), Vec::new());
        self.maybe_flush()
    }

    /// Insert one row (a new MVCC version under a fresh transaction id).
    pub fn insert(&mut self, table: &str, values: Vec<Value>) -> Result<()> {
        self.ensure_writable()?;
        let arity = self
            .tables
            .get(table)
            .ok_or_else(|| PvError::TableNotFound(table.into()))?
            .columns
            .len();
        if values.len() != arity {
            return Err(PvError::Schema(format!(
                "table `{table}` expects {arity} columns, got {}",
                values.len()
            )));
        }
        let tx = self.txm.begin_write();
        let envelope = RecordEnvelope::new(tx, 0);
        self.tables
            .get_mut(table)
            .expect("existence checked above")
            .rows
            .push(VersionRow { envelope, values });
        self.maybe_flush()
    }

    /// Tombstone every currently-visible row whose `column` equals `value`.
    /// Returns the number of versions deleted.
    pub fn delete(&mut self, table: &str, column: &str, value: &Value) -> Result<usize> {
        self.ensure_writable()?;
        let tx = self.txm.begin_write();
        let table_data = self
            .tables
            .get_mut(table)
            .ok_or_else(|| PvError::TableNotFound(table.into()))?;
        let col_ix = table_data
            .columns
            .iter()
            .position(|c| c == column)
            .ok_or_else(|| PvError::Schema(format!("no column `{column}` in `{table}`")))?;
        let mut deleted = 0;
        for vrow in table_data.rows.iter_mut() {
            if vrow.envelope.is_active() && &vrow.values[col_ix] == value {
                vrow.envelope.mark_deleted(tx);
                deleted += 1;
            }
        }
        self.maybe_flush()?;
        Ok(deleted)
    }

    /// Read a table through a snapshot. `before = Some(tx)` time-travels to that
    /// transaction id; `None` reads the latest committed state.
    pub fn select(&self, table: &str, before: Option<u64>) -> Result<(Vec<String>, Vec<Row>)> {
        let table_data = self
            .tables
            .get(table)
            .ok_or_else(|| PvError::TableNotFound(table.into()))?;
        let snapshot = Snapshot::as_of(before.unwrap_or_else(|| self.txm.current()));
        let rows = table_data
            .rows
            .iter()
            .filter(|v| snapshot.sees(&v.envelope))
            .map(|v| v.values.clone())
            .collect();
        Ok((table_data.columns.clone(), rows))
    }

    // --- compliance & extensions -------------------------------------------

    /// Run the licensing compliance hook against the supplied metrics.
    pub fn assert_compliance(&self, metrics: &RuntimeMetrics) -> Result<()> {
        self.compliance
            .assert_compliance(metrics)
            .map_err(PvError::from)
    }

    /// Borrow the compliance monitor (to inspect thresholds).
    pub fn compliance_monitor(&self) -> &ComplianceMonitor {
        &self.compliance
    }

    /// Replace the compliance monitor (e.g. to register a commercial key policy).
    pub fn set_compliance_monitor(&mut self, monitor: ComplianceMonitor) {
        self.compliance = monitor;
    }

    /// Load a WASM extension and invoke `func(ptr, len) -> i32` over `input`.
    pub fn run_wasm_scalar(&self, wasm_bytes: &[u8], func: &str, input: &[u8]) -> Result<i32> {
        WasmRuntime::new()
            .load(wasm_bytes)?
            .call_scalar(func, input)
    }

    // --- introspection / control -------------------------------------------

    /// The most recently committed transaction id.
    pub fn current_tx(&self) -> TxId {
        self.txm.current()
    }

    /// Whether this handle accepts mutations.
    pub fn is_writable(&self) -> bool {
        self.backend.is_writable()
    }

    /// Names of all tables, sorted.
    pub fn table_names(&self) -> Vec<String> {
        self.tables.keys().cloned().collect()
    }

    /// Toggle eager persistence after each mutation (development mode only).
    pub fn set_autocommit(&mut self, on: bool) {
        self.autocommit = on;
    }

    /// Force a flush of in-memory state to the workspace.
    pub fn flush_now(&mut self) -> Result<()> {
        self.flush()
    }

    // --- internals ----------------------------------------------------------

    fn ensure_writable(&self) -> Result<()> {
        if self.backend.is_writable() {
            Ok(())
        } else {
            Err(PvError::ReadOnly)
        }
    }

    fn maybe_flush(&mut self) -> Result<()> {
        if self.autocommit {
            self.flush()
        } else {
            Ok(())
        }
    }

    fn flush(&mut self) -> Result<()> {
        let root = match &self.root {
            Some(r) => r.clone(),
            None => return Ok(()),
        };
        match &mut self.backend {
            Backend::Dev(dev) => {
                self.table_pages = rebuild_and_write_pages(dev, &mut self.cas, &self.tables)?;
            }
            Backend::Prod(_) => return Ok(()),
        }
        let manifest = self.build_manifest(false)?;
        fs::write(
            root.join(MANIFEST_FILE),
            serde_json::to_vec_pretty(&manifest)?,
        )?;
        Ok(())
    }

    fn build_manifest(&self, include_cas_dir: bool) -> Result<Manifest> {
        let mut cas_hashes = Vec::with_capacity(self.cas.len());
        for id in 0..self.cas.len() as u64 {
            cas_hashes.push(self.cas.hash_hex(id)?);
        }
        let cas_dir = if include_cas_dir {
            self.cas.pack()?.1
        } else {
            Vec::new()
        };
        let tables = self
            .tables
            .iter()
            .map(|(name, t)| TableMeta {
                name: name.clone(),
                columns: t.columns.clone(),
                pages: self.table_pages.get(name).cloned().unwrap_or_default(),
            })
            .collect();
        Ok(Manifest {
            page_count: self.backend.page_count(),
            clock: self.txm.current(),
            tables,
            cas_hashes,
            cas_dir,
        })
    }
}

// ---------------------------------------------------------------------------
// Free-function surface matching the specification's names
// ---------------------------------------------------------------------------

/// Open or create a development-mode database. See [`Database::open_dev`].
pub fn pv_open_dev(path: impl AsRef<Path>) -> Result<Database> {
    Database::open_dev(path)
}

/// Open a production-mode (baked) database. See [`Database::open_prod`].
pub fn pv_open_prod(path: impl AsRef<Path>) -> Result<Database> {
    Database::open_prod(path)
}

/// Compile a development workspace at `workspace` into a monolith at `out_path`.
pub fn pv_bake(workspace: impl AsRef<Path>, out_path: impl AsRef<Path>) -> Result<()> {
    Database::open_dev(workspace)?.bake(out_path)
}

// ---------------------------------------------------------------------------
// Shared persistence helpers
// ---------------------------------------------------------------------------

fn load_tables(
    backend: &Backend,
    cas: &CasStore,
    metas: &[TableMeta],
) -> Result<(TableMap, PageMap)> {
    let mut tables = BTreeMap::new();
    let mut table_pages = BTreeMap::new();
    for meta in metas {
        let mut rows = Vec::new();
        for &pid in &meta.pages {
            let page = RowPage::from_bytes(backend.read_page(pid)?)?;
            for (_slot, rec) in page.iter() {
                let (envelope, values) = decode_record(rec, cas)?;
                rows.push(VersionRow { envelope, values });
            }
        }
        tables.insert(
            meta.name.clone(),
            TableData {
                columns: meta.columns.clone(),
                rows,
            },
        );
        table_pages.insert(meta.name.clone(), meta.pages.clone());
    }
    Ok((tables, table_pages))
}

fn rebuild_and_write_pages(
    dev: &mut DevStore,
    cas: &mut CasStore,
    tables: &TableMap,
) -> Result<PageMap> {
    // Serialize all tables into an in-memory page set, then persist it with a
    // single bulk write per chunk file (see `DevStore::write_pages`).
    let mut pages: Vec<PageBuf> = Vec::new();
    let mut table_pages = BTreeMap::new();
    for (name, table) in tables {
        let mut ids = Vec::new();
        let mut current = RowPage::new(pages.len() as u64);
        let mut used = false;
        for vrow in &table.rows {
            let record = encode_record(&vrow.envelope, &vrow.values, cas)?;
            if record.len() > MAX_RECORD {
                return Err(PvError::Schema(format!(
                    "record of {} bytes exceeds page capacity ({MAX_RECORD})",
                    record.len()
                )));
            }
            match current.insert(&record) {
                Ok(_) => used = true,
                Err(PvError::PageFull { .. }) => {
                    ids.push(pages.len() as u64);
                    pages.push(current.into_bytes());
                    current = RowPage::new(pages.len() as u64);
                    current.insert(&record)?;
                    used = true;
                }
                Err(e) => return Err(e),
            }
        }
        if used {
            ids.push(pages.len() as u64);
            pages.push(current.into_bytes());
        }
        table_pages.insert(name.clone(), ids);
    }
    dev.write_pages(&pages)?;
    dev.set_page_count(pages.len() as u64);
    Ok(table_pages)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dev_insert_select_and_reopen() {
        let tmp = tempfile::tempdir().unwrap();
        let ws = tmp.path().join("ws");

        {
            let mut db = Database::open_dev(&ws).unwrap();
            db.query("CREATE TABLE users (id, name)").unwrap();
            db.query("INSERT INTO users VALUES (1, 'alice')").unwrap();
            db.query("INSERT INTO users VALUES (2, 'bob')").unwrap();
            let res = db.query("SELECT * FROM users").unwrap();
            assert_eq!(res.rows().unwrap().len(), 2);
        }

        // Reopen: state survives via the manifest + chunk files.
        let mut db = Database::open_dev(&ws).unwrap();
        let res = db.query("SELECT * FROM users").unwrap();
        assert_eq!(res.rows().unwrap().len(), 2);
        assert_eq!(db.table_names(), vec!["users".to_string()]);
    }

    #[test]
    fn delete_and_time_travel() {
        let tmp = tempfile::tempdir().unwrap();
        let mut db = Database::open_dev(tmp.path().join("ws")).unwrap();
        db.query("CREATE TABLE t (id)").unwrap();
        db.query("INSERT INTO t VALUES (1)").unwrap(); // tx 1
        db.query("INSERT INTO t VALUES (2)").unwrap(); // tx 2
        let before_delete = db.current_tx();
        let n = db.delete("t", "id", &Value::Int(1)).unwrap(); // tx 3
        assert_eq!(n, 1);

        // Latest view: only id=2 remains.
        let now = db.query("SELECT * FROM t").unwrap();
        assert_eq!(now.rows().unwrap().len(), 1);
        assert_eq!(now.rows().unwrap()[0][0], Value::Int(2));

        // Time-travel to before the delete: both rows visible again.
        let past = db
            .query(&format!("SELECT * FROM t BEFORE {before_delete}"))
            .unwrap();
        assert_eq!(past.rows().unwrap().len(), 2);
    }

    #[test]
    fn bake_then_open_prod_with_cas_blob() {
        let tmp = tempfile::tempdir().unwrap();
        let ws = tmp.path().join("ws");
        let monolith = tmp.path().join("app.pvdb");
        let big = "z".repeat(100); // > 16 bytes -> interned into CAS

        {
            let mut db = Database::open_dev(&ws).unwrap();
            db.query("CREATE TABLE docs (id, body)").unwrap();
            db.insert("docs", vec![Value::Int(1), Value::Text(big.clone())])
                .unwrap();
            db.bake(&monolith).unwrap();
        }

        let mut prod = Database::open_prod(&monolith).unwrap();
        assert!(!prod.is_writable());
        let res = prod.query("SELECT * FROM docs").unwrap();
        let rows = res.rows().unwrap();
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0][1], Value::Text(big)); // CAS blob resolved from mmap

        // Mutations are rejected in production mode.
        assert!(matches!(
            prod.query("INSERT INTO docs VALUES (2, 'x')"),
            Err(PvError::ReadOnly)
        ));
    }

    #[test]
    fn compliance_hook_is_wired_in() {
        let tmp = tempfile::tempdir().unwrap();
        let db = Database::open_dev(tmp.path().join("ws")).unwrap();
        assert!(db.assert_compliance(&RuntimeMetrics::free_tier()).is_ok());
        let over = RuntimeMetrics {
            current_mau: 60_000,
            monthly_revenue: 0.0,
            has_authorizing_key: false,
        };
        assert!(matches!(
            db.assert_compliance(&over),
            Err(PvError::Compliance(_))
        ));
    }
}
