//! Content-Addressable Storage (CAS) broker (spec §4.A).
//!
//! Payloads larger than [`CAS_INLINE_THRESHOLD`] are hashed with BLAKE3 and
//! interned: identical bytes are stored once and referenced by a stable 8-byte
//! id. The id is the insertion index, which keeps row records compact and makes
//! the on-disk blob pool a simple id-ordered concatenation.
//!
//! Three backends share one interface:
//! * **memory**, blobs held in RAM (used while building a database in RAM);
//! * **dev**, blobs additionally mirrored to `.pv/blobs/<aa>/<hash>` files;
//! * **prod**, blobs read by reference out of an mmap'd `.pvdb` blob pool.

use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use memmap2::Mmap;

use crate::core::errors::{PvError, Result};
use crate::core::types::{CAS_INLINE_THRESHOLD, CAS_POINTER_SIZE};

/// An 8-byte CAS redirect pointer as stored inside a record slot.
pub type CasId = u64;

/// Directory of `(offset, len)` pairs, one per blob id, locating each blob
/// within a packed pool.
pub type CasDir = Vec<(u64, u64)>;

#[derive(Debug)]
enum Blob {
    Owned(Vec<u8>),
    Mapped {
        mmap: Arc<Mmap>,
        offset: usize,
        len: usize,
    },
}

struct CasEntry {
    hash: [u8; 32],
    blob: Blob,
}

/// The deduplicating blob interner.
pub struct CasStore {
    by_hash: HashMap<[u8; 32], CasId>,
    entries: Vec<CasEntry>,
    dev_root: Option<PathBuf>,
}

impl CasStore {
    /// An empty, in-memory store.
    pub fn new_memory() -> Self {
        Self {
            by_hash: HashMap::new(),
            entries: Vec::new(),
            dev_root: None,
        }
    }

    /// An empty store that mirrors every blob into `<root>/blobs/`.
    pub fn new_dev(root: impl Into<PathBuf>) -> Self {
        Self {
            by_hash: HashMap::new(),
            entries: Vec::new(),
            dev_root: Some(root.into()),
        }
    }

    /// Whether a payload of this length must be redirected through CAS.
    #[inline]
    pub fn should_intern(len: usize) -> bool {
        len > CAS_INLINE_THRESHOLD
    }

    /// Number of distinct blobs held.
    pub fn len(&self) -> usize {
        self.entries.len()
    }

    /// Whether the store holds no blobs.
    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    /// Intern `data`, returning its stable id. Identical bytes return the
    /// existing id without storing a second copy.
    pub fn put(&mut self, data: &[u8]) -> Result<CasId> {
        let hash: [u8; 32] = *blake3::hash(data).as_bytes();
        if let Some(&id) = self.by_hash.get(&hash) {
            return Ok(id);
        }
        let id = self.entries.len() as CasId;
        if let Some(root) = &self.dev_root {
            write_blob_file(root, &hash, data)?;
        }
        self.entries.push(CasEntry {
            hash,
            blob: Blob::Owned(data.to_vec()),
        });
        self.by_hash.insert(hash, id);
        Ok(id)
    }

    /// Borrow the blob bytes for `id`.
    pub fn get(&self, id: CasId) -> Result<&[u8]> {
        let entry = self
            .entries
            .get(id as usize)
            .ok_or_else(|| PvError::CasMiss(format!("id {id}")))?;
        Ok(match &entry.blob {
            Blob::Owned(v) => v.as_slice(),
            Blob::Mapped { mmap, offset, len } => &mmap[*offset..*offset + *len],
        })
    }

    /// Lower-hex BLAKE3 digest of blob `id` (the dev-mode file name).
    pub fn hash_hex(&self, id: CasId) -> Result<String> {
        let entry = self
            .entries
            .get(id as usize)
            .ok_or_else(|| PvError::CasMiss(format!("id {id}")))?;
        Ok(hex(&entry.hash))
    }

    /// Pack every blob, in id order, into a contiguous pool. Returns the bytes
    /// plus a directory of `(offset, len)` pairs (offsets relative to the pool
    /// start), exactly the form baked into a `.pvdb` CAS blob pool.
    pub fn pack(&self) -> Result<(Vec<u8>, CasDir)> {
        let mut pool = Vec::new();
        let mut dir = Vec::with_capacity(self.entries.len());
        for id in 0..self.entries.len() as CasId {
            let bytes = self.get(id)?;
            dir.push((pool.len() as u64, bytes.len() as u64));
            pool.extend_from_slice(bytes);
        }
        Ok((pool, dir))
    }

    /// Reconstruct a dev-mode store from the ordered hash catalog, reading each
    /// blob back out of `<root>/blobs/`.
    pub fn load_dev(root: impl Into<PathBuf>, hashes: &[String]) -> Result<Self> {
        let root = root.into();
        let mut store = Self::new_dev(root.clone());
        for hex_hash in hashes {
            // SECURITY: validate the hash is exactly 64 hex chars BEFORE using it
            // to build a path. An unvalidated manifest string could otherwise
            // contain `..` / path separators (arbitrary file read) or be too
            // short for `blob_path`'s `[..2]` slice (panic).
            let expected = parse_hex32(hex_hash)?;
            let path = blob_path(&root, hex_hash);
            let data = fs::read(&path)?;
            // Verify the file content actually hashes to the claimed digest, so a
            // tampered workspace cannot substitute blob contents.
            if blake3::hash(&data).as_bytes() != &expected {
                return Err(PvError::Corruption(format!(
                    "CAS blob {hex_hash} failed integrity check"
                )));
            }
            store.put(&data)?;
        }
        Ok(store)
    }

    /// Reconstruct a read-only store whose blobs live inside an mmap'd monolith.
    ///
    /// `base` is the absolute offset of the blob pool within `mmap`; `dir` holds
    /// `(relative_offset, len)` per id; `hashes` are the matching digests.
    pub fn from_mapped(
        mmap: Arc<Mmap>,
        base: usize,
        dir: &[(u64, u64)],
        hashes: &[String],
    ) -> Result<Self> {
        if dir.len() != hashes.len() {
            return Err(PvError::Corruption(
                "CAS directory / hash catalog length mismatch".into(),
            ));
        }
        let map_len = mmap.len();
        let mut store = Self::new_memory();
        for (id, (&(rel_off, len), hex_hash)) in dir.iter().zip(hashes).enumerate() {
            let hash = parse_hex32(hex_hash)?;
            // SECURITY: validate every (offset, len) lies within the mapping with
            // checked arithmetic, so `get()`'s slice can never panic out of bounds.
            let offset = base
                .checked_add(rel_off as usize)
                .ok_or_else(|| PvError::Corruption("CAS offset overflow".into()))?;
            let end = offset
                .checked_add(len as usize)
                .ok_or_else(|| PvError::Corruption("CAS extent overflow".into()))?;
            if end > map_len {
                return Err(PvError::Corruption(format!(
                    "CAS blob {id} extends past end of file ({end} > {map_len})"
                )));
            }
            store.entries.push(CasEntry {
                hash,
                blob: Blob::Mapped {
                    mmap: mmap.clone(),
                    offset,
                    len: len as usize,
                },
            });
            store.by_hash.insert(hash, id as CasId);
        }
        Ok(store)
    }
}

fn blob_path(root: &Path, hex_hash: &str) -> PathBuf {
    let shard = &hex_hash[..2];
    root.join("blobs").join(shard).join(hex_hash)
}

fn write_blob_file(root: &Path, hash: &[u8; 32], data: &[u8]) -> Result<()> {
    let hex_hash = hex(hash);
    let path = blob_path(root, &hex_hash);
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    // Content-addressed: if the file already exists its bytes are identical.
    if !path.exists() {
        fs::write(&path, data)?;
    }
    Ok(())
}

fn hex(bytes: &[u8]) -> String {
    let mut s = String::with_capacity(bytes.len() * 2);
    for b in bytes {
        s.push_str(&format!("{b:02x}"));
    }
    s
}

fn parse_hex32(s: &str) -> Result<[u8; 32]> {
    // Operate on bytes (not `&s[..]` slicing) so a 64-byte string containing a
    // multibyte UTF-8 char can't panic on a non-char-boundary. Only ASCII hex
    // bytes are accepted, which also guarantees the string is a safe file name
    // (no path separators / `..`) when later used by `blob_path`.
    let bytes = s.as_bytes();
    if bytes.len() != 64 {
        return Err(PvError::Corruption(format!(
            "bad CAS hash length: {}",
            bytes.len()
        )));
    }
    let mut out = [0u8; 32];
    for (i, byte) in out.iter_mut().enumerate() {
        *byte = (hex_nibble(bytes[i * 2])? << 4) | hex_nibble(bytes[i * 2 + 1])?;
    }
    Ok(out)
}

fn hex_nibble(c: u8) -> Result<u8> {
    match c {
        b'0'..=b'9' => Ok(c - b'0'),
        b'a'..=b'f' => Ok(c - b'a' + 10),
        b'A'..=b'F' => Ok(c - b'A' + 10),
        _ => Err(PvError::Corruption("non-hex CAS digest".into())),
    }
}

const _: () = assert!(CAS_POINTER_SIZE == std::mem::size_of::<CasId>());

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dedups_identical_payloads() {
        let mut cas = CasStore::new_memory();
        let a = cas.put(b"the quick brown fox jumps").unwrap();
        let b = cas.put(b"the quick brown fox jumps").unwrap();
        let c = cas.put(b"a different long payload here").unwrap();
        assert_eq!(a, b);
        assert_ne!(a, c);
        assert_eq!(cas.len(), 2);
        assert_eq!(cas.get(a).unwrap(), b"the quick brown fox jumps");
    }

    #[test]
    fn threshold_predicate_matches_spec() {
        assert!(!CasStore::should_intern(16)); // 16 bytes -> inline
        assert!(CasStore::should_intern(17)); // 17 bytes -> CAS
    }

    #[test]
    fn pack_produces_id_ordered_pool() {
        let mut cas = CasStore::new_memory();
        let id0 = cas.put(&[0xAAu8; 20]).unwrap();
        let id1 = cas.put(&[0xBBu8; 30]).unwrap();
        let (pool, dir) = cas.pack().unwrap();
        assert_eq!(pool.len(), 50);
        assert_eq!(dir[id0 as usize], (0, 20));
        assert_eq!(dir[id1 as usize], (20, 30));
    }

    #[test]
    fn dev_round_trips_through_files() {
        let tmp = tempfile::tempdir().unwrap();
        let mut cas = CasStore::new_dev(tmp.path());
        let id = cas
            .put(b"persisted blob payload exceeding sixteen")
            .unwrap();
        let hashes = vec![cas.hash_hex(id).unwrap()];

        let reopened = CasStore::load_dev(tmp.path(), &hashes).unwrap();
        assert_eq!(
            reopened.get(id).unwrap(),
            b"persisted blob payload exceeding sixteen"
        );
    }

    #[test]
    fn load_dev_rejects_malicious_hash_strings() {
        let tmp = tempfile::tempdir().unwrap();
        // Path traversal attempt must be rejected, never turned into a path read.
        assert!(CasStore::load_dev(tmp.path(), &["../../../../etc/passwd".into()]).is_err());
        // Too-short / non-ASCII strings must error, not panic on slicing.
        assert!(CasStore::load_dev(tmp.path(), &["a".into()]).is_err());
        assert!(CasStore::load_dev(tmp.path(), &["€".repeat(22)]).is_err());
        // 64 ASCII chars that aren't all hex must be rejected too.
        assert!(CasStore::load_dev(tmp.path(), &["g".repeat(64)]).is_err());
    }

    #[test]
    fn from_mapped_rejects_out_of_bounds_directory() {
        use std::io::Write;
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("pool");
        std::fs::File::create(&path)
            .unwrap()
            .write_all(&[0u8; 64])
            .unwrap();
        let file = std::fs::File::open(&path).unwrap();
        // SAFETY: read-only mapping used only for slicing in this test.
        let mmap = Arc::new(unsafe { Mmap::map(&file).unwrap() });
        let valid_hash = "00".repeat(32); // 64 hex chars
                                          // A blob claiming to extend past the 64-byte mapping must be rejected.
        let result = CasStore::from_mapped(mmap, 0, &[(0, 1_000)], &[valid_hash]);
        assert!(matches!(result, Err(PvError::Corruption(_))));
    }
}
