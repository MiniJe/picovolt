//! Evaluation harness: exercises PicoVolt across several "environments" —
//! persistence modes, read workloads, dedup, dev-vs-prod, columnar compression,
//! and the SQL-vs-API path — and prints measured numbers.
//!
//! Run with: `cargo run --release --example bench`
//!
//! These are wall-clock measurements on the host machine; treat them as ballpark
//! and focus on the *relative* behavior across modes, not absolute figures.

use picovolt::storage::page::ColumnarPage;
use picovolt::{Database, Value};
use std::error::Error;
use std::hint::black_box;
use std::path::Path;
use std::time::Instant;

fn main() -> Result<(), Box<dyn Error>> {
    let build = if cfg!(debug_assertions) {
        "debug  <-- re-run with --release for meaningful numbers!"
    } else {
        "release"
    };
    println!("PicoVolt evaluation harness  (build profile: {build})");

    insert_modes()?;
    read_throughput()?;
    cas_dedup()?;
    dev_vs_prod()?;
    columnar_compression()?;
    sql_vs_api()?;
    larger_than_ram()?;
    indexed_lookup()?;

    println!("\nAll scenarios complete.");
    Ok(())
}

// 1. Insert throughput under the three persistence strategies.
fn insert_modes() -> Result<(), Box<dyn Error>> {
    section("1. Insert throughput by persistence mode");

    {
        let tmp = tempfile::tempdir()?;
        let mut db = Database::open_dev(tmp.path())?;
        db.set_autocommit(false);
        db.query("CREATE TABLE t (a, b)")?;
        let n = 100_000;
        let (_, secs) = timed(|| {
            for i in 0..n as i64 {
                db.insert("t", vec![Value::Int(i), Value::Int(i * 2)])
                    .unwrap();
            }
        });
        rate_row("in-memory append (no flush)", n, secs, None);
    }

    {
        let tmp = tempfile::tempdir()?;
        let mut db = Database::open_dev(tmp.path())?;
        db.set_autocommit(false);
        db.query("CREATE TABLE t (a, b)")?;
        let n = 20_000;
        let (_, ins) = timed(|| {
            for i in 0..n as i64 {
                db.insert("t", vec![Value::Int(i), Value::Int(i * 2)])
                    .unwrap();
            }
        });
        let (_, flush) = timed(|| db.flush_now().unwrap());
        rate_row(
            "batched (insert + one flush)",
            n,
            ins + flush,
            Some(format!("flush {:.0} ms", flush * 1000.0)),
        );
    }

    // Autocommit is now durable-per-insert *and* linear: 4x the rows should take
    // ~4x the time (it used to be ~16x, quadratic).
    let mut prev: Option<(usize, f64)> = None;
    for n in [2_000usize, 8_000] {
        let tmp = tempfile::tempdir()?;
        let mut db = Database::open_dev(tmp.path())?; // autocommit on by default
        db.query("CREATE TABLE t (a, b)")?;
        let (_, secs) = timed(|| {
            for i in 0..n as i64 {
                db.insert("t", vec![Value::Int(i), Value::Int(i * 2)])
                    .unwrap();
            }
        });
        let note = prev.map(|(pn, ps)| {
            format!(
                "{:.1}x time for {:.0}x rows (linear)",
                secs / ps,
                n as f64 / pn as f64
            )
        });
        rate_row("autocommit (durable per insert)", n, secs, note);
        prev = Some((n, secs));
    }
    Ok(())
}

// 2. Read throughput: full scan and time-travel scan (both in-memory post-load).
fn read_throughput() -> Result<(), Box<dyn Error>> {
    section("2. Read throughput (full scan, in-memory)");
    let tmp = tempfile::tempdir()?;
    let mut db = Database::open_dev(tmp.path())?;
    db.set_autocommit(false);
    db.query("CREATE TABLE t (a, b)")?;
    let n = 20_000;
    for i in 0..n as i64 {
        db.insert("t", vec![Value::Int(i), Value::Int(i)])?;
    }
    db.flush_now()?;

    let iters = 100;
    let (_, secs) = timed(|| {
        let mut sink = 0usize;
        for _ in 0..iters {
            sink += db.select("t", None).unwrap().1.len();
        }
        black_box(sink);
    });
    rate_row(
        "latest snapshot scan",
        n * iters,
        secs,
        Some(format!("{iters} scans")),
    );

    let mid = (db.current_tx() / 2).max(1);
    let (_, secs2) = timed(|| {
        let mut sink = 0usize;
        for _ in 0..iters {
            sink += db.select("t", Some(mid)).unwrap().1.len();
        }
        black_box(sink);
    });
    rate_row(
        &format!("time-travel scan (BEFORE {mid})"),
        n * iters,
        secs2,
        None,
    );
    Ok(())
}

// 3. Content-addressable dedup of repeated large payloads.
fn cas_dedup() -> Result<(), Box<dyn Error>> {
    section("3. CAS dedup (5,000 rows, ~1 KiB bodies, 10 distinct)");
    let tmp = tempfile::tempdir()?;
    let ws = tmp.path().join("ws");
    let baked = tmp.path().join("db.pvdb");

    let mut db = Database::open_dev(&ws)?;
    db.set_autocommit(false);
    db.query("CREATE TABLE docs (id, body)")?;
    let distinct = 10usize;
    let n = 5_000usize;
    let payloads: Vec<String> = (0..distinct)
        .map(|k| format!("body{k}").repeat(125))
        .collect();
    let body_len = payloads[0].len();
    for i in 0..n {
        db.insert(
            "docs",
            vec![
                Value::Int(i as i64),
                Value::from(payloads[i % distinct].clone()),
            ],
        )?;
    }
    db.flush_now()?;
    db.bake(&baked)?;

    let naive = (n * body_len) as u64;
    let dev = dir_size(&ws);
    let baked_sz = std::fs::metadata(&baked)?.len();
    println!(
        "  distinct bodies:          {distinct} of {n} rows ({} each)",
        human(body_len as u64)
    );
    println!("  naive (store every body): {}", human(naive));
    println!("  dev workspace on disk:    {}", human(dev));
    println!(
        "  baked .pvdb on disk:      {}   ({:.0}x smaller than naive)",
        human(baked_sz),
        naive as f64 / baked_sz as f64
    );
    Ok(())
}

// 4. Dev vs Prod: open cost and on-disk footprint.
fn dev_vs_prod() -> Result<(), Box<dyn Error>> {
    section("4. Dev vs Prod open cost & footprint (20,000 rows)");
    let tmp = tempfile::tempdir()?;
    let ws = tmp.path().join("ws");
    let baked = tmp.path().join("db.pvdb");

    {
        let mut db = Database::open_dev(&ws)?;
        db.set_autocommit(false);
        db.query("CREATE TABLE t (a, b, c)")?;
        for i in 0..20_000i64 {
            db.insert(
                "t",
                vec![
                    Value::Int(i),
                    Value::from(format!("row-{i}")),
                    Value::Int(i * 7),
                ],
            )?;
        }
        db.flush_now()?;
        let (_, bake) = timed(|| db.bake(&baked).unwrap());
        println!("  bake (compile monolith):  {:.1} ms", bake * 1000.0);
    }

    let (dev, dsec) = timed(|| Database::open_dev(&ws).unwrap());
    let (prod, psec) = timed(|| Database::open_prod(&baked).unwrap());
    println!("  open dev (read chunks):   {:.1} ms", dsec * 1000.0);
    println!("  open prod (mmap+decode):  {:.1} ms", psec * 1000.0);
    println!("  dev workspace footprint:  {}", human(dir_size(&ws)));
    println!(
        "  baked .pvdb footprint:    {}",
        human(std::fs::metadata(&baked)?.len())
    );

    let (_, dr) = timed(|| {
        for _ in 0..50 {
            black_box(dev.select("t", None).unwrap().1.len());
        }
    });
    let (_, pr) = timed(|| {
        for _ in 0..50 {
            black_box(prod.select("t", None).unwrap().1.len());
        }
    });
    println!("  50x scan dev:             {:.1} ms", dr * 1000.0);
    println!(
        "  50x scan prod:            {:.1} ms   (parity expected: both in-memory post-open)",
        pr * 1000.0
    );
    Ok(())
}

// 5. Columnar transposition + compression on a friendly dataset.
fn columnar_compression() -> Result<(), Box<dyn Error>> {
    section("5. Columnar transposition + compression (2,000 rows)");
    let n = 2_000;
    let statuses = ["Pending", "Active", "Archived"];
    let rows: Vec<Vec<Value>> = (0..n)
        .map(|i| {
            vec![
                Value::Int(1_700_000_000 + i as i64), // monotonic timestamp -> Delta-Z
                Value::from(statuses[i % 3]),         // low cardinality   -> dictionary
                Value::Int(i as i64),                 // sequential id     -> Delta-Z
            ]
        })
        .collect();

    let columnar = ColumnarPage::from_rows(0, &rows)?;
    let naive: usize = rows
        .iter()
        .map(|r| r.iter().map(value_size).sum::<usize>())
        .sum();
    println!("  rows:                     {n}");
    println!("  naive row encoding:       {}", human(naive as u64));
    println!(
        "  columnar page bytes:      {}   ({:.0}x smaller)",
        human(columnar.len() as u64),
        naive as f64 / columnar.len() as f64
    );
    Ok(())
}

// 6. SQL parsing overhead vs. the programmatic API.
fn sql_vs_api() -> Result<(), Box<dyn Error>> {
    section("6. SQL parser overhead vs programmatic API (5,000 inserts)");
    let n = 5_000;

    {
        let tmp = tempfile::tempdir()?;
        let mut db = Database::open_dev(tmp.path())?;
        db.set_autocommit(false);
        db.query("CREATE TABLE t (a, b)")?;
        let (_, s) = timed(|| {
            for i in 0..n as i64 {
                db.insert("t", vec![Value::Int(i), Value::Int(i)]).unwrap();
            }
        });
        rate_row("programmatic insert()", n, s, None);
    }
    {
        let tmp = tempfile::tempdir()?;
        let mut db = Database::open_dev(tmp.path())?;
        db.set_autocommit(false);
        db.query("CREATE TABLE t (a, b)")?;
        let (_, s) = timed(|| {
            for i in 0..n as i64 {
                db.query(&format!("INSERT INTO t VALUES ({i}, {i})"))
                    .unwrap();
            }
        });
        rate_row(
            "query() SQL insert",
            n,
            s,
            Some("includes tokenize + parse".into()),
        );
    }
    Ok(())
}

// 7. Serve a dataset much larger than the buffer pool.
fn larger_than_ram() -> Result<(), Box<dyn Error>> {
    section("7. Larger-than-RAM: serving with a tiny buffer pool");
    let tmp = tempfile::tempdir()?;
    let ws = tmp.path().join("ws");
    let baked = tmp.path().join("db.pvdb");
    let n = 50_000usize;
    {
        let mut db = Database::open_dev(&ws)?;
        db.set_autocommit(false);
        db.query("CREATE TABLE t (id, payload)")?;
        for i in 0..n as i64 {
            db.insert("t", vec![Value::Int(i), Value::from(format!("row-{i:08}"))])?;
        }
        db.flush_now()?;
        db.bake(&baked)?;
    }

    let prod = Database::open_prod(&baked)?;
    prod.set_cache_capacity(16)?; // 16 pages = 64 KiB
    let (_, secs) = timed(|| {
        black_box(prod.select("t", None).unwrap().1.len());
    });
    let bytes = std::fs::metadata(&baked)?.len();
    println!(
        "  dataset on disk:          {}  (~{} pages)",
        human(bytes),
        bytes / 4096
    );
    println!("  buffer pool cap:          16 pages (64 KiB)");
    println!(
        "  full scan of {} rows:  {:.1} ms",
        thousands(n as u64),
        secs * 1000.0
    );
    println!(
        "  pages resident after:     {}  (bounded — dataset never fully resident)",
        prod.cache_resident()
    );
    Ok(())
}

// 8. Equality lookup: secondary index vs full scan.
fn indexed_lookup() -> Result<(), Box<dyn Error>> {
    section("8. Indexed lookup vs full scan (50,000 rows, 1 match)");
    let tmp = tempfile::tempdir()?;
    let mut db = Database::open_dev(tmp.path())?;
    db.set_autocommit(false);
    db.query("CREATE TABLE t (id, tag)")?;
    let n = 50_000;
    for i in 0..n as i64 {
        let tag = if i == 40_000 { "needle" } else { "hay" };
        db.insert("t", vec![Value::Int(i), Value::from(tag)])?;
    }
    db.flush_now()?;

    let needle = Value::from("needle");
    let iters = 50;
    let (_, scan_secs) = timed(|| {
        for _ in 0..iters {
            black_box(db.select_where("t", "tag", &needle, None).unwrap().1.len());
        }
    });

    db.query("CREATE INDEX ON t (tag)")?;
    let (_, idx_secs) = timed(|| {
        for _ in 0..iters {
            black_box(db.select_where("t", "tag", &needle, None).unwrap().1.len());
        }
    });

    println!(
        "  full scan:                {:.3} ms/query",
        scan_secs / iters as f64 * 1000.0
    );
    println!(
        "  indexed lookup:           {:.4} ms/query   ({:.0}x faster)",
        idx_secs / iters as f64 * 1000.0,
        scan_secs / idx_secs.max(1e-9)
    );
    Ok(())
}

// --- helpers ---------------------------------------------------------------

fn timed<T>(f: impl FnOnce() -> T) -> (T, f64) {
    let start = Instant::now();
    let out = f();
    (out, start.elapsed().as_secs_f64())
}

fn section(title: &str) {
    println!("\n=== {title} ===");
}

fn rate_row(label: &str, n: usize, secs: f64, note: Option<String>) {
    let rate = if secs > 0.0 {
        n as f64 / secs
    } else {
        f64::INFINITY
    };
    let suffix = note.map(|x| format!("   [{x}]")).unwrap_or_default();
    println!(
        "  {label:<32} {:>8} rows  {:>8.1} ms  {:>11}/s{suffix}",
        thousands(n as u64),
        secs * 1000.0,
        thousands(rate as u64),
    );
}

fn value_size(v: &Value) -> usize {
    match v {
        Value::Null => 1,
        Value::Int(_) => 8,
        Value::Text(s) => s.len(),
        Value::Blob(b) => b.len(),
    }
}

fn dir_size(path: &Path) -> u64 {
    let mut total = 0;
    if let Ok(entries) = std::fs::read_dir(path) {
        for entry in entries.flatten() {
            let p = entry.path();
            if p.is_dir() {
                total += dir_size(&p);
            } else if let Ok(meta) = std::fs::metadata(&p) {
                total += meta.len();
            }
        }
    }
    total
}

fn human(n: u64) -> String {
    const UNITS: [&str; 4] = ["B", "KiB", "MiB", "GiB"];
    let mut v = n as f64;
    let mut i = 0;
    while v >= 1024.0 && i < UNITS.len() - 1 {
        v /= 1024.0;
        i += 1;
    }
    if i == 0 {
        format!("{n} B")
    } else {
        format!("{v:.1} {}", UNITS[i])
    }
}

fn thousands(n: u64) -> String {
    let digits = n.to_string();
    let bytes = digits.as_bytes();
    let mut out = String::with_capacity(digits.len() + digits.len() / 3);
    for (i, c) in bytes.iter().enumerate() {
        if i > 0 && (bytes.len() - i) % 3 == 0 {
            out.push(',');
        }
        out.push(*c as char);
    }
    out
}
