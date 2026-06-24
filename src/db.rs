//! The outer developer surface: [`Database`] plus the dev/prod lifecycle.
//!
//! This is the integration layer. As of the page-backed engine it composes:
//!
//! * a **buffer pool** ([`crate::storage::cache::PageCache`]) so reads stream
//!   through a bounded set of resident pages, datasets need not fit in RAM;
//! * **append-only page chains**, inserts append to a table's tail page and
//!   write only that page (plus a small manifest), so autocommit is O(1) per
//!   insert instead of rewriting the whole table;
//! * **secondary indexes** ([`crate::storage::index`]), opt-in equality indexes
//!   turn `WHERE col = value` into a lookup instead of a full scan.
//!
//! A table is a singly linked chain of row pages (each header points to the
//! next), so the manifest stores only a head page id per table, O(tables), not
//! O(pages), keeping per-insert manifest writes cheap.

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
use crate::core::value::{Row, Value, DECIMAL_DEN};
use crate::engine::compliance::{ComplianceMonitor, RuntimeMetrics};
use crate::engine::mvcc::{Snapshot, TxManager};
use crate::engine::query::{
    parse, AggFunc, Aggregate, CompareOp, OrderBy, Predicate, Projection, SelectItem, Statement,
};
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
    /// where there is no filesystem. Data lives only in RAM, export it with
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
    /// database, the inverse of [`bake_to_bytes`](Self::bake_to_bytes).
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

    /// Execute a single SQL statement with `?` placeholders bound to `params`.
    /// Each placeholder is replaced by its parameter rendered as a safely-escaped
    /// SQL literal, so values containing quotes or SQL syntax cannot be injected.
    pub fn query_with(&mut self, sql: &str, params: &[Value]) -> Result<QueryResult> {
        let bound = crate::engine::query::bind_params(sql, params)?;
        self.query(&bound)
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
                group_by,
                order,
                limit,
            } => {
                // Fast path: `ORDER BY indexed_col` with no `WHERE`, grouping, or
                // aggregates reads the ordered index in key order, skipping the
                // sort and (with `LIMIT`) stopping early.
                if group_by.is_empty()
                    && filter.is_none()
                    && !matches!(projection, Projection::Items(_))
                {
                    if let Some(ob) = &order {
                        if self.has_index(&table, &ob.column) {
                            let (columns, rows) =
                                self.select_ordered_by_index(&table, ob, before, limit)?;
                            return project_select(columns, rows, projection, None, None);
                        }
                    }
                }
                let (columns, rows) = self.select_filtered(&table, filter.as_ref(), before)?;
                if !group_by.is_empty() || matches!(projection, Projection::Items(_)) {
                    let items = projection_to_items(projection)?;
                    project_grouped(columns, rows, items, group_by, order, limit)
                } else {
                    project_select(columns, rows, projection, order, limit)
                }
            }
            Statement::Update { table, set, filter } => {
                let n = self.update_where(&table, &set.0, &set.1, &filter)?;
                Ok(QueryResult::Mutated(n))
            }
            Statement::Delete { table, filter } => {
                let n = self.delete_where(&table, &filter)?;
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
        self.delete_where(table_name, &Predicate::eq(column, value.clone()))
    }

    /// Delete rows matching `pred` (an MVCC tombstone). Returns the number deleted.
    pub fn delete_where(&mut self, table_name: &str, pred: &Predicate) -> Result<usize> {
        self.ensure_writable()?;
        let tx = self.txm.begin_write();
        let snapshot = self.txm.snapshot();
        let matches = self.collect_matching(table_name, pred, &snapshot)?;

        let table = self.tables.get_mut(table_name).expect("existence checked");
        {
            let mut cache = self.cache.borrow_mut();
            for (addr, _) in &matches {
                patch_delete_at(&mut cache, table, *addr, tx)?;
            }
        }
        self.maybe_flush()?;
        Ok(matches.len())
    }

    /// Collect `(address, row)` for every visible row matching `pred`, using the
    /// index when `pred` carries an indexed `col = value` or `col <op> value`
    /// (possibly as an `AND` conjunct), otherwise a filtered scan.
    fn collect_matching(
        &self,
        table_name: &str,
        pred: &Predicate,
        snapshot: &Snapshot,
    ) -> Result<Vec<(RecordAddr, Row)>> {
        let table = self
            .tables
            .get(table_name)
            .ok_or_else(|| PvError::TableNotFound(table_name.into()))?;
        let columns = table.columns.clone();
        check_predicate_columns(&columns, pred)?;
        let mut hits: Vec<(RecordAddr, Row)> = Vec::new();
        let mut cache = self.cache.borrow_mut();
        if let Some(addrs) = index_candidates(table, pred) {
            for addr in addrs {
                let (env, row) = read_record_at(&mut cache, table, &self.cas, addr)?;
                if snapshot.sees(&env) && row_matches(pred, &columns, &row)? {
                    hits.push((addr, row));
                }
            }
        } else {
            scan(&mut cache, table, &self.cas, |addr, env, row| {
                if snapshot.sees(env) && row_matches(pred, &columns, row)? {
                    hits.push((addr, row.clone()));
                }
                Ok(())
            })?;
        }
        Ok(hits)
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

    /// The column names of `table`, in order.
    pub fn column_names(&self, table_name: &str) -> Result<Vec<String>> {
        self.tables
            .get(table_name)
            .map(|t| t.columns.clone())
            .ok_or_else(|| PvError::TableNotFound(table_name.into()))
    }

    /// Stream every visible row of `table` (as of `before`, or the latest
    /// transaction) to `visit`, one row at a time, without materializing the full
    /// result. Rows arrive in scan order; pair this with [`column_names`] to
    /// interpret them. Returning `Err` from `visit` stops the scan early and
    /// propagates the error.
    ///
    /// The page cache is borrowed for the duration of the scan, so `visit` must
    /// not call back into this database (`query`, `select`, `for_each_row`, ...);
    /// doing so panics. Use this to process or export large results with bounded
    /// memory.
    ///
    /// [`column_names`]: Database::column_names
    pub fn for_each_row<F>(&self, table_name: &str, before: Option<u64>, mut visit: F) -> Result<()>
    where
        F: FnMut(&Row) -> Result<()>,
    {
        let table = self
            .tables
            .get(table_name)
            .ok_or_else(|| PvError::TableNotFound(table_name.into()))?;
        let snapshot = Snapshot::as_of(before.unwrap_or_else(|| self.txm.current()));
        let mut cache = self.cache.borrow_mut();
        scan(&mut cache, table, &self.cas, |_addr, env, row| {
            if snapshot.sees(env) {
                visit(row)
            } else {
                Ok(())
            }
        })
    }

    /// Read rows where `column == value`, using a secondary index if one exists.
    /// `before` optionally time-travels.
    pub fn select_where(
        &self,
        table_name: &str,
        column: &str,
        value: &Value,
        before: Option<u64>,
    ) -> Result<(Vec<String>, Vec<Row>)> {
        self.select_filtered(
            table_name,
            Some(&Predicate::eq(column, value.clone())),
            before,
        )
    }

    /// Read rows matching an optional `WHERE` predicate. Uses the equality index
    /// when the predicate carries a simple `indexed_col = value` (possibly as an
    /// `AND` conjunct), otherwise a filtered scan. `before` optionally time-travels.
    pub fn select_filtered(
        &self,
        table_name: &str,
        filter: Option<&Predicate>,
        before: Option<u64>,
    ) -> Result<(Vec<String>, Vec<Row>)> {
        let table = self
            .tables
            .get(table_name)
            .ok_or_else(|| PvError::TableNotFound(table_name.into()))?;
        let snapshot = Snapshot::as_of(before.unwrap_or_else(|| self.txm.current()));
        let columns = table.columns.clone();
        let mut rows = Vec::new();
        let mut cache = self.cache.borrow_mut();
        match filter {
            None => {
                scan(&mut cache, table, &self.cas, |_a, env, row| {
                    if snapshot.sees(env) {
                        rows.push(row.clone());
                    }
                    Ok(())
                })?;
            }
            Some(pred) => {
                check_predicate_columns(&columns, pred)?;
                if let Some(addrs) = index_candidates(table, pred) {
                    for addr in addrs {
                        let (env, row) = read_record_at(&mut cache, table, &self.cas, addr)?;
                        if snapshot.sees(&env) && row_matches(pred, &columns, &row)? {
                            rows.push(row);
                        }
                    }
                } else {
                    scan(&mut cache, table, &self.cas, |_a, env, row| {
                        if snapshot.sees(env) && row_matches(pred, &columns, row)? {
                            rows.push(row.clone());
                        }
                        Ok(())
                    })?;
                }
            }
        }
        Ok((columns, rows))
    }

    /// Whether `column` of `table` has a secondary index.
    fn has_index(&self, table_name: &str, column: &str) -> bool {
        self.tables
            .get(table_name)
            .is_some_and(|t| t.indexes.contains_key(column))
    }

    /// Read all visible rows in the order of an index on `ob.column`, descending
    /// when requested, stopping once `limit` visible rows are collected. The
    /// caller must have checked that the column is indexed. Used to satisfy
    /// `ORDER BY indexed_col` without a sort.
    fn select_ordered_by_index(
        &self,
        table_name: &str,
        ob: &OrderBy,
        before: Option<u64>,
        limit: Option<usize>,
    ) -> Result<(Vec<String>, Vec<Row>)> {
        let table = self
            .tables
            .get(table_name)
            .ok_or_else(|| PvError::TableNotFound(table_name.into()))?;
        let index = table
            .indexes
            .get(&ob.column)
            .expect("caller checked the column is indexed");
        let snapshot = Snapshot::as_of(before.unwrap_or_else(|| self.txm.current()));
        let columns = table.columns.clone();
        let mut rows = Vec::new();
        let mut cache = self.cache.borrow_mut();
        for addr in index.ordered_addrs(ob.descending) {
            let (env, row) = read_record_at(&mut cache, table, &self.cas, addr)?;
            if snapshot.sees(&env) {
                rows.push(row);
                if limit.is_some_and(|n| rows.len() >= n) {
                    break;
                }
            }
        }
        Ok((columns, rows))
    }

    /// Update rows where `where_column == where_value`, assigning `set_value` to
    /// `set_column`. Returns the number updated.
    pub fn update(
        &mut self,
        table_name: &str,
        set_column: &str,
        set_value: &Value,
        where_column: &str,
        where_value: &Value,
    ) -> Result<usize> {
        self.update_where(
            table_name,
            set_column,
            set_value,
            &Predicate::eq(where_column, where_value.clone()),
        )
    }

    /// Update rows matching `pred`, assigning `set_value` to `set_column`.
    /// Append-only (MVCC): matching versions are tombstoned and new versions
    /// carrying the change are inserted. Returns the number updated.
    pub fn update_where(
        &mut self,
        table_name: &str,
        set_column: &str,
        set_value: &Value,
        pred: &Predicate,
    ) -> Result<usize> {
        self.ensure_writable()?;
        let set_ix = {
            let table = self
                .tables
                .get(table_name)
                .ok_or_else(|| PvError::TableNotFound(table_name.into()))?;
            column_index(table, set_column)?
        };
        let snapshot = self.txm.snapshot();
        let matches = self.collect_matching(table_name, pred, &snapshot)?;
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
    /// read the (in-place mutated) output region back out as bytes, the
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
    /// present in full, never a torn write.
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

/// Apply `*` / column projection, `ORDER BY`, and `LIMIT` to a result set.
/// Grouped and aggregate queries go through [`project_grouped`] instead.
fn project_select(
    columns: Vec<String>,
    mut rows: Vec<Row>,
    projection: Projection,
    order: Option<OrderBy>,
    limit: Option<usize>,
) -> Result<QueryResult> {
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
        Projection::Items(_) => unreachable!("items go through project_grouped"),
    };

    if let Some(n) = limit {
        out_rows.truncate(n);
    }
    Ok(QueryResult::Rows {
        columns: out_columns,
        rows: out_rows,
    })
}

/// Total ordering over values (`Null` &lt; `Int` &lt; `Text` &lt; `Blob`), as
/// derived on [`Value`]. Used by `ORDER BY`, `MIN`/`MAX`, and range comparisons.
fn cmp_values(a: &Value, b: &Value) -> std::cmp::Ordering {
    a.cmp(b)
}

// ---------------------------------------------------------------------------
// WHERE predicates & aggregates
// ---------------------------------------------------------------------------

/// Candidate addresses from an ordered index for `pred`, if one applies: an
/// indexed `col = value` (point lookup) or `col <op> value` for a range operator
/// (ordered scan), directly or as an `AND` conjunct, never under `OR`. Returns
/// `None` to fall back to a full scan. Candidates are re-checked against the full
/// predicate by the caller, so an over-broad set is still correct.
fn index_candidates(table: &Table, pred: &Predicate) -> Option<Vec<RecordAddr>> {
    use std::ops::Bound::{Excluded, Included, Unbounded};
    match pred {
        Predicate::Compare { column, op, value } => {
            let idx = table.indexes.get(column)?;
            let v = || value.clone();
            match op {
                CompareOp::Eq => Some(idx.lookup(value).to_vec()),
                CompareOp::Lt => Some(idx.range((Unbounded, Excluded(v())))),
                CompareOp::Le => Some(idx.range((Unbounded, Included(v())))),
                CompareOp::Gt => Some(idx.range((Excluded(v()), Unbounded))),
                CompareOp::Ge => Some(idx.range((Included(v()), Unbounded))),
                // `!=` and `LIKE` aren't range-shaped, a scan is no worse.
                CompareOp::Ne | CompareOp::Like => None,
            }
        }
        Predicate::And(a, b) => index_candidates(table, a).or_else(|| index_candidates(table, b)),
        Predicate::Or(_, _) => None,
    }
}

/// Error if the predicate references a column the table doesn't have.
fn check_predicate_columns(columns: &[String], pred: &Predicate) -> Result<()> {
    match pred {
        Predicate::And(a, b) | Predicate::Or(a, b) => {
            check_predicate_columns(columns, a)?;
            check_predicate_columns(columns, b)
        }
        Predicate::Compare { column, .. } => {
            if columns.iter().any(|c| c == column) {
                Ok(())
            } else {
                Err(PvError::Schema(format!("no column `{column}`")))
            }
        }
    }
}

/// Evaluate a predicate against one row.
fn row_matches(pred: &Predicate, columns: &[String], row: &[Value]) -> Result<bool> {
    match pred {
        Predicate::And(a, b) => Ok(row_matches(a, columns, row)? && row_matches(b, columns, row)?),
        Predicate::Or(a, b) => Ok(row_matches(a, columns, row)? || row_matches(b, columns, row)?),
        Predicate::Compare { column, op, value } => {
            let ix = columns
                .iter()
                .position(|c| c == column)
                .ok_or_else(|| PvError::Schema(format!("no column `{column}`")))?;
            Ok(eval_compare(&row[ix], *op, value))
        }
    }
}

/// Apply one comparison. Ordering comparisons against `NULL` are never true
/// (SQL three-valued logic); `=`/`!=` compare by value, `LIKE` needs two texts.
fn eval_compare(lhs: &Value, op: CompareOp, rhs: &Value) -> bool {
    use std::cmp::Ordering;
    match op {
        CompareOp::Eq => lhs == rhs,
        CompareOp::Ne => lhs != rhs,
        CompareOp::Like => match (lhs, rhs) {
            (Value::Text(t), Value::Text(p)) => like_match(t, p),
            _ => false,
        },
        CompareOp::Lt | CompareOp::Le | CompareOp::Gt | CompareOp::Ge => {
            if matches!(lhs, Value::Null) || matches!(rhs, Value::Null) {
                return false;
            }
            let ord = cmp_values(lhs, rhs);
            match op {
                CompareOp::Lt => ord == Ordering::Less,
                CompareOp::Le => ord != Ordering::Greater,
                CompareOp::Gt => ord == Ordering::Greater,
                CompareOp::Ge => ord != Ordering::Less,
                _ => unreachable!(),
            }
        }
    }
}

/// SQL `LIKE`: `%` matches any run (including empty), `_` any single char.
/// Case-sensitive. Linear-time two-pointer match with `%` backtracking.
fn like_match(text: &str, pattern: &str) -> bool {
    let t: Vec<char> = text.chars().collect();
    let p: Vec<char> = pattern.chars().collect();
    let (mut ti, mut pi) = (0usize, 0usize);
    let (mut star_p, mut star_t): (Option<usize>, usize) = (None, 0);
    while ti < t.len() {
        if pi < p.len() && (p[pi] == '_' || p[pi] == t[ti]) {
            ti += 1;
            pi += 1;
        } else if pi < p.len() && p[pi] == '%' {
            star_p = Some(pi);
            star_t = ti;
            pi += 1;
        } else if let Some(sp) = star_p {
            pi = sp + 1;
            star_t += 1;
            ti = star_t;
        } else {
            return false;
        }
    }
    while pi < p.len() && p[pi] == '%' {
        pi += 1;
    }
    pi == p.len()
}

/// Turn a projection into select items for the grouped path. `SELECT *` cannot be
/// combined with grouping or aggregates.
fn projection_to_items(projection: Projection) -> Result<Vec<SelectItem>> {
    match projection {
        Projection::Items(items) => Ok(items),
        Projection::Columns(cols) => Ok(cols.into_iter().map(SelectItem::Column).collect()),
        Projection::All => Err(PvError::Query(
            "SELECT * cannot be combined with GROUP BY or aggregates".into(),
        )),
    }
}

/// Evaluate a grouped or whole-table aggregate query: partition `rows` by the
/// `group_by` columns (a single group when `group_by` is empty), evaluate each
/// select item per group, then apply `ORDER BY` and `LIMIT` to the result.
fn project_grouped(
    columns: Vec<String>,
    rows: Vec<Row>,
    items: Vec<SelectItem>,
    group_by: Vec<String>,
    order: Option<OrderBy>,
    limit: Option<usize>,
) -> Result<QueryResult> {
    // A bare column in the select list must be a grouping column.
    for item in &items {
        if let SelectItem::Column(c) = item {
            if !group_by.iter().any(|g| g == c) {
                return Err(PvError::Schema(format!(
                    "column `{c}` must appear in GROUP BY or inside an aggregate"
                )));
            }
        }
    }
    // Group-by column indices (also validates the columns exist).
    let gb_idx: Vec<usize> = group_by
        .iter()
        .map(|c| {
            columns
                .iter()
                .position(|x| x == c)
                .ok_or_else(|| PvError::Schema(format!("no column `{c}`")))
        })
        .collect::<Result<_>>()?;

    // Partition into groups, ordered by group key.
    let mut groups: BTreeMap<Vec<Value>, Vec<Row>> = BTreeMap::new();
    if group_by.is_empty() {
        groups.insert(Vec::new(), rows);
    } else {
        for row in rows {
            let key: Vec<Value> = gb_idx.iter().map(|&i| row[i].clone()).collect();
            groups.entry(key).or_default().push(row);
        }
    }

    let out_columns: Vec<String> = items
        .iter()
        .map(|it| match it {
            SelectItem::Column(c) => c.clone(),
            SelectItem::Aggregate(a) => agg_label(a),
        })
        .collect();

    let mut out_rows: Vec<Row> = Vec::with_capacity(groups.len());
    for (key, group_rows) in &groups {
        let mut out = Vec::with_capacity(items.len());
        for item in &items {
            match item {
                SelectItem::Column(c) => {
                    let gi = group_by
                        .iter()
                        .position(|g| g == c)
                        .expect("validated above");
                    out.push(key[gi].clone());
                }
                SelectItem::Aggregate(a) => {
                    out.push(compute_one_aggregate(a, &columns, group_rows)?)
                }
            }
        }
        out_rows.push(out);
    }

    if let Some(ob) = &order {
        let ix = out_columns
            .iter()
            .position(|c| c == &ob.column)
            .ok_or_else(|| PvError::Schema(format!("no column `{}` to order by", ob.column)))?;
        out_rows.sort_by(|a, b| {
            let o = cmp_values(&a[ix], &b[ix]);
            if ob.descending {
                o.reverse()
            } else {
                o
            }
        });
    }
    if let Some(n) = limit {
        out_rows.truncate(n);
    }
    Ok(QueryResult::Rows {
        columns: out_columns,
        rows: out_rows,
    })
}

fn agg_label(agg: &Aggregate) -> String {
    let f = match agg.func {
        AggFunc::Count => "count",
        AggFunc::Sum => "sum",
        AggFunc::Min => "min",
        AggFunc::Max => "max",
        AggFunc::Avg => "avg",
    };
    match &agg.column {
        None => f.to_string(),
        Some(c) => format!("{f}({c})"),
    }
}

fn compute_one_aggregate(agg: &Aggregate, columns: &[String], rows: &[Row]) -> Result<Value> {
    let col_ix = match &agg.column {
        None => None,
        Some(c) => Some(
            columns
                .iter()
                .position(|x| x == c)
                .ok_or_else(|| PvError::Schema(format!("no column `{c}`")))?,
        ),
    };
    let value = match agg.func {
        AggFunc::Count => {
            let n = match col_ix {
                None => rows.len(),
                Some(ix) => rows
                    .iter()
                    .filter(|r| !matches!(r[ix], Value::Null))
                    .count(),
            };
            Value::Int(n as i64)
        }
        AggFunc::Sum => {
            let ix = col_ix.expect("SUM requires a column");
            let mut sum: i128 = 0;
            let mut saw = false;
            for r in rows {
                match &r[ix] {
                    Value::Int(i) => {
                        sum += *i as i128;
                        saw = true;
                    }
                    Value::Null => {}
                    other => {
                        return Err(PvError::Schema(format!(
                            "SUM requires integer values, found {other:?}"
                        )))
                    }
                }
            }
            // An empty or all-null group sums to NULL, matching MIN/MAX/AVG and
            // standard SQL (only COUNT returns 0 for an empty input).
            if !saw {
                Value::Null
            } else {
                Value::Int(
                    i64::try_from(sum).map_err(|_| PvError::Schema("SUM overflowed i64".into()))?,
                )
            }
        }
        AggFunc::Min | AggFunc::Max => {
            let ix = col_ix.expect("MIN/MAX requires a column");
            let mut acc: Option<&Value> = None;
            for r in rows {
                if matches!(r[ix], Value::Null) {
                    continue;
                }
                acc = Some(match acc {
                    None => &r[ix],
                    Some(cur) => {
                        let take = match agg.func {
                            AggFunc::Min => cmp_values(&r[ix], cur).is_lt(),
                            AggFunc::Max => cmp_values(&r[ix], cur).is_gt(),
                            _ => unreachable!(),
                        };
                        if take {
                            &r[ix]
                        } else {
                            cur
                        }
                    }
                });
            }
            acc.cloned().unwrap_or(Value::Null)
        }
        AggFunc::Avg => {
            let ix = col_ix.expect("AVG requires a column");
            let mut sum: i128 = 0;
            let mut count: i128 = 0;
            for r in rows {
                match &r[ix] {
                    Value::Int(i) => {
                        sum += *i as i128;
                        count += 1;
                    }
                    Value::Null => {}
                    other => {
                        return Err(PvError::Schema(format!(
                            "AVG requires integer values, found {other:?}"
                        )))
                    }
                }
            }
            // An empty or all-null group averages to NULL, matching MIN/MAX.
            // Otherwise produce an exact fixed-point decimal (computed in i128,
            // never f64), which is numeric and orderable unlike the old text form.
            if count == 0 {
                Value::Null
            } else {
                Value::Decimal(decimal_from_ratio(sum, count))
            }
        }
    };
    Ok(value)
}

/// Round `sum / count` (with `count != 0`) to a scale-[`DECIMAL_SCALE`] decimal
/// mantissa, half away from zero, in exact integer arithmetic. Avoiding `f64`
/// keeps large integer averages exact (a value above 2^53 would otherwise be
/// misrendered) and makes rounding deterministic.
fn decimal_from_ratio(sum: i128, count: i128) -> i128 {
    let negative = sum < 0;
    let num = sum.unsigned_abs() * DECIMAL_DEN as u128; // |sum| * 10^scale
    let c = count.unsigned_abs();
    // round(num / c) with halves rounded up (away from zero).
    let scaled = (num * 2 + c) / (c * 2);
    let m = scaled as i128;
    if negative {
        -m
    } else {
        m
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
                    if row.len() != table.columns.len() {
                        return Err(PvError::Corruption(
                            "record field count does not match table columns".into(),
                        ));
                    }
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
                if row.len() != table.columns.len() {
                    return Err(PvError::Corruption(
                        "record field count does not match table columns".into(),
                    ));
                }
                visit(pack_addr(pid, slot), &env, &row)?;
            }
            Ok(page.next_page())
        })?;
    }
    Ok(())
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
    fn for_each_row_streams_visible_rows() {
        let mut db = Database::open_memory();
        db.query("CREATE TABLE t (id, name)").unwrap();
        for i in 1..=5 {
            db.query(&format!("INSERT INTO t VALUES ({i}, 'r{i}')"))
                .unwrap();
        }
        let before_delete = db.current_tx();
        db.query("DELETE FROM t WHERE id = 3").unwrap();

        // Streams the visible rows one at a time (id 3 was deleted).
        let mut ids = Vec::new();
        db.for_each_row("t", None, |row| {
            if let Value::Int(i) = row[0] {
                ids.push(i);
            }
            Ok(())
        })
        .unwrap();
        ids.sort();
        assert_eq!(ids, vec![1, 2, 4, 5]);

        // Time-travel: as of before the delete, all five are visible.
        let mut count = 0;
        db.for_each_row("t", Some(before_delete), |_row| {
            count += 1;
            Ok(())
        })
        .unwrap();
        assert_eq!(count, 5);

        // Returning Err stops the scan and propagates.
        let stopped = db.for_each_row("t", None, |_row| Err(PvError::Query("stop".into())));
        assert!(stopped.is_err());

        // Schema accessor, and an unknown table errors.
        assert_eq!(
            db.column_names("t").unwrap(),
            vec!["id".to_string(), "name".to_string()]
        );
        assert!(db.column_names("nope").is_err());
        assert!(db.for_each_row("nope", None, |_| Ok(())).is_err());
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

    /// Sorted `id`s of a SELECT result, a small assertion helper for the SQL tests.
    fn ids(db: &mut Database, sql: &str) -> Vec<i64> {
        let r = db.query(sql).unwrap();
        let ix = r.columns().unwrap().iter().position(|c| c == "id").unwrap();
        let mut v: Vec<i64> = r
            .rows()
            .unwrap()
            .iter()
            .map(|row| match row[ix] {
                Value::Int(i) => i,
                ref other => panic!("non-int id: {other:?}"),
            })
            .collect();
        v.sort();
        v
    }

    #[test]
    fn where_comparisons_boolean_and_like() {
        let mut db = Database::open_memory();
        db.query("CREATE TABLE t (id, name, age)").unwrap();
        db.query("INSERT INTO t VALUES (1, 'alice', 30)").unwrap();
        db.query("INSERT INTO t VALUES (2, 'bob', 25)").unwrap();
        db.query("INSERT INTO t VALUES (3, 'carol', 40)").unwrap();
        db.query("INSERT INTO t VALUES (4, 'dave', 25)").unwrap();

        // Comparison operators.
        assert_eq!(ids(&mut db, "SELECT * FROM t WHERE age > 25"), vec![1, 3]);
        assert_eq!(ids(&mut db, "SELECT * FROM t WHERE age >= 30"), vec![1, 3]);
        assert_eq!(ids(&mut db, "SELECT * FROM t WHERE age != 25"), vec![1, 3]);
        assert_eq!(ids(&mut db, "SELECT id FROM t WHERE id < 3"), vec![1, 2]);
        assert_eq!(ids(&mut db, "SELECT id FROM t WHERE id <= 2"), vec![1, 2]);

        // Boolean combinations: AND binds tighter than OR.
        assert_eq!(
            ids(&mut db, "SELECT * FROM t WHERE age = 25 OR id = 1"),
            vec![1, 2, 4]
        );
        assert_eq!(
            ids(&mut db, "SELECT * FROM t WHERE age > 25 AND id < 3"),
            vec![1]
        );
        assert_eq!(
            ids(
                &mut db,
                "SELECT * FROM t WHERE (id = 1 OR id = 4) AND age = 25"
            ),
            vec![4]
        );

        // LIKE: `%` any run, `_` one char.
        assert_eq!(
            ids(&mut db, "SELECT * FROM t WHERE name LIKE 'a%'"),
            vec![1]
        );
        assert_eq!(
            ids(&mut db, "SELECT id FROM t WHERE name LIKE '_a%'"),
            vec![3, 4]
        );
    }

    #[test]
    fn whole_table_aggregates() {
        let mut db = Database::open_memory();
        db.query("CREATE TABLE s (id, amount)").unwrap();
        db.query("INSERT INTO s VALUES (1, 10)").unwrap();
        db.query("INSERT INTO s VALUES (2, 30)").unwrap();
        db.query("INSERT INTO s VALUES (3, 20)").unwrap();

        let r = db
            .query("SELECT COUNT(*), SUM(amount), MIN(amount), MAX(amount) FROM s")
            .unwrap();
        assert_eq!(
            r.columns().unwrap(),
            &["count", "sum(amount)", "min(amount)", "max(amount)"]
        );
        assert_eq!(
            r.rows().unwrap(),
            &[vec![
                Value::Int(3),
                Value::Int(60),
                Value::Int(10),
                Value::Int(30)
            ]]
        );

        // Aggregate over a WHERE-filtered subset.
        let r = db
            .query("SELECT SUM(amount) FROM s WHERE amount >= 20")
            .unwrap();
        assert_eq!(r.rows().unwrap(), &[vec![Value::Int(50)]]);

        // MIN/MAX order text too; SUM over text is a clean error.
        db.query("CREATE TABLE w (name)").unwrap();
        db.query("INSERT INTO w VALUES ('bob')").unwrap();
        db.query("INSERT INTO w VALUES ('alice')").unwrap();
        let r = db.query("SELECT MIN(name), MAX(name) FROM w").unwrap();
        assert_eq!(
            r.rows().unwrap(),
            &[vec![Value::Text("alice".into()), Value::Text("bob".into())]]
        );
        assert!(db.query("SELECT SUM(name) FROM w").is_err());
    }

    #[test]
    fn group_by_aggregates() {
        let mut db = Database::open_memory();
        db.query("CREATE TABLE orders (id, customer, amount)")
            .unwrap();
        for (i, c, a) in [
            (1, "alice", 120),
            (2, "bob", 45),
            (3, "alice", 60),
            (4, "carol", 200),
            (5, "bob", 55),
        ] {
            db.query(&format!("INSERT INTO orders VALUES ({i}, '{c}', {a})"))
                .unwrap();
        }

        // One row per group, with per-group aggregates. Groups come out in key
        // order (alice, bob, carol).
        let r = db
            .query("SELECT customer, COUNT(*), SUM(amount) FROM orders GROUP BY customer")
            .unwrap();
        assert_eq!(
            r.columns().unwrap(),
            &[
                "customer".to_string(),
                "count".to_string(),
                "sum(amount)".to_string()
            ]
        );
        assert_eq!(
            r.rows().unwrap(),
            &[
                vec![Value::Text("alice".into()), Value::Int(2), Value::Int(180)],
                vec![Value::Text("bob".into()), Value::Int(2), Value::Int(100)],
                vec![Value::Text("carol".into()), Value::Int(1), Value::Int(200)],
            ]
        );

        // WHERE filters rows before grouping; MIN/MAX per group.
        let r = db
            .query("SELECT customer, MIN(amount), MAX(amount) FROM orders WHERE amount > 50 GROUP BY customer")
            .unwrap();
        assert_eq!(
            r.rows().unwrap(),
            &[
                vec![Value::Text("alice".into()), Value::Int(60), Value::Int(120)],
                vec![Value::Text("bob".into()), Value::Int(55), Value::Int(55)],
                vec![
                    Value::Text("carol".into()),
                    Value::Int(200),
                    Value::Int(200)
                ],
            ]
        );

        // GROUP BY a column alone yields the distinct group keys.
        let distinct = db
            .query("SELECT customer FROM orders GROUP BY customer")
            .unwrap();
        let names: Vec<_> = distinct
            .rows()
            .unwrap()
            .iter()
            .map(|r| r[0].clone())
            .collect();
        assert_eq!(
            names,
            vec![
                Value::Text("alice".into()),
                Value::Text("bob".into()),
                Value::Text("carol".into()),
            ]
        );

        // Invalid combinations are rejected, not silently wrong.
        assert!(db
            .query("SELECT customer, amount FROM orders GROUP BY customer")
            .is_err()); // bare non-grouped column
        assert!(db.query("SELECT * FROM orders GROUP BY customer").is_err()); // SELECT *
        assert!(db.query("SELECT customer, COUNT(*) FROM orders").is_err()); // mix without GROUP BY
    }

    #[test]
    fn avg_aggregate() {
        let mut db = Database::open_memory();
        db.query("CREATE TABLE t (id, amount)").unwrap();
        db.query("INSERT INTO t VALUES (1, 1)").unwrap();
        db.query("INSERT INTO t VALUES (2, 2)").unwrap();

        // AVG returns an exact decimal: 1 and 2 average to 1.5 (mantissa
        // 1_500_000 at scale 6), which displays as "1.500000".
        let r = db.query("SELECT AVG(amount) FROM t").unwrap();
        assert_eq!(r.columns().unwrap(), &["avg(amount)".to_string()]);
        assert_eq!(r.rows().unwrap(), &[vec![Value::Decimal(1_500_000)]]);
        assert_eq!(r.rows().unwrap()[0][0].to_string(), "1.500000");

        // AVG ignores NULLs: the divisor is the non-null count.
        db.query("INSERT INTO t VALUES (3, NULL)").unwrap();
        assert_eq!(
            db.query("SELECT AVG(amount) FROM t")
                .unwrap()
                .rows()
                .unwrap(),
            &[vec![Value::Decimal(1_500_000)]]
        );
        db.query("INSERT INTO t VALUES (4, 9)").unwrap(); // (1 + 2 + 9) / 3 = 4.0
        assert_eq!(
            db.query("SELECT AVG(amount) FROM t")
                .unwrap()
                .rows()
                .unwrap(),
            &[vec![Value::Decimal(4_000_000)]]
        );

        // Empty and all-null groups average to NULL.
        db.query("CREATE TABLE e (x)").unwrap();
        assert_eq!(
            db.query("SELECT AVG(x) FROM e").unwrap().rows().unwrap(),
            &[vec![Value::Null]]
        );
        db.query("INSERT INTO e VALUES (NULL)").unwrap();
        assert_eq!(
            db.query("SELECT AVG(x) FROM e").unwrap().rows().unwrap(),
            &[vec![Value::Null]]
        );

        // AVG over non-integer text errors, like SUM.
        db.query("CREATE TABLE w (name)").unwrap();
        db.query("INSERT INTO w VALUES ('bob')").unwrap();
        assert!(db.query("SELECT AVG(name) FROM w").is_err());

        // AVG under GROUP BY: one average per group.
        let mut g = Database::open_memory();
        g.query("CREATE TABLE s (team, score)").unwrap();
        for (t, sc) in [("a", 10), ("a", 20), ("b", 5), ("b", 6)] {
            g.query(&format!("INSERT INTO s VALUES ('{t}', {sc})"))
                .unwrap();
        }
        let r = g
            .query("SELECT team, AVG(score) FROM s GROUP BY team")
            .unwrap();
        assert_eq!(
            r.rows().unwrap(),
            &[
                vec![Value::Text("a".into()), Value::Decimal(15_000_000)],
                vec![Value::Text("b".into()), Value::Decimal(5_500_000)],
            ]
        );
    }

    #[test]
    fn avg_is_exact_and_rounds_half_away_from_zero() {
        // Large integers are exact (computed in i128, not through f64). The
        // average of i64::MAX over one row is that value exactly, not off by one.
        let mut big = Database::open_memory();
        big.query("CREATE TABLE b (v)").unwrap();
        big.query("INSERT INTO b VALUES (9223372036854775807)")
            .unwrap();
        let avg = big.query("SELECT AVG(v) FROM b").unwrap();
        assert_eq!(
            avg.rows().unwrap(),
            &[vec![Value::Decimal(
                9_223_372_036_854_775_807_i128 * 1_000_000
            )]]
        );
        assert_eq!(
            avg.rows().unwrap()[0][0].to_string(),
            "9223372036854775807.000000"
        );

        // 5 / 8 = 0.625 is exact at scale 6 (mantissa 625_000), no rounding needed.
        let mut exact = Database::open_memory();
        exact.query("CREATE TABLE r (v)").unwrap();
        for v in [5, 0, 0, 0, 0, 0, 0, 0] {
            exact.query(&format!("INSERT INTO r VALUES ({v})")).unwrap();
        }
        assert_eq!(
            exact.query("SELECT AVG(v) FROM r").unwrap().rows().unwrap(),
            &[vec![Value::Decimal(625_000)]]
        );

        // 1 / 128 = 0.0078125 falls exactly on the scale-6 half; it rounds away
        // from zero to 0.007813 (mantissa 7813), not banker's 0.007812.
        let mut half = Database::open_memory();
        half.query("CREATE TABLE h (v)").unwrap();
        for i in 0..128 {
            let v = if i == 0 { 1 } else { 0 };
            half.query(&format!("INSERT INTO h VALUES ({v})")).unwrap();
        }
        let r = half.query("SELECT AVG(v) FROM h").unwrap();
        assert_eq!(r.rows().unwrap(), &[vec![Value::Decimal(7813)]]);
        assert_eq!(r.rows().unwrap()[0][0].to_string(), "0.007813");

        // Negative averages keep a sign; -3 / 2 = -1.5.
        let mut neg = Database::open_memory();
        neg.query("CREATE TABLE n (v)").unwrap();
        neg.query("INSERT INTO n VALUES (-1)").unwrap();
        neg.query("INSERT INTO n VALUES (-2)").unwrap();
        let r = neg.query("SELECT AVG(v) FROM n").unwrap();
        assert_eq!(r.rows().unwrap(), &[vec![Value::Decimal(-1_500_000)]]);
        assert_eq!(r.rows().unwrap()[0][0].to_string(), "-1.500000");
    }

    #[test]
    fn decimal_literals_are_storable_and_round_trip() {
        let mut db = Database::open_memory();
        db.query("CREATE TABLE t (x)").unwrap();
        // Programmatic insert of a decimal now persists.
        db.insert("t", vec![Value::Decimal(1_500_000)]).unwrap();
        // SQL decimal literal: 12.50 -> mantissa 12_500_000 at scale 6.
        db.query("INSERT INTO t VALUES (12.50)").unwrap();
        // Extra fractional digits truncate to the scale and the sign is kept.
        db.query("INSERT INTO t VALUES (-0.0000019)").unwrap();
        let rows = db
            .query("SELECT * FROM t")
            .unwrap()
            .rows()
            .unwrap()
            .to_vec();
        assert_eq!(
            rows,
            vec![
                vec![Value::Decimal(1_500_000)],
                vec![Value::Decimal(12_500_000)],
                vec![Value::Decimal(-1)],
            ]
        );
        // Round-trips through a baked .pvdb image.
        let bytes = db.bake_to_bytes().unwrap();
        let mut restored = Database::import_bytes(&bytes).unwrap();
        assert_eq!(
            restored
                .query("SELECT * FROM t")
                .unwrap()
                .rows()
                .unwrap()
                .len(),
            3
        );
    }

    #[test]
    fn parameterized_queries_bind_safely() {
        let mut db = Database::open_memory();
        db.query("CREATE TABLE u (id, name)").unwrap();
        db.query_with(
            "INSERT INTO u VALUES (?, ?)",
            &[Value::Int(1), Value::Text("a'b".into())],
        )
        .unwrap();
        db.query_with("INSERT INTO u VALUES (?, ?)", &[Value::Int(2), Value::Null])
            .unwrap();
        // A `?` inside a string literal is data, not a placeholder.
        db.query_with("INSERT INTO u VALUES (3, '?')", &[]).unwrap();
        let rows = db
            .query_with("SELECT name FROM u WHERE id = ?", &[Value::Int(1)])
            .unwrap()
            .rows()
            .unwrap()
            .to_vec();
        assert_eq!(rows, vec![vec![Value::Text("a'b".into())]]);
        // An injection attempt is escaped into a single string value, not executed.
        db.query_with(
            "INSERT INTO u VALUES (4, ?)",
            &[Value::Text("x'); DROP TABLE u; --".into())],
        )
        .unwrap();
        assert_eq!(
            db.query("SELECT COUNT(*) FROM u").unwrap().rows().unwrap(),
            &[vec![Value::Int(4)]]
        );
        // Arity mismatches are errors.
        assert!(db.query_with("SELECT * FROM u WHERE id = ?", &[]).is_err());
        assert!(db
            .query_with(
                "SELECT * FROM u WHERE id = ?",
                &[Value::Int(1), Value::Int(2)]
            )
            .is_err());
    }

    #[test]
    fn sum_of_empty_or_all_null_is_null() {
        let mut db = Database::open_memory();
        db.query("CREATE TABLE t (x)").unwrap();
        // Empty input sums to NULL (matching MIN/MAX/AVG and standard SQL).
        assert_eq!(
            db.query("SELECT SUM(x) FROM t").unwrap().rows().unwrap(),
            &[vec![Value::Null]]
        );
        // All-null input also sums to NULL.
        db.query("INSERT INTO t VALUES (NULL)").unwrap();
        assert_eq!(
            db.query("SELECT SUM(x) FROM t").unwrap().rows().unwrap(),
            &[vec![Value::Null]]
        );
        // A real value makes it an integer sum again.
        db.query("INSERT INTO t VALUES (7)").unwrap();
        assert_eq!(
            db.query("SELECT SUM(x) FROM t").unwrap().rows().unwrap(),
            &[vec![Value::Int(7)]]
        );
    }

    #[test]
    fn update_and_delete_with_predicates() {
        let mut db = Database::open_memory();
        db.query("CREATE TABLE t (id, tier)").unwrap();
        for i in 1..=5 {
            db.query(&format!("INSERT INTO t VALUES ({i}, 'free')"))
                .unwrap();
        }
        // UPDATE with a range predicate.
        assert_eq!(
            db.query("UPDATE t SET tier = 'pro' WHERE id > 3").unwrap(),
            QueryResult::Mutated(2)
        );
        let pros = db
            .query("SELECT COUNT(*) FROM t WHERE tier = 'pro'")
            .unwrap();
        assert_eq!(pros.rows().unwrap(), &[vec![Value::Int(2)]]);

        // DELETE with OR.
        assert_eq!(
            db.query("DELETE FROM t WHERE id = 1 OR id = 2").unwrap(),
            QueryResult::Mutated(2)
        );
        assert_eq!(ids(&mut db, "SELECT id FROM t"), vec![3, 4, 5]);
    }

    #[test]
    fn indexed_equality_used_within_and_predicate() {
        let mut db = Database::open_memory();
        db.query("CREATE TABLE t (id, tier)").unwrap();
        db.query("INSERT INTO t VALUES (1, 'pro')").unwrap();
        db.query("INSERT INTO t VALUES (2, 'free')").unwrap();
        db.query("INSERT INTO t VALUES (3, 'pro')").unwrap();
        db.query("CREATE INDEX ON t (tier)").unwrap();
        // `tier = 'pro'` (indexed) is the fast path; `id > 1` filters the candidates.
        assert_eq!(
            ids(&mut db, "SELECT id FROM t WHERE tier = 'pro' AND id > 1"),
            vec![3]
        );
        // Still correct after the index exists for a plain equality, too.
        assert_eq!(
            ids(&mut db, "SELECT id FROM t WHERE tier = 'free'"),
            vec![2]
        );
    }

    #[test]
    fn range_query_with_index_matches_scan() {
        // Identical data, one table indexed and one not: a range predicate must
        // return the same rows whether it goes through the ordered index or a scan.
        let build = |index: bool| {
            let mut db = Database::open_memory();
            db.query("CREATE TABLE t (id, score)").unwrap();
            for (i, s) in [(1, 50), (2, 90), (3, 70), (4, 90), (5, 10)] {
                db.query(&format!("INSERT INTO t VALUES ({i}, {s})"))
                    .unwrap();
            }
            if index {
                db.query("CREATE INDEX ON t (score)").unwrap();
            }
            db
        };
        let mut indexed = build(true);
        let mut plain = build(false);
        for sql in [
            "SELECT id FROM t WHERE score > 50",
            "SELECT id FROM t WHERE score >= 70",
            "SELECT id FROM t WHERE score < 70",
            "SELECT id FROM t WHERE score <= 50",
            "SELECT id FROM t WHERE score > 10 AND score < 90",
            "SELECT id FROM t WHERE score = 90",
        ] {
            assert_eq!(
                ids(&mut indexed, sql),
                ids(&mut plain, sql),
                "mismatch: {sql}"
            );
        }
        assert_eq!(
            ids(&mut indexed, "SELECT id FROM t WHERE score >= 70"),
            vec![2, 3, 4]
        );

        // MVCC: after an UPDATE the ordered index reflects the new value, with the
        // old version tombstoned (not visible), so range results track the change.
        indexed
            .query("UPDATE t SET score = 5 WHERE id = 2")
            .unwrap();
        assert_eq!(
            ids(&mut indexed, "SELECT id FROM t WHERE score >= 70"),
            vec![3, 4]
        );
        assert_eq!(
            ids(&mut indexed, "SELECT id FROM t WHERE score < 50"),
            vec![2, 5]
        );
    }

    #[test]
    fn order_by_index_matches_sort() {
        let build = |index: bool| {
            let mut db = Database::open_memory();
            db.query("CREATE TABLE t (id, score)").unwrap();
            for (i, s) in [(1, 50), (2, 90), (3, 70), (4, 90), (5, 10)] {
                db.query(&format!("INSERT INTO t VALUES ({i}, {s})"))
                    .unwrap();
            }
            if index {
                db.query("CREATE INDEX ON t (score)").unwrap();
            }
            db
        };
        let mut indexed = build(true);
        let mut plain = build(false);

        // The `score` sequence a query returns, in result order (not re-sorted).
        let scores = |db: &mut Database, sql: &str| -> Vec<i64> {
            let r = db.query(sql).unwrap();
            let si = r
                .columns()
                .unwrap()
                .iter()
                .position(|c| c == "score")
                .unwrap();
            r.rows()
                .unwrap()
                .iter()
                .map(|row| match row[si] {
                    Value::Int(v) => v,
                    ref other => panic!("non-int score: {other:?}"),
                })
                .collect()
        };

        // The index-ordered fast path (indexed) and the sort path (plain) must
        // agree on the score order for ascending and descending, with and without
        // a limit.
        for sql in [
            "SELECT score, id FROM t ORDER BY score",
            "SELECT score, id FROM t ORDER BY score ASC",
            "SELECT score, id FROM t ORDER BY score DESC",
            "SELECT score, id FROM t ORDER BY score LIMIT 3",
            "SELECT score, id FROM t ORDER BY score DESC LIMIT 2",
        ] {
            assert_eq!(
                scores(&mut indexed, sql),
                scores(&mut plain, sql),
                "order: {sql}"
            );
        }

        assert_eq!(
            scores(&mut indexed, "SELECT score FROM t ORDER BY score"),
            vec![10, 50, 70, 90, 90]
        );
        assert_eq!(
            scores(
                &mut indexed,
                "SELECT score FROM t ORDER BY score DESC LIMIT 2"
            ),
            vec![90, 90]
        );

        // Projection composes with the fast path: the smallest score is id 5.
        let proj = indexed
            .query("SELECT id FROM t ORDER BY score LIMIT 1")
            .unwrap();
        assert_eq!(proj.columns().unwrap(), &["id".to_string()]);
        assert_eq!(proj.rows().unwrap(), &[vec![Value::Int(5)]]);

        // The fast path honors MVCC visibility: a deleted row drops out of the
        // index-ordered result.
        indexed.query("DELETE FROM t WHERE id = 5").unwrap();
        assert_eq!(
            scores(&mut indexed, "SELECT score FROM t ORDER BY score"),
            vec![50, 70, 90, 90]
        );
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
        // forever, the scan caps traversal at the total page count.
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
