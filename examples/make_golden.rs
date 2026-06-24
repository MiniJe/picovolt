//! Regenerates the committed golden `.pvdb` fixture exercised by the
//! format-stability test in `tests/format_robustness.rs`.
//!
//! Run this **only** when the on-disk format is intentionally changed (and bump
//! the fixture name / `FORMAT_VERSION` alongside it):
//!
//! ```sh
//! cargo run --example make_golden
//! ```
//!
//! The fixture is deterministic — baking the same dataset twice yields identical
//! bytes — so it doubles as a guard that an accidental format change does not
//! slip through unnoticed.

use picovolt::core::value::Value;
use picovolt::Database;

fn main() {
    let dir = std::path::Path::new("tests/fixtures");
    std::fs::create_dir_all(dir).unwrap();
    let ws = dir.join("_golden_ws");
    let _ = std::fs::remove_dir_all(&ws);

    let mut db = Database::open_dev(&ws).unwrap();
    db.query("CREATE TABLE users (id, name, city)").unwrap();
    db.query("INSERT INTO users VALUES (1, 'alice', 'paris')")
        .unwrap();
    db.query("INSERT INTO users VALUES (2, 'bob', 'berlin')")
        .unwrap();
    db.query("INSERT INTO users VALUES (3, 'carol', 'cairo')")
        .unwrap();
    // An UPDATE leaves prior versions behind, so the file carries MVCC history.
    db.query("UPDATE users SET city = 'london' WHERE id = 1")
        .unwrap();

    // A long value, to exercise CAS interning of large payloads in a baked file.
    db.query("CREATE TABLE notes (id, body)").unwrap();
    db.query_with(
        "INSERT INTO notes VALUES (?, ?)",
        &[Value::Int(1), Value::Text("x".repeat(500))],
    )
    .unwrap();

    let out = dir.join("golden_v0_11_0.pvdb");
    db.bake(&out).unwrap();
    let _ = std::fs::remove_dir_all(&ws);

    let size = std::fs::metadata(&out).unwrap().len();
    println!("wrote {} ({size} bytes)", out.display());
}
