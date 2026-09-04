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

    // A version-3 golden carrying persisted schema constraints.
    let ws3 = dir.join("_golden_ws3");
    let _ = std::fs::remove_dir_all(&ws3);
    let mut db3 = Database::open_dev(&ws3).unwrap();
    db3.query("CREATE TABLE accounts (id PRIMARY KEY, email UNIQUE, name NOT NULL)")
        .unwrap();
    db3.query("INSERT INTO accounts VALUES (1, 'ada@example.com', 'Ada')")
        .unwrap();
    let out3 = dir.join("golden_v1_4_0.pvdb");
    db3.bake(&out3).unwrap();
    let _ = std::fs::remove_dir_all(&ws3);
    let size3 = std::fs::metadata(&out3).unwrap().len();
    println!("wrote {} ({size3} bytes)", out3.display());

    // A version-4 golden carrying literal defaults and a CHECK constraint.
    let ws4 = dir.join("_golden_ws4");
    let _ = std::fs::remove_dir_all(&ws4);
    let mut db4 = Database::open_dev(&ws4).unwrap();
    db4.query(
        "CREATE TABLE jobs (id INTEGER PRIMARY KEY, state TEXT DEFAULT 'queued', \
         attempts INTEGER DEFAULT 0 CHECK (attempts >= 0))",
    )
    .unwrap();
    db4.query("INSERT INTO jobs (id) VALUES (1)").unwrap();
    let out4 = dir.join("golden_v1_7_0.pvdb");
    db4.bake(&out4).unwrap();
    let _ = std::fs::remove_dir_all(&ws4);
    let size4 = std::fs::metadata(&out4).unwrap().len();
    println!("wrote {} ({size4} bytes)", out4.display());

    // A version-5 golden with an integrated MVCC cold page and packed decimal
    // column. The tail stays row-slotted so the imported image remains writable.
    let ws5 = dir.join("_golden_ws5");
    let _ = std::fs::remove_dir_all(&ws5);
    let mut db5 = Database::open_dev(&ws5).unwrap();
    db5.query("CREATE TABLE ledger (id PRIMARY KEY, state, amount)")
        .unwrap();
    for id in 0..240i64 {
        db5.insert(
            "ledger",
            vec![
                Value::Int(id),
                Value::Text(if id % 2 == 0 { "open" } else { "closed" }.into()),
                Value::Decimal((id % 7) as i128 * 1_000_000),
            ],
        )
        .unwrap();
    }
    let compacted = db5.compact_step(64).unwrap();
    assert!(compacted.compacted_pages > 0);
    let out5 = dir.join("golden_v1_9_0.pvdb");
    db5.bake(&out5).unwrap();
    let _ = std::fs::remove_dir_all(&ws5);
    let size5 = std::fs::metadata(&out5).unwrap().len();
    println!("wrote {} ({size5} bytes)", out5.display());
}
