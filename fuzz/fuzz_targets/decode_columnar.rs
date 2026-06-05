//! Fuzz the columnar page decoder (`ColumnarPage::to_rows`): delta-Z, dictionary
//! bit-packing, and the raw value fallback.
//!
//! Run (Linux + nightly): `cargo +nightly fuzz run decode_columnar`
#![no_main]

use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    let _ = picovolt::storage::page::ColumnarPage::to_rows(data);
});
