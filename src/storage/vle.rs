//! Virtualization Layer Engine (VLE), the router that hides whether pages live
//! in a mutable `.pv/` directory (Development Mode) or inside a single immutable
//! memory-mapped `.pvdb` monolith (Production Mode), per spec §2.
//!
//! * [`DevStore`], append-only 4096-byte pages spread across `chunks/*.pvd`
//!   files capped at 64 MiB each.
//! * [`Monolith`], a read-only mmap over a baked `.pvdb`, slicing pages and
//!   exposing the CAS / manifest regions by absolute offset.
//! * [`bake_monolith`], compiles a dev workspace's pages + CAS pool + manifest
//!   into the contiguous monolith layout.

use std::cell::RefCell;
use std::collections::BTreeSet;
use std::fs::{self, File, OpenOptions};
use std::io::{Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};
use std::sync::Arc;

use memmap2::Mmap;

use crate::core::errors::{PvError, Result};
use crate::core::types::{
    FileHeader, CHUNK_CAP_BYTES, FILE_HEADER_SIZE, PAGE_CHECKSUM_OFFSET, PAGE_SIZE,
};

/// Pages per 64 MiB chunk file: 16 384.
pub const PAGES_PER_CHUNK: u64 = CHUNK_CAP_BYTES / PAGE_SIZE as u64;

/// A boxed 4096-byte page buffer.
pub type PageBuf = Box<[u8; PAGE_SIZE]>;

// ---------------------------------------------------------------------------
// Per-page integrity checksum
// ---------------------------------------------------------------------------

/// Compute a page's integrity checksum: a 32-bit truncation of BLAKE3 over every
/// byte except the 4-byte checksum field itself (at [`PAGE_CHECKSUM_OFFSET`]).
///
/// BLAKE3 is already a dependency and builds cleanly on wasm, so this needs no
/// new crate. The checksum guards against torn writes and bit-rot, not an
/// adversary, so 32 bits is ample. It never returns `0`: that value is reserved
/// to mean "unstamped" (see [`verify_page_checksum`]).
pub fn page_checksum(page: &[u8; PAGE_SIZE]) -> u32 {
    let mut hasher = blake3::Hasher::new();
    hasher.update(&page[..PAGE_CHECKSUM_OFFSET]);
    hasher.update(&page[PAGE_CHECKSUM_OFFSET + 4..]);
    let bytes = hasher.finalize();
    let bytes = bytes.as_bytes();
    let value = u32::from_le_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]);
    if value == 0 {
        1
    } else {
        value
    }
}

/// Stamp the freshly-computed checksum into a page's checksum field. Call this on
/// every page immediately before it is handed to a backend to be written.
pub fn stamp_page_checksum(page: &mut [u8; PAGE_SIZE]) {
    let checksum = page_checksum(page);
    page[PAGE_CHECKSUM_OFFSET..PAGE_CHECKSUM_OFFSET + 4].copy_from_slice(&checksum.to_le_bytes());
}

/// Verify a page's stored checksum, returning [`PvError::Corruption`] on
/// mismatch. A stored value of `0` marks a page that was never stamped (blank, or
/// written outside the cache) and is accepted as-is.
pub fn verify_page_checksum(id: u64, page: &[u8; PAGE_SIZE]) -> Result<()> {
    let stored = u32::from_le_bytes(
        page[PAGE_CHECKSUM_OFFSET..PAGE_CHECKSUM_OFFSET + 4]
            .try_into()
            .expect("4-byte checksum field"),
    );
    if stored == 0 {
        return Ok(());
    }
    let computed = page_checksum(page);
    if stored != computed {
        return Err(PvError::Corruption(format!(
            "page {id} checksum mismatch (stored {stored:#010x}, computed {computed:#010x})"
        )));
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// Development Mode: directory of append-only chunk files
// ---------------------------------------------------------------------------

/// Mutable page store backed by `chunks/chunk_NNNNN.pvd` files under a workspace.
///
/// Keeps the write handle for the most-recently-written chunk open, so a stream
/// of appends (autocommit) does not reopen the file per page. Reads use separate
/// handles; Rust opens files with shared read/write modes, so this is safe.
/// [`sync_data`](Self::sync_data) `fsync`s every chunk written since the last
/// sync (tracked in `dirty_chunks`).
pub struct DevStore {
    root: PathBuf,
    page_count: u64,
    write_handle: RefCell<Option<(u64, File)>>,
    dirty_chunks: RefCell<BTreeSet<u64>>,
}

impl DevStore {
    /// Create a fresh, empty workspace at `root` (creates `root/chunks/`).
    pub fn create(root: impl Into<PathBuf>) -> Result<Self> {
        let root = root.into();
        fs::create_dir_all(root.join("chunks"))?;
        Ok(Self {
            root,
            page_count: 0,
            write_handle: RefCell::new(None),
            dirty_chunks: RefCell::new(BTreeSet::new()),
        })
    }

    /// Re-open an existing workspace whose page count is known from the manifest.
    pub fn open(root: impl Into<PathBuf>, page_count: u64) -> Result<Self> {
        let root = root.into();
        fs::create_dir_all(root.join("chunks"))?;
        Ok(Self {
            root,
            page_count,
            write_handle: RefCell::new(None),
            dirty_chunks: RefCell::new(BTreeSet::new()),
        })
    }

    /// Number of pages allocated so far.
    pub fn page_count(&self) -> u64 {
        self.page_count
    }

    /// Overwrite the page count (used when the workspace is rewritten wholesale
    /// during a flush). Pages beyond `n` in the chunk files become unreachable.
    pub fn set_page_count(&mut self, n: u64) {
        self.page_count = n;
    }

    fn chunk_path(&self, chunk_ix: u64) -> PathBuf {
        self.root
            .join("chunks")
            .join(format!("chunk_{chunk_ix:05}.pvd"))
    }

    /// Reserve a new page id (the caller is expected to `write_page` it next).
    pub fn alloc_page(&mut self) -> u64 {
        let id = self.page_count;
        self.page_count += 1;
        id
    }

    /// Run `f` against the (cached) write handle for `chunk_ix`, opening and
    /// caching it if a different chunk's handle is currently held.
    fn with_chunk_write<R>(
        &self,
        chunk_ix: u64,
        f: impl FnOnce(&mut File) -> Result<R>,
    ) -> Result<R> {
        self.dirty_chunks.borrow_mut().insert(chunk_ix);
        let mut slot = self.write_handle.borrow_mut();
        let needs_open = slot.as_ref().map(|(ix, _)| *ix) != Some(chunk_ix);
        if needs_open {
            let file = OpenOptions::new()
                .read(true)
                .write(true)
                .create(true)
                .truncate(false) // pages are written at offsets; keep existing content
                .open(self.chunk_path(chunk_ix))?;
            *slot = Some((chunk_ix, file));
        }
        let (_, file) = slot.as_mut().expect("handle present");
        f(file)
    }

    /// Write a full page to its chunk file, extending the file if necessary.
    pub fn write_page(&self, id: u64, page: &[u8; PAGE_SIZE]) -> Result<()> {
        let within = (id % PAGES_PER_CHUNK) * PAGE_SIZE as u64;
        self.with_chunk_write(id / PAGES_PER_CHUNK, |file| {
            file.seek(SeekFrom::Start(within))?;
            file.write_all(page)?;
            Ok(())
        })
    }

    /// Write `pages` at consecutive ids starting from `start_id`, opening each
    /// chunk file **once** and issuing one bulk write per chunk. Dramatically
    /// faster than per-page [`write_page`] calls.
    pub fn write_pages_from(&self, start_id: u64, pages: &[&PageBuf]) -> Result<()> {
        let end = start_id + pages.len() as u64;
        let mut id = start_id;
        let mut idx = 0usize;
        while id < end {
            let chunk_ix = id / PAGES_PER_CHUNK;
            let chunk_start = chunk_ix * PAGES_PER_CHUNK;
            let chunk_page_end = ((chunk_ix + 1) * PAGES_PER_CHUNK).min(end);
            let count = (chunk_page_end - id) as usize;
            let mut batch = Vec::with_capacity(count * PAGE_SIZE);
            for page in &pages[idx..idx + count] {
                batch.extend_from_slice(&page[..]);
            }
            self.with_chunk_write(chunk_ix, |file| {
                file.seek(SeekFrom::Start((id - chunk_start) * PAGE_SIZE as u64))?;
                file.write_all(&batch)?;
                Ok(())
            })?;
            idx += count;
            id = chunk_page_end;
        }
        Ok(())
    }

    /// `fsync` every chunk written since the last sync to stable storage.
    ///
    /// `fsync` flushes a file's OS-cached writes regardless of which handle
    /// initiated them, so chunks no longer held by the cached write handle are
    /// synced by briefly reopening them. This gives full crash-safety across a
    /// workspace spanning multiple 64 MiB chunks.
    pub fn sync_data(&self) -> Result<()> {
        let dirty: Vec<u64> = self.dirty_chunks.borrow().iter().copied().collect();
        for chunk_ix in dirty {
            let cached = self.write_handle.borrow().as_ref().map(|(ix, _)| *ix) == Some(chunk_ix);
            if cached {
                if let Some((_, file)) = self.write_handle.borrow().as_ref() {
                    file.sync_all()?;
                }
            } else {
                OpenOptions::new()
                    .read(true)
                    .write(true)
                    .open(self.chunk_path(chunk_ix))?
                    .sync_all()?;
            }
        }
        self.dirty_chunks.borrow_mut().clear();
        Ok(())
    }

    /// Read a full page out of its chunk file.
    pub fn read_page(&self, id: u64) -> Result<PageBuf> {
        if id >= self.page_count {
            return Err(PvError::PageFault { page_id: id });
        }
        let within = (id % PAGES_PER_CHUNK) * PAGE_SIZE as u64;
        let path = self.chunk_path(id / PAGES_PER_CHUNK);
        let mut file = File::open(&path)?;
        file.seek(SeekFrom::Start(within))?;
        let mut buf: PageBuf = Box::new([0u8; PAGE_SIZE]);
        file.read_exact(&mut buf[..])?;
        Ok(buf)
    }

    /// Read every allocated page, in id order, opening each chunk file once.
    pub fn read_all_pages(&self) -> Result<Vec<PageBuf>> {
        let mut out = Vec::with_capacity(self.page_count as usize);
        let mut id = 0u64;
        while id < self.page_count {
            let chunk_ix = id / PAGES_PER_CHUNK;
            let chunk_start = chunk_ix * PAGES_PER_CHUNK;
            let chunk_end = ((chunk_ix + 1) * PAGES_PER_CHUNK).min(self.page_count);
            let path = self.chunk_path(chunk_ix);
            let mut file = File::open(&path)?;
            file.seek(SeekFrom::Start((id - chunk_start) * PAGE_SIZE as u64))?;
            for _ in id..chunk_end {
                let mut buf: PageBuf = Box::new([0u8; PAGE_SIZE]);
                file.read_exact(&mut buf[..])?;
                out.push(buf);
            }
            id = chunk_end;
        }
        Ok(out)
    }
}

// ---------------------------------------------------------------------------
// Production Mode: read-only mmap over a baked monolith
// ---------------------------------------------------------------------------

/// A memory-mapped, read-only `.pvdb` monolith.
pub struct Monolith {
    mmap: Arc<Mmap>,
    header: FileHeader,
}

impl Monolith {
    /// Open and validate a `.pvdb` file (checks magic, reads the offset header).
    pub fn open(path: impl AsRef<Path>) -> Result<Self> {
        let file = File::open(path)?;
        // SAFETY: the file is opened read-only and the resulting `Mmap` is never
        // written through; callers only ever read slices of it.
        let mmap = unsafe { Mmap::map(&file)? };
        if mmap.len() < FILE_HEADER_SIZE {
            return Err(PvError::Corruption(
                "monolith smaller than file header".into(),
            ));
        }
        let header = FileHeader::decode(&mmap[..FILE_HEADER_SIZE])?;
        if header.cas_offset < FILE_HEADER_SIZE as u64
            || header.manifest_offset < header.cas_offset
            || header.manifest_offset as usize > mmap.len()
            || (header.cas_offset as usize - FILE_HEADER_SIZE) % PAGE_SIZE != 0
        {
            return Err(PvError::Corruption(
                "monolith offsets are inconsistent".into(),
            ));
        }
        Ok(Self {
            mmap: Arc::new(mmap),
            header,
        })
    }

    /// Number of pages packed into the page-data block.
    pub fn page_count(&self) -> u64 {
        ((self.header.cas_offset as usize - FILE_HEADER_SIZE) / PAGE_SIZE) as u64
    }

    /// Read a page out of the mapped page-data block.
    pub fn read_page(&self, id: u64) -> Result<PageBuf> {
        if id >= self.page_count() {
            return Err(PvError::PageFault { page_id: id });
        }
        let start = FILE_HEADER_SIZE + id as usize * PAGE_SIZE;
        let mut buf: PageBuf = Box::new([0u8; PAGE_SIZE]);
        buf.copy_from_slice(&self.mmap[start..start + PAGE_SIZE]);
        Ok(buf)
    }

    /// Absolute offset of the CAS blob pool within the file.
    pub fn cas_offset(&self) -> usize {
        self.header.cas_offset as usize
    }

    /// The trailing JSON manifest payload.
    pub fn manifest_bytes(&self) -> &[u8] {
        &self.mmap[self.header.manifest_offset as usize..]
    }

    /// A clonable handle to the underlying mapping (for [`crate::storage::cas`]).
    pub fn mmap(&self) -> Arc<Mmap> {
        self.mmap.clone()
    }
}

// ---------------------------------------------------------------------------
// In-memory backend (no filesystem), `Database::open_memory` and wasm targets
// ---------------------------------------------------------------------------

/// A page store held entirely in RAM (no filesystem / mmap). Useful for tests,
/// ephemeral databases, and `wasm32` targets (browser/Node) where there is no
/// filesystem available at runtime.
#[derive(Default)]
pub struct MemStore {
    pages: RefCell<Vec<PageBuf>>,
}

impl MemStore {
    /// An empty in-memory store.
    pub fn new() -> Self {
        Self::default()
    }

    /// Number of pages held.
    pub fn page_count(&self) -> u64 {
        self.pages.borrow().len() as u64
    }

    /// Allocate a fresh zeroed page, returning its id.
    pub fn alloc_page(&self) -> u64 {
        let mut pages = self.pages.borrow_mut();
        let id = pages.len() as u64;
        pages.push(Box::new([0u8; PAGE_SIZE]));
        id
    }

    /// Overwrite an allocated page.
    pub fn write_page(&self, id: u64, page: &[u8; PAGE_SIZE]) -> Result<()> {
        let mut pages = self.pages.borrow_mut();
        let slot = pages
            .get_mut(id as usize)
            .ok_or(PvError::PageFault { page_id: id })?;
        **slot = *page;
        Ok(())
    }

    /// Write consecutive pages starting at `start_id`.
    pub fn write_pages_from(&self, start_id: u64, pages: &[&PageBuf]) -> Result<()> {
        for (i, page) in pages.iter().enumerate() {
            self.write_page(start_id + i as u64, page)?;
        }
        Ok(())
    }

    /// Read a page.
    pub fn read_page(&self, id: u64) -> Result<PageBuf> {
        self.pages
            .borrow()
            .get(id as usize)
            .cloned()
            .ok_or(PvError::PageFault { page_id: id })
    }

    /// Snapshot every page (for baking an in-memory database to bytes).
    pub fn read_all_pages(&self) -> Result<Vec<PageBuf>> {
        Ok(self.pages.borrow().clone())
    }
}

// ---------------------------------------------------------------------------
// Backend router
// ---------------------------------------------------------------------------

/// Uniform access over the three backends, used by the buffer pool and engine.
pub enum Backend {
    /// Mutable development workspace (filesystem).
    Dev(DevStore),
    /// Mutable in-memory store (no filesystem).
    Mem(MemStore),
    /// Read-only production monolith (mmap).
    Prod(Monolith),
}

impl Backend {
    /// Total pages available.
    pub fn page_count(&self) -> u64 {
        match self {
            Backend::Dev(d) => d.page_count(),
            Backend::Mem(m) => m.page_count(),
            Backend::Prod(m) => m.page_count(),
        }
    }

    /// Read a page from whichever backend is active.
    pub fn read_page(&self, id: u64) -> Result<PageBuf> {
        match self {
            Backend::Dev(d) => d.read_page(id),
            Backend::Mem(m) => m.read_page(id),
            Backend::Prod(m) => m.read_page(id),
        }
    }

    /// Whether the backend accepts writes (dev and in-memory, not production).
    pub fn is_writable(&self) -> bool {
        matches!(self, Backend::Dev(_) | Backend::Mem(_))
    }

    /// Allocate a new page id (writable backends only).
    pub fn alloc_page(&mut self) -> Result<u64> {
        match self {
            Backend::Dev(d) => Ok(d.alloc_page()),
            Backend::Mem(m) => Ok(m.alloc_page()),
            Backend::Prod(_) => Err(PvError::ReadOnly),
        }
    }

    /// Write a single page (writable backends only).
    pub fn write_page(&self, id: u64, page: &[u8; PAGE_SIZE]) -> Result<()> {
        match self {
            Backend::Dev(d) => d.write_page(id, page),
            Backend::Mem(m) => m.write_page(id, page),
            Backend::Prod(_) => Err(PvError::ReadOnly),
        }
    }

    /// Bulk-write consecutive pages (writable backends only).
    pub fn write_pages_from(&self, start_id: u64, pages: &[&PageBuf]) -> Result<()> {
        match self {
            Backend::Dev(d) => d.write_pages_from(start_id, pages),
            Backend::Mem(m) => m.write_pages_from(start_id, pages),
            Backend::Prod(_) => Err(PvError::ReadOnly),
        }
    }

    /// `fsync` durable backends (no-op for in-memory / read-only).
    pub fn sync_data(&self) -> Result<()> {
        match self {
            Backend::Dev(d) => d.sync_data(),
            _ => Ok(()),
        }
    }

    /// Read every page in id order.
    pub fn read_all_pages(&self) -> Result<Vec<PageBuf>> {
        match self {
            Backend::Dev(d) => d.read_all_pages(),
            Backend::Mem(m) => m.read_all_pages(),
            Backend::Prod(m) => (0..m.page_count()).map(|id| m.read_page(id)).collect(),
        }
    }
}

// ---------------------------------------------------------------------------
// Bake: directory -> monolith compilation (spec §2.B, pv_bake)
// ---------------------------------------------------------------------------

/// Compile a page set, CAS blob pool, and manifest into the `.pvdb` monolith
/// **byte image** (no filesystem). Used directly for in-memory/wasm export and
/// by [`bake_monolith`] for the file path.
///
/// Layout: `header(FILE_HEADER_SIZE) | pages | cas_pool | manifest`, with
/// `cas_offset` and `manifest_offset` recorded in the header. Each page already
/// carries its own integrity checksum (see [`stamp_page_checksum`]).
pub fn bake_monolith_bytes(
    pages: &[PageBuf],
    cas_pool: &[u8],
    manifest_json: &[u8],
) -> Result<Vec<u8>> {
    let overflow = || PvError::Corruption("monolith size overflows address space".into());
    let cas_offset = pages
        .len()
        .checked_mul(PAGE_SIZE)
        .and_then(|n| n.checked_add(FILE_HEADER_SIZE))
        .ok_or_else(overflow)?;
    let manifest_offset = cas_offset
        .checked_add(cas_pool.len())
        .ok_or_else(overflow)?;
    let total = manifest_offset
        .checked_add(manifest_json.len())
        .ok_or_else(overflow)?;
    let header = FileHeader::new(manifest_offset as u64, cas_offset as u64);

    let mut out = Vec::with_capacity(total);
    out.extend_from_slice(&header.encode());
    for page in pages {
        out.extend_from_slice(&page[..]);
    }
    out.extend_from_slice(cas_pool);
    out.extend_from_slice(manifest_json);
    Ok(out)
}

/// Compile a page set, CAS blob pool, and manifest into a `.pvdb` file.
pub fn bake_monolith(
    out_path: impl AsRef<Path>,
    pages: &[PageBuf],
    cas_pool: &[u8],
    manifest_json: &[u8],
) -> Result<()> {
    let bytes = bake_monolith_bytes(pages, cas_pool, manifest_json)?;
    let mut out = File::create(out_path)?;
    out.write_all(&bytes)?;
    out.flush()?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dev_store_round_trips_pages() {
        let tmp = tempfile::tempdir().unwrap();
        let mut dev = DevStore::create(tmp.path()).unwrap();
        let id0 = dev.alloc_page();
        let id1 = dev.alloc_page();
        let mut p0: PageBuf = Box::new([0u8; PAGE_SIZE]);
        p0[0] = 0xAA;
        let mut p1: PageBuf = Box::new([0u8; PAGE_SIZE]);
        p1[4095] = 0xBB;
        dev.write_page(id0, &p0).unwrap();
        dev.write_page(id1, &p1).unwrap();

        assert_eq!(dev.read_page(id0).unwrap()[0], 0xAA);
        assert_eq!(dev.read_page(id1).unwrap()[4095], 0xBB);
        assert!(matches!(dev.read_page(2), Err(PvError::PageFault { .. })));
    }

    #[test]
    fn bake_then_open_monolith_matches_pages() {
        let tmp = tempfile::tempdir().unwrap();
        let mut dev = DevStore::create(tmp.path().join("ws")).unwrap();
        let id = dev.alloc_page();
        let mut page: PageBuf = Box::new([0u8; PAGE_SIZE]);
        page[10] = 0x42;
        dev.write_page(id, &page).unwrap();

        let pages = dev.read_all_pages().unwrap();
        let cas_pool = b"blobpool".to_vec();
        let manifest = br#"{"ok":true}"#.to_vec();
        let out = tmp.path().join("db.pvdb");
        bake_monolith(&out, &pages, &cas_pool, &manifest).unwrap();

        let mono = Monolith::open(&out).unwrap();
        assert_eq!(mono.page_count(), 1);
        assert_eq!(mono.read_page(0).unwrap()[10], 0x42);
        assert_eq!(mono.manifest_bytes(), manifest.as_slice());
        // CAS pool sits between pages and manifest.
        let pool_end = mono.cas_offset() + cas_pool.len();
        assert_eq!(
            &mono.mmap()[mono.cas_offset()..pool_end],
            cas_pool.as_slice()
        );
    }

    #[test]
    fn open_rejects_bad_magic() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("bad.pvdb");
        std::fs::write(&path, vec![0u8; 64]).unwrap();
        assert!(matches!(
            Monolith::open(&path),
            Err(PvError::SignatureMismatch { .. })
        ));
    }

    #[test]
    fn sync_data_fsyncs_all_dirty_chunks() {
        let tmp = tempfile::tempdir().unwrap();
        let dev = DevStore::create(tmp.path()).unwrap();
        let page = [7u8; PAGE_SIZE];
        dev.write_page(0, &page).unwrap(); // chunk 0
        dev.write_page(PAGES_PER_CHUNK, &page).unwrap(); // chunk 1 (different file)
                                                         // Must fsync both chunks (the cached one and the reopened one) without error.
        dev.sync_data().unwrap();
        assert!(tmp.path().join("chunks").join("chunk_00000.pvd").exists());
        assert!(tmp.path().join("chunks").join("chunk_00001.pvd").exists());
    }

    #[test]
    fn page_checksum_excludes_its_own_field_and_is_nonzero() {
        let mut page: PageBuf = Box::new([0u8; PAGE_SIZE]);
        page[0] = 0xAB;
        page[100] = 0xCD;
        let c1 = page_checksum(&page);
        assert_ne!(c1, 0);
        // Overwriting the checksum field must not change the computed checksum.
        page[PAGE_CHECKSUM_OFFSET..PAGE_CHECKSUM_OFFSET + 4].copy_from_slice(&[1, 2, 3, 4]);
        assert_eq!(page_checksum(&page), c1);
    }

    #[test]
    fn stamp_then_verify_round_trips() {
        let mut page: PageBuf = Box::new([0u8; PAGE_SIZE]);
        page[42] = 0x7F;
        stamp_page_checksum(&mut page);
        assert!(verify_page_checksum(7, &page).is_ok());
    }

    #[test]
    fn verify_detects_a_flipped_body_byte() {
        let mut page: PageBuf = Box::new([0u8; PAGE_SIZE]);
        page[42] = 0x7F;
        stamp_page_checksum(&mut page);
        page[42] ^= 0x01; // simulate bit-rot in the page body
        assert!(matches!(
            verify_page_checksum(7, &page),
            Err(PvError::Corruption(_))
        ));
    }

    #[test]
    fn verify_detects_a_corrupt_checksum_field() {
        let mut page: PageBuf = Box::new([0u8; PAGE_SIZE]);
        page[42] = 0x7F;
        stamp_page_checksum(&mut page);
        page[PAGE_CHECKSUM_OFFSET] ^= 0xFF; // corrupt the stored checksum itself
        assert!(matches!(
            verify_page_checksum(7, &page),
            Err(PvError::Corruption(_))
        ));
    }

    #[test]
    fn verify_accepts_an_unstamped_blank_page() {
        // A zeroed (allocated-but-never-written) page has a zero checksum field and
        // is accepted without verification.
        let page: PageBuf = Box::new([0u8; PAGE_SIZE]);
        assert!(verify_page_checksum(0, &page).is_ok());
    }
}
