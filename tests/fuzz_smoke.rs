//! Lightweight "fuzz-lite": throw many deterministic pseudo-random and
//! bit-flipped byte strings at the in-memory decoders and assert none panic
//! (returning `Err` is fine; *panicking* on malformed input is the bug).
//!
//! This runs on every platform via `cargo test`. The deeper, coverage-guided
//! fuzzing lives in the `fuzz/` cargo-fuzz crate (Linux + nightly).

use picovolt::storage::cas::CasStore;
use picovolt::storage::page::ColumnarPage;
use picovolt::storage::record::decode_row;
use picovolt::{Interpreter, Value};

/// A tiny deterministic LCG — no external rand dependency, reproducible runs.
struct Lcg(u64);

impl Lcg {
    fn next(&mut self) -> u64 {
        self.0 = self
            .0
            .wrapping_mul(6_364_136_223_846_793_005)
            .wrapping_add(1_442_695_040_888_963_407);
        self.0
    }

    fn byte(&mut self) -> u8 {
        (self.next() >> 33) as u8
    }

    fn below(&mut self, n: usize) -> usize {
        if n == 0 {
            0
        } else {
            (self.next() as usize) % n
        }
    }

    fn garbage(&mut self, max: usize) -> Vec<u8> {
        let len = self.below(max + 1);
        (0..len).map(|_| self.byte()).collect()
    }

    fn mutate(&mut self, base: &[u8]) -> Vec<u8> {
        let mut v = base.to_vec();
        if !v.is_empty() {
            for _ in 0..1 + self.below(4) {
                let i = self.below(v.len());
                v[i] ^= self.byte();
            }
        }
        v
    }
}

#[test]
fn decoders_never_panic_on_garbage_or_mutation() {
    let cas = CasStore::new_memory();
    let mut rng = Lcg(0x0123_4567_89ab_cdef);

    // Valid seeds — bit-flipping these reaches far deeper parser states than
    // pure random bytes (which rarely form a valid header).
    let columnar = ColumnarPage::from_rows(
        0,
        &[
            vec![Value::Int(1), Value::from("a")],
            vec![Value::Int(1_000_000), Value::from("b")],
        ],
    )
    .unwrap();
    let wasm = wat::parse_str(
        r#"(module (memory (export "memory") 1)
            (func (export "f") (param i32 i32) (result i32) (local.get 0)))"#,
    )
    .unwrap();

    for _ in 0..10_000 {
        // Pure garbage.
        let g = rng.garbage(96);
        let _ = ColumnarPage::to_rows(&g);
        let _ = decode_row(&g, &cas);
        let _ = Interpreter::new().load(&g);

        // Bit-flipped valid inputs.
        let _ = ColumnarPage::to_rows(&rng.mutate(&columnar));
        let _ = decode_row(&rng.mutate(&columnar), &cas);
        let _ = Interpreter::new().load(&rng.mutate(&wasm));
    }
}
