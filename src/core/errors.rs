//! Error taxonomy for the PicoVolt engine.
//!
//! [`PvError`] is the single crate-wide error type. Variants are grouped by the
//! layer that raises them: storage/page faults, on-disk signature & corruption
//! checks, content-addressable storage misses, the WASM runtime, and the
//! licensing compliance hook. [`ComplianceError`] is kept as its own type so the
//! compliance module (Phase 4) can expose the exact `Result<(), ComplianceError>`
//! surface described in the specification while still composing into [`PvError`]
//! via `?`.

use thiserror::Error;

/// Convenience alias used throughout the crate.
pub type Result<T> = std::result::Result<T, PvError>;

/// The unified error type for every fallible PicoVolt operation.
#[derive(Debug, Error)]
#[non_exhaustive]
pub enum PvError {
    /// An underlying OS / file-system I/O failure.
    #[error("i/o error: {0}")]
    Io(#[from] std::io::Error),

    /// A file did not begin with the expected `PVDB` magic bytes, or otherwise
    /// failed signature validation. Raised when opening a baked `.pvdb` monolith.
    #[error("invalid signature: expected magic {expected:02X?}, found {found:02X?}")]
    SignatureMismatch {
        /// The magic bytes PicoVolt requires.
        expected: [u8; 4],
        /// The bytes actually read from the file head.
        found: [u8; 4],
    },

    /// Structural corruption detected while parsing on-disk bytes (bad offsets,
    /// truncated records, checksum failure, …). The string carries context.
    #[error("data corruption: {0}")]
    Corruption(String),

    /// A requested page is not resident / could not be mapped. The storage-layer
    /// analogue of a hardware page fault.
    #[error("page fault: page {page_id} is not resident")]
    PageFault {
        /// Identifier of the page that faulted.
        page_id: u64,
    },

    /// A byte slice handed to a decoder was shorter than the structure requires.
    #[error("buffer too small: needed {needed} bytes, got {actual}")]
    BufferTooSmall {
        /// Minimum number of bytes the decode operation needs.
        needed: usize,
        /// Number of bytes actually supplied.
        actual: usize,
    },

    /// An access targeted a byte offset outside the bounds of a page/buffer.
    #[error("offset {offset} out of bounds for region of {size} bytes")]
    OutOfBounds {
        /// The offending offset.
        offset: usize,
        /// Size of the region being indexed.
        size: usize,
    },

    /// A page-type discriminant byte did not match any known [`crate::core::types::PageType`].
    #[error("invalid page type discriminant: 0x{0:02X}")]
    InvalidPageType(u8),

    /// A page cannot satisfy an allocation request because insufficient free
    /// space remains between the slot array and the record storage array.
    #[error("page full: requested {needed} bytes, only {available} available")]
    PageFull {
        /// Bytes requested by the insert.
        needed: usize,
        /// Bytes currently free in the page.
        available: usize,
    },

    /// A CAS pointer referenced a blob that is absent from the blob pool.
    #[error("content-addressable storage miss for hash {0}")]
    CasMiss(String),

    /// An error originating inside the sandboxed WASM guest runtime.
    #[error("wasm runtime error: {0}")]
    Wasm(String),

    /// A mutation was attempted against a read-only (production / mmap) database.
    #[error("database is read-only (production mode); mutations are not permitted")]
    ReadOnly,

    /// A referenced table does not exist in the catalog.
    #[error("table not found: {0}")]
    TableNotFound(String),

    /// A row did not match the table's declared arity, or types were invalid.
    #[error("schema error: {0}")]
    Schema(String),

    /// A query string could not be parsed.
    #[error("query parse error: {0}")]
    Query(String),

    /// Failure (de)serializing the JSON manifest.
    #[error("manifest error: {0}")]
    Manifest(#[from] serde_json::Error),

    /// The dual-license compliance hook rejected the current runtime metrics.
    #[error(transparent)]
    Compliance(#[from] ComplianceError),
}

/// Errors raised by the optional, application-driven usage-policy hook
/// ([`crate::engine::compliance`]).
///
/// This is **not** a license requirement of PicoVolt itself (the engine is
/// Apache-2.0). It is an opt-in utility an application can call to enforce its
/// *own* business rules (e.g. a self-imposed free-tier cap). It is local-only
/// and performs no network calls.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Error)]
#[non_exhaustive]
pub enum ComplianceError {
    /// A configured usage threshold was exceeded without an authorizing key.
    #[error("usage threshold exceeded without an authorizing key")]
    ThresholdExceeded,
}
