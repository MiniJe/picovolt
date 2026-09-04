//! Fuzz tagged row-record bodies, including fixed-width decimal and CAS tags.
#![no_main]

use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    let cas = picovolt::storage::cas::CasStore::new_memory();
    let _ = picovolt::storage::record::decode_row(data, &cas);
});
