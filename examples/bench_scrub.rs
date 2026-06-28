//! Micro-benchmark for the "Rewind" scrub query, to see where the time goes when
//! time-travelling a top-N query over the 213k-row sample.
//!
//! Run: `cargo run --release --example bench_scrub`
//! (expects the dataset at ../picovolt-rewind/rewind.pvdb)

use std::time::Instant;

use picovolt::Database;

fn avg_ms(db: &mut Database, sql_for: impl Fn(u64) -> String, iters: u32) -> f64 {
    // Warm up (build any caches, fault pages in).
    db.query(&sql_for(123456)).unwrap();
    let start = Instant::now();
    for i in 0..iters {
        let tx = 5_000 + ((i as u64) * 7919) % 205_000; // vary the time-travel point
        db.query(&sql_for(tx)).unwrap();
    }
    start.elapsed().as_secs_f64() * 1000.0 / iters as f64
}

fn main() {
    let bytes = std::fs::read("../picovolt-rewind/rewind.pvdb").expect("read rewind.pvdb");
    let mut db = Database::import_bytes(&bytes).expect("import");
    let total = db.query("SELECT COUNT(*) FROM crates").unwrap();
    println!("dataset: {total:?}");

    let iters = 40;
    let scrub = avg_ms(
        &mut db,
        |tx| {
            format!("SELECT name, category, downloads FROM crates BEFORE {tx} ORDER BY downloads DESC LIMIT 50")
        },
        iters,
    );
    let scan_only = avg_ms(
        &mut db,
        |tx| format!("SELECT downloads FROM crates BEFORE {tx}"),
        iters,
    );
    let count = avg_ms(
        &mut db,
        |tx| format!("SELECT COUNT(*) FROM crates BEFORE {tx}"),
        iters,
    );

    // With a secondary index on `downloads`, the top-N scrub walks the ordered
    // index and stops at the limit instead of scanning + selecting every row.
    db.query("CREATE INDEX ON crates (downloads)").unwrap();
    let indexed = avg_ms(
        &mut db,
        |tx| {
            format!("SELECT name, category, downloads FROM crates BEFORE {tx} ORDER BY downloads DESC LIMIT 50")
        },
        iters,
    );

    println!("scrub (ORDER BY downloads DESC LIMIT 50): {scrub:.2} ms/query");
    println!("scan+materialize (SELECT downloads, no sort): {scan_only:.2} ms/query");
    println!("count (COUNT(*)): {count:.2} ms/query");
    println!("=> approx sort cost: {:.2} ms/query", scrub - scan_only);
    println!("scrub WITH index on downloads: {indexed:.2} ms/query");
}
