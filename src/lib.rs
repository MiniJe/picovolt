//! # PicoVolt (PVDB)
//!
//! A polymorphic embedded data engine. PicoVolt decouples logic from storage
//! representation through a Virtualization Layer Engine (VLE), shifting between a
//! directory of mutable append-only files (Development Mode) and an immutable,
//! memory-mappable single-file binary (`.pvdb`, Production Mode).
//!
//! ## Layout
//!
//! | Layer | Modules | Responsibility |
//! |-------|---------|----------------|
//! | core | [`core::types`], [`core::errors`], [`core::value`] | byte layouts, errors, value model |
//! | storage | [`storage::page`], [`storage::cas`], [`storage::compress`], [`storage::record`], [`storage::vle`] | pages, dedup, compression, serialization, file router |
//! | engine | [`engine::mvcc`], [`engine::wasm`], [`engine::query`], [`engine::compliance`] | snapshots, sandbox, SQL, licensing |
//! | surface | [`Database`] | dev/prod lifecycle, `query`, `bake` |
//!
//! ## Quick start
//!
//! Low-level layouts round-trip through explicit little-endian encoders:
//!
//! ```
//! use picovolt::{RecordEnvelope, RowPageHeader, FileHeader, MAGIC_BYTES};
//!
//! let env = RecordEnvelope::new(/* tx_inserted */ 1, /* prev_version */ 0);
//! assert!(env.is_active());
//! assert_eq!(RecordEnvelope::decode(&env.encode()).unwrap(), env);
//!
//! let page = RowPageHeader::new(0);
//! assert_eq!(RowPageHeader::decode(&page.encode()).unwrap(), page);
//!
//! let file = FileHeader::new(0x1000, 0x2000);
//! assert_eq!(&file.encode()[0..4], &MAGIC_BYTES);
//! ```
//!
//! The high-level surface opens a workspace, runs SQL (including `BEFORE tx`
//! time-travel), and bakes a production monolith:
//!
//! ```no_run
//! use picovolt::{Database, QueryResult};
//!
//! let mut db = Database::open_dev("./demo.pv")?;
//! db.query("CREATE TABLE users (id, name)")?;
//! db.query("INSERT INTO users VALUES (1, 'alice')")?;
//!
//! if let QueryResult::Rows { rows, .. } = db.query("SELECT * FROM users")? {
//!     assert_eq!(rows.len(), 1);
//! }
//!
//! db.bake("./app.pvdb")?;                  // compile to a single mmap'able file
//! let mut prod = Database::open_prod("./app.pvdb")?; // read-only
//! # Ok::<(), picovolt::PvError>(())
//! ```

pub mod core;
pub mod engine;
pub mod storage;

mod db;

#[doc(inline)]
pub use crate::core::errors::{ComplianceError, PvError, Result};
#[doc(inline)]
pub use crate::core::types::*;
#[doc(inline)]
pub use crate::core::value::{Row, Value};
#[doc(inline)]
pub use crate::db::{pv_bake, pv_open_dev, pv_open_prod, Database, QueryResult, MANIFEST_FILE};
#[doc(inline)]
pub use crate::engine::compliance::{ComplianceMonitor, RuntimeMetrics};
#[doc(inline)]
pub use crate::engine::interp::{Interpreter, PvModule};
#[doc(inline)]
pub use crate::engine::mvcc::{Snapshot, TxManager};
#[doc(inline)]
pub use crate::engine::wasm::{WasmExec, WasmModule, WasmRuntime};
