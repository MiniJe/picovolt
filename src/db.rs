//! The outer developer surface: [`Database`] plus the dev/prod lifecycle.
//!
//! This is the integration layer. As of the page-backed engine it composes:
//!
//! * a **buffer pool** ([`crate::storage::cache::PageCache`]) so reads stream
//!   through a bounded set of resident pages — datasets need not fit in RAM;
//! * **append-only page chains** — inserts append to a table's tail page and
//!   write only that page (plus a small manifest), so autocommit is O(1) per
//!   insert instead of rewriting the whole table;
//! * **secondary indexes** ([`crate::storage::index`]) — opt-in equality indexes
//!   turn `WHERE col = value` into a lookup instead of a full scan.
//!
//! A table is a singly linked chain of row pages (each header points to the
//! next), so the manifest stores only a head page id per table — O(tables), not
//! O(pages) — keeping per-insert manifest writes cheap.

use std::cell::RefCell;
use std::collections::BTreeMap;
use std::fs::{self, File, OpenOptions};
use std::io::{Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::core::errors::{PvError, Result};
use crate::core::types::{
    pack_addr, unpack_addr, PageId, RecordAddr, RecordEnvelope, TxId, PAGE_HEADER_SIZE, PAGE_SIZE,
};
use crate::core::value::{Row, Value};
use crate::engine::compliance::{ComplianceMonitor, RuntimeMetrics};
use crate::engine::mvcc::{Snapshot, TxManager};
use crate::engine::query::{parse, Statement};
use crate::engine::wasm::WasmRuntime;
use crate::storage::cache::{PageCache, DEFAULT_CACHE_PAGES};
use crate::storage::cas::CasStore;
use crate::storage::index::SecondaryIndex;
use crate::storage::page::{RowPage, RowPageRef, SLOT_SIZE};
use crate::storage::record::{decode_record, encode_record};
use crate::storage::vle::{bake_monolith, Backend, DevStore, Monolith};

/// Manifest file name within a development workspace.
pub const MANIFEST_FILE: &str = "pv_manifest.json";

/// Largest record (envelope + body) that fits on a fresh page.
const MAX_RECORD: usize = PAGE_SIZE - PAGE_HEADER_SIZE - SLOT_SIZE;

// ---------------------------------------------------------------------------
// In-memory table metadata (bounded: O(tables), not O(rows))
// ---------------------------------------------------------------------------

struct Table {
    columns: Vec<String>,
    first_page: Option<PageId>,
    tail_id: Option<PageId>,
    /// Resident write buffer (the current tail page); `None` in read-only mode.
    tail: Option<RowPage>,
    row_versions: u64,
    indexes: BTreeMap<String, SecondaryIndex>,
}

// ---------------------------------------------------------------------------
// Persisted manifest
// ---------------------------------------------------------------------------

#[derive(Serialize, Deserialize, Default)]
struct Manifest {
    clock: u64,
    page_count: u64,
    tables: Vec<TableMeta>,
    cas_hashes: Vec<String>,
    #[serde(default)]
    cas_dir: Vec<(u64, u64)>,
}

#[derive(Serialize, Deserialize)]
struct TableMeta {
    name: String,
    columns: Vec<String>,
    first_page: Option<u64>,
    tail_id: Option<u64>,
    row_versions: u64,
    #[serde(default)]
    indexed_columns: Vec<String>,
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
    cache: RefCell<PageCache>,
    cas: CasStore,
    txm: TxManager,
    tables: BTreeMap<String, Table>,
    compliance: ComplianceMonitor,
    root: Option<PathBuf>,
    autocommit: bool,
    /// Cached write handle for the manifest, so autocommit doesn't reopen it.
    manifest_file: RefCell<Option<File>>,
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
            let mut cache = PageCache::new(Backend::Dev(dev), DEFAULT_CACHE_PAGES);
            let cas = CasStore::load_dev(&root, &manifest.cas_hashes)?;
            let tables = build_tables(&mut cache, &cas, &manifest, true)?;
            Ok(Self {
                cache: RefCell::new(cache),
                cas,
                txm: TxManager::with_clock(manifest.clock),
                tables,
                compliance: ComplianceMonitor::new(),
                root: Some(root),
                autocommit: true,
                manifest_file: RefCell::new(None),
            })
        } else {
            let dev = DevStore::create(&root)?;
            Ok(Self {
                cache: RefCell::new(PageCache::new(Backend::Dev(dev), DEFAULT_CACHE_PAGES)),
                cas: CasStore::new_dev(&root),
                txm: TxManager::new(),
                tables: BTreeMap::new(),
                compliance: ComplianceMonitor::new(),
                root: Some(root),
                autocommit: true,
                manifest_file: RefCell::new(None),
            })
        }
    }

    /// Open a baked `.pvdb` monolith, read-only, via mmap + buffer pool.
    pub fn open_prod(path: impl AsRef<Path>) -> Result<Self> {
        let mono = Monolith::open(path)?;
        let manifest: Manifest = serde_json::from_slice(mono.manifest_bytes())?;
        let cas = CasStore::from_mapped(
            mono.mmap(),
            mono.cas_offset(),
            &manifest.cas_dir,
            &manifest.cas_hashes,
        )?;
        let mut cache = PageCache::new(Backend::Prod(mono), DEFAULT_CACHE_PAGES);
        let tables = build_tables(&mut cache, &cas, &manifest, false)?;
        Ok(Self {
            cache: RefCell::new(cache),
            cas,
            txm: TxManager::with_clock(manifest.clock),
            tables,
            compliance: ComplianceMonitor::new(),
            root: None,
            autocommit: false,
            manifest_file: RefCell::new(None),
        })
    }

    /// Compile the current workspace into a `.pvdb` monolith at `out_path`.
    pub fn bake(&mut self, out_path: impl AsRef<Path>) -> Result<()> {
        self.flush()?;
        let pages = match self.cache.borrow().backend() {
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
            Statement::CreateIndex { table, column } => {
                self.create_index(&table, &column)?;
                Ok(QueryResult::Done)
            }
            Statement::Insert { table, values } => {
                self.insert(&table, values)?;
                Ok(QueryResult::Mutated(1))
            }
            Statement::Select {
                table,
                before,
                filter,
            } => {
                let (columns, rows) = match filter {
                    Some((column, value)) => self.select_where(&table, &column, &value, before)?,
                    None => self.select(&table, before)?,
                };
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

    // --- programmatic API --------------------------------------------------

    /// Create a table with the given column names.
    pub fn create_table(&mut self, name: &str, columns: Vec<String>) -> Result<()> {
        self.ensure_writable()?;
        if self.tables.contains_key(name) {
            return Err(PvError::Schema(format!("table `{name}` already exists")));
        }
        self.tables.insert(
            name.to_string(),
            Table {
                columns,
                first_page: None,
                tail_id: None,
                tail: None,
                row_versions: 0,
                indexes: BTreeMap::new(),
            },
        );
        self.maybe_flush()
    }

    /// Create an equality index on `column`, built from the current rows.
    pub fn create_index(&mut self, table_name: &str, column: &str) -> Result<()> {
        let mut index = SecondaryIndex::new();
        {
            let table = self
                .tables
                .get(table_name)
                .ok_or_else(|| PvError::TableNotFound(table_name.into()))?;
            let col_ix = column_index(table, column)?;
            let mut cache = self.cache.borrow_mut();
            scan(&mut cache, table, &self.cas, |addr, _env, row| {
                index.insert(&row[col_ix], addr);
                Ok(())
            })?;
        }
        self.tables
            .get_mut(table_name)
            .expect("existence checked above")
            .indexes
            .insert(column.to_string(), index);
        self.maybe_flush()
    }

    /// Insert one row (a new MVCC version under a fresh transaction id).
    pub fn insert(&mut self, table_name: &str, values: Vec<Value>) -> Result<()> {
        self.ensure_writable()?;
        let arity = self
            .tables
            .get(table_name)
            .ok_or_else(|| PvError::TableNotFound(table_name.into()))?
            .columns
            .len();
        if values.len() != arity {
            return Err(PvError::Schema(format!(
                "table `{table_name}` expects {arity} columns, got {}",
                values.len()
            )));
        }

        let tx = self.txm.begin_write();
        let envelope = RecordEnvelope::new(tx, 0);
        let record = encode_record(&envelope, &values, &mut self.cas)?;
        if record.len() > MAX_RECORD {
            return Err(PvError::Schema(format!(
                "record of {} bytes exceeds page capacity ({MAX_RECORD})",
                record.len()
            )));
        }

        let addr = {
            let mut cache = self.cache.borrow_mut();
            let table = self.tables.get_mut(table_name).expect("existence checked");
            append_record(&mut cache, table, &record)?
        };

        // Maintain any indexes on this table.
        let table = self.tables.get_mut(table_name).expect("existence checked");
        if !table.indexes.is_empty() {
            let indexed: Vec<usize> = table
                .columns
                .iter()
                .enumerate()
                .filter(|(_, c)| table.indexes.contains_key(*c))
                .map(|(i, _)| i)
                .collect();
            for ix in indexed {
                if let Some(index) = table.indexes.get_mut(&table.columns[ix]) {
                    index.insert(&values[ix], addr);
                }
            }
        }
        self.maybe_flush()
    }

    /// Tombstone every currently-visible row whose `column` equals `value`.
    pub fn delete(&mut self, table_name: &str, column: &str, value: &Value) -> Result<usize> {
        self.ensure_writable()?;
        let tx = self.txm.begin_write();
        let snapshot = self.txm.snapshot();

        let to_delete: Vec<RecordAddr> = {
            let table = self
                .tables
                .get(table_name)
                .ok_or_else(|| PvError::TableNotFound(table_name.into()))?;
            let col_ix = column_index(table, column)?;
            let mut cache = self.cache.borrow_mut();
            let mut hits = Vec::new();
            for_each_candidate(
                &mut cache,
                table,
                &self.cas,
                column,
                value,
                |addr, env, row| {
                    if snapshot.sees(env) && &row[col_ix] == value {
                        hits.push(addr);
                    }
                    Ok(())
                },
            )?;
            hits
        };

        let table = self.tables.get_mut(table_name).expect("existence checked");
        {
            let mut cache = self.cache.borrow_mut();
            for &addr in &to_delete {
                patch_delete_at(&mut cache, table, addr, tx)?;
            }
        }
        self.maybe_flush()?;
        Ok(to_delete.len())
    }

    /// Read a table through a snapshot. `before = Some(tx)` time-travels.
    pub fn select(&self, table_name: &str, before: Option<u64>) -> Result<(Vec<String>, Vec<Row>)> {
        let table = self
            .tables
            .get(table_name)
            .ok_or_else(|| PvError::TableNotFound(table_name.into()))?;
        let snapshot = Snapshot::as_of(before.unwrap_or_else(|| self.txm.current()));
        let columns = table.columns.clone();
        let mut rows = Vec::new();
        let mut cache = self.cache.borrow_mut();
        scan(&mut cache, table, &self.cas, |_addr, env, row| {
            if snapshot.sees(env) {
                rows.push(row.clone());
            }
            Ok(())
        })?;
        Ok((columns, rows))
    }

    /// Read rows where `column == value`, using a secondary index if one exists
    /// (otherwise a filtered scan). `before` optionally time-travels.
    pub fn select_where(
        &self,
        table_name: &str,
        column: &str,
        value: &Value,
        before: Option<u64>,
    ) -> Result<(Vec<String>, Vec<Row>)> {
        let table = self
            .tables
            .get(table_name)
            .ok_or_else(|| PvError::TableNotFound(table_name.into()))?;
        let col_ix = column_index(table, column)?;
        let snapshot = Snapshot::as_of(before.unwrap_or_else(|| self.txm.current()));
        let columns = table.columns.clone();
        let mut rows = Vec::new();
        let mut cache = self.cache.borrow_mut();
        for_each_candidate(
            &mut cache,
            table,
            &self.cas,
            column,
            value,
            |_addr, env, row| {
                if snapshot.sees(env) && &row[col_ix] == value {
                    rows.push(row.clone());
                }
                Ok(())
            },
        )?;
        Ok((columns, rows))
    }

    // --- compliance & extensions -------------------------------------------

    /// Run the licensing compliance hook against the supplied metrics.
    pub fn assert_compliance(&self, metrics: &RuntimeMetrics) -> Result<()> {
        self.compliance
            .assert_compliance(metrics)
            .map_err(PvError::from)
    }

    /// Borrow the compliance monitor.
    pub fn compliance_monitor(&self) -> &ComplianceMonitor {
        &self.compliance
    }

    /// Replace the compliance monitor.
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
        self.cache.borrow().is_writable()
    }

    /// Names of all tables, sorted.
    pub fn table_names(&self) -> Vec<String> {
        self.tables.keys().cloned().collect()
    }

    /// Toggle eager persistence after each mutation (development mode only).
    pub fn set_autocommit(&mut self, on: bool) {
        self.autocommit = on;
    }

    /// Resize the buffer pool (in pages). Smaller bounds memory; larger caches more.
    pub fn set_cache_capacity(&self, pages: usize) -> Result<()> {
        self.cache.borrow_mut().set_capacity(pages)
    }

    /// Number of pages currently resident in the buffer pool.
    pub fn cache_resident(&self) -> usize {
        self.cache.borrow().resident()
    }

    /// Force a flush of in-memory state to the workspace.
    pub fn flush_now(&mut self) -> Result<()> {
        self.flush()
    }

    // --- internals ----------------------------------------------------------

    fn ensure_writable(&self) -> Result<()> {
        if self.cache.borrow().is_writable() {
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
        if !self.cache.borrow().is_writable() {
            return Ok(());
        }
        {
            let mut cache = self.cache.borrow_mut();
            for table in self.tables.values() {
                if let (Some(id), Some(tail)) = (table.tail_id, &table.tail) {
                    cache.write(id, Box::new(*tail.as_bytes()))?;
                }
            }
            cache.flush()?;
        }
        let manifest = self.build_manifest(false)?;
        let json = serde_json::to_vec_pretty(&manifest)?;
        let mut slot = self.manifest_file.borrow_mut();
        if slot.is_none() {
            *slot = Some(
                OpenOptions::new()
                    .read(true)
                    .write(true)
                    .create(true)
                    .truncate(false)
                    .open(root.join(MANIFEST_FILE))?,
            );
        }
        let file = slot.as_mut().expect("manifest handle present");
        file.seek(SeekFrom::Start(0))?;
        file.write_all(&json)?;
        file.set_len(json.len() as u64)?;
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
                first_page: t.first_page,
                tail_id: t.tail_id,
                row_versions: t.row_versions,
                indexed_columns: t.indexes.keys().cloned().collect(),
            })
            .collect();
        let page_count = self.cache.borrow().backend().page_count();
        Ok(Manifest {
            clock: self.txm.current(),
            page_count,
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
// Page-backed helpers (free functions to keep field borrows disjoint)
// ---------------------------------------------------------------------------

fn column_index(table: &Table, column: &str) -> Result<usize> {
    table
        .columns
        .iter()
        .position(|c| c == column)
        .ok_or_else(|| PvError::Schema(format!("no column `{column}`")))
}

/// Append `record` to a table's tail page, allocating + linking a new page when
/// the tail is full. Returns the new record's stable address.
fn append_record(cache: &mut PageCache, table: &mut Table, record: &[u8]) -> Result<RecordAddr> {
    if table.tail.is_none() {
        let id = cache.alloc_page()?;
        table.tail = Some(RowPage::new(id));
        table.tail_id = Some(id);
        if table.first_page.is_none() {
            table.first_page = Some(id);
        }
    }
    let tail_id = table.tail_id.expect("tail set above");
    match table.tail.as_mut().expect("tail set above").insert(record) {
        Ok(slot) => {
            table.row_versions += 1;
            Ok(pack_addr(tail_id, slot))
        }
        Err(PvError::PageFull { .. }) => {
            let new_id = cache.alloc_page()?;
            let mut finalized = table.tail.take().expect("tail set above");
            finalized.set_next_page(Some(new_id));
            cache.write(tail_id, finalized.into_bytes())?;
            let mut fresh = RowPage::new(new_id);
            let slot = fresh.insert(record)?;
            table.tail = Some(fresh);
            table.tail_id = Some(new_id);
            table.row_versions += 1;
            Ok(pack_addr(new_id, slot))
        }
        Err(e) => Err(e),
    }
}

/// Read the record at `addr`, consulting the resident tail page when applicable.
fn read_record_at(
    cache: &mut PageCache,
    table: &Table,
    cas: &CasStore,
    addr: RecordAddr,
) -> Result<(RecordEnvelope, Row)> {
    let (pid, slot) = unpack_addr(addr);
    if Some(pid) == table.tail_id {
        if let Some(tail) = &table.tail {
            return decode_record(tail.record(slot)?, cas);
        }
    }
    cache.with_page(pid, |buf| {
        let page = RowPageRef::new(buf)?;
        decode_record(page.record(slot)?, cas)
    })
}

/// Visit every record version in a table, following the page chain through the
/// buffer pool (bounded memory) and the resident tail.
fn scan(
    cache: &mut PageCache,
    table: &Table,
    cas: &CasStore,
    mut visit: impl FnMut(RecordAddr, &RecordEnvelope, &Row) -> Result<()>,
) -> Result<()> {
    let mut next = table.first_page;
    while let Some(pid) = next {
        if Some(pid) == table.tail_id {
            if let Some(tail) = &table.tail {
                for (slot, rec) in tail.iter() {
                    let (env, row) = decode_record(rec, cas)?;
                    visit(pack_addr(pid, slot), &env, &row)?;
                }
                next = tail.next_page();
                continue;
            }
        }
        next = cache.with_page(pid, |buf| {
            let page = RowPageRef::new(buf)?;
            for (slot, rec) in page.iter() {
                let (env, row) = decode_record(rec, cas)?;
                visit(pack_addr(pid, slot), &env, &row)?;
            }
            Ok(page.next_page())
        })?;
    }
    Ok(())
}

/// Visit candidate records for `column == value`: index lookups when an index
/// exists, otherwise a full scan.
fn for_each_candidate(
    cache: &mut PageCache,
    table: &Table,
    cas: &CasStore,
    column: &str,
    value: &Value,
    mut visit: impl FnMut(RecordAddr, &RecordEnvelope, &Row) -> Result<()>,
) -> Result<()> {
    if let Some(index) = table.indexes.get(column) {
        for &addr in index.lookup(value) {
            let (env, row) = read_record_at(cache, table, cas, addr)?;
            visit(addr, &env, &row)?;
        }
        Ok(())
    } else {
        scan(cache, table, cas, |addr, env, row| visit(addr, env, row))
    }
}

/// Tombstone the record at `addr` under transaction `tx`.
fn patch_delete_at(
    cache: &mut PageCache,
    table: &mut Table,
    addr: RecordAddr,
    tx: TxId,
) -> Result<()> {
    let (pid, slot) = unpack_addr(addr);
    if Some(pid) == table.tail_id {
        if let Some(tail) = table.tail.as_mut() {
            return tail.patch_envelope_deleted(slot, tx);
        }
    }
    cache.with_page_mut(pid, |page| page.patch_envelope_deleted(slot, tx))
}

/// Reconstruct in-memory table metadata (and indexes) from a manifest.
fn build_tables(
    cache: &mut PageCache,
    cas: &CasStore,
    manifest: &Manifest,
    writable: bool,
) -> Result<BTreeMap<String, Table>> {
    let mut tables = BTreeMap::new();
    for meta in &manifest.tables {
        let mut table = Table {
            columns: meta.columns.clone(),
            first_page: meta.first_page,
            tail_id: meta.tail_id,
            tail: None,
            row_versions: meta.row_versions,
            indexes: BTreeMap::new(),
        };
        // In development mode, load the tail page resident so appends continue.
        if writable {
            if let Some(id) = meta.tail_id {
                let buf = cache.backend().read_page(id)?;
                table.tail = Some(RowPage::from_bytes(buf)?);
            }
        }
        tables.insert(meta.name.clone(), table);
    }

    // Rebuild indexes via streaming scans (bounded memory).
    for meta in &manifest.tables {
        for column in &meta.indexed_columns {
            let table = tables.get(&meta.name).expect("just inserted");
            let col_ix = column_index(table, column)?;
            let mut index = SecondaryIndex::new();
            scan(cache, table, cas, |addr, _env, row| {
                index.insert(&row[col_ix], addr);
                Ok(())
            })?;
            tables
                .get_mut(&meta.name)
                .expect("just inserted")
                .indexes
                .insert(column.clone(), index);
        }
    }
    Ok(tables)
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
            assert_eq!(
                db.query("SELECT * FROM users")
                    .unwrap()
                    .rows()
                    .unwrap()
                    .len(),
                2
            );
        }
        let mut db = Database::open_dev(&ws).unwrap();
        assert_eq!(
            db.query("SELECT * FROM users")
                .unwrap()
                .rows()
                .unwrap()
                .len(),
            2
        );
        assert_eq!(db.table_names(), vec!["users".to_string()]);
    }

    #[test]
    fn delete_and_time_travel() {
        let tmp = tempfile::tempdir().unwrap();
        let mut db = Database::open_dev(tmp.path().join("ws")).unwrap();
        db.query("CREATE TABLE t (id)").unwrap();
        db.query("INSERT INTO t VALUES (1)").unwrap();
        db.query("INSERT INTO t VALUES (2)").unwrap();
        let before_delete = db.current_tx();
        assert_eq!(db.delete("t", "id", &Value::Int(1)).unwrap(), 1);

        let now = db.query("SELECT * FROM t").unwrap();
        assert_eq!(now.rows().unwrap().len(), 1);
        assert_eq!(now.rows().unwrap()[0][0], Value::Int(2));

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
        let big = "z".repeat(100);
        {
            let mut db = Database::open_dev(&ws).unwrap();
            db.query("CREATE TABLE docs (id, body)").unwrap();
            db.insert("docs", vec![Value::Int(1), Value::Text(big.clone())])
                .unwrap();
            db.bake(&monolith).unwrap();
        }
        let mut prod = Database::open_prod(&monolith).unwrap();
        assert!(!prod.is_writable());
        let rows = prod.query("SELECT * FROM docs").unwrap();
        assert_eq!(rows.rows().unwrap()[0][1], Value::Text(big));
        assert!(matches!(
            prod.query("INSERT INTO docs VALUES (2, 'x')"),
            Err(PvError::ReadOnly)
        ));
    }

    #[test]
    fn spans_many_pages_with_a_tiny_cache() {
        // Larger-than-RAM proof: 5,000 rows across many pages, cache capped at
        // 4 pages. Correct results without holding the dataset resident.
        let tmp = tempfile::tempdir().unwrap();
        let mut db = Database::open_dev(tmp.path().join("ws")).unwrap();
        db.set_autocommit(false);
        db.set_cache_capacity(4).unwrap();
        db.query("CREATE TABLE t (id, pad)").unwrap();
        for i in 0..5_000i64 {
            db.insert("t", vec![Value::Int(i), Value::Int(i * 3)])
                .unwrap();
        }
        db.flush_now().unwrap();
        let (_c, rows) = db.select("t", None).unwrap();
        assert_eq!(rows.len(), 5_000);
        assert!(db.cache_resident() <= 5, "buffer pool must stay bounded");
        // Spot-check a row that lives well past the first page.
        assert_eq!(rows[4_999], vec![Value::Int(4_999), Value::Int(14_997)]);
    }

    #[test]
    fn index_lookup_matches_scan_and_survives_reopen() {
        let tmp = tempfile::tempdir().unwrap();
        let ws = tmp.path().join("ws");
        {
            let mut db = Database::open_dev(&ws).unwrap();
            db.query("CREATE TABLE events (id, kind)").unwrap();
            db.set_autocommit(false);
            for i in 0..1_000i64 {
                let kind = if i % 7 == 0 { "rare" } else { "common" };
                db.insert("events", vec![Value::Int(i), Value::from(kind)])
                    .unwrap();
            }
            db.query("CREATE INDEX ON events (kind)").unwrap();
            db.flush_now().unwrap();

            let indexed = db
                .query("SELECT * FROM events WHERE kind = 'rare'")
                .unwrap();
            assert_eq!(indexed.rows().unwrap().len(), 143); // 0,7,...,994
        }
        // Index is rebuilt on reopen.
        let mut db = Database::open_dev(&ws).unwrap();
        let indexed = db
            .query("SELECT * FROM events WHERE kind = 'rare'")
            .unwrap();
        assert_eq!(indexed.rows().unwrap().len(), 143);
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
