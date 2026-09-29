//! Core-only large-corpus companion to audit_23_bench. Persistent retrieval is
//! capped at 10,000 documents, so larger sizes exercise storage and SQL only.
//! Run: cargo run --release --locked --example audit_23_core_bench -- 100000 1000000
use picovolt::{Database, Value};
use serde_json::json;
use std::error::Error;
use std::hint::black_box;
use std::time::Instant;

fn timed(f: impl FnOnce() -> Result<(), Box<dyn Error>>) -> Result<f64, Box<dyn Error>> {
    let start = Instant::now();
    f()?;
    Ok(start.elapsed().as_secs_f64() * 1000.0)
}

fn main() -> Result<(), Box<dyn Error>> {
    let sizes = std::env::args()
        .skip(1)
        .map(|value| value.parse::<usize>())
        .collect::<Result<Vec<_>, _>>()?;
    if sizes.is_empty() {
        return Err("pass one or more row counts".into());
    }
    let mut runs = Vec::new();
    for n in sizes {
        let temp = tempfile::tempdir()?;
        let image = temp.path().join("core.pvdb");
        let mut db = Database::open_memory();
        db.query("CREATE TABLE facts (id,bucket,body)")?;
        let insert_ms = timed(|| {
            for id in 0..n {
                db.insert(
                    "facts",
                    vec![
                        Value::Int(id as i64),
                        Value::Int((id % 1000) as i64),
                        Value::Text(format!("row-{id:08}")),
                    ],
                )?;
            }
            Ok(())
        })?;
        let index_build_ms = timed(|| {
            db.query("CREATE INDEX ON facts (id)")?;
            Ok(())
        })?;
        let bake_ms = timed(|| {
            db.bake(&image)?;
            Ok(())
        })?;
        let file_bytes = image.metadata()?.len();
        let mut prod = None;
        let open_ms = timed(|| {
            prod = Some(Database::open_prod(&image)?);
            Ok(())
        })?;
        let mut prod = prod.unwrap();
        let point_ms = timed(|| {
            black_box(prod.query_with(
                "SELECT body FROM facts WHERE id=?",
                &[Value::Int((n / 2) as i64)],
            )?);
            Ok(())
        })?;
        let scan_ms = timed(|| {
            black_box(prod.query("SELECT id FROM facts")?);
            Ok(())
        })?;
        let status = std::fs::read_to_string("/proc/self/status").ok();
        let memory = |field: &str| {
            status.as_ref().and_then(|status| {
                status
                    .lines()
                    .find(|line| line.starts_with(field))
                    .and_then(|line| line.split_whitespace().nth(1))
                    .and_then(|value| value.parse::<u64>().ok())
            })
        };
        runs.push(json!({"rows":n,"bulk_insert_ms":insert_ms,"bulk_insert_rows_per_sec":n as f64 * 1000.0 / insert_ms,"index_build_ms":index_build_ms,"bake_ms":bake_ms,"file_bytes":file_bytes,"open_ms":open_ms,"point_ms":point_ms,"scan_ms":scan_ms,"process_peak_rss_kb":memory("VmHWM:"),"process_resident_rss_kb":memory("VmRSS:")}));
    }
    println!(
        "{}",
        json!({"source":"v2.3.0","mode":if cfg!(debug_assertions) {"debug"} else {"release"},"runs":runs})
    );
    Ok(())
}
