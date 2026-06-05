//! Fuzz the production `.pvdb` open path: file header, JSON manifest, CAS
//! directory, page-chain traversal, and record decoding.
//!
//! Run (Linux + nightly): `cargo +nightly fuzz run decode_monolith`
#![no_main]

use libfuzzer_sys::fuzz_target;
use std::io::Write;

fuzz_target!(|data: &[u8]| {
    // `open_prod` mmaps a file, so materialize the input as a temp `.pvdb`.
    if let Ok(mut file) = tempfile::NamedTempFile::new() {
        if file.write_all(data).is_ok() && file.flush().is_ok() {
            // Must never panic — only return Ok/Err — on arbitrary bytes.
            let _ = picovolt::Database::open_prod(file.path());
        }
    }
});
