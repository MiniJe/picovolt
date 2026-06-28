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

/// A tiny deterministic LCG, no external rand dependency, reproducible runs.
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

    // Valid seeds, bit-flipping these reaches far deeper parser states than
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

#[test]
fn importing_a_mutated_indexed_image_never_panics() {
    use picovolt::Database;
    let mut rng = Lcg(0x00c0_ffee_1234_5678);

    // A valid version-2 image: a table carrying a binary secondary-index region.
    let mut db = Database::open_memory();
    db.query("CREATE TABLE t (id, v)").unwrap();
    db.query("CREATE INDEX ON t (v)").unwrap();
    for i in 0..60u64 {
        db.query(&format!("INSERT INTO t VALUES ({i}, {})", i % 7))
            .unwrap();
    }
    let image = db.bake_to_bytes().unwrap();

    for _ in 0..4000 {
        // Bit-flipped v2 image: mutations that keep the header valid but corrupt the
        // region or its descriptors drive slice_index_region + decode_binary on
        // hostile bytes. Must return Err (or open), never panic / read out of bounds.
        let _ = Database::import_bytes(&rng.mutate(&image));
        let _ = Database::import_bytes(&rng.garbage(image.len().min(512)));
    }
}

#[test]
fn sql_parser_never_panics_on_garbage_or_mutation() {
    use picovolt::Database;
    let mut rng = Lcg(0xfeed_face_dead_beef);

    // Valid seeds spanning the 0.12.0 SQL surface; bit-flipping these reaches deep
    // parser states (mid-clause, mid-predicate) that random ASCII rarely forms.
    let seeds = [
        "SELECT DISTINCT id AS uid FROM t WHERE x IN (1,2) AND y BETWEEN 3 AND 9 ORDER BY a ASC, b DESC LIMIT 5",
        "SELECT city, COUNT(*) AS n FROM t GROUP BY city HAVING AVG(score) > 1 OR SUM(x) <= 10",
        "SELECT * FROM t WHERE name NOT LIKE 'a%' AND age IS NOT NULL AND z NOT IN (1, NULL)",
        "INSERT INTO t VALUES (1, 'a', 2.50, 3, 4, 5, 6, 'c', 7, 8, 9)",
        "UPDATE t SET v = 3 WHERE id = 1",
    ];

    let mut db = Database::open_memory();
    let _ = db.query("CREATE TABLE t (id, name, score, x, y, a, b, city, age, z, v)");

    for _ in 0..3000 {
        // Printable-ASCII garbage.
        let g: String = (0..rng.below(40))
            .map(|_| (0x20 + (rng.byte() % 0x5f)) as char)
            .collect();
        let _ = db.query(&g); // must return Err, never panic

        // Bit-flipped valid SQL (only when the mutation stays valid UTF-8).
        for seed in seeds {
            let mutated = rng.mutate(seed.as_bytes());
            if let Ok(s) = std::str::from_utf8(&mutated) {
                let _ = db.query(s);
            }
        }
    }
}
