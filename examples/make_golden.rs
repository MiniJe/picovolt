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

    // A version-2 golden: a table carrying a secondary index, so the baked file
    // exercises the binary index region (format §6.1) and stays at version 2.
    let ws2 = dir.join("_golden_ws2");
    let _ = std::fs::remove_dir_all(&ws2);
    let mut db2 = Database::open_dev(&ws2).unwrap();
    db2.query("CREATE TABLE crates (id, name, downloads)")
        .unwrap();
    db2.query("CREATE INDEX ON crates (downloads)").unwrap();
    for (id, name, dl) in [
        (1, "serde", 90_000),
        (2, "tokio", 80_000),
        (3, "rand", 70_000),
        (4, "clap", 60_000),
        (5, "log", 50_000),
    ] {
        db2.query(&format!("INSERT INTO crates VALUES ({id}, '{name}', {dl})"))
            .unwrap();
    }
    // An UPDATE leaves MVCC history behind so the fixture also covers time-travel.
    db2.query("UPDATE crates SET downloads = 95000 WHERE id = 1")
        .unwrap();
    let out2 = dir.join("golden_v1_3_0.pvdb");
    db2.bake(&out2).unwrap();
    let _ = std::fs::remove_dir_all(&ws2);

    let size2 = std::fs::metadata(&out2).unwrap().len();
    println!("wrote {} ({size2} bytes)", out2.display());
}
