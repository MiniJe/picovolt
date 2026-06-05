//! Fuzz the hand-written `pv-wasm` binary decoder (`Interpreter::load`).
//!
//! Run (Linux + nightly): `cargo +nightly fuzz run decode_wasm`
#![no_main]

use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    // Decoding arbitrary bytes must never panic or allocate unboundedly.
    let _ = picovolt::Interpreter::new().load(data);
});
