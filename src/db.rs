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
use std::collections::{BTreeMap, BTreeSet};
use std::fs::{self, File, OpenOptions};
use std::io::{Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};
#[cfg(feature = "enterprise")]
use std::sync::Arc;
use std::time::Instant;

#[cfg(not(target_arch = "wasm32"))]
use fs2::FileExt;
use serde::{Deserialize, Serialize};

use crate::core::errors::{PvError, Result};
use crate::core::types::{
    pack_addr, unpack_addr, FileHeader, PageId, RecordAddr, RecordEnvelope, TxId, FILE_HEADER_SIZE,
    FORMAT_VERSION, FORMAT_VERSION_BASE, FORMAT_VERSION_INDEX, PAGE_HEADER_SIZE, PAGE_SIZE,
};
use crate::core::value::{Row, Value, DECIMAL_DEN};
use crate::engine::compliance::{ComplianceMonitor, RuntimeMetrics};
use crate::engine::mvcc::{Snapshot, TxManager};
use crate::engine::query::{
    agg_label, parse, AggFunc, Aggregate, CompareOp, HavingPred, HavingTerm, OrderBy, Predicate,
    Projection, SelectExpr, SelectItem, Statement,
};
use crate::engine::wasm::WasmRuntime;
use crate::storage::cache::{PageCache, DEFAULT_CACHE_PAGES};
use crate::storage::cas::{verify_blob_hash_hex, CasStore};
use crate::storage::index::SecondaryIndex;
use crate::storage::page::{RowPage, RowPageRef, SLOT_SIZE};
use crate::storage::record::{decode_record, encode_record};
use crate::storage::vle::{
    bake_monolith_bytes_with_index, verify_page_checksum, Backend, DevStore, MemStore, Monolith,
    RangeReader, RemoteStore,
};

/// Manifest file name within a development workspace.
pub const MANIFEST_FILE: &str = "pv_manifest.json";
/// Recovery marker written before a filesystem transaction may mutate pages.
pub const TRANSACTION_MARKER_FILE: &str = ".pv_transaction_active";
/// Private copy of the last committed workspace used for crash recovery.
pub const TRANSACTION_BACKUP_DIR: &str = ".pv_transaction_backup";
/// Lock file used to distinguish a live filesystem transaction from a crash.
pub const TRANSACTION_LOCK_FILE: &str = ".pv_transaction.lock";
const MAX_STREAM_TAIL_BYTES: usize = 64 * 1024 * 1024;

/// Largest record (envelope + body) that fits on a fresh page.
const MAX_RECORD: usize = PAGE_SIZE - PAGE_HEADER_SIZE - SLOT_SIZE;

// ---------------------------------------------------------------------------
// In-memory table metadata (bounded: O(tables), not O(rows))
// ---------------------------------------------------------------------------

struct Table {
    columns: Vec<String>,
    unique_columns: BTreeSet<String>,
    not_null_columns: BTreeSet<String>,
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
    /// On-disk format version of this catalog (see [`FORMAT_VERSION`]). Pre-freeze
    /// (0.10.x) workspaces have no such field, so they deserialize as `0` and are
    /// rejected by [`check_manifest_version`].
    #[serde(default)]
    format_version: u16,
    clock: u64,
    page_count: u64,
    tables: Vec<TableMeta>,
    cas_hashes: Vec<String>,
    #[serde(default)]
    cas_dir: Vec<(u64, u64)>,
    /// `(absolute offset, length)` of the binary secondary-index region within the
    /// monolith, present only in version-2 files that carry one. Absent for
    /// version-1 files and development workspaces (which persist indexes as JSON
    /// `pairs` instead).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    index_region: Option<(u64, u64)>,
}

/// Reject a manifest whose format version this build cannot read: `0` (a
/// pre-freeze workspace) or any value newer than [`FORMAT_VERSION`]. This is the
/// only version gate for development workspaces, which have no file header.
fn check_manifest_version(m: &Manifest) -> Result<()> {
    if m.format_version == 0 || m.format_version > FORMAT_VERSION {
        return Err(PvError::Corruption(format!(
            "unsupported workspace format version {}; this build reads up to {FORMAT_VERSION}",
            m.format_version
        )));
    }
    Ok(())
}

#[derive(Serialize, Deserialize)]
struct TableMeta {
    name: String,
    columns: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    unique_columns: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    not_null_columns: Vec<String>,
    first_page: Option<u64>,
    tail_id: Option<u64>,
    row_versions: u64,
    #[serde(default)]
    indexed_columns: Vec<String>,
    /// Secondary indexes serialized as JSON `(key, addresses)` pairs directly in
    /// the manifest. Used by development workspaces (which have no file region).
    /// Version-2 monoliths leave this empty and use `binary_indexes` instead;
    /// pre-1.2 files leave it empty and the index is rebuilt from
    /// `indexed_columns`.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    indexes: Vec<PersistedIndex>,
    /// Descriptors for secondary indexes stored in the monolith's binary index
    /// region. Each `offset` is relative to the region start (see
    /// [`Manifest::index_region`]). Present only in version-2 files.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    binary_indexes: Vec<BinIndexDesc>,
}

/// One secondary index serialized into the manifest as JSON: a column and its
/// `(key, addresses)` pairs in key order. Dev-workspace persistence.
#[derive(Serialize, Deserialize)]
struct PersistedIndex {
    column: String,
    pairs: Vec<(Value, Vec<RecordAddr>)>,
}

/// Locates one secondary index inside the monolith's binary index region:
/// the column it covers and the `[offset, offset + len)` byte slice (relative to
/// the region start) holding its [`SecondaryIndex::encode_binary`] blob.
#[derive(Serialize, Deserialize, Clone)]
struct BinIndexDesc {
    column: String,
    offset: u64,
    len: u64,
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

/// Resource bounds for executing SQL supplied by an untrusted caller.
#[derive(Debug, Clone, Copy)]
pub struct QueryLimits {
    /// Maximum record versions inspected by one statement.
    pub max_rows_scanned: usize,
    /// Approximate maximum bytes retained while building a result or mutation set.
    pub max_materialized_bytes: usize,
    /// Maximum rows returned to the caller.
    pub max_result_rows: usize,
    /// Optional wall-clock deadline, checked throughout scans and before returning.
    pub deadline: Option<Instant>,
}

/// A reusable, validated SQL template with positional `?` parameters.
///
/// PicoVolt does not retain a borrow of the database, so a prepared statement
/// can be cached by the caller and executed against any compatible handle.
#[derive(Debug, Clone)]
pub struct PreparedStatement {
    sql: String,
    parameter_count: usize,
}

impl PreparedStatement {
    /// The number of positional values required by [`execute`](Self::execute).
    pub fn parameter_count(&self) -> usize {
        self.parameter_count
    }

    /// Execute this statement against `database`.
    pub fn execute(&self, database: &mut Database, params: &[Value]) -> Result<QueryResult> {
        if params.len() != self.parameter_count {
            return Err(PvError::Schema(format!(
                "prepared statement expects {} parameters, got {}",
                self.parameter_count,
                params.len()
            )));
        }
        database.query_with(&self.sql, params)
    }
}

impl QueryLimits {
    /// Construct explicit query limits.
    pub const fn new(
        max_rows_scanned: usize,
        max_materialized_bytes: usize,
        max_result_rows: usize,
        deadline: Option<Instant>,
    ) -> Self {
        Self {
            max_rows_scanned,
            max_materialized_bytes,
            max_result_rows,
            deadline,
        }
    }
}

struct QueryBudget {
    limits: QueryLimits,
    rows_scanned: usize,
    materialized_bytes: usize,
}

impl QueryBudget {
    fn new(limits: QueryLimits) -> Self {
        Self {
            limits,
            rows_scanned: 0,
            materialized_bytes: 0,
        }
    }

    fn checkpoint(&self) -> Result<()> {
        if self
            .limits
            .deadline
            .is_some_and(|deadline| Instant::now() >= deadline)
        {
            return Err(PvError::ResourceLimit("deadline expired".into()));
        }
        Ok(())
    }

    fn scan_row(&mut self) -> Result<()> {
        self.checkpoint()?;
        self.rows_scanned = self.rows_scanned.saturating_add(1);
        if self.rows_scanned > self.limits.max_rows_scanned {
            return Err(PvError::ResourceLimit(format!(
                "scanned more than {} rows",
                self.limits.max_rows_scanned
            )));
        }
        Ok(())
    }

    fn materialize(&mut self, row: &Row) -> Result<()> {
        let bytes = row.iter().fold(std::mem::size_of::<Row>(), |total, value| {
            total.saturating_add(match value {
                Value::Null | Value::Int(_) | Value::Decimal(_) => std::mem::size_of::<Value>(),
                Value::Text(value) => std::mem::size_of::<Value>().saturating_add(value.len()),
                Value::Blob(value) => std::mem::size_of::<Value>().saturating_add(value.len()),
            })
        });
        self.materialized_bytes = self.materialized_bytes.saturating_add(bytes);
        if self.materialized_bytes > self.limits.max_materialized_bytes {
            return Err(PvError::ResourceLimit(format!(
                "materialized more than {} bytes",
                self.limits.max_materialized_bytes
            )));
        }
        Ok(())
    }

    fn check_result(&self, result: &QueryResult) -> Result<()> {
        if let QueryResult::Rows { rows, .. } = result {
            self.checkpoint()?;
            if rows.len() > self.limits.max_result_rows {
                return Err(PvError::ResourceLimit(format!(
                    "result has more than {} rows; add or lower LIMIT",
                    self.limits.max_result_rows
                )));
            }
        }
        Ok(())
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

enum TransactionRollback {
    Memory(Vec<u8>),
    Filesystem(PathBuf),
}

struct ActiveTransaction {
    rollback: TransactionRollback,
    previous_autocommit: bool,
    previous_durability: Durability,
    /// Held from before backup preparation through commit or rollback. The file
    /// itself persists, but the OS lock is released automatically on drop.
    _filesystem_lock: Option<File>,
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
    active_transaction: Option<ActiveTransaction>,
    #[cfg(feature = "enterprise")]
    enterprise: crate::enterprise::EnterpriseRuntime,
}

impl Database {
    /// Open (or create) a development workspace rooted at `path`.
    pub fn open_dev(path: impl AsRef<Path>) -> Result<Self> {
        let root = path.as_ref().to_path_buf();
        fs::create_dir_all(&root)?;
        let recovery_lock = acquire_transaction_lock(&root)?;
        recover_workspace_transaction(&root)?;
        drop(recovery_lock);
        let manifest_path = root.join(MANIFEST_FILE);

        if manifest_path.exists() {
            let manifest: Manifest = serde_json::from_slice(&fs::read(&manifest_path)?)?;
            check_manifest_version(&manifest)?;
            let dev = DevStore::open(&root, manifest.page_count)?;
            let mut cache = PageCache::new(Backend::Dev(dev), DEFAULT_CACHE_PAGES);
            let cas = CasStore::load_dev(&root, &manifest.cas_hashes)?;
            // Development workspaces persist indexes as JSON pairs, not a region.
            let tables = build_tables(&mut cache, &cas, &manifest, true, &[])?;
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
                active_transaction: None,
                #[cfg(feature = "enterprise")]
                enterprise: crate::enterprise::EnterpriseRuntime::default(),
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
                active_transaction: None,
                #[cfg(feature = "enterprise")]
                enterprise: crate::enterprise::EnterpriseRuntime::default(),
            })
        }
    }

    /// Open a baked `.pvdb` monolith as an owned, read-only snapshot.
    ///
    /// Snapshotting prevents another process from mutating a file-backed mapping
    /// while it is borrowed. For large images that should be fetched lazily, use
    /// [`open_streamed`](Self::open_streamed).
    pub fn open_prod(path: impl AsRef<Path>) -> Result<Self> {
        let mono = Monolith::open(path)?;
        let manifest: Manifest = serde_json::from_slice(mono.manifest_bytes())?;
        check_manifest_version(&manifest)?;
        let cas = CasStore::from_mapped(
            mono.mmap(),
            mono.cas_offset(),
            cas_pool_end(&manifest, mono.cas_offset(), mono.manifest_offset())?,
            &manifest.cas_dir,
            &manifest.cas_hashes,
        )?;
        // Slice the binary index region out of the mapping. The `Arc<Mmap>` handle
        // keeps the mapping alive past the move of `mono` into the cache below.
        let map = mono.mmap();
        let region = slice_index_region(
            &map,
            &manifest,
            mono.cas_offset() as u64,
            mono.manifest_offset(),
        )?;
        let mut cache = PageCache::new(Backend::Prod(mono), DEFAULT_CACHE_PAGES);
        let tables = build_tables(&mut cache, &cas, &manifest, false, region)?;
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
            active_transaction: None,
            #[cfg(feature = "enterprise")]
            enterprise: crate::enterprise::EnterpriseRuntime::default(),
        })
    }

    /// Open a baked `.pvdb` monolith through a [`RangeReader`], fetching pages on
    /// demand instead of holding the whole image in memory. The header, CAS pool,
    /// and manifest are read once up front; pages stream in as queries touch them.
    /// Read-only, with full time-travel history intact. `total_size` is the byte
    /// length of the image (e.g. an HTTP `Content-Length`).
    pub fn open_streamed(reader: Box<dyn RangeReader>, total_size: u64) -> Result<Self> {
        let header_bytes = reader.read_at(0, FILE_HEADER_SIZE)?;
        if header_bytes.len() != FILE_HEADER_SIZE {
            return Err(PvError::Corruption(
                "streamed: header range has the wrong length".into(),
            ));
        }
        let header = FileHeader::decode(&header_bytes)?; // validates magic + version
        let total_size = usize::try_from(total_size).map_err(|_| {
            PvError::Corruption("streamed: image is too large for this platform".into())
        })?;
        let cas_offset = usize::try_from(header.cas_offset)
            .map_err(|_| PvError::Corruption("streamed: CAS offset is too large".into()))?;
        let manifest_offset = usize::try_from(header.manifest_offset)
            .map_err(|_| PvError::Corruption("streamed: manifest offset is too large".into()))?;
        if cas_offset < FILE_HEADER_SIZE
            || manifest_offset < cas_offset
            || manifest_offset > total_size
            || (cas_offset - FILE_HEADER_SIZE) % PAGE_SIZE != 0
        {
            return Err(PvError::Corruption("streamed: inconsistent offsets".into()));
        }
        let page_count = ((cas_offset - FILE_HEADER_SIZE) / PAGE_SIZE) as u64;

        // The tail (CAS pool + manifest) is small relative to the pages and is read
        // once on open; the pages themselves are fetched lazily through the cache.
        let tail_len = total_size
            .checked_sub(cas_offset)
            .ok_or_else(|| PvError::Corruption("streamed: CAS offset past end of image".into()))?;
        if tail_len > MAX_STREAM_TAIL_BYTES {
            return Err(PvError::Corruption(format!(
                "streamed: CAS/index/manifest tail exceeds the {MAX_STREAM_TAIL_BYTES}-byte limit"
            )));
        }
        let tail = reader.read_at(cas_offset as u64, tail_len)?;
        if tail.len() != tail_len {
            return Err(PvError::Corruption(
                "streamed: tail range has the wrong length".into(),
            ));
        }
        let split = manifest_offset - cas_offset;
        if split > tail.len() {
            return Err(PvError::Corruption(
                "streamed: manifest offset is outside the returned tail".into(),
            ));
        }
        let manifest: Manifest = serde_json::from_slice(&tail[split..])?;
        check_manifest_version(&manifest)?;
        let pool_end = cas_pool_end(&manifest, cas_offset, manifest_offset)? - cas_offset;
        let pool = &tail[..pool_end];

        if manifest.cas_dir.len() != manifest.cas_hashes.len() {
            return Err(PvError::Corruption(
                "streamed: CAS dir/hash length mismatch".into(),
            ));
        }
        let mut cas = CasStore::new_memory();
        for (&(off, len), expected_hash) in manifest.cas_dir.iter().zip(&manifest.cas_hashes) {
            let off = off as usize;
            let end = off
                .checked_add(len as usize)
                .filter(|&e| e <= pool.len())
                .ok_or_else(|| PvError::Corruption("streamed: CAS blob out of bounds".into()))?;
            let blob = &pool[off..end];
            verify_blob_hash_hex(blob, expected_hash)?;
            cas.put(blob)?;
        }

        // The binary index region (if any) was fetched as part of the tail; it sits
        // between the CAS pool and the manifest, so its tail-relative offset is
        // `absolute offset - cas_offset`.
        let region: &[u8] = match manifest.index_region {
            None => &[],
            Some((off, len)) => {
                let rel = (off as usize).checked_sub(cas_offset).ok_or_else(|| {
                    PvError::Corruption("streamed: index region before CAS pool".into())
                })?;
                let end = rel
                    .checked_add(len as usize)
                    .filter(|&e| e <= tail.len())
                    .ok_or_else(|| {
                        PvError::Corruption("streamed: index region out of bounds".into())
                    })?;
                &tail[rel..end]
            }
        };

        let backend = Backend::Remote(RemoteStore::new(reader, page_count));
        let mut cache = PageCache::new(backend, DEFAULT_CACHE_PAGES);
        let tables = build_tables(&mut cache, &cas, &manifest, false, region)?;
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
            active_transaction: None,
            #[cfg(feature = "enterprise")]
            enterprise: crate::enterprise::EnterpriseRuntime::default(),
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
            active_transaction: None,
            #[cfg(feature = "enterprise")]
            enterprise: crate::enterprise::EnterpriseRuntime::default(),
        }
    }

    /// Load a baked `.pvdb` **byte image** into a fresh, **writable** in-memory
    /// database, the inverse of [`bake_to_bytes`](Self::bake_to_bytes).
    ///
    /// Unlike [`open_prod`](Self::open_prod) (read-only), this copies the pages
    /// into a writable store so editing can continue, and it preserves the full MVCC
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
        check_manifest_version(&manifest)?;

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
        let pool_end = cas_pool_end(&manifest, cas_offset, manifest_offset)?;
        let pool = &bytes[cas_offset..pool_end];
        let mut cas = CasStore::new_memory();
        for (&(off, len), expected_hash) in manifest.cas_dir.iter().zip(&manifest.cas_hashes) {
            let off = off as usize;
            let end = off
                .checked_add(len as usize)
                .filter(|&e| e <= pool.len())
                .ok_or_else(|| PvError::Corruption("import: CAS blob out of bounds".into()))?;
            let blob = &pool[off..end];
            verify_blob_hash_hex(blob, expected_hash)?;
            cas.put(blob)?;
        }

        let region = slice_index_region(bytes, &manifest, cas_offset as u64, manifest_offset)?;
        let mut cache = PageCache::new(Backend::Mem(mem), DEFAULT_CACHE_PAGES);
        let tables = build_tables(&mut cache, &cas, &manifest, true, region)?;
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
            active_transaction: None,
            #[cfg(feature = "enterprise")]
            enterprise: crate::enterprise::EnterpriseRuntime::default(),
        })
    }

    /// Compile the current database into a `.pvdb` monolith **byte image** (no
    /// filesystem). Works for any backend; the natural way to export an
    /// in-memory database.
    pub fn bake_to_bytes(&mut self) -> Result<Vec<u8>> {
        self.flush()?;
        let pages = self.cache.borrow().backend().read_all_pages()?;
        let (cas_pool, _dir) = self.cas.pack()?;

        // Serialize secondary indexes into a compact binary region that sits
        // between the CAS pool and the manifest; its absolute offset is fixed once
        // the page block and CAS pool are sized.
        let (region, descs) = self.build_index_region();
        let region_offset = (FILE_HEADER_SIZE + pages.len() * PAGE_SIZE + cas_pool.len()) as u64;
        let has_constraints = self
            .tables
            .values()
            .any(|table| !table.unique_columns.is_empty() || !table.not_null_columns.is_empty());
        let format_version = if has_constraints {
            FORMAT_VERSION
        } else if region.is_empty() {
            FORMAT_VERSION_BASE
        } else {
            FORMAT_VERSION_INDEX
        };

        let manifest = self.build_manifest(
            true,
            &IndexPlan::Binary {
                descs,
                offset: region_offset,
                len: region.len() as u64,
            },
        )?;
        let json = serde_json::to_vec(&manifest)?;
        bake_monolith_bytes_with_index(&pages, &cas_pool, &region, &json, format_version)
    }

    /// Encode every table's secondary indexes into one contiguous binary blob,
    /// returning the blob and, per table, the descriptors locating each column's
    /// index within it (offsets relative to the blob start).
    fn build_index_region(&self) -> (Vec<u8>, Vec<(String, Vec<BinIndexDesc>)>) {
        let mut region = Vec::new();
        let mut all = Vec::new();
        for (name, t) in &self.tables {
            let mut descs = Vec::new();
            for (col, idx) in &t.indexes {
                let blob = idx.encode_binary();
                let offset = region.len() as u64;
                let len = blob.len() as u64;
                region.extend_from_slice(&blob);
                descs.push(BinIndexDesc {
                    column: col.clone(),
                    offset,
                    len,
                });
            }
            if !descs.is_empty() {
                all.push((name.clone(), descs));
            }
        }
        (region, all)
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

    /// Validate and retain a reusable SQL template.
    pub fn prepare(&self, sql: impl Into<String>) -> Result<PreparedStatement> {
        let sql = sql.into();
        let parameter_count = crate::engine::query::parameter_count(&sql);
        let placeholders = vec![Value::Null; parameter_count];
        let bound = crate::engine::query::bind_params(&sql, &placeholders)?;
        parse(&bound)?;
        Ok(PreparedStatement {
            sql,
            parameter_count,
        })
    }

    /// Run a closure atomically against an in-memory database or development
    /// workspace.
    ///
    /// Filesystem transactions make a durable copy of the last committed
    /// workspace before the callback runs. This is intentionally conservative:
    /// transaction start is O(database size), but an error or process crash can
    /// restore the complete prior state rather than attempting best-effort page
    /// rollback. Use the explicit begin, commit, and rollback methods when a
    /// closure does not fit the caller's control flow.
    pub fn transaction<T>(
        &mut self,
        operation: impl FnOnce(&mut Database) -> Result<T>,
    ) -> Result<T> {
        self.begin_transaction()?;
        match operation(self) {
            Ok(value) => {
                if !self.in_transaction() {
                    return Err(PvError::Transaction(
                        "transaction callback changed the transaction state".into(),
                    ));
                }
                match self.commit_transaction() {
                    Ok(()) => Ok(value),
                    Err(commit_error) => {
                        let _ = self.rollback_transaction();
                        Err(commit_error)
                    }
                }
            }
            Err(error) => {
                self.rollback_transaction().map_err(|rollback_error| {
                    PvError::Transaction(format!(
                        "operation failed ({error}); rollback also failed ({rollback_error})"
                    ))
                })?;
                Err(error)
            }
        }
    }

    /// Begin an explicit multi-statement transaction.
    ///
    /// Nested transactions are rejected. For a development workspace, this
    /// writes and syncs a recovery copy plus a marker before returning.
    pub fn begin_transaction(&mut self) -> Result<()> {
        self.ensure_writable()?;
        if self.active_transaction.is_some() {
            return Err(PvError::Transaction(
                "a transaction is already active".into(),
            ));
        }

        let previous_autocommit = self.autocommit;
        let previous_durability = self.durability;
        let (rollback, filesystem_lock) = if let Some(root) = self.root.clone() {
            let lock = acquire_transaction_lock(&root)?;
            self.durability = Durability::Sync;
            if let Err(error) = self.flush() {
                self.durability = previous_durability;
                return Err(error);
            }
            self.durability = previous_durability;
            prepare_workspace_transaction(&root)?;
            (TransactionRollback::Filesystem(root), Some(lock))
        } else {
            (TransactionRollback::Memory(self.bake_to_bytes()?), None)
        };

        self.autocommit = false;
        self.active_transaction = Some(ActiveTransaction {
            rollback,
            previous_autocommit,
            previous_durability,
            _filesystem_lock: filesystem_lock,
        });
        #[cfg(feature = "enterprise")]
        self.enterprise.emit(crate::enterprise::AuditEvent::pending(
            crate::enterprise::AuditEventKind::TransactionBegan,
            self.current_tx(),
        ));
        Ok(())
    }

    /// Commit the active transaction.
    ///
    /// Filesystem data and the manifest are synced before the recovery marker is
    /// removed. Removing that marker is the commit point: before it, reopening
    /// rolls back; after it, reopening keeps the new state.
    pub fn commit_transaction(&mut self) -> Result<()> {
        let (filesystem_root, previous_durability) = match self.active_transaction.as_ref() {
            Some(state) => (
                match &state.rollback {
                    TransactionRollback::Filesystem(root) => Some(root.clone()),
                    TransactionRollback::Memory(_) => None,
                },
                state.previous_durability,
            ),
            None => return Err(PvError::Transaction("no transaction is active".into())),
        };

        if filesystem_root.is_some() {
            self.durability = Durability::Sync;
        }
        if let Err(error) = self.flush() {
            self.durability = previous_durability;
            return Err(error);
        }

        if let Some(root) = &filesystem_root {
            commit_workspace_transaction(root)?;
        }

        let state = self
            .active_transaction
            .take()
            .expect("transaction state checked above");
        self.autocommit = state.previous_autocommit;
        self.durability = state.previous_durability;
        if let TransactionRollback::Filesystem(root) = state.rollback {
            // The marker is already gone, so cleanup failure cannot make the
            // committed data ambiguous. A later open also removes an orphan.
            let _ = fs::remove_dir_all(root.join(TRANSACTION_BACKUP_DIR));
        }
        #[cfg(feature = "enterprise")]
        self.enterprise.emit(crate::enterprise::AuditEvent::pending(
            crate::enterprise::AuditEventKind::TransactionCommitted,
            self.current_tx(),
        ));
        Ok(())
    }

    /// Restore the state captured by begin_transaction.
    pub fn rollback_transaction(&mut self) -> Result<()> {
        let Some(mut state) = self.active_transaction.take() else {
            return Err(PvError::Transaction("no transaction is active".into()));
        };

        #[cfg(feature = "enterprise")]
        let enterprise = self.enterprise.clone();
        match state.rollback {
            TransactionRollback::Memory(snapshot) => {
                let mut restored = Database::import_bytes(&snapshot)?;
                restored.autocommit = state.previous_autocommit;
                restored.durability = state.previous_durability;
                #[cfg(feature = "enterprise")]
                {
                    restored.enterprise = enterprise;
                    restored
                        .enterprise
                        .emit(crate::enterprise::AuditEvent::pending(
                            crate::enterprise::AuditEventKind::TransactionRolledBack,
                            restored.current_tx(),
                        ));
                }
                *self = restored;
            }
            TransactionRollback::Filesystem(root) => {
                // Drop all cached filesystem handles before replacing live files.
                *self = Database::open_memory();
                restore_workspace_transaction(&root)?;
                // Recovery is complete and its marker is gone. Release the
                // transaction lock before the normal open path acquires it for
                // its own recovery check.
                drop(state._filesystem_lock.take());
                let mut restored = Database::open_dev(&root)?;
                restored.autocommit = state.previous_autocommit;
                restored.durability = state.previous_durability;
                #[cfg(feature = "enterprise")]
                {
                    restored.enterprise = enterprise;
                    restored
                        .enterprise
                        .emit(crate::enterprise::AuditEvent::pending(
                            crate::enterprise::AuditEventKind::TransactionRolledBack,
                            restored.current_tx(),
                        ));
                }
                *self = restored;
            }
        }
        Ok(())
    }

    /// Whether this handle currently owns an explicit transaction.
    pub fn in_transaction(&self) -> bool {
        self.active_transaction.is_some()
    }

    /// Execute a parameterized statement with explicit resource limits. This is
    /// intended for servers and other trust boundaries; embedded callers can use
    /// [`query_with`](Database::query_with) without imposed limits.
    pub fn query_with_limits(
        &mut self,
        sql: &str,
        params: &[Value],
        limits: QueryLimits,
    ) -> Result<QueryResult> {
        let bound = crate::engine::query::bind_params(sql, params)?;
        let mut budget = QueryBudget::new(limits);
        budget.checkpoint()?;
        let result = self.execute_statement(parse(&bound)?, Some(&mut budget))?;
        budget.check_result(&result)?;
        Ok(result)
    }

    /// Execute a single SQL statement.
    pub fn query(&mut self, sql: &str) -> Result<QueryResult> {
        self.execute_statement(parse(sql)?, None)
    }

    fn execute_statement(
        &mut self,
        statement: Statement,
        mut budget: Option<&mut QueryBudget>,
    ) -> Result<QueryResult> {
        match statement {
            Statement::Begin => {
                self.begin_transaction()?;
                Ok(QueryResult::Done)
            }
            Statement::Commit => {
                self.commit_transaction()?;
                Ok(QueryResult::Done)
            }
            Statement::Rollback => {
                self.rollback_transaction()?;
                Ok(QueryResult::Done)
            }
            Statement::CreateTable {
                name,
                columns,
                unique_columns,
                not_null_columns,
            } => {
                self.create_table_with_constraints(
                    &name,
                    columns,
                    unique_columns,
                    not_null_columns,
                )?;
                Ok(QueryResult::Done)
            }
            Statement::CreateTableIfNotExists {
                name,
                columns,
                unique_columns,
                not_null_columns,
            } => {
                if !self.tables.contains_key(&name) {
                    self.create_table_with_constraints(
                        &name,
                        columns,
                        unique_columns,
                        not_null_columns,
                    )?;
                }
                Ok(QueryResult::Done)
            }
            Statement::CreateIndex {
                table,
                column,
                unique,
            } => {
                if unique {
                    self.validate_unique(&table, &column)?;
                }
                self.create_index_bounded(&table, &column, budget.as_deref_mut())?;
                if unique {
                    self.tables
                        .get_mut(&table)
                        .expect("existence checked")
                        .unique_columns
                        .insert(column);
                    self.maybe_flush()?;
                }
                Ok(QueryResult::Done)
            }
            Statement::Insert { table, values } => {
                self.insert(&table, values)?;
                Ok(QueryResult::Mutated(1))
            }
            Statement::InsertMany { table, rows } => {
                let count = rows.len();
                let mut pending = Vec::with_capacity(count);
                for values in &rows {
                    self.validate_insert_values(&table, values, &pending)?;
                    pending.push(values.clone());
                }
                for values in rows {
                    self.insert(&table, values)?;
                }
                Ok(QueryResult::Mutated(count))
            }
            Statement::Select {
                table,
                projection,
                distinct,
                before,
                filter,
                group_by,
                having,
                order,
                limit,
                offset,
            } => {
                // Fast path: a bare `COUNT(*)` (no WHERE/GROUP BY/HAVING/ORDER/LIMIT)
                // only needs record visibility, so count envelopes without decoding
                // any row bodies.
                if filter.is_none()
                    && group_by.is_empty()
                    && having.is_none()
                    && order.is_empty()
                    && limit.is_none()
                    && offset == 0
                    && !distinct
                {
                    if let Some(alias) = count_star_only(&projection) {
                        let n = self.count_visible(&table, before, budget.as_deref_mut())?;
                        return Ok(QueryResult::Rows {
                            columns: vec![alias.unwrap_or_else(|| "count".into())],
                            rows: vec![vec![Value::Int(n as i64)]],
                        });
                    }
                }

                let grouped = !group_by.is_empty()
                    || projection_has_aggregate(&projection)
                    || having.is_some();
                // Fast path: a single-column `ORDER BY` on an indexed column with no
                // `WHERE`, grouping, aggregate, or `DISTINCT` reads the ordered index
                // in key order, skipping the sort and (with `LIMIT`) stopping early.
                if !grouped && filter.is_none() && !distinct && order.len() == 1 {
                    let ob = &order[0];
                    if self.has_index(&table, &ob.column) {
                        let (columns, rows) = self.select_ordered_by_index(
                            &table,
                            ob,
                            before,
                            limit.map(|n| n.saturating_add(offset)),
                            budget.as_deref_mut(),
                        )?;
                        return project_select(
                            columns,
                            rows,
                            projection,
                            &[],
                            false,
                            limit,
                            offset,
                        );
                    }
                }
                let (columns, rows) = self.select_filtered_bounded(
                    &table,
                    filter.as_ref(),
                    before,
                    budget.as_deref_mut(),
                )?;
                if grouped {
                    let items = projection_to_items(projection)?;
                    project_grouped(
                        columns, rows, items, group_by, having, order, distinct, limit, offset,
                    )
                } else {
                    project_select(columns, rows, projection, &order, distinct, limit, offset)
                }
            }
            Statement::SelectJoin {
                projection,
                distinct,
                left_table,
                right_table,
                left_column,
                right_column,
                left_join,
                filter,
                order,
                limit,
                offset,
            } => self.select_join(
                &left_table,
                &right_table,
                &left_column,
                &right_column,
                left_join,
                projection,
                distinct,
                filter.as_ref(),
                &order,
                limit,
                offset,
                budget.as_deref_mut(),
            ),
            Statement::Update { table, set, filter } => {
                let n = self.update_where_bounded(
                    &table,
                    &set.0,
                    &set.1,
                    &filter,
                    budget.as_deref_mut(),
                )?;
                Ok(QueryResult::Mutated(n))
            }
            Statement::Delete { table, filter } => {
                let n = self.delete_where_bounded(&table, &filter, budget)?;
                Ok(QueryResult::Mutated(n))
            }
            Statement::DropTable { table } => {
                self.drop_table(&table)?;
                Ok(QueryResult::Done)
            }
            Statement::DropTableIfExists { table } => {
                if self.tables.contains_key(&table) {
                    self.drop_table(&table)?;
                }
                Ok(QueryResult::Done)
            }
        }
    }

    // --- programmatic API --------------------------------------------------

    /// Create a table with the given column names.
    pub fn create_table(&mut self, name: &str, columns: Vec<String>) -> Result<()> {
        self.create_table_with_constraints(name, columns, Vec::new(), Vec::new())
    }

    fn create_table_with_constraints(
        &mut self,
        name: &str,
        columns: Vec<String>,
        unique_columns: Vec<String>,
        not_null_columns: Vec<String>,
    ) -> Result<()> {
        self.ensure_writable()?;
        if self.tables.contains_key(name) {
            return Err(PvError::Schema(format!("table `{name}` already exists")));
        }
        self.tables.insert(
            name.to_string(),
            Table {
                columns,
                unique_columns: unique_columns.into_iter().collect(),
                not_null_columns: not_null_columns.into_iter().collect(),
                first_page: None,
                tail_id: None,
                tail: None,
                row_versions: 0,
                indexes: BTreeMap::new(),
            },
        );
        self.maybe_flush()
    }

    fn validate_unique(&self, table_name: &str, column: &str) -> Result<()> {
        let (_, rows) = self.select(table_name, None)?;
        let table = self
            .tables
            .get(table_name)
            .ok_or_else(|| PvError::TableNotFound(table_name.into()))?;
        let ix = column_index(table, column)?;
        let mut seen: Vec<Value> = Vec::new();
        for row in rows {
            if row[ix] != Value::Null {
                if seen.iter().any(|value| values_equal(value, &row[ix])) {
                    return Err(PvError::Schema(format!(
                        "cannot create unique index: duplicate value in `{column}`"
                    )));
                }
                seen.push(row[ix].clone());
            }
        }
        Ok(())
    }

    /// Create an equality index on `column`, built from the current rows.
    pub fn create_index(&mut self, table_name: &str, column: &str) -> Result<()> {
        self.create_index_bounded(table_name, column, None)
    }

    fn create_index_bounded(
        &mut self,
        table_name: &str,
        column: &str,
        mut budget: Option<&mut QueryBudget>,
    ) -> Result<()> {
        let mut index = SecondaryIndex::new();
        {
            let table = self
                .tables
                .get(table_name)
                .ok_or_else(|| PvError::TableNotFound(table_name.into()))?;
            let col_ix = column_index(table, column)?;
            let mut cache = self.cache.borrow_mut();
            scan(&mut cache, table, &self.cas, |addr, _env, row| {
                if let Some(budget) = budget.as_deref_mut() {
                    budget.scan_row()?;
                    budget.materialize(row)?;
                }
                index.insert(&row[col_ix], addr);
                Ok(())
            })?;
        }
        if let Some(budget) = budget {
            budget.checkpoint()?;
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
        self.validate_insert_values(table_name, &values, &[])?;
        self.insert_validated(table_name, values)
    }

    fn validate_insert_values(
        &self,
        table_name: &str,
        values: &[Value],
        pending: &[Vec<Value>],
    ) -> Result<()> {
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

        let (columns, unique_columns, not_null_columns) = {
            let table = self.tables.get(table_name).expect("existence checked");
            (
                table.columns.clone(),
                table.unique_columns.clone(),
                table.not_null_columns.clone(),
            )
        };
        for column in &not_null_columns {
            let ix = col_pos(&columns, column)?;
            if values[ix] == Value::Null {
                return Err(PvError::Schema(format!(
                    "column `{column}` may not be NULL"
                )));
            }
        }
        for column in &unique_columns {
            let ix = col_pos(&columns, column)?;
            if values[ix] != Value::Null {
                let (_, existing) = self.select(table_name, None)?;
                if existing
                    .iter()
                    .chain(pending.iter())
                    .any(|row| values_equal(&row[ix], &values[ix]))
                {
                    return Err(PvError::Schema(format!(
                        "duplicate value for unique column `{column}`"
                    )));
                }
            }
        }

        Ok(())
    }

    fn insert_validated(&mut self, table_name: &str, values: Vec<Value>) -> Result<()> {
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
        self.delete_where_bounded(table_name, pred, None)
    }

    fn delete_where_bounded(
        &mut self,
        table_name: &str,
        pred: &Predicate,
        mut budget: Option<&mut QueryBudget>,
    ) -> Result<usize> {
        self.ensure_writable()?;
        let snapshot = self.txm.snapshot();
        let matches = self.collect_matching(table_name, pred, &snapshot, budget.as_deref_mut())?;
        if let Some(budget) = budget {
            budget.checkpoint()?;
        }
        let tx = self.txm.begin_write();

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
        mut budget: Option<&mut QueryBudget>,
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
                if let Some(budget) = budget.as_deref_mut() {
                    budget.scan_row()?;
                }
                let (env, row) = read_record_at(&mut cache, table, &self.cas, addr)?;
                if snapshot.sees(&env) && row_matches(pred, &columns, &row)? {
                    if let Some(budget) = budget.as_deref_mut() {
                        budget.materialize(&row)?;
                    }
                    hits.push((addr, row));
                }
            }
        } else {
            scan(&mut cache, table, &self.cas, |addr, env, row| {
                if let Some(budget) = budget.as_deref_mut() {
                    budget.scan_row()?;
                }
                if snapshot.sees(env) && row_matches(pred, &columns, row)? {
                    if let Some(budget) = budget.as_deref_mut() {
                        budget.materialize(row)?;
                    }
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

    /// Execute a basic equality join. The right side is hashed once, giving
    /// linear expected execution rather than a nested-loop scan.
    #[allow(clippy::too_many_arguments)] // each argument is a parsed JOIN clause
    fn select_join(
        &self,
        left_table: &str,
        right_table: &str,
        left_column: &str,
        right_column: &str,
        left_join: bool,
        projection: Projection,
        distinct: bool,
        filter: Option<&Predicate>,
        order: &[OrderBy],
        limit: Option<usize>,
        offset: usize,
        mut budget: Option<&mut QueryBudget>,
    ) -> Result<QueryResult> {
        let (left_columns, left_rows) =
            self.select_filtered_bounded(left_table, None, None, budget.as_deref_mut())?;
        let (right_columns, right_rows) =
            self.select_filtered_bounded(right_table, None, None, budget.as_deref_mut())?;
        let left_key = join_side_col_pos(&left_columns, left_table, left_column)?;
        let right_key = join_side_col_pos(&right_columns, right_table, right_column)?;

        let mut right_by_key: BTreeMap<Value, Vec<&Row>> = BTreeMap::new();
        for row in &right_rows {
            if row[right_key] != Value::Null {
                right_by_key
                    .entry(join_key(&row[right_key]))
                    .or_default()
                    .push(row);
            }
        }

        let mut rows = Vec::new();
        for left in &left_rows {
            let matches = if left[left_key] == Value::Null {
                None
            } else {
                right_by_key.get(&join_key(&left[left_key]))
            };
            if let Some(matches) = matches {
                for right in matches {
                    let mut row = left.clone();
                    row.extend_from_slice(right);
                    if let Some(budget) = budget.as_deref_mut() {
                        budget.materialize(&row)?;
                    }
                    rows.push(row);
                }
            } else if left_join {
                let mut row = left.clone();
                row.resize(row.len() + right_columns.len(), Value::Null);
                if let Some(budget) = budget.as_deref_mut() {
                    budget.materialize(&row)?;
                }
                rows.push(row);
            }
        }

        let columns: Vec<String> = left_columns
            .iter()
            .map(|column| format!("{left_table}.{column}"))
            .chain(
                right_columns
                    .iter()
                    .map(|column| format!("{right_table}.{column}")),
            )
            .collect();
        if let Some(filter) = filter {
            check_predicate_columns(&columns, filter)?;
            rows = rows
                .into_iter()
                .filter_map(|row| match row_matches(filter, &columns, &row) {
                    Ok(true) => Some(Ok(row)),
                    Ok(false) => None,
                    Err(error) => Some(Err(error)),
                })
                .collect::<Result<Vec<_>>>()?;
        }
        project_select(columns, rows, projection, order, distinct, limit, offset)
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
        self.select_filtered_bounded(table_name, filter, before, None)
    }

    fn select_filtered_bounded(
        &self,
        table_name: &str,
        filter: Option<&Predicate>,
        before: Option<u64>,
        mut budget: Option<&mut QueryBudget>,
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
                    if let Some(budget) = budget.as_deref_mut() {
                        budget.scan_row()?;
                    }
                    if snapshot.sees(env) {
                        if let Some(budget) = budget.as_deref_mut() {
                            budget.materialize(row)?;
                        }
                        rows.push(row.clone());
                    }
                    Ok(())
                })?;
            }
            Some(pred) => {
                check_predicate_columns(&columns, pred)?;
                if let Some(addrs) = index_candidates(table, pred) {
                    for addr in addrs {
                        if let Some(budget) = budget.as_deref_mut() {
                            budget.scan_row()?;
                        }
                        let (env, row) = read_record_at(&mut cache, table, &self.cas, addr)?;
                        if snapshot.sees(&env) && row_matches(pred, &columns, &row)? {
                            if let Some(budget) = budget.as_deref_mut() {
                                budget.materialize(&row)?;
                            }
                            rows.push(row);
                        }
                    }
                } else {
                    scan(&mut cache, table, &self.cas, |_a, env, row| {
                        if let Some(budget) = budget.as_deref_mut() {
                            budget.scan_row()?;
                        }
                        if snapshot.sees(env) && row_matches(pred, &columns, row)? {
                            if let Some(budget) = budget.as_deref_mut() {
                                budget.materialize(row)?;
                            }
                            rows.push(row.clone());
                        }
                        Ok(())
                    })?;
                }
            }
        }
        Ok((columns, rows))
    }

    /// Count records visible as of `before` (or the latest tx) by reading only the
    /// MVCC envelopes, skipping all row-body decoding. Powers the bare `COUNT(*)`
    /// fast path.
    fn count_visible(
        &self,
        table_name: &str,
        before: Option<u64>,
        mut budget: Option<&mut QueryBudget>,
    ) -> Result<u64> {
        let table = self
            .tables
            .get(table_name)
            .ok_or_else(|| PvError::TableNotFound(table_name.into()))?;
        let snapshot = Snapshot::as_of(before.unwrap_or_else(|| self.txm.current()));
        let mut count = 0u64;
        let mut cache = self.cache.borrow_mut();
        scan_envelopes(&mut cache, table, |env| {
            if let Some(budget) = budget.as_deref_mut() {
                budget.scan_row()?;
            }
            if snapshot.sees(env) {
                count += 1;
            }
            Ok(())
        })?;
        Ok(count)
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
        mut budget: Option<&mut QueryBudget>,
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
            if let Some(budget) = budget.as_deref_mut() {
                budget.scan_row()?;
            }
            let (env, row) = read_record_at(&mut cache, table, &self.cas, addr)?;
            if snapshot.sees(&env) {
                if let Some(budget) = budget.as_deref_mut() {
                    budget.materialize(&row)?;
                }
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
        self.update_where_bounded(table_name, set_column, set_value, pred, None)
    }

    fn update_where_bounded(
        &mut self,
        table_name: &str,
        set_column: &str,
        set_value: &Value,
        pred: &Predicate,
        mut budget: Option<&mut QueryBudget>,
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
        let matches = self.collect_matching(table_name, pred, &snapshot, budget.as_deref_mut())?;
        let count = matches.len();
        if count == 0 {
            return Ok(0);
        }
        let (is_unique, is_not_null) = {
            let table = self.tables.get(table_name).expect("existence checked");
            (
                table.unique_columns.contains(set_column),
                table.not_null_columns.contains(set_column),
            )
        };
        if is_not_null && *set_value == Value::Null {
            return Err(PvError::Schema(format!(
                "column `{set_column}` may not be NULL"
            )));
        }
        if is_unique && *set_value != Value::Null {
            let already_matching = matches
                .iter()
                .filter(|(_, row)| row[set_ix] == *set_value)
                .count();
            let existing = self
                .select(table_name, None)?
                .1
                .iter()
                .filter(|row| values_equal(&row[set_ix], set_value))
                .count();
            if count > 1 || existing > already_matching {
                return Err(PvError::Schema(format!(
                    "duplicate value for unique column `{set_column}`"
                )));
            }
        }
        if let Some(budget) = budget {
            budget.checkpoint()?;
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

    // --- enterprise integration, compliance & extensions -------------------

    /// Attach a stable identity used by optional enterprise event sinks.
    #[cfg(feature = "enterprise")]
    pub fn configure_enterprise(&mut self, config: crate::enterprise::EnterpriseConfig) {
        self.enterprise.configure(config);
    }

    /// Borrow the configured enterprise identity, if any.
    #[cfg(feature = "enterprise")]
    pub fn enterprise_config(&self) -> Option<&crate::enterprise::EnterpriseConfig> {
        self.enterprise.config()
    }

    /// Attach a host-owned transaction audit destination.
    ///
    /// Events contain no SQL, values, paths, credentials, or user identities.
    #[cfg(feature = "enterprise")]
    pub fn set_audit_sink(&mut self, sink: Arc<dyn crate::enterprise::AuditSink>) {
        self.enterprise.set_sink(sink);
    }

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
        let manifest = self.build_manifest(false, &IndexPlan::Json)?;
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

    fn build_manifest(&self, include_cas_dir: bool, plan: &IndexPlan) -> Result<Manifest> {
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
            .map(|(name, t)| {
                let (indexes, binary_indexes) = match plan {
                    IndexPlan::Json => (
                        t.indexes
                            .iter()
                            .map(|(col, idx)| PersistedIndex {
                                column: col.clone(),
                                pairs: idx.to_pairs(),
                            })
                            .collect(),
                        Vec::new(),
                    ),
                    IndexPlan::Binary { descs, .. } => (
                        Vec::new(),
                        descs
                            .iter()
                            .find(|(n, _)| n == name)
                            .map(|(_, d)| d.clone())
                            .unwrap_or_default(),
                    ),
                };
                TableMeta {
                    name: name.clone(),
                    columns: t.columns.clone(),
                    unique_columns: t.unique_columns.iter().cloned().collect(),
                    not_null_columns: t.not_null_columns.iter().cloned().collect(),
                    first_page: t.first_page,
                    tail_id: t.tail_id,
                    row_versions: t.row_versions,
                    indexed_columns: t.indexes.keys().cloned().collect(),
                    indexes,
                    binary_indexes,
                }
            })
            .collect();
        let page_count = self.cache.borrow().backend().page_count();
        let has_constraints = self
            .tables
            .values()
            .any(|table| !table.unique_columns.is_empty() || !table.not_null_columns.is_empty());
        let (format_version, index_region) = match plan {
            // Dev workspaces and region-less files stay at the base version so an
            // older build can still read them.
            IndexPlan::Json if has_constraints => (FORMAT_VERSION, None),
            IndexPlan::Json => (FORMAT_VERSION_BASE, None),
            IndexPlan::Binary { offset, len, .. } if *len > 0 => {
                let version = if has_constraints {
                    FORMAT_VERSION
                } else {
                    FORMAT_VERSION_INDEX
                };
                (version, Some((*offset, *len)))
            }
            IndexPlan::Binary { .. } if has_constraints => (FORMAT_VERSION, None),
            IndexPlan::Binary { .. } => (FORMAT_VERSION_BASE, None),
        };
        Ok(Manifest {
            format_version,
            clock: self.txm.current(),
            page_count,
            tables,
            cas_hashes,
            cas_dir,
            index_region,
        })
    }
}

/// How a [`Manifest`] should persist secondary indexes.
enum IndexPlan {
    /// JSON `(key, addresses)` pairs inline in the manifest (development
    /// workspaces, which have no monolith region).
    Json,
    /// Descriptors into a binary index region at absolute file `offset` spanning
    /// `len` bytes. `descs` maps each table name to its per-column descriptors.
    Binary {
        descs: Vec<(String, Vec<BinIndexDesc>)>,
        offset: u64,
        len: u64,
    },
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

fn prepare_workspace_transaction(root: &Path) -> Result<()> {
    let marker = root.join(TRANSACTION_MARKER_FILE);
    let marker_tmp = root.join(".pv_transaction_active.tmp");
    let backup = root.join(TRANSACTION_BACKUP_DIR);
    if marker.exists() {
        return Err(PvError::Transaction(
            "workspace already has an active recovery marker".into(),
        ));
    }
    if backup.exists() {
        fs::remove_dir_all(&backup)?;
    }
    if marker_tmp.exists() {
        fs::remove_file(&marker_tmp)?;
    }

    fs::create_dir(&backup)?;
    for name in [MANIFEST_FILE, "chunks", "blobs"] {
        let source = root.join(name);
        if source.exists() {
            copy_tree_synced(&source, &backup.join(name))?;
        }
    }
    sync_directory(&backup)?;

    {
        let mut file = File::create(&marker_tmp)?;
        file.write_all(b"PVTX1\n")?;
        file.sync_all()?;
    }
    fs::rename(&marker_tmp, &marker)?;
    sync_directory(root)?;
    Ok(())
}

#[cfg(not(target_arch = "wasm32"))]
fn acquire_transaction_lock(root: &Path) -> Result<File> {
    let lock = OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .open(root.join(TRANSACTION_LOCK_FILE))?;
    lock.try_lock_exclusive().map_err(|error| {
        PvError::Transaction(format!(
            "workspace transaction is active in another handle or process: {error}"
        ))
    })?;
    Ok(lock)
}

#[cfg(target_arch = "wasm32")]
fn acquire_transaction_lock(root: &Path) -> Result<File> {
    // Browser databases use the in-memory/OPFS wrapper and never enter this
    // filesystem transaction path. Keep the native API compilable for wasm.
    Ok(OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .open(root.join(TRANSACTION_LOCK_FILE))?)
}

fn commit_workspace_transaction(root: &Path) -> Result<()> {
    let marker = root.join(TRANSACTION_MARKER_FILE);
    if !marker.exists() {
        return Err(PvError::Transaction(
            "filesystem transaction recovery marker is missing".into(),
        ));
    }
    fs::remove_file(marker)?;
    sync_directory(root)
}

fn recover_workspace_transaction(root: &Path) -> Result<()> {
    let marker = root.join(TRANSACTION_MARKER_FILE);
    let marker_tmp = root.join(".pv_transaction_active.tmp");
    let backup = root.join(TRANSACTION_BACKUP_DIR);

    if marker.exists() {
        if !backup.join(MANIFEST_FILE).is_file() {
            return Err(PvError::Transaction(
                "active transaction has no valid recovery manifest".into(),
            ));
        }
        restore_workspace_transaction(root)?;
    } else if backup.exists() {
        // A backup without a marker is either from before mutations began or
        // after the commit point. In both cases the live workspace is canonical.
        fs::remove_dir_all(&backup)?;
    }
    if marker_tmp.exists() {
        fs::remove_file(marker_tmp)?;
    }
    Ok(())
}

fn restore_workspace_transaction(root: &Path) -> Result<()> {
    let marker = root.join(TRANSACTION_MARKER_FILE);
    let backup = root.join(TRANSACTION_BACKUP_DIR);
    if !marker.exists() || !backup.join(MANIFEST_FILE).is_file() {
        return Err(PvError::Transaction(
            "workspace transaction recovery files are incomplete".into(),
        ));
    }

    // Validate the complete recovery image before touching live data. This
    // keeps a corrupted or attacker-modified backup from producing a partial
    // restore and ensures links never escape the workspace boundary.
    for name in [MANIFEST_FILE, "chunks", "blobs"] {
        let saved = backup.join(name);
        if saved.exists() {
            validate_copy_tree(&saved)?;
        }
    }

    for name in [MANIFEST_FILE, "chunks", "blobs"] {
        let live = root.join(name);
        if let Ok(metadata) = fs::symlink_metadata(&live) {
            if metadata.file_type().is_symlink() {
                return Err(PvError::Transaction(format!(
                    "refusing to replace symlink during transaction recovery: {}",
                    live.display()
                )));
            }
            if metadata.is_dir() {
                fs::remove_dir_all(&live)?;
            } else {
                fs::remove_file(&live)?;
            }
        }
        let saved = backup.join(name);
        if saved.exists() {
            copy_tree_synced(&saved, &live)?;
        }
    }
    sync_directory(root)?;

    // Removing the marker is the recovery commit point. Keep the backup until
    // after this succeeds so an interrupted restore can be repeated.
    fs::remove_file(&marker)?;
    sync_directory(root)?;
    fs::remove_dir_all(backup)?;
    Ok(())
}

fn validate_copy_tree(path: &Path) -> Result<()> {
    let metadata = fs::symlink_metadata(path)?;
    if metadata.file_type().is_symlink() {
        return Err(PvError::Transaction(format!(
            "refusing to copy symlink in workspace transaction: {}",
            path.display()
        )));
    }
    if metadata.is_file() {
        return Ok(());
    }
    if !metadata.is_dir() {
        return Err(PvError::Transaction(format!(
            "unsupported workspace entry during transaction: {}",
            path.display()
        )));
    }
    for entry in fs::read_dir(path)? {
        validate_copy_tree(&entry?.path())?;
    }
    Ok(())
}

fn copy_tree_synced(source: &Path, destination: &Path) -> Result<()> {
    let metadata = fs::symlink_metadata(source)?;
    if metadata.file_type().is_symlink() {
        return Err(PvError::Transaction(format!(
            "refusing to copy symlink in workspace transaction: {}",
            source.display()
        )));
    }
    if metadata.is_file() {
        if let Some(parent) = destination.parent() {
            fs::create_dir_all(parent)?;
        }
        fs::copy(source, destination)?;
        OpenOptions::new()
            .read(true)
            .write(true)
            .open(destination)?
            .sync_all()?;
        return Ok(());
    }
    if !metadata.is_dir() {
        return Err(PvError::Transaction(format!(
            "unsupported workspace entry during transaction: {}",
            source.display()
        )));
    }

    fs::create_dir_all(destination)?;
    for entry in fs::read_dir(source)? {
        let entry = entry?;
        copy_tree_synced(&entry.path(), &destination.join(entry.file_name()))?;
    }
    sync_directory(destination)
}

#[cfg(unix)]
fn sync_directory(path: &Path) -> Result<()> {
    File::open(path)?.sync_all()?;
    Ok(())
}

#[cfg(not(unix))]
fn sync_directory(_path: &Path) -> Result<()> {
    // Rust does not expose portable directory fsync on Windows. File data and
    // markers are still individually synced; rename/remove provide the commit
    // boundary supported by the platform.
    Ok(())
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
    order: &[OrderBy],
    distinct: bool,
    limit: Option<usize>,
    offset: usize,
) -> Result<QueryResult> {
    // Sort on the full row, before projection can drop a sort column. For
    // `ORDER BY ... LIMIT k` without DISTINCT, only the top-k rows are needed, so
    // select them in one pass instead of sorting every matched row.
    match limit.map(|k| k.saturating_add(offset)) {
        Some(k) if !distinct && !order.is_empty() => {
            rows = take_top_n(rows, &columns, order, k)?;
        }
        _ => sort_rows(&mut rows, &columns, order)?,
    }

    let (out_columns, mut out_rows) = match projection {
        Projection::All => (columns, rows),
        Projection::Columns(cols) => {
            let idxs = cols
                .iter()
                .map(|c| projection_col_pos(&columns, c))
                .collect::<Result<Vec<_>>>()?;
            let projected = rows
                .into_iter()
                .map(|r| idxs.iter().map(|&i| r[i].clone()).collect())
                .collect();
            (cols, projected)
        }
        Projection::Items(items) => {
            // Only non-aggregate items reach this path (aggregates and grouping go
            // through `project_grouped`). Each item projects its source column and is
            // named by its alias when present.
            let mut idxs = Vec::with_capacity(items.len());
            let mut names = Vec::with_capacity(items.len());
            for it in &items {
                match &it.expr {
                    SelectExpr::Column(c) => {
                        let ix = projection_col_pos(&columns, c)?;
                        idxs.push(ix);
                        names.push(it.alias.clone().unwrap_or_else(|| c.clone()));
                    }
                    SelectExpr::Aggregate(_) => {
                        unreachable!("aggregates go through project_grouped")
                    }
                }
            }
            let projected = rows
                .into_iter()
                .map(|r| idxs.iter().map(|&i| r[i].clone()).collect())
                .collect();
            (names, projected)
        }
    };

    if distinct {
        dedup_rows(&mut out_rows);
    }
    if offset > 0 {
        out_rows = out_rows.into_iter().skip(offset).collect();
    }
    if let Some(n) = limit {
        out_rows.truncate(n);
    }
    Ok(QueryResult::Rows {
        columns: out_columns,
        rows: out_rows,
    })
}

/// Resolve an output column, accepting a unique unqualified suffix for joined
/// rows (for example `name` for `users.name`). Ambiguous names stay errors.
fn projection_col_pos(columns: &[String], name: &str) -> Result<usize> {
    if let Some(ix) = columns.iter().position(|column| column == name) {
        return Ok(ix);
    }
    let suffix = format!(".{name}");
    let mut matches = columns
        .iter()
        .enumerate()
        .filter(|(_, column)| column.ends_with(&suffix));
    let Some((ix, _)) = matches.next() else {
        return Err(PvError::Schema(format!("no column `{name}`")));
    };
    if matches.next().is_some() {
        return Err(PvError::Schema(format!("ambiguous column `{name}`")));
    }
    Ok(ix)
}

fn join_side_col_pos(columns: &[String], table: &str, reference: &str) -> Result<usize> {
    match reference.split_once('.') {
        Some((qualifier, column)) if qualifier == table => col_pos(columns, column),
        Some((qualifier, _)) => Err(PvError::Schema(format!(
            "join column `{reference}` belongs to `{qualifier}`, expected `{table}`"
        ))),
        None => col_pos(columns, reference),
    }
}

/// Resolve `order` into `(column index, descending)` keys against `columns`.
fn order_keys(columns: &[String], order: &[OrderBy]) -> Result<Vec<(usize, bool)>> {
    order
        .iter()
        .map(|ob| projection_col_pos(columns, &ob.column).map(|ix| (ix, ob.descending)))
        .collect()
}

/// Compare two rows by resolved order keys (left to right, with per-key descending).
fn cmp_by_keys(a: &Row, b: &Row, keys: &[(usize, bool)]) -> std::cmp::Ordering {
    for &(ix, descending) in keys {
        let ord = cmp_values(&a[ix], &b[ix]);
        let ord = if descending { ord.reverse() } else { ord };
        if ord != std::cmp::Ordering::Equal {
            return ord;
        }
    }
    std::cmp::Ordering::Equal
}

/// Return the `k` rows that come first under `order`, sorted, without sorting the
/// whole input. It partitions an array of light indices (so the heavy `Row`s are
/// never moved during selection), turning an `ORDER BY ... LIMIT k` over many rows
/// from an O(n log n) sort of full rows into an O(n) partition plus a k-row sort.
fn take_top_n(rows: Vec<Row>, columns: &[String], order: &[OrderBy], k: usize) -> Result<Vec<Row>> {
    if order.is_empty() {
        let mut rows = rows;
        rows.truncate(k);
        return Ok(rows);
    }
    if k >= rows.len() {
        let mut rows = rows;
        sort_rows(&mut rows, columns, order)?;
        return Ok(rows);
    }
    let keys = order_keys(columns, order)?;
    let mut idx: Vec<usize> = (0..rows.len()).collect();
    idx.select_nth_unstable_by(k, |&a, &b| cmp_by_keys(&rows[a], &rows[b], &keys));
    idx.truncate(k);
    idx.sort_by(|&a, &b| cmp_by_keys(&rows[a], &rows[b], &keys));
    Ok(idx.into_iter().map(|i| rows[i].clone()).collect())
}

/// Sort `rows` in place by `order` keys, applied left to right, resolving each
/// key's column against `columns`. A no-op when `order` is empty.
fn sort_rows(rows: &mut [Row], columns: &[String], order: &[OrderBy]) -> Result<()> {
    if order.is_empty() {
        return Ok(());
    }
    let keys = order_keys(columns, order)?;
    rows.sort_by(|a, b| cmp_by_keys(a, b, &keys));
    Ok(())
}

/// Drop duplicate rows, preserving first-occurrence order (for `SELECT DISTINCT`).
fn dedup_rows(rows: &mut Vec<Row>) {
    let mut seen = std::collections::BTreeSet::new();
    rows.retain(|r| seen.insert(r.clone()));
}

/// Whether a projection contains at least one aggregate term.
fn projection_has_aggregate(projection: &Projection) -> bool {
    matches!(
        projection,
        Projection::Items(items) if items.iter().any(|i| matches!(i.expr, SelectExpr::Aggregate(_)))
    )
}

/// If the projection is exactly one bare `COUNT(*)`, return its optional alias.
fn count_star_only(projection: &Projection) -> Option<Option<String>> {
    if let Projection::Items(items) = projection {
        if items.len() == 1 {
            if let SelectExpr::Aggregate(Aggregate {
                func: AggFunc::Count,
                column: None,
            }) = &items[0].expr
            {
                return Some(items[0].alias.clone());
            }
        }
    }
    None
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
                // `!=` and `LIKE`/`NOT LIKE` aren't range-shaped, a scan is no worse.
                CompareOp::Ne | CompareOp::Like | CompareOp::NotLike => None,
            }
        }
        Predicate::And(a, b) => index_candidates(table, a).or_else(|| index_candidates(table, b)),
        // IN / BETWEEN / IS NULL aren't lowered to the index yet: a full scan is
        // correct (the caller re-checks the full predicate), just not optimized.
        Predicate::In { .. }
        | Predicate::Between { .. }
        | Predicate::IsNull { .. }
        | Predicate::Or(_, _) => None,
    }
}

/// Error if the predicate references a column the table doesn't have.
fn check_predicate_columns(columns: &[String], pred: &Predicate) -> Result<()> {
    match pred {
        Predicate::And(a, b) | Predicate::Or(a, b) => {
            check_predicate_columns(columns, a)?;
            check_predicate_columns(columns, b)
        }
        Predicate::Compare { column, .. }
        | Predicate::In { column, .. }
        | Predicate::Between { column, .. }
        | Predicate::IsNull { column, .. } => projection_col_pos(columns, column).map(|_| ()),
    }
}

/// Evaluate a predicate against one row.
fn row_matches(pred: &Predicate, columns: &[String], row: &[Value]) -> Result<bool> {
    match pred {
        Predicate::And(a, b) => Ok(row_matches(a, columns, row)? && row_matches(b, columns, row)?),
        Predicate::Or(a, b) => Ok(row_matches(a, columns, row)? || row_matches(b, columns, row)?),
        Predicate::Compare { column, op, value } => Ok(eval_compare(
            &row[projection_col_pos(columns, column)?],
            *op,
            value,
        )),
        Predicate::In {
            column,
            values,
            negated,
        } => {
            let x = &row[projection_col_pos(columns, column)?];
            // A null column value matches neither IN nor NOT IN.
            if matches!(x, Value::Null) {
                return Ok(false);
            }
            // SQL three-valued logic for a NULL inside the list: if nothing matches
            // but the list contains a NULL, the result is UNKNOWN — neither IN nor
            // NOT IN holds.
            let mut matched = false;
            let mut saw_null = false;
            for v in values {
                if matches!(v, Value::Null) {
                    saw_null = true;
                } else if values_equal(x, v) {
                    matched = true;
                }
            }
            if matched {
                Ok(!negated)
            } else if saw_null {
                Ok(false)
            } else {
                Ok(*negated)
            }
        }
        Predicate::Between {
            column,
            low,
            high,
            negated,
        } => {
            let x = &row[projection_col_pos(columns, column)?];
            // A null column value matches neither BETWEEN nor NOT BETWEEN.
            if matches!(x, Value::Null) {
                return Ok(false);
            }
            let in_range = numeric_cmp(x, low) != std::cmp::Ordering::Less
                && numeric_cmp(x, high) != std::cmp::Ordering::Greater;
            Ok(in_range != *negated)
        }
        Predicate::IsNull { column, negated } => {
            let is_null = matches!(row[projection_col_pos(columns, column)?], Value::Null);
            Ok(is_null != *negated)
        }
    }
}

/// Index of `column` within `columns`, or a schema error if it is absent.
fn col_pos(columns: &[String], column: &str) -> Result<usize> {
    columns
        .iter()
        .position(|c| c == column)
        .ok_or_else(|| PvError::Schema(format!("no column `{column}`")))
}

/// Promote an integer to the decimal mantissa scale for cross-type comparison,
/// saturating rather than panicking on the (unreachable for valid `i64`) overflow.
fn promote_int(i: i64) -> i128 {
    (i as i128).saturating_mul(DECIMAL_DEN)
}

/// Numeric-aware comparison used by **predicates** (`WHERE`/`BETWEEN`/`IN`/`HAVING`
/// and `MIN`/`MAX`): an `Int` and a `Decimal` are compared by magnitude (the `Int`
/// promoted to the decimal scale); every other type pairing uses the total
/// [`cmp_values`] order. It is deliberately NOT used for `ORDER BY`, `GROUP BY`, or
/// `DISTINCT`, which need a type-strict total order to stay consistent.
fn numeric_cmp(a: &Value, b: &Value) -> std::cmp::Ordering {
    match (a, b) {
        (Value::Int(x), Value::Decimal(y)) => promote_int(*x).cmp(y),
        (Value::Decimal(x), Value::Int(y)) => x.cmp(&promote_int(*y)),
        _ => cmp_values(a, b),
    }
}

/// Numeric-aware equality: `Int(n)` equals `Decimal(n * DECIMAL_DEN)`; otherwise
/// structural equality (so a null only equals a null).
fn values_equal(a: &Value, b: &Value) -> bool {
    match (a, b) {
        (Value::Int(_), Value::Decimal(_)) | (Value::Decimal(_), Value::Int(_)) => {
            numeric_cmp(a, b) == std::cmp::Ordering::Equal
        }
        _ => a == b,
    }
}

/// Normalize numeric equality keys so joins agree with WHERE semantics:
/// `1` and `1.000000` must hash to the same bucket.
fn join_key(value: &Value) -> Value {
    match value {
        Value::Int(value) => Value::Decimal(promote_int(*value)),
        value => value.clone(),
    }
}

/// Apply one comparison. Ordering comparisons against `NULL` are never true (SQL
/// three-valued logic); `=`/`!=` and the ordering operators compare numerically
/// across `Int`/`Decimal`; `LIKE`/`NOT LIKE` need two texts.
fn eval_compare(lhs: &Value, op: CompareOp, rhs: &Value) -> bool {
    use std::cmp::Ordering;
    match op {
        CompareOp::Eq => values_equal(lhs, rhs),
        CompareOp::Ne => !values_equal(lhs, rhs),
        CompareOp::Like => match (lhs, rhs) {
            (Value::Text(t), Value::Text(p)) => like_match(t, p),
            _ => false,
        },
        CompareOp::NotLike => match (lhs, rhs) {
            (Value::Text(t), Value::Text(p)) => !like_match(t, p),
            // A null or non-text value matches neither LIKE nor NOT LIKE.
            _ => false,
        },
        CompareOp::Lt | CompareOp::Le | CompareOp::Gt | CompareOp::Ge => {
            if matches!(lhs, Value::Null) || matches!(rhs, Value::Null) {
                return false;
            }
            let ord = numeric_cmp(lhs, rhs);
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
        Projection::Columns(cols) => Ok(cols
            .into_iter()
            .map(|c| SelectItem {
                expr: SelectExpr::Column(c),
                alias: None,
            })
            .collect()),
        Projection::All => Err(PvError::Query(
            "SELECT * cannot be combined with GROUP BY or aggregates".into(),
        )),
    }
}

/// Evaluate a grouped or whole-table aggregate query: partition `rows` by the
/// `group_by` columns (a single group when `group_by` is empty), evaluate each
/// select item per group, then apply `ORDER BY` and `LIMIT` to the result.
#[allow(clippy::too_many_arguments)] // the grouped path genuinely needs each clause
fn project_grouped(
    columns: Vec<String>,
    rows: Vec<Row>,
    items: Vec<SelectItem>,
    group_by: Vec<String>,
    having: Option<HavingPred>,
    order: Vec<OrderBy>,
    distinct: bool,
    limit: Option<usize>,
    offset: usize,
) -> Result<QueryResult> {
    // A bare column in the select list must be a grouping column.
    for item in &items {
        if let SelectExpr::Column(c) = &item.expr {
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

    // Output column names: the alias if present, else the column name or the
    // aggregate label (e.g. `sum(amount)`).
    let out_columns: Vec<String> = items
        .iter()
        .map(|it| match (&it.alias, &it.expr) {
            (Some(a), _) => a.clone(),
            (None, SelectExpr::Column(c)) => c.clone(),
            (None, SelectExpr::Aggregate(a)) => agg_label(a),
        })
        .collect();

    let mut out_rows: Vec<Row> = Vec::with_capacity(groups.len());
    for (key, group_rows) in &groups {
        let mut out = Vec::with_capacity(items.len());
        for item in &items {
            match &item.expr {
                SelectExpr::Column(c) => {
                    let gi = group_by
                        .iter()
                        .position(|g| g == c)
                        .expect("validated above");
                    out.push(key[gi].clone());
                }
                SelectExpr::Aggregate(a) => {
                    out.push(compute_one_aggregate(a, &columns, group_rows)?)
                }
            }
        }
        // HAVING filters groups: a column term resolves against this group's output
        // row, an aggregate term is computed over the group's source rows.
        let keep = match &having {
            None => true,
            Some(h) => eval_having(h, &out_columns, &out, &columns, group_rows)?,
        };
        if keep {
            out_rows.push(out);
        }
    }

    sort_rows(&mut out_rows, &out_columns, &order)?;
    if distinct {
        dedup_rows(&mut out_rows);
    }
    if offset > 0 {
        out_rows = out_rows.into_iter().skip(offset).collect();
    }
    if let Some(n) = limit {
        out_rows.truncate(n);
    }
    Ok(QueryResult::Rows {
        columns: out_columns,
        rows: out_rows,
    })
}

/// Evaluate a `HAVING` predicate for one group. A column term resolves against the
/// group's output row (`out_columns` / `out_row`); an aggregate term is computed
/// over the group's source rows, so `HAVING` can filter on an aggregate that is not
/// in the `SELECT` list.
fn eval_having(
    pred: &HavingPred,
    out_columns: &[String],
    out_row: &[Value],
    columns: &[String],
    group_rows: &[Row],
) -> Result<bool> {
    match pred {
        HavingPred::And(a, b) => Ok(eval_having(a, out_columns, out_row, columns, group_rows)?
            && eval_having(b, out_columns, out_row, columns, group_rows)?),
        HavingPred::Or(a, b) => Ok(eval_having(a, out_columns, out_row, columns, group_rows)?
            || eval_having(b, out_columns, out_row, columns, group_rows)?),
        HavingPred::Compare { term, op, value } => {
            let lhs = match term {
                HavingTerm::Column(name) => out_row[col_pos(out_columns, name)?].clone(),
                HavingTerm::Aggregate(agg) => compute_one_aggregate(agg, columns, group_rows)?,
            };
            Ok(eval_compare(&lhs, *op, value))
        }
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
            let (int_sum, dec_sum, any_decimal, count) = sum_numeric_column(rows, ix, "SUM")?;
            // An empty or all-null group sums to NULL, matching MIN/MAX/AVG and
            // standard SQL (only COUNT returns 0 for an empty input). A column that
            // holds any decimal sums to a decimal; a pure-integer column stays an
            // integer (its historical type).
            if count == 0 {
                Value::Null
            } else if any_decimal {
                Value::Decimal(combine_mantissa(int_sum, dec_sum)?)
            } else {
                Value::Int(
                    i64::try_from(int_sum)
                        .map_err(|_| PvError::Schema("SUM overflowed i64".into()))?,
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
                            AggFunc::Min => numeric_cmp(&r[ix], cur).is_lt(),
                            AggFunc::Max => numeric_cmp(&r[ix], cur).is_gt(),
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
            let (int_sum, dec_sum, _any, count) = sum_numeric_column(rows, ix, "AVG")?;
            // An empty or all-null group averages to NULL, matching MIN/MAX.
            // Otherwise produce an exact fixed-point decimal, computed in i128 (never
            // f64): scale the integer part, add the decimal mantissas, and round the
            // average half away from zero. `AVG` always yields a decimal.
            if count == 0 {
                Value::Null
            } else {
                Value::Decimal(round_div_half_away(
                    combine_mantissa(int_sum, dec_sum)?,
                    count,
                ))
            }
        }
    };
    Ok(value)
}

/// Round `num / den` (`den != 0`) to the nearest integer, halves away from zero, in
/// exact integer arithmetic — no `f64`, so large averages stay exact and rounding
/// is deterministic. `den` is a row count, so the remainder doubling cannot
/// overflow even when `num` is a near-`i128::MAX` mantissa.
fn round_div_half_away(num: i128, den: i128) -> i128 {
    let negative = (num < 0) ^ (den < 0);
    let n = num.unsigned_abs();
    let d = den.unsigned_abs();
    let q = n / d;
    let r = n % d;
    let q = if r * 2 >= d { q + 1 } else { q };
    let q = q as i128;
    if negative {
        -q
    } else {
        q
    }
}

/// Accumulate a numeric column for `SUM`/`AVG`: integer values are summed into the
/// first returned field, decimal mantissas into the second, `any_decimal` flags
/// whether any decimal was seen, and `count` is the number of non-null values.
/// Nulls are skipped; a non-numeric value is an error.
fn sum_numeric_column(rows: &[Row], ix: usize, label: &str) -> Result<(i128, i128, bool, i128)> {
    let mut int_sum: i128 = 0;
    let mut dec_sum: i128 = 0;
    let mut any_decimal = false;
    let mut count: i128 = 0;
    for r in rows {
        match &r[ix] {
            Value::Int(i) => {
                int_sum = int_sum
                    .checked_add(*i as i128)
                    .ok_or_else(|| PvError::Schema(format!("{label} overflowed")))?;
                count += 1;
            }
            Value::Decimal(m) => {
                dec_sum = dec_sum
                    .checked_add(*m)
                    .ok_or_else(|| PvError::Schema(format!("{label} overflowed")))?;
                any_decimal = true;
                count += 1;
            }
            Value::Null => {}
            other => {
                return Err(PvError::Schema(format!(
                    "{label} requires numeric values, found {other:?}"
                )))
            }
        }
    }
    Ok((int_sum, dec_sum, any_decimal, count))
}

/// Combine an integer sum and a decimal-mantissa sum into one scale-`DECIMAL_SCALE`
/// mantissa: the integer part is scaled up by `DECIMAL_DEN` and added to the
/// mantissa part. Errors on i128 overflow.
fn combine_mantissa(int_sum: i128, dec_sum: i128) -> Result<i128> {
    int_sum
        .checked_mul(DECIMAL_DEN)
        .and_then(|s| s.checked_add(dec_sum))
        .ok_or_else(|| PvError::Schema("numeric aggregate overflowed".into()))
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

/// Like [`scan`] but decodes only each record's MVCC envelope, not its body, for
/// consumers (the bare `COUNT(*)` path) that need visibility but no column values.
fn scan_envelopes(
    cache: &mut PageCache,
    table: &Table,
    mut visit: impl FnMut(&RecordEnvelope) -> Result<()>,
) -> Result<()> {
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
                    visit(&RecordEnvelope::decode(tail.record(slot)?)?)?;
                }
                next = tail.next_page();
                continue;
            }
        }
        next = cache.with_page(pid, |buf| {
            let page = RowPageRef::new(buf)?;
            for slot in 0..page.slot_count() {
                visit(&RecordEnvelope::decode(page.record(slot)?)?)?;
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

/// Slice the binary index region out of a full `.pvdb` byte image (an mmap or an
/// imported buffer, both starting at file offset 0). Returns an empty slice for a
/// version-1 file that carries no region. The region must sit at or after
/// `cas_offset` and within the image; descriptors and the index decoder do the
/// finer-grained bounds checks.
fn slice_index_region<'a>(
    image: &'a [u8],
    manifest: &Manifest,
    cas_offset: u64,
    manifest_offset: usize,
) -> Result<&'a [u8]> {
    match manifest.index_region {
        None => Ok(&[]),
        Some((off, len)) => {
            if off < cas_offset {
                return Err(PvError::Corruption(
                    "index region overlaps the CAS pool".into(),
                ));
            }
            let off = off as usize;
            let end = off
                .checked_add(len as usize)
                .filter(|&e| e <= image.len() && e <= manifest_offset)
                .ok_or_else(|| PvError::Corruption("index region out of bounds".into()))?;
            Ok(&image[off..end])
        }
    }
}

/// Return the exclusive end of the CAS pool. Version-2 images place the binary
/// index immediately after it; older images place the manifest there directly.
fn cas_pool_end(manifest: &Manifest, cas_offset: usize, manifest_offset: usize) -> Result<usize> {
    let end = match manifest.index_region {
        Some((off, _)) => usize::try_from(off)
            .map_err(|_| PvError::Corruption("index region offset is too large".into()))?,
        None => manifest_offset,
    };
    if end < cas_offset || end > manifest_offset {
        return Err(PvError::Corruption(
            "CAS pool / index region bounds are inconsistent".into(),
        ));
    }
    Ok(end)
}

/// Reconstruct in-memory table metadata (and indexes) from a manifest.
///
/// `index_region` is the raw bytes of the monolith's binary index region (empty
/// for version-1 files and development workspaces); descriptors in the manifest
/// slice into it.
fn build_tables(
    cache: &mut PageCache,
    cas: &CasStore,
    manifest: &Manifest,
    writable: bool,
    index_region: &[u8],
) -> Result<BTreeMap<String, Table>> {
    let mut tables = BTreeMap::new();
    for meta in &manifest.tables {
        let mut table = Table {
            columns: meta.columns.clone(),
            unique_columns: meta.unique_columns.iter().cloned().collect(),
            not_null_columns: meta.not_null_columns.iter().cloned().collect(),
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
                verify_page_checksum(id, &buf)?;
                table.tail = Some(RowPage::from_bytes(buf)?);
            }
        }
        tables.insert(meta.name.clone(), table);
    }

    // Load persisted indexes where present; otherwise (pre-1.2 files) rebuild them
    // from a streaming scan. Loading avoids re-reading every page on open, which is
    // what lets a large or streamed database open quickly with indexes intact.
    // Precedence: a binary index region (v2) wins over JSON `pairs` (dev / v1)
    // which wins over a from-scratch rebuild (pre-1.2 files).
    for meta in &manifest.tables {
        if !meta.binary_indexes.is_empty() {
            let table = tables.get_mut(&meta.name).expect("just inserted");
            for desc in &meta.binary_indexes {
                let start = desc.offset as usize;
                let end = start
                    .checked_add(desc.len as usize)
                    .filter(|&e| e <= index_region.len())
                    .ok_or_else(|| {
                        PvError::Corruption("index region descriptor out of bounds".into())
                    })?;
                table.indexes.insert(
                    desc.column.clone(),
                    SecondaryIndex::decode_binary(&index_region[start..end])?,
                );
            }
        } else if !meta.indexes.is_empty() {
            let table = tables.get_mut(&meta.name).expect("just inserted");
            for pi in &meta.indexes {
                table.indexes.insert(
                    pi.column.clone(),
                    SecondaryIndex::from_pairs(pi.pairs.clone()),
                );
            }
        } else {
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
            unique_columns: BTreeSet::new(),
            not_null_columns: BTreeSet::new(),
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
    fn filesystem_transaction_commits_all_statements() {
        let tmp = tempfile::tempdir().unwrap();
        let ws = tmp.path().join("ws");
        {
            let mut db = Database::open_dev(&ws).unwrap();
            db.query("CREATE TABLE accounts (id PRIMARY KEY, balance)")
                .unwrap();
            db.transaction(|tx| {
                tx.query("INSERT INTO accounts VALUES (1, 40)")?;
                tx.query("INSERT INTO accounts VALUES (2, 60)")?;
                Ok(())
            })
            .unwrap();
            assert!(!db.in_transaction());
        }

        let mut reopened = Database::open_dev(&ws).unwrap();
        let rows = reopened
            .query("SELECT * FROM accounts ORDER BY id")
            .unwrap();
        assert_eq!(rows.rows().unwrap().len(), 2);
        assert!(!ws.join(TRANSACTION_MARKER_FILE).exists());
        assert!(!ws.join(TRANSACTION_BACKUP_DIR).exists());
    }

    #[test]
    fn sql_transaction_control_works_for_filesystem_workspaces() {
        let tmp = tempfile::tempdir().unwrap();
        let ws = tmp.path().join("ws");
        let mut db = Database::open_dev(&ws).unwrap();
        db.query("CREATE TABLE t (id)").unwrap();
        db.query("BEGIN TRANSACTION").unwrap();
        db.query("INSERT INTO t VALUES (1)").unwrap();
        db.query("ROLLBACK").unwrap();
        assert_eq!(
            db.query("SELECT COUNT(*) FROM t").unwrap().rows().unwrap(),
            &[vec![Value::Int(0)]]
        );

        db.query("BEGIN").unwrap();
        db.query("INSERT INTO t VALUES (2)").unwrap();
        db.query("COMMIT TRANSACTION").unwrap();
        assert_eq!(
            db.query("SELECT * FROM t").unwrap().rows().unwrap(),
            &[vec![Value::Int(2)]]
        );
    }

    #[test]
    fn filesystem_transaction_error_restores_complete_state() {
        let tmp = tempfile::tempdir().unwrap();
        let ws = tmp.path().join("ws");
        let mut db = Database::open_dev(&ws).unwrap();
        db.query("CREATE TABLE accounts (id PRIMARY KEY, balance)")
            .unwrap();
        db.query("INSERT INTO accounts VALUES (1, 100)").unwrap();
        let before_tx = db.current_tx();

        let result = db.transaction(|tx| {
            tx.query("UPDATE accounts SET balance = 20 WHERE id = 1")?;
            tx.query("INSERT INTO accounts VALUES (2, 80)")?;
            Err::<(), _>(PvError::Schema("application rejected transfer".into()))
        });
        assert!(matches!(result, Err(PvError::Schema(_))));
        assert_eq!(db.current_tx(), before_tx);
        let rows = db.query("SELECT * FROM accounts ORDER BY id").unwrap();
        assert_eq!(
            rows.rows().unwrap(),
            &[vec![Value::Int(1), Value::Int(100)]]
        );

        drop(db);
        let mut reopened = Database::open_dev(&ws).unwrap();
        assert_eq!(
            reopened
                .query("SELECT * FROM accounts")
                .unwrap()
                .rows()
                .unwrap(),
            &[vec![Value::Int(1), Value::Int(100)]]
        );
    }

    #[test]
    fn unfinished_filesystem_transaction_recovers_on_open() {
        let tmp = tempfile::tempdir().unwrap();
        let ws = tmp.path().join("ws");
        {
            let mut db = Database::open_dev(&ws).unwrap();
            db.set_durability(Durability::Sync);
            db.query("CREATE TABLE events (id PRIMARY KEY, state)")
                .unwrap();
            db.query("INSERT INTO events VALUES (1, 'committed')")
                .unwrap();
            db.begin_transaction().unwrap();
            db.query("UPDATE events SET state = 'partial' WHERE id = 1")
                .unwrap();
            db.query("INSERT INTO events VALUES (2, 'partial')")
                .unwrap();
            // Simulate dirty pages reaching disk before the process disappears.
            db.flush_now().unwrap();
            assert!(ws.join(TRANSACTION_MARKER_FILE).exists());
        }

        let mut recovered = Database::open_dev(&ws).unwrap();
        let rows = recovered.query("SELECT * FROM events ORDER BY id").unwrap();
        assert_eq!(
            rows.rows().unwrap(),
            &[vec![Value::Int(1), Value::Text("committed".into())]]
        );
        assert!(!ws.join(TRANSACTION_MARKER_FILE).exists());
        assert!(!ws.join(TRANSACTION_BACKUP_DIR).exists());
    }

    #[test]
    fn live_filesystem_transaction_cannot_be_mistaken_for_a_crash() {
        let tmp = tempfile::tempdir().unwrap();
        let ws = tmp.path().join("ws");
        let mut owner = Database::open_dev(&ws).unwrap();
        owner.query("CREATE TABLE t (id)").unwrap();
        owner.begin_transaction().unwrap();
        owner.query("INSERT INTO t VALUES (1)").unwrap();

        assert!(matches!(
            Database::open_dev(&ws),
            Err(PvError::Transaction(_))
        ));
        owner.rollback_transaction().unwrap();
        assert!(Database::open_dev(&ws).is_ok());
    }

    #[test]
    fn explicit_transactions_reject_invalid_lifecycle() {
        let mut db = Database::open_memory();
        assert!(matches!(
            db.commit_transaction(),
            Err(PvError::Transaction(_))
        ));
        db.begin_transaction().unwrap();
        assert!(matches!(
            db.begin_transaction(),
            Err(PvError::Transaction(_))
        ));
        db.rollback_transaction().unwrap();
        assert!(!db.in_transaction());
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

    #[test]
    fn bounded_queries_stop_before_untrusted_work_grows_without_limit() {
        let mut db = Database::open_memory();
        db.query("CREATE TABLE t (id, body)").unwrap();
        for id in 0..10 {
            db.query(&format!("INSERT INTO t VALUES ({id}, 'payload')"))
                .unwrap();
        }

        let scan_limited = QueryLimits::new(3, usize::MAX, usize::MAX, None);
        assert!(matches!(
            db.query_with_limits("SELECT * FROM t", &[], scan_limited),
            Err(PvError::ResourceLimit(_))
        ));

        let result_limited = QueryLimits::new(100, usize::MAX, 2, None);
        assert!(matches!(
            db.query_with_limits("SELECT * FROM t", &[], result_limited),
            Err(PvError::ResourceLimit(_))
        ));

        let expired = QueryLimits::new(100, usize::MAX, 100, Some(Instant::now()));
        assert!(matches!(
            db.query_with_limits("SELECT * FROM t", &[], expired),
            Err(PvError::ResourceLimit(_))
        ));

        let mutation_limited = QueryLimits::new(3, usize::MAX, 100, None);
        assert!(matches!(
            db.query_with_limits("DELETE FROM t WHERE id >= 0", &[], mutation_limited),
            Err(PvError::ResourceLimit(_))
        ));
        assert_eq!(db.row_count("t", None).unwrap(), 10);
    }
}
