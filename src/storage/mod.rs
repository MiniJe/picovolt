//! Storage layer: page structures, content-addressable storage, compression
//! primitives, record serialization, and the Virtualization Layer Engine.

pub mod cache;
pub mod cas;
pub mod compress;
pub mod index;
pub mod page;
pub mod record;
pub mod vle;
