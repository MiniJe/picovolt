//! Virtualization Layer Engine (VLE) — the router that hides whether pages live
//! in a mutable `.pv/` directory (Development Mode) or inside a single immutable
//! memory-mapped `.pvdb` monolith (Production Mode), per spec §2.
//!
//! * [`DevStore`] — append-only 4096-byte pages spread across `chunks/*.pvd`
//!   files capped at 64 MiB each.
//! * [`Monolith`] — a read-only mmap over a baked `.pvdb`, slicing pages and
//!   exposing the CAS / manifest regions by absolute offset.
//! * [`bake_monolith`] — compiles a dev workspace's pages + CAS pool + manifest
//!   into the contiguous monolith layout.

use std::cell::RefCell;
use std::fs::{self, File, OpenOptions};
use std::io::{Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};
use std::sync::Arc;

use memmap2::Mmap;

use crate::core::errors::{PvError, Result};
use crate::core::types::{FileHeader, CHUNK_CAP_BYTES, FILE_HEADER_SIZE, PAGE_SIZE};

/// Pages per 64 MiB chunk file: 16 384.
pub const PAGES_PER_CHUNK: u64 = CHUNK_CAP_BYTES / PAGE_SIZE as u64;

/// A boxed 4096-byte page buffer.
pub type PageBuf = Box<[u8; PAGE_SIZE]>;

// ---------------------------------------------------------------------------
// Development Mode: directory of append-only chunk files
// ---------------------------------------------------------------------------

/// Mutable page store backed by `chunks/chunk_NNNNN.pvd` files under a workspace.
///
/// Keeps the write handle for the most-recently-written chunk open, so a stream
/// of appends (autocommit) does not reopen the file per page. Reads use separate
/// handles; Rust opens files with shared read/write modes, so this is safe, and
/// durability is unchanged (neither path issues an explicit `fsync`).
pub struct DevStore {
    root: PathBuf,
    page_count: u64,
    write_handle: RefCell<Option<(u64, File)>>,
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

    /// `fsync` the active chunk write handle to stable storage.
    ///
    /// Note: only the currently-open chunk is fsync'd. For the common case of a
    /// workspace within one 64 MiB chunk this is full crash-safety; spanning
    /// multiple chunks would need per-chunk fsync (future work).
    pub fn sync_data(&self) -> Result<()> {
        if let Some((_, file)) = self.write_handle.borrow().as_ref() {
            file.sync_all()?;
        }
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
// Backend router
// ---------------------------------------------------------------------------

/// Uniform read access over either backend, used by the engine while loading.
pub enum Backend {
    /// Mutable development workspace.
    Dev(DevStore),
    /// Read-only production monolith.
    Prod(Monolith),
}

impl Backend {
    /// Total pages available.
    pub fn page_count(&self) -> u64 {
        match self {
            Backend::Dev(d) => d.page_count(),
            Backend::Prod(m) => m.page_count(),
        }
    }

    /// Read a page from whichever backend is active.
    pub fn read_page(&self, id: u64) -> Result<PageBuf> {
        match self {
            Backend::Dev(d) => d.read_page(id),
            Backend::Prod(m) => m.read_page(id),
        }
    }

    /// Whether the backend accepts writes (true only for development mode).
    pub fn is_writable(&self) -> bool {
        matches!(self, Backend::Dev(_))
    }
}

// ---------------------------------------------------------------------------
// Bake: directory -> monolith compilation (spec §2.B, pv_bake)
// ---------------------------------------------------------------------------

/// Compile a page set, CAS blob pool, and manifest into a `.pvdb` monolith.
///
/// Layout: `header(20) | pages | cas_pool | manifest`, with `cas_offset` and
/// `manifest_offset` recorded in the header.
pub fn bake_monolith(
    out_path: impl AsRef<Path>,
    pages: &[PageBuf],
    cas_pool: &[u8],
    manifest_json: &[u8],
) -> Result<()> {
    let overflow = || PvError::Corruption("monolith size overflows address space".into());
    let cas_offset = pages
        .len()
        .checked_mul(PAGE_SIZE)
        .and_then(|n| n.checked_add(FILE_HEADER_SIZE))
        .ok_or_else(overflow)?;
    let manifest_offset = cas_offset
        .checked_add(cas_pool.len())
        .ok_or_else(overflow)?;
    let header = FileHeader::new(manifest_offset as u64, cas_offset as u64);

    let mut out = File::create(out_path)?;
    out.write_all(&header.encode())?;
    for page in pages {
        out.write_all(&page[..])?;
    }
    out.write_all(cas_pool)?;
    out.write_all(manifest_json)?;
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
}
