//! Core foundations: primitive types, fixed byte layouts, and the error taxonomy.
//!
//! This module is intentionally free of I/O and engine logic. It defines the
//! vocabulary (constants, identifiers, [`types::RecordEnvelope`], page headers)
//! and the error surface ([`errors::PvError`]) that every other layer builds on.

pub mod errors;
pub mod types;
pub mod value;
