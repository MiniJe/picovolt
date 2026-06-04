//! Primitive types, on-disk constants, and fixed byte layouts for PicoVolt.
//!
//! # Encoding policy
//!
//! Every persisted structure in this module is serialized through an **explicit,
//! little-endian byte encoding** (`encode` / `decode`) rather than by casting a
//! `#[repr(C)]` struct to bytes. This is deliberate: relying on the in-memory
//! layout of a struct for persistence couples the file format to a particular
//! compiler, target architecture, and padding strategy. Explicit encoders keep
//! the format stable and self-documenting, and let us honour the exact offsets
//! given in the specification regardless of native alignment.
//!
//! All multi-byte integers are little-endian, matching the `.pvdb` monolith
//! header definition.

use crate::core::errors::{PvError, Result};

// ---------------------------------------------------------------------------
// On-disk constants
// ---------------------------------------------------------------------------

/// Size of a single physical page, in bytes. All storage I/O happens in units
/// of this size.
pub const PAGE_SIZE: usize = 4096;

/// Fixed header size shared by both the row and columnar page layouts.
pub const PAGE_HEADER_SIZE: usize = 24;

/// Magic bytes identifying a baked monolithic `.pvdb` file: ASCII `"PVDB"`.
pub const MAGIC_BYTES: [u8; 4] = [0x50, 0x56, 0x44, 0x42];

/// Size of the `.pvdb` file header: `magic(4) + manifest_offset(8) + cas_offset(8)`.
pub const FILE_HEADER_SIZE: usize = 20;

/// Hard cap on a development-mode append-only chunk file: 64 MiB.
pub const CHUNK_CAP_BYTES: u64 = 67_108_864;

/// Payloads **strictly larger** than this many bytes are intercepted and
/// redirected to Content-Addressable Storage, replaced inline by an 8-byte
/// pointer. Payloads of this size or smaller are stored inline.
pub const CAS_INLINE_THRESHOLD: usize = 16;

/// Width of a CAS redirect pointer, in bytes.
pub const CAS_POINTER_SIZE: usize = 8;

/// Length of a BLAKE3 digest, in bytes.
pub const BLAKE3_HASH_LEN: usize = 32;

/// Idle interval (seconds) after which a hot row page becomes eligible for
/// background transposition into the cold columnar layout.
pub const COLD_CONVERSION_SECS: u64 = 300;

// ---------------------------------------------------------------------------
// Primitive identifiers
// ---------------------------------------------------------------------------

/// Transaction identifier. Monotonically increasing; [`TX_NULL`] is reserved as
/// the "unset" sentinel.
pub type TxId = u64;

/// Logical page identifier.
pub type PageId = u64;

/// Absolute physical address of a record (byte offset within the data block),
/// used to chain MVCC versions together.
pub type PhysAddr = u64;

/// Reserved transaction id meaning "no transaction" — used by
/// [`RecordEnvelope::tx_deleted`] to mark a record as still active.
pub const TX_NULL: TxId = 0;

// ---------------------------------------------------------------------------
// Page type discriminant
// ---------------------------------------------------------------------------

/// The Chameleon layout a page is currently using.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[repr(u8)]
pub enum PageType {
    /// Hot, mutable slotted row-buffer page (`0x01`).
    Row = 0x01,
    /// Cold, packed columnar page (`0x02`).
    Columnar = 0x02,
}

impl PageType {
    /// The raw discriminant byte stored in the page header.
    #[inline]
    pub const fn as_byte(self) -> u8 {
        self as u8
    }
}

impl TryFrom<u8> for PageType {
    type Error = PvError;

    fn try_from(value: u8) -> Result<Self> {
        match value {
            0x01 => Ok(PageType::Row),
            0x02 => Ok(PageType::Columnar),
            other => Err(PvError::InvalidPageType(other)),
        }
    }
}

// ---------------------------------------------------------------------------
// MVCC record envelope (spec §5)
// ---------------------------------------------------------------------------

/// Transaction-management envelope wrapping every record or column-pointer entry.
///
/// Layout (24 bytes, little-endian):
///
/// | Offset | Size | Field          |
/// |-------:|-----:|----------------|
/// | 0      | 8    | `tx_inserted`  |
/// | 8      | 8    | `tx_deleted`   |
/// | 16     | 8    | `prev_version` |
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(C)]
pub struct RecordEnvelope {
    /// Id of the transaction that created this record version.
    pub tx_inserted: TxId,
    /// Id of the transaction that deleted this version, or [`TX_NULL`] if active.
    pub tx_deleted: TxId,
    /// Absolute physical address of the previous version, forming the MVCC chain.
    pub prev_version: PhysAddr,
}

impl RecordEnvelope {
    /// Serialized length of an envelope, in bytes.
    pub const ENCODED_LEN: usize = 24;

    /// Create an envelope for a freshly inserted, active record version.
    #[inline]
    pub const fn new(tx_inserted: TxId, prev_version: PhysAddr) -> Self {
        Self {
            tx_inserted,
            tx_deleted: TX_NULL,
            prev_version,
        }
    }

    /// Whether this version is currently live (not yet deleted by any transaction).
    #[inline]
    pub const fn is_active(&self) -> bool {
        self.tx_deleted == TX_NULL
    }

    /// Tombstone this version under transaction `tx`.
    #[inline]
    pub fn mark_deleted(&mut self, tx: TxId) {
        self.tx_deleted = tx;
    }

    /// Snapshot-isolation visibility test (spec §5).
    ///
    /// A version is visible to a snapshot taken at `target_tx` iff it was created
    /// at or before `target_tx` **and** it was either never deleted or deleted
    /// strictly after `target_tx`.
    #[inline]
    pub const fn is_visible(&self, target_tx: TxId) -> bool {
        let created_before_target = self.tx_inserted <= target_tx;
        let active_at_target = self.tx_deleted == TX_NULL || self.tx_deleted > target_tx;
        created_before_target && active_at_target
    }

    /// Encode to the fixed 24-byte little-endian wire form.
    pub fn encode(&self) -> [u8; Self::ENCODED_LEN] {
        let mut buf = [0u8; Self::ENCODED_LEN];
        buf[0..8].copy_from_slice(&self.tx_inserted.to_le_bytes());
        buf[8..16].copy_from_slice(&self.tx_deleted.to_le_bytes());
        buf[16..24].copy_from_slice(&self.prev_version.to_le_bytes());
        buf
    }

    /// Decode from a byte slice of at least [`Self::ENCODED_LEN`] bytes.
    pub fn decode(buf: &[u8]) -> Result<Self> {
        if buf.len() < Self::ENCODED_LEN {
            return Err(PvError::BufferTooSmall {
                needed: Self::ENCODED_LEN,
                actual: buf.len(),
            });
        }
        Ok(Self {
            tx_inserted: read_u64_le(buf, 0),
            tx_deleted: read_u64_le(buf, 8),
            prev_version: read_u64_le(buf, 16),
        })
    }
}

/// Free-function form of [`RecordEnvelope::is_visible`], matching the spec's
/// `is_record_visible(envelope, target_tx)` signature.
#[inline]
pub const fn is_record_visible(envelope: &RecordEnvelope, target_tx: TxId) -> bool {
    envelope.is_visible(target_tx)
}

// ---------------------------------------------------------------------------
// Page headers (spec §3)
// ---------------------------------------------------------------------------

/// Header of a **hot** slotted row-buffer page (`Page_Type = 0x01`).
///
/// On-disk layout within the fixed [`PAGE_HEADER_SIZE`]-byte header region:
///
/// | Offset | Size | Field                       |
/// |-------:|-----:|-----------------------------|
/// | 0      | 8    | `page_id` (u64)             |
/// | 8      | 1    | page type discriminant `0x01` |
/// | 9      | 2    | `slot_count` (u16)          |
/// | 11     | 2    | `free_space_ptr` (u16)      |
/// | 13     | 11   | reserved (zeroed)           |
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RowPageHeader {
    /// Identifier of this page.
    pub page_id: PageId,
    /// Number of occupied slots in the slot array.
    pub slot_count: u16,
    /// Boundary between free space and the upward-growing record storage array.
    pub free_space_ptr: u16,
}

impl RowPageHeader {
    /// The page-type discriminant for this header.
    pub const PAGE_TYPE: PageType = PageType::Row;

    /// Header for a brand-new, empty row page. `free_space_ptr` starts at the end
    /// of the page; record payloads grow downward from there.
    #[inline]
    pub const fn new(page_id: PageId) -> Self {
        Self {
            page_id,
            slot_count: 0,
            free_space_ptr: PAGE_SIZE as u16,
        }
    }

    /// Encode into the fixed [`PAGE_HEADER_SIZE`]-byte header.
    pub fn encode(&self) -> [u8; PAGE_HEADER_SIZE] {
        let mut buf = [0u8; PAGE_HEADER_SIZE];
        buf[0..8].copy_from_slice(&self.page_id.to_le_bytes());
        buf[8] = Self::PAGE_TYPE.as_byte();
        buf[9..11].copy_from_slice(&self.slot_count.to_le_bytes());
        buf[11..13].copy_from_slice(&self.free_space_ptr.to_le_bytes());
        // bytes 13..24 remain reserved / zero
        buf
    }

    /// Decode from the head of a page buffer, validating the type discriminant.
    pub fn decode(buf: &[u8]) -> Result<Self> {
        ensure_len(buf, PAGE_HEADER_SIZE)?;
        expect_page_type(buf[8], Self::PAGE_TYPE)?;
        Ok(Self {
            page_id: read_u64_le(buf, 0),
            slot_count: read_u16_le(buf, 9),
            free_space_ptr: read_u16_le(buf, 11),
        })
    }
}

/// Header of a **cold** packed columnar page (`Page_Type = 0x02`).
///
/// On-disk layout within the fixed [`PAGE_HEADER_SIZE`]-byte header region:
///
/// | Offset | Size | Field                       |
/// |-------:|-----:|-----------------------------|
/// | 0      | 8    | `page_id` (u64)             |
/// | 8      | 1    | page type discriminant `0x02` |
/// | 9      | 2    | `row_count` (u16)           |
/// | 11     | 13   | reserved (zeroed)           |
///
/// Note: the specification labels the reserved tail `u48` (6 bytes); a 6-byte
/// reserve would leave the header at 17 bytes, not the fixed 24. To honour the
/// 24-byte header invariant we reserve the full remaining 13 bytes. (`u48` is
/// also not a native Rust integer type.)
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ColumnarPageHeader {
    /// Identifier of this page.
    pub page_id: PageId,
    /// Number of logical rows encoded across the column arrays.
    pub row_count: u16,
}

impl ColumnarPageHeader {
    /// The page-type discriminant for this header.
    pub const PAGE_TYPE: PageType = PageType::Columnar;

    /// Header for an empty columnar page.
    #[inline]
    pub const fn new(page_id: PageId) -> Self {
        Self {
            page_id,
            row_count: 0,
        }
    }

    /// Encode into the fixed [`PAGE_HEADER_SIZE`]-byte header.
    pub fn encode(&self) -> [u8; PAGE_HEADER_SIZE] {
        let mut buf = [0u8; PAGE_HEADER_SIZE];
        buf[0..8].copy_from_slice(&self.page_id.to_le_bytes());
        buf[8] = Self::PAGE_TYPE.as_byte();
        buf[9..11].copy_from_slice(&self.row_count.to_le_bytes());
        // bytes 11..24 remain reserved / zero
        buf
    }

    /// Decode from the head of a page buffer, validating the type discriminant.
    pub fn decode(buf: &[u8]) -> Result<Self> {
        ensure_len(buf, PAGE_HEADER_SIZE)?;
        expect_page_type(buf[8], Self::PAGE_TYPE)?;
        Ok(Self {
            page_id: read_u64_le(buf, 0),
            row_count: read_u16_le(buf, 9),
        })
    }
}

// ---------------------------------------------------------------------------
// Monolithic `.pvdb` file header (spec §2.B)
// ---------------------------------------------------------------------------

/// The 20-byte header at the start of a baked monolithic `.pvdb` file.
///
/// | Offset | Size | Field             |
/// |-------:|-----:|-------------------|
/// | 0      | 4    | [`MAGIC_BYTES`]   |
/// | 4      | 8    | `manifest_offset` |
/// | 12     | 8    | `cas_offset`      |
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FileHeader {
    /// Absolute byte offset of the manifest (catalog) block.
    pub manifest_offset: u64,
    /// Absolute byte offset of the CAS blob pool.
    pub cas_offset: u64,
}

impl FileHeader {
    /// Serialized length, in bytes.
    pub const ENCODED_LEN: usize = FILE_HEADER_SIZE;

    /// Construct a header from its two offsets.
    #[inline]
    pub const fn new(manifest_offset: u64, cas_offset: u64) -> Self {
        Self {
            manifest_offset,
            cas_offset,
        }
    }

    /// Encode to the fixed 20-byte little-endian wire form, including magic bytes.
    pub fn encode(&self) -> [u8; FILE_HEADER_SIZE] {
        let mut buf = [0u8; FILE_HEADER_SIZE];
        buf[0..4].copy_from_slice(&MAGIC_BYTES);
        buf[4..12].copy_from_slice(&self.manifest_offset.to_le_bytes());
        buf[12..20].copy_from_slice(&self.cas_offset.to_le_bytes());
        buf
    }

    /// Decode and validate the magic signature.
    ///
    /// Returns [`PvError::SignatureMismatch`] if the leading bytes are not
    /// [`MAGIC_BYTES`].
    pub fn decode(buf: &[u8]) -> Result<Self> {
        ensure_len(buf, FILE_HEADER_SIZE)?;
        let found: [u8; 4] = [buf[0], buf[1], buf[2], buf[3]];
        if found != MAGIC_BYTES {
            return Err(PvError::SignatureMismatch {
                expected: MAGIC_BYTES,
                found,
            });
        }
        Ok(Self {
            manifest_offset: read_u64_le(buf, 4),
            cas_offset: read_u64_le(buf, 12),
        })
    }
}

// ---------------------------------------------------------------------------
// Little-endian read helpers (bounds are checked by callers via `ensure_len`)
// ---------------------------------------------------------------------------

#[inline]
fn read_u64_le(buf: &[u8], at: usize) -> u64 {
    let mut bytes = [0u8; 8];
    bytes.copy_from_slice(&buf[at..at + 8]);
    u64::from_le_bytes(bytes)
}

#[inline]
fn read_u16_le(buf: &[u8], at: usize) -> u16 {
    let mut bytes = [0u8; 2];
    bytes.copy_from_slice(&buf[at..at + 2]);
    u16::from_le_bytes(bytes)
}

#[inline]
fn ensure_len(buf: &[u8], needed: usize) -> Result<()> {
    if buf.len() < needed {
        Err(PvError::BufferTooSmall {
            needed,
            actual: buf.len(),
        })
    } else {
        Ok(())
    }
}

#[inline]
fn expect_page_type(byte: u8, want: PageType) -> Result<()> {
    let got = PageType::try_from(byte)?;
    if got == want {
        Ok(())
    } else {
        Err(PvError::InvalidPageType(byte))
    }
}

// ---------------------------------------------------------------------------
// Compile-time layout guarantees
// ---------------------------------------------------------------------------

const _: () = assert!(std::mem::size_of::<RecordEnvelope>() == RecordEnvelope::ENCODED_LEN);
const _: () = assert!(PAGE_HEADER_SIZE == 24);
const _: () = assert!(FILE_HEADER_SIZE == 20);
const _: () = assert!(CHUNK_CAP_BYTES == 64 * 1024 * 1024);
const _: () = assert!(PAGE_SIZE <= u16::MAX as usize + 1);

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn page_type_round_trips() {
        for ty in [PageType::Row, PageType::Columnar] {
            assert_eq!(PageType::try_from(ty.as_byte()).unwrap(), ty);
        }
        assert!(matches!(
            PageType::try_from(0xFF),
            Err(PvError::InvalidPageType(0xFF))
        ));
    }

    #[test]
    fn record_envelope_round_trips() {
        let env = RecordEnvelope {
            tx_inserted: 7,
            tx_deleted: 42,
            prev_version: 0xDEAD_BEEF,
        };
        let bytes = env.encode();
        assert_eq!(bytes.len(), RecordEnvelope::ENCODED_LEN);
        assert_eq!(RecordEnvelope::decode(&bytes).unwrap(), env);
    }

    #[test]
    fn record_envelope_decode_rejects_short_buffer() {
        let short = [0u8; RecordEnvelope::ENCODED_LEN - 1];
        assert!(matches!(
            RecordEnvelope::decode(&short),
            Err(PvError::BufferTooSmall {
                needed: 24,
                actual: 23
            })
        ));
    }

    #[test]
    fn visibility_matches_spec_truth_table() {
        // Inserted at tx=5, deleted at tx=10.
        let env = RecordEnvelope {
            tx_inserted: 5,
            tx_deleted: 10,
            prev_version: 0,
        };
        assert!(!env.is_visible(4)); // before insert  -> invisible
        assert!(env.is_visible(5)); // at insert       -> visible
        assert!(env.is_visible(9)); // after insert    -> visible
        assert!(!env.is_visible(10)); // at deletion    -> invisible
        assert!(!env.is_visible(11)); // after deletion -> invisible

        // Free function agrees with the method.
        assert_eq!(is_record_visible(&env, 7), env.is_visible(7));

        // An active (never-deleted) record is visible to any later snapshot.
        let active = RecordEnvelope::new(5, 0);
        assert!(active.is_active());
        assert!(active.is_visible(5));
        assert!(active.is_visible(u64::MAX));
        assert!(!active.is_visible(4));
    }

    #[test]
    fn row_header_round_trips() {
        let h = RowPageHeader {
            page_id: 0x0102_0304_0506_0708,
            slot_count: 3,
            free_space_ptr: 3850,
        };
        let bytes = h.encode();
        assert_eq!(bytes.len(), PAGE_HEADER_SIZE);
        assert_eq!(bytes[8], PageType::Row.as_byte());
        assert_eq!(RowPageHeader::decode(&bytes).unwrap(), h);
        // Reserved tail is zeroed.
        assert!(bytes[13..].iter().all(|&b| b == 0));
    }

    #[test]
    fn new_row_header_starts_empty_with_full_free_space() {
        let h = RowPageHeader::new(1);
        assert_eq!(h.slot_count, 0);
        assert_eq!(h.free_space_ptr as usize, PAGE_SIZE);
    }

    #[test]
    fn columnar_header_round_trips() {
        let h = ColumnarPageHeader {
            page_id: 99,
            row_count: 1000,
        };
        let bytes = h.encode();
        assert_eq!(bytes[8], PageType::Columnar.as_byte());
        assert_eq!(ColumnarPageHeader::decode(&bytes).unwrap(), h);
        assert!(bytes[11..].iter().all(|&b| b == 0));
    }

    #[test]
    fn header_decoders_reject_wrong_page_type() {
        let row = RowPageHeader::new(1).encode();
        assert!(ColumnarPageHeader::decode(&row).is_err());
        let col = ColumnarPageHeader::new(1).encode();
        assert!(RowPageHeader::decode(&col).is_err());
    }

    #[test]
    fn file_header_round_trips_and_carries_magic() {
        let h = FileHeader::new(0x1000, 0x2000_0000);
        let bytes = h.encode();
        assert_eq!(&bytes[0..4], &MAGIC_BYTES);
        assert_eq!(FileHeader::decode(&bytes).unwrap(), h);
    }

    #[test]
    fn file_header_decode_rejects_bad_magic() {
        let mut bytes = FileHeader::new(0, 0).encode();
        bytes[1] = 0x00; // corrupt the 'V'
        match FileHeader::decode(&bytes) {
            Err(PvError::SignatureMismatch { expected, found }) => {
                assert_eq!(expected, MAGIC_BYTES);
                assert_ne!(found, MAGIC_BYTES);
            }
            other => panic!("expected SignatureMismatch, got {other:?}"),
        }
    }
}
