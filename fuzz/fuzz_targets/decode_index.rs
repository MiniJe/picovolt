//! Fuzz the self-contained binary secondary-index decoder.
#![no_main]

use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    let _ = picovolt::storage::index::SecondaryIndex::decode_binary(data);
});
