//! Fuzz the bounded hand-written SQL parser.
#![no_main]

use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    if let Ok(sql) = std::str::from_utf8(data) {
        let _ = picovolt::engine::query::parse(sql);
    }
});
