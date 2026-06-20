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
    pack_addr, unpack_addr, FileHeader, PageId, RecordAddr, RecordEnvelope, TxId, FILE_HEADER_SIZE,
    PAGE_HEADER_SIZE, PAGE_SIZE,
};
use crate::core::value::{Row, Value};
use crate::engine::compliance::{ComplianceMonitor, RuntimeMetrics};
use crate::engine::mvcc::{Snapshot, TxManager};
use crate::engine::query::{parse, OrderBy, Projection, Statement};
use crate::engine::wasm::WasmRuntime;
use crate::storage::cache::{PageCache, DEFAULT_CACHE_PAGES};
use crate::storage::cas::CasStore;
use crate::storage::index::SecondaryIndex;
use crate::storage::page::{RowPage, RowPageRef, SLOT_SIZE};
use crate::storage::record::{decode_record, encode_record};
use crate::storage::vle::{bake_monolith_bytes, Backend, DevStore, MemStore, Monolith};

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

    /// Borrow the column names, if this is a `SELECT` result.
    pub fn columns(&self) -> Option<&[String]> {
        match self {
            QueryResult::Rows { columns, .. } => Some(columns),
            _ => None,
        }
    }
}

// ---------------------------------------------------------------------------
// Database
// ---------------------------------------------------------------------------

/// Durability policy for flushes.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Durability {
    /// Fast (default): writes land in the OS page cache; durable on clean exit
    /// but a power-loss crash can lose recent writes. No `fsync`.
    #[default]
    Fast,
    /// Crash-safe: each flush `fsync`s the data pages and commits the manifest
    /// atomically (write-temp + `fsync` + rename). Much slower per flush.
    Sync,
}

/// A PicoVolt database handle.
pub struct Database {
    cache: RefCell<PageCache>,
    cas: CasStore,
    txm: TxManager,
    tables: BTreeMap<String, Table>,
    compliance: ComplianceMonitor,
    root: Option<PathBuf>,
    autocommit: bool,
    durability: Durability,
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
                durability: Durability::Fast,
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
                durability: Durability::Fast,
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
            durability: Durability::Fast,
            manifest_file: RefCell::new(None),
        })
    }

    /// Open a fresh in-memory database (no filesystem or mmap).
    ///
    /// Ideal for tests, ephemeral data, and `wasm32` targets (browser / Node)
    /// where there is no filesystem. Data lives only in RAM — export it with
    /// [`bake_to_bytes`](Self::bake_to_bytes) to persist.
    pub fn open_memory() -> Self {
        Self {
            cache: RefCell::new(PageCache::new(
                Backend::Mem(MemStore::new()),
                DEFAULT_CACHE_PAGES,
            )),
            cas: CasStore::new_memory(),
            txm: TxManager::new(),
            tables: BTreeMap::new(),
            compliance: ComplianceMonitor::new(),
            root: None,
            autocommit: false,
            durability: Durability::Fast,
            manifest_file: RefCell::new(None),
        }
    }

    /// Load a baked `.pvdb` **byte image** into a fresh, **writable** in-memory
    /// database — the inverse of [`bake_to_bytes`](Self::bake_to_bytes).
    ///
    /// Unlike [`open_prod`](Self::open_prod) (read-only, mmap'd), this copies the
    /// pages into RAM so editing can continue, and it preserves the full MVCC
    /// history (so `... BEFORE tx` time-travel survives a round trip). The input
    /// is untrusted: all offsets, the CAS directory, and the page chains are
    /// bounds-checked, so a malformed image yields an error, never a panic.
    pub fn import_bytes(bytes: &[u8]) -> Result<Self> {
        let header = FileHeader::decode(bytes)?; // validates the magic signature
        let cas_offset = header.cas_offset as usize;
        let manifest_offset = header.manifest_offset as usize;
        if cas_offset < FILE_HEADER_SIZE
            || manifest_offset < cas_offset
            || manifest_offset > bytes.len()
            || (cas_offset - FILE_HEADER_SIZE) % PAGE_SIZE != 0
        {
            return Err(PvError::Corruption("import: inconsistent offsets".into()));
        }
        let manifest: Manifest = serde_json::from_slice(&bytes[manifest_offset..])?;

        // Copy the page-data block into an in-memory store.
        let mem = MemStore::new();
        let page_count = (cas_offset - FILE_HEADER_SIZE) / PAGE_SIZE;
        for i in 0..page_count {
            let start = FILE_HEADER_SIZE + i * PAGE_SIZE;
            let page: &[u8; PAGE_SIZE] = bytes[start..start + PAGE_SIZE]
                .try_into()
                .expect("slice is exactly PAGE_SIZE");
            let id = mem.alloc_page();
            mem.write_page(id, page)?;
        }

        // Rebuild the CAS pool in memory, validating every blob extent.
        if manifest.cas_dir.len() != manifest.cas_hashes.len() {
            return Err(PvError::Corruption(
                "import: CAS dir/hash length mismatch".into(),
            ));
        }
        let pool = &bytes[cas_offset..manifest_offset];
        let mut cas = CasStore::new_memory();
        for &(off, len) in &manifest.cas_dir {
            let off = off as usize;
            let end = off
                .checked_add(len as usize)
                .filter(|&e| e <= pool.len())
                .ok_or_else(|| PvError::Corruption("import: CAS blob out of bounds".into()))?;
            cas.put(&pool[off..end])?;
        }

        let mut cache = PageCache::new(Backend::Mem(mem), DEFAULT_CACHE_PAGES);
        let tables = build_tables(&mut cache, &cas, &manifest, true)?;
        Ok(Self {
            cache: RefCell::new(cache),
            cas,
            txm: TxManager::with_clock(manifest.clock),
            tables,
            compliance: ComplianceMonitor::new(),
            root: None,
            autocommit: false,
            durability: Durability::Fast,
            manifest_file: RefCell::new(None),
        })
    }

    /// Compile the current database into a `.pvdb` monolith **byte image** (no
    /// filesystem). Works for any backend; the natural way to export an
    /// in-memory database.
    pub fn bake_to_bytes(&mut self) -> Result<Vec<u8>> {
        self.flush()?;
        let pages = self.cache.borrow().backend().read_all_pages()?;
        let (cas_pool, _dir) = self.cas.pack()?;
        let manifest = self.build_manifest(true)?;
        let json = serde_json::to_vec(&manifest)?;
        bake_monolith_bytes(&pages, &cas_pool, &json)
    }

    /// Compile the current database into a `.pvdb` monolith at `out_path`.
    pub fn bake(&mut self, out_path: impl AsRef<Path>) -> Result<()> {
        let bytes = self.bake_to_bytes()?;
        fs::write(out_path, bytes)?;
        Ok(())
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
                projection,
                before,
                filter,
                order,
                limit,
            } => {
                let (columns, rows) = match filter {
                    Some((column, value)) => self.select_where(&table, &column, &value, before)?,
                    None => self.select(&table, before)?,
                };
                project_select(columns, rows, projection, order, limit)
            }
            Statement::Update { table, set, filter } => {
                let n = self.update(&table, &set.0, &set.1, &filter.0, &filter.1)?;
                Ok(QueryResult::Mutated(n))
            }
            Statement::Delete {
                table,
                column,
                value,
            } => {
                let n = self.delete(&table, &column, &value)?;
                Ok(QueryResult::Mutated(n))
            }
            Statement::DropTable { table } => {
                self.drop_table(&table)?;
                Ok(QueryResult::Done)
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

    /// Update rows where `where_column == where_value`, assigning `set_value` to
    /// `set_column`. Append-only (MVCC): matching versions are tombstoned and new
    /// versions carrying the change are inserted. Returns the number updated.
    pub fn update(
        &mut self,
        table_name: &str,
        set_column: &str,
        set_value: &Value,
        where_column: &str,
        where_value: &Value,
    ) -> Result<usize> {
        self.ensure_writable()?;

        // Collect matching visible rows (address + a copy of their values).
        let (set_ix, matches) = {
            let table = self
                .tables
                .get(table_name)
                .ok_or_else(|| PvError::TableNotFound(table_name.into()))?;
            let set_ix = column_index(table, set_column)?;
            let where_ix = column_index(table, where_column)?;
            let snapshot = self.txm.snapshot();
            let mut hits: Vec<(RecordAddr, Row)> = Vec::new();
            let mut cache = self.cache.borrow_mut();
            for_each_candidate(
                &mut cache,
                table,
                &self.cas,
                where_column,
                where_value,
                |addr, env, row| {
                    if snapshot.sees(env) && &row[where_ix] == where_value {
                        hits.push((addr, row.clone()));
                    }
                    Ok(())
                },
            )?;
            (set_ix, hits)
        };

        let count = matches.len();
        if count == 0 {
            return Ok(0);
        }

        // Tombstone the old versions, then insert updated copies.
        let del_tx = self.txm.begin_write();
        {
            let table = self.tables.get_mut(table_name).expect("existence checked");
            let mut cache = self.cache.borrow_mut();
            for (addr, _) in &matches {
                patch_delete_at(&mut cache, table, *addr, del_tx)?;
            }
        }
        for (_, mut row) in matches {
            row[set_ix] = set_value.clone();
            self.insert(table_name, row)?;
        }
        self.maybe_flush()?;
        Ok(count)
    }

    /// Drop a table from the catalog. (Its pages are orphaned until a future
    /// vacuum reclaims them.)
    pub fn drop_table(&mut self, name: &str) -> Result<()> {
        self.ensure_writable()?;
        if self.tables.remove(name).is_none() {
            return Err(PvError::TableNotFound(name.into()));
        }
        self.maybe_flush()
    }

    /// Count rows visible at the given snapshot, without materializing them.
    pub fn row_count(&self, table_name: &str, before: Option<u64>) -> Result<usize> {
        let table = self
            .tables
            .get(table_name)
            .ok_or_else(|| PvError::TableNotFound(table_name.into()))?;
        let snapshot = Snapshot::as_of(before.unwrap_or_else(|| self.txm.current()));
        let mut count = 0usize;
        let mut cache = self.cache.borrow_mut();
        scan(&mut cache, table, &self.cas, |_, env, _| {
            if snapshot.sees(env) {
                count += 1;
            }
            Ok(())
        })?;
        Ok(count)
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

    /// Load a WASM extension and invoke `func(ptr, len) -> i32` over `input`,
    /// returning the scalar result. See [`crate::engine::wasm`] for the guest ABI.
    ///
    /// This is the supported seam for sandboxed third-party extensions; pair it
    /// with [`run_wasm_apply`](Database::run_wasm_apply) for byte-stream output.
    pub fn run_wasm_scalar(&self, wasm_bytes: &[u8], func: &str, input: &[u8]) -> Result<i32> {
        WasmRuntime::new()
            .load(wasm_bytes)?
            .call_scalar(func, input)
    }

    /// Load a WASM extension, invoke `func(ptr, len) -> i32` over `input`, and
    /// read the (in-place mutated) output region back out as bytes — the
    /// transform counterpart to [`run_wasm_scalar`](Database::run_wasm_scalar).
    pub fn run_wasm_apply(&self, wasm_bytes: &[u8], func: &str, input: &[u8]) -> Result<Vec<u8>> {
        WasmRuntime::new()
            .load(wasm_bytes)?
            .apply_in_place(func, input)
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

    /// Set the durability policy applied on each flush. See [`Durability`].
    pub fn set_durability(&mut self, durability: Durability) {
        self.durability = durability;
    }

    /// The current durability policy.
    pub fn durability(&self) -> Durability {
        self.durability
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
        if !self.cache.borrow().is_writable() {
            return Ok(()); // production / read-only: nothing to flush
        }
        // fsync only makes sense for a filesystem-backed (dev) database.
        let durable = self.durability == Durability::Sync && self.root.is_some();
        {
            let mut cache = self.cache.borrow_mut();
            for table in self.tables.values() {
                if let (Some(id), Some(tail)) = (table.tail_id, &table.tail) {
                    cache.write(id, Box::new(*tail.as_bytes()))?;
                }
            }
            cache.flush()?;
            // Crash-safety: data pages are fsync'd BEFORE the manifest commits,
            // so the manifest never references unflushed pages.
            if durable {
                cache.sync()?;
            }
        }
        // The manifest only exists for filesystem-backed databases; an in-memory
        // database (no `root`) keeps its catalog in RAM.
        let Some(root) = self.root.clone() else {
            return Ok(());
        };
        let manifest = self.build_manifest(false)?;
        let json = serde_json::to_vec_pretty(&manifest)?;
        if self.durability == Durability::Sync {
            self.write_manifest_atomic(&root, &json)
        } else {
            self.write_manifest_fast(&root, &json)
        }
    }

    /// Overwrite the manifest in place through a cached handle (fast, not atomic).
    fn write_manifest_fast(&self, root: &Path, json: &[u8]) -> Result<()> {
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
        file.write_all(json)?;
        file.set_len(json.len() as u64)?;
        Ok(())
    }

    /// Commit the manifest atomically: write a temp file, `fsync` it, then rename
    /// over the live manifest. After a crash either the old or new manifest is
    /// present in full — never a torn write.
    fn write_manifest_atomic(&self, root: &Path, json: &[u8]) -> Result<()> {
        // The renamed-away inode would orphan a cached handle; drop it.
        *self.manifest_file.borrow_mut() = None;
        let tmp = root.join("pv_manifest.json.tmp");
        {
            let mut file = File::create(&tmp)?;
            file.write_all(json)?;
            file.sync_all()?;
        }
        fs::rename(&tmp, root.join(MANIFEST_FILE))?;
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
// Query post-processing: projection, ORDER BY, LIMIT
// ---------------------------------------------------------------------------

/// Apply `COUNT(*)` / column projection, `ORDER BY`, and `LIMIT` to a result set.
fn project_select(
    columns: Vec<String>,
    mut rows: Vec<Row>,
    projection: Projection,
    order: Option<OrderBy>,
    limit: Option<usize>,
) -> Result<QueryResult> {
    if matches!(projection, Projection::Count) {
        return Ok(QueryResult::Rows {
            columns: vec!["count".to_string()],
            rows: vec![vec![Value::Int(rows.len() as i64)]],
        });
    }

    // Sort on the full row, before projection can drop the sort column.
    if let Some(ob) = &order {
        let ix = columns
            .iter()
            .position(|c| c == &ob.column)
            .ok_or_else(|| PvError::Schema(format!("no column `{}` to order by", ob.column)))?;
        rows.sort_by(|a, b| {
            let o = cmp_values(&a[ix], &b[ix]);
            if ob.descending {
                o.reverse()
            } else {
                o
            }
        });
    }

    let (out_columns, mut out_rows) = match projection {
        Projection::All => (columns, rows),
        Projection::Columns(cols) => {
            let idxs = cols
                .iter()
                .map(|c| {
                    columns
                        .iter()
                        .position(|x| x == c)
                        .ok_or_else(|| PvError::Schema(format!("no column `{c}`")))
                })
                .collect::<Result<Vec<_>>>()?;
            let projected = rows
                .into_iter()
                .map(|r| idxs.iter().map(|&i| r[i].clone()).collect())
                .collect();
            (cols, projected)
        }
        Projection::Count => unreachable!("handled above"),
    };

    if let Some(n) = limit {
        out_rows.truncate(n);
    }
    Ok(QueryResult::Rows {
        columns: out_columns,
        rows: out_rows,
    })
}

/// Total ordering over values for `ORDER BY` (Null &lt; Int &lt; Text &lt; Blob).
fn cmp_values(a: &Value, b: &Value) -> std::cmp::Ordering {
    fn rank(v: &Value) -> u8 {
        match v {
            Value::Null => 0,
            Value::Int(_) => 1,
            Value::Text(_) => 2,
            Value::Blob(_) => 3,
        }
    }
    match (a, b) {
        (Value::Int(x), Value::Int(y)) => x.cmp(y),
        (Value::Text(x), Value::Text(y)) => x.cmp(y),
        (Value::Blob(x), Value::Blob(y)) => x.cmp(y),
        (Value::Null, Value::Null) => std::cmp::Ordering::Equal,
        _ => rank(a).cmp(&rank(b)),
    }
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
    // SECURITY: bound the traversal so a crafted cyclic `next_page` chain (e.g. a
    // page that links to itself) cannot loop forever. A valid chain visits each
    // page at most once, so it can never exceed the total page count.
    let max_hops = cache.backend().page_count().saturating_add(1);
    let mut hops = 0u64;
    let mut next = table.first_page;
    while let Some(pid) = next {
        hops += 1;
        if hops > max_hops {
            return Err(PvError::Corruption(
                "page chain longer than total page count (cycle?)".into(),
            ));
        }
        if Some(pid) == table.tail_id {
            if let Some(tail) = &table.tail {
                for slot in 0..tail.slot_count() {
                    let (env, row) = decode_record(tail.record(slot)?, cas)?;
                    visit(pack_addr(pid, slot), &env, &row)?;
                }
                next = tail.next_page();
                continue;
            }
        }
        next = cache.with_page(pid, |buf| {
            let page = RowPageRef::new(buf)?;
            for slot in 0..page.slot_count() {
                let (env, row) = decode_record(page.record(slot)?, cas)?;
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
    fn projection_order_by_and_count() {
        let mut db = Database::open_memory();
        db.query("CREATE TABLE p (id, name)").unwrap();
        db.query("INSERT INTO p VALUES (3, 'carol')").unwrap();
        db.query("INSERT INTO p VALUES (1, 'alice')").unwrap();
        db.query("INSERT INTO p VALUES (2, 'bob')").unwrap();

        // COUNT(*) returns a single count row.
        let c = db.query("SELECT COUNT(*) FROM p").unwrap();
        assert_eq!(c.columns().unwrap(), &["count".to_string()]);
        assert_eq!(c.rows().unwrap(), &[vec![Value::Int(3)]]);

        // ORDER BY ascending (default).
        let asc = db.query("SELECT id FROM p ORDER BY id").unwrap();
        assert_eq!(asc.columns().unwrap(), &["id".to_string()]);
        let ids: Vec<_> = asc.rows().unwrap().iter().map(|r| r[0].clone()).collect();
        assert_eq!(ids, vec![Value::Int(1), Value::Int(2), Value::Int(3)]);

        // ORDER BY DESC + column projection drops `id` from the output but still
        // sorts by it.
        let desc = db.query("SELECT name FROM p ORDER BY id DESC").unwrap();
        assert_eq!(desc.columns().unwrap(), &["name".to_string()]);
        let names: Vec<_> = desc.rows().unwrap().iter().map(|r| r[0].clone()).collect();
        assert_eq!(
            names,
            vec![
                Value::Text("carol".into()),
                Value::Text("bob".into()),
                Value::Text("alice".into()),
            ]
        );

        // Multi-column projection preserves requested order.
        let proj = db
            .query("SELECT name, id FROM p ORDER BY name LIMIT 2")
            .unwrap();
        assert_eq!(
            proj.columns().unwrap(),
            &["name".to_string(), "id".to_string()]
        );
        assert_eq!(
            proj.rows().unwrap(),
            &[
                vec![Value::Text("alice".into()), Value::Int(1)],
                vec![Value::Text("bob".into()), Value::Int(2)],
            ]
        );

        // Unknown projection / order column is a clean schema error, not a panic.
        assert!(db.query("SELECT nope FROM p").is_err());
        assert!(db.query("SELECT * FROM p ORDER BY nope").is_err());
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

    #[test]
    fn wasm_extension_seam_runs_scalar_and_apply() {
        // The supported third-party extension seam: a sandboxed guest exporting
        // `memory` plus a `fn(ptr, len) -> i32`. `sum` returns a scalar; `inc`
        // mutates the input region in place and reports the output length.
        let guest = wat::parse_str(
            r#"
            (module
              (memory (export "memory") 1)
              (func (export "sum") (param $ptr i32) (param $len i32) (result i32)
                (local $i i32) (local $acc i32)
                (block $done (loop $loop
                  (br_if $done (i32.ge_u (local.get $i) (local.get $len)))
                  (local.set $acc (i32.add (local.get $acc)
                    (i32.load8_u (i32.add (local.get $ptr) (local.get $i)))))
                  (local.set $i (i32.add (local.get $i) (i32.const 1)))
                  (br $loop)))
                (local.get $acc))
              (func (export "inc") (param $ptr i32) (param $len i32) (result i32)
                (local $i i32)
                (block $done (loop $loop
                  (br_if $done (i32.ge_u (local.get $i) (local.get $len)))
                  (i32.store8 (i32.add (local.get $ptr) (local.get $i))
                    (i32.add (i32.load8_u (i32.add (local.get $ptr) (local.get $i))) (i32.const 1)))
                  (local.set $i (i32.add (local.get $i) (i32.const 1)))
                  (br $loop)))
                (local.get $len)))
            "#,
        )
        .unwrap();

        let db = Database::open_memory();
        assert_eq!(
            db.run_wasm_scalar(&guest, "sum", &[1, 2, 3, 4, 10])
                .unwrap(),
            20
        );
        assert_eq!(
            db.run_wasm_apply(&guest, "inc", &[0, 9, 254]).unwrap(),
            vec![1, 10, 255]
        );
        assert!(db.run_wasm_scalar(&guest, "missing", &[]).is_err());
    }

    #[test]
    fn cyclic_page_chain_errors_rather_than_hangs() {
        // SECURITY: a crafted page whose next-link points to itself must not loop
        // forever — the scan caps traversal at the total page count.
        let tmp = tempfile::tempdir().unwrap();
        let mut dev = DevStore::create(tmp.path()).unwrap();
        let pid = dev.alloc_page(); // 0; page_count -> 1
        let mut cas = CasStore::new_memory();
        let record = encode_record(&RecordEnvelope::new(1, 0), &[], &mut cas).unwrap();
        let mut page = RowPage::new(pid);
        page.insert(&record).unwrap();
        page.set_next_page(Some(pid)); // self-cycle
        dev.write_page(pid, page.as_bytes()).unwrap();

        let mut cache = PageCache::new(Backend::Dev(dev), 8);
        let table = Table {
            columns: vec!["x".into()],
            first_page: Some(pid),
            tail_id: None,
            tail: None,
            row_versions: 0,
            indexes: BTreeMap::new(),
        };
        let result = scan(&mut cache, &table, &cas, |_, _, _| Ok(()));
        assert!(result.is_err(), "cyclic chain must error, not hang");
    }

    #[test]
    fn sync_durability_round_trips() {
        let tmp = tempfile::tempdir().unwrap();
        let ws = tmp.path().join("ws");
        {
            let mut db = Database::open_dev(&ws).unwrap();
            db.set_durability(Durability::Sync);
            db.query("CREATE TABLE t (id, name)").unwrap();
            db.query("INSERT INTO t VALUES (1, 'alice')").unwrap();
            db.query("INSERT INTO t VALUES (2, 'bob')").unwrap();
        }
        let mut db = Database::open_dev(&ws).unwrap();
        assert_eq!(
            db.query("SELECT * FROM t").unwrap().rows().unwrap().len(),
            2
        );
        // The atomic-commit temp file must not linger.
        assert!(!ws.join("pv_manifest.json.tmp").exists());
    }

    #[test]
    fn update_limit_drop_and_count() {
        let tmp = tempfile::tempdir().unwrap();
        let mut db = Database::open_dev(tmp.path().join("ws")).unwrap();
        db.query("CREATE TABLE t (id, status)").unwrap();
        for i in 1..=5i64 {
            db.query(&format!("INSERT INTO t VALUES ({i}, 'open')"))
                .unwrap();
        }
        assert_eq!(db.row_count("t", None).unwrap(), 5);

        // UPDATE replaces one row's value (tombstone + reinsert), count unchanged.
        assert_eq!(
            db.query("UPDATE t SET status = 'closed' WHERE id = 3")
                .unwrap(),
            QueryResult::Mutated(1)
        );
        assert_eq!(db.row_count("t", None).unwrap(), 5);
        let closed = db.query("SELECT * FROM t WHERE status = 'closed'").unwrap();
        assert_eq!(closed.rows().unwrap().len(), 1);
        assert_eq!(closed.rows().unwrap()[0][0], Value::Int(3));

        // LIMIT caps the result.
        assert_eq!(
            db.query("SELECT * FROM t LIMIT 2")
                .unwrap()
                .rows()
                .unwrap()
                .len(),
            2
        );

        // DROP TABLE removes it.
        db.query("DROP TABLE t").unwrap();
        assert!(db.query("SELECT * FROM t").is_err());
    }

    #[test]
    fn in_memory_database_works_and_exports() {
        let mut db = Database::open_memory();
        assert!(db.is_writable());
        db.query("CREATE TABLE t (id, name)").unwrap();
        db.query("INSERT INTO t VALUES (1, 'alice')").unwrap();
        db.query("INSERT INTO t VALUES (2, 'bob')").unwrap();
        db.query("CREATE INDEX ON t (name)").unwrap();
        assert_eq!(
            db.query("SELECT * FROM t").unwrap().rows().unwrap().len(),
            2
        );
        assert_eq!(
            db.query("SELECT * FROM t WHERE name = 'alice'")
                .unwrap()
                .rows()
                .unwrap()
                .len(),
            1
        );
        assert_eq!(db.row_count("t", None).unwrap(), 2);

        // Export the in-memory database to a .pvdb byte image and reopen it.
        let bytes = db.bake_to_bytes().unwrap();
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("export.pvdb");
        std::fs::write(&path, &bytes).unwrap();
        let mut prod = Database::open_prod(&path).unwrap();
        assert_eq!(
            prod.query("SELECT * FROM t").unwrap().rows().unwrap().len(),
            2
        );
    }

    #[test]
    fn import_bytes_round_trips_with_history_and_stays_writable() {
        let mut db = Database::open_memory();
        db.query("CREATE TABLE t (id, status)").unwrap();
        db.query("INSERT INTO t VALUES (1, 'open')").unwrap();
        db.query("INSERT INTO t VALUES (2, 'open')").unwrap();
        let before = db.current_tx();
        db.query("UPDATE t SET status = 'closed' WHERE id = 1")
            .unwrap();
        let bytes = db.bake_to_bytes().unwrap();

        // Re-import into a fresh, writable in-memory database.
        let mut db2 = Database::import_bytes(&bytes).unwrap();
        assert!(db2.is_writable());
        assert_eq!(
            db2.query("SELECT * FROM t").unwrap().rows().unwrap().len(),
            2
        );

        // MVCC history survives the round trip: id=1 was 'open' before the update.
        let past = db2
            .query(&format!("SELECT * FROM t WHERE id = 1 BEFORE {before}"))
            .unwrap();
        assert_eq!(past.rows().unwrap()[0][1], Value::Text("open".into()));

        // Editing continues after import.
        db2.query("INSERT INTO t VALUES (3, 'open')").unwrap();
        assert_eq!(
            db2.query("SELECT * FROM t").unwrap().rows().unwrap().len(),
            3
        );

        // Malformed images error rather than panic.
        assert!(Database::import_bytes(&[0u8; 10]).is_err());
        assert!(Database::import_bytes(&bytes[..bytes.len() / 2]).is_err());
    }
}
