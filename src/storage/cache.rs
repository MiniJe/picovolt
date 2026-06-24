//! A bounded buffer pool (page cache) over a VLE [`Backend`].
//!
//! This is what lets PicoVolt serve datasets larger than RAM: scans and index
//! lookups read pages **through** the cache, which keeps at most `capacity`
//! pages resident and evicts least-recently-used pages (writing them back first
//! if dirty). Writes go through the cache too, so the on-disk image and the
//! resident image never diverge.

use std::collections::HashMap;

use crate::core::errors::Result;
use crate::core::types::{PageId, PAGE_SIZE};
use crate::storage::page::RowPage;
use crate::storage::vle::{stamp_page_checksum, verify_page_checksum, Backend, PageBuf};

/// Default buffer-pool size in pages (4096 pages = 16 MiB).
pub const DEFAULT_CACHE_PAGES: usize = 4096;

struct Entry {
    page: PageBuf,
    dirty: bool,
    last_used: u64,
}

/// A least-recently-used, write-back page cache.
pub struct PageCache {
    backend: Backend,
    capacity: usize,
    entries: HashMap<PageId, Entry>,
    clock: u64,
}

impl PageCache {
    /// Wrap a backend with a buffer pool of `capacity` pages (min 1).
    pub fn new(backend: Backend, capacity: usize) -> Self {
        Self {
            backend,
            capacity: capacity.max(1),
            entries: HashMap::new(),
            clock: 0,
        }
    }

    /// Whether the backend accepts writes.
    pub fn is_writable(&self) -> bool {
        self.backend.is_writable()
    }

    /// The configured resident-page capacity.
    pub fn capacity(&self) -> usize {
        self.capacity
    }

    /// Resize the buffer pool (min 1 page).
    pub fn set_capacity(&mut self, capacity: usize) -> Result<()> {
        self.capacity = capacity.max(1);
        self.evict_if_needed()
    }

    /// Number of pages currently resident.
    pub fn resident(&self) -> usize {
        self.entries.len()
    }

    /// Borrow the backend (for baking).
    pub fn backend(&self) -> &Backend {
        &self.backend
    }

    /// Allocate a fresh page id (writable backends only).
    pub fn alloc_page(&mut self) -> Result<PageId> {
        self.backend.alloc_page()
    }

    fn tick(&mut self) -> u64 {
        self.clock += 1;
        self.clock
    }

    fn ensure_loaded(&mut self, id: PageId) -> Result<()> {
        if self.entries.contains_key(&id) {
            return Ok(());
        }
        let page = self.backend.read_page(id)?;
        verify_page_checksum(id, &page)?;
        self.evict_if_needed()?;
        let last_used = self.tick();
        self.entries.insert(
            id,
            Entry {
                page,
                dirty: false,
                last_used,
            },
        );
        Ok(())
    }

    fn evict_if_needed(&mut self) -> Result<()> {
        while self.entries.len() >= self.capacity {
            let victim = self
                .entries
                .iter()
                .min_by_key(|(_, e)| e.last_used)
                .map(|(id, _)| *id);
            let Some(id) = victim else { break };
            let mut entry = self.entries.remove(&id).expect("victim exists");
            if entry.dirty {
                stamp_page_checksum(&mut entry.page);
                self.backend.write_page(id, &entry.page)?;
            }
        }
        Ok(())
    }

    /// Read a page through the cache and apply `f` to its bytes.
    pub fn with_page<R>(
        &mut self,
        id: PageId,
        f: impl FnOnce(&[u8; PAGE_SIZE]) -> Result<R>,
    ) -> Result<R> {
        self.ensure_loaded(id)?;
        let last_used = self.tick();
        let entry = self.entries.get_mut(&id).expect("just loaded");
        entry.last_used = last_used;
        f(&entry.page)
    }

    /// Insert/replace a page in the cache as **dirty** (deferred write-back).
    pub fn write(&mut self, id: PageId, page: PageBuf) -> Result<()> {
        if !self.entries.contains_key(&id) {
            self.evict_if_needed()?;
        }
        let last_used = self.tick();
        self.entries.insert(
            id,
            Entry {
                page,
                dirty: true,
                last_used,
            },
        );
        Ok(())
    }

    /// Read-modify-write a page as an owned [`RowPage`], marking it dirty.
    pub fn with_page_mut<R>(
        &mut self,
        id: PageId,
        f: impl FnOnce(&mut RowPage) -> Result<R>,
    ) -> Result<R> {
        self.ensure_loaded(id)?;
        let last_used = self.tick();
        let entry = self.entries.get_mut(&id).expect("just loaded");
        entry.last_used = last_used;
        let mut page = RowPage::from_bytes(Box::new(*entry.page))?;
        let out = f(&mut page)?;
        entry.page = page.into_bytes();
        entry.dirty = true;
        Ok(out)
    }

    /// `fsync` durable backends to stable storage (no-op for in-memory).
    pub fn sync(&self) -> Result<()> {
        self.backend.sync_data()
    }

    /// Write all dirty pages back to the backend (bulk per contiguous run).
    pub fn flush(&mut self) -> Result<()> {
        if !self.backend.is_writable() {
            return Ok(());
        }
        let mut ids: Vec<PageId> = self
            .entries
            .iter()
            .filter(|(_, e)| e.dirty)
            .map(|(id, _)| *id)
            .collect();
        ids.sort_unstable();

        // Stamp every dirty page's integrity checksum before it is written back,
        // so the on-disk image is always self-verifying.
        for id in &ids {
            if let Some(e) = self.entries.get_mut(id) {
                stamp_page_checksum(&mut e.page);
            }
        }

        let mut i = 0;
        while i < ids.len() {
            let mut j = i;
            while j + 1 < ids.len() && ids[j + 1] == ids[j] + 1 {
                j += 1;
            }
            let run: Vec<&PageBuf> = ids[i..=j].iter().map(|id| &self.entries[id].page).collect();
            self.backend.write_pages_from(ids[i], &run)?;
            i = j + 1;
        }

        for id in ids {
            if let Some(e) = self.entries.get_mut(&id) {
                e.dirty = false;
            }
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::storage::vle::DevStore;

    fn dev_cache(tmp: &std::path::Path, capacity: usize) -> PageCache {
        PageCache::new(Backend::Dev(DevStore::create(tmp).unwrap()), capacity)
    }

    #[test]
    fn writes_survive_eviction_with_tiny_cache() {
        let tmp = tempfile::tempdir().unwrap();
        // Capacity of 2 pages, but we write 10, eviction must persist them.
        let mut cache = dev_cache(tmp.path(), 2);
        for _ in 0..10 {
            let id = cache.alloc_page().unwrap();
            let mut page = RowPage::new(id);
            page.insert(&[id as u8; 8]).unwrap();
            cache.write(id, page.into_bytes()).unwrap();
        }
        cache.flush().unwrap();
        assert!(cache.resident() <= 2, "cache must stay bounded");

        // Every page is readable back with correct contents.
        for id in 0..10u64 {
            let first = cache
                .with_page(id, |buf| {
                    let rp = crate::storage::page::RowPageRef::new(buf)?;
                    Ok(rp.record(0)?[0])
                })
                .unwrap();
            assert_eq!(first, id as u8);
        }
    }

    #[test]
    fn patch_then_read_is_consistent() {
        let tmp = tempfile::tempdir().unwrap();
        let mut cache = dev_cache(tmp.path(), 4);
        let id = cache.alloc_page().unwrap();
        let mut page = RowPage::new(id);
        // 24-byte envelope + marker; patch tx_deleted.
        page.insert(&[0u8; 25]).unwrap();
        cache.write(id, page.into_bytes()).unwrap();

        cache
            .with_page_mut(id, |p| p.patch_envelope_deleted(0, 7))
            .unwrap();
        cache.flush().unwrap();

        let tx_deleted_byte = cache
            .with_page(id, |buf| {
                let rp = crate::storage::page::RowPageRef::new(buf)?;
                Ok(rp.record(0)?[8])
            })
            .unwrap();
        assert_eq!(tx_deleted_byte, 7);
    }

    #[test]
    fn detects_backend_corruption_on_reload() {
        let tmp = tempfile::tempdir().unwrap();
        // Capacity 1, so making a second page resident forces page 0 out.
        let mut cache = dev_cache(tmp.path(), 1);

        let id = cache.alloc_page().unwrap();
        let mut page = RowPage::new(id);
        page.insert(b"important record").unwrap();
        cache.write(id, page.into_bytes()).unwrap();
        cache.flush().unwrap(); // stamps the integrity checksum into the chunk file

        // Corrupt one body byte directly in the chunk file, beneath the cache.
        let chunk = tmp.path().join("chunks").join("chunk_00000.pvd");
        let mut bytes = std::fs::read(&chunk).unwrap();
        bytes[crate::core::types::PAGE_HEADER_SIZE + 1] ^= 0xFF;
        std::fs::write(&chunk, &bytes).unwrap();

        // Evict the (clean) page 0 by making a second page resident.
        let other = cache.alloc_page().unwrap();
        cache
            .write(other, RowPage::new(other).into_bytes())
            .unwrap();

        // Reloading page 0 from the corrupted backend must be rejected.
        let result = cache.with_page(id, |_| Ok(()));
        assert!(
            matches!(result, Err(crate::core::errors::PvError::Corruption(_))),
            "expected corruption, got {result:?}"
        );
    }
}
