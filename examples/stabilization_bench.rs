//! Machine-readable 1.9 performance smoke gate.
//!
//! The thresholds are intentionally broad enough for shared CI runners. Their
//! job is to catch algorithmic regressions; dedicated-runner trend analysis can
//! apply the tighter percentage budgets documented in `BENCHMARKS.md`.

use std::collections::BTreeMap;
use std::error::Error;
use std::hint::black_box;
use std::path::Path;
use std::process::Command;
use std::time::Instant;

use picovolt::{Database, Value};
use serde_json::json;

fn main() -> Result<(), Box<dyn Error>> {
    let arguments = std::env::args().skip(1).collect::<Vec<_>>();
    if arguments.first().map(String::as_str) == Some("--leave-uncommitted") {
        let workspace = arguments.get(1).ok_or("missing crash workspace")?;
        leave_uncommitted_transaction(Path::new(workspace))?;
    }
    let check = arguments.iter().any(|argument| argument == "--check");
    let budgets: BTreeMap<String, f64> =
        serde_json::from_str(include_str!("../benchmarks/stabilization-budgets.json"))?;
    let temp = tempfile::tempdir()?;
    let workspace = temp.path().join("bench.pv");
    let image = temp.path().join("bench.pvdb");

    let mut source = Database::open_dev(&workspace)?;
    source.set_autocommit(false);
    source.query("CREATE TABLE facts (id PRIMARY KEY, bucket, payload)")?;
    source.query("CREATE TABLE wanted (bucket PRIMARY KEY)")?;
    for bucket in 0..32i64 {
        source.insert("wanted", vec![Value::Int(bucket)])?;
    }
    for id in 0..10_000i64 {
        source.insert(
            "facts",
            vec![
                Value::Int(id),
                Value::Int(id % 256),
                Value::Text(format!("row-{id:08}")),
            ],
        )?;
    }
    source.query("CREATE INDEX ON facts (bucket)")?;
    source.flush_now()?;
    source.bake(&image)?;

    let mut results = BTreeMap::<String, f64>::new();
    results.insert(
        "open".into(),
        median_ms(|| {
            black_box(Database::open_prod(&image).unwrap());
        }),
    );
    let production = Database::open_prod(&image)?;
    results.insert(
        "scan".into(),
        median_ms(|| {
            black_box(production.select("facts", None).unwrap());
        }),
    );
    results.insert(
        "point_lookup".into(),
        median_ms(|| {
            black_box(
                production
                    .select_where("facts", "bucket", &Value::Int(73), None)
                    .unwrap(),
            );
        }),
    );
    let mut queryable = Database::open_prod(&image)?;
    results.insert(
        "top_n".into(),
        median_ms(|| {
            black_box(
                queryable
                    .query("SELECT id FROM facts ORDER BY id DESC LIMIT 20")
                    .unwrap(),
            );
        }),
    );
    results.insert(
        "join".into(),
        median_ms(|| {
            black_box(
                queryable
                    .query(
                        "SELECT w.bucket, f.id FROM wanted w \
                         JOIN facts f ON w.bucket = f.bucket LIMIT 100",
                    )
                    .unwrap(),
            );
        }),
    );
    results.insert(
        "bake".into(),
        median_ms(|| {
            black_box(source.bake_to_bytes().unwrap());
        }),
    );
    drop(source);
    results.insert("recovery_open".into(), recovery_open_median_ms(&workspace)?);

    let measurements = results
        .iter()
        .map(|(name, median_ms)| {
            let budget_ms = budgets[name];
            json!({
                "workload": name,
                "median_ms": median_ms,
                "budget_ms": budget_ms,
                "passed": median_ms <= &budget_ms,
            })
        })
        .collect::<Vec<_>>();
    let passed = measurements
        .iter()
        .all(|measurement| measurement["passed"] == true);
    println!(
        "{}",
        serde_json::to_string_pretty(&json!({
            "schema": 1,
            "rows": 10_000,
            "samples": 5,
            "passed": passed,
            "measurements": measurements,
        }))?
    );
    if check && !passed {
        return Err("one or more stabilization performance budgets were exceeded".into());
    }
    Ok(())
}

fn leave_uncommitted_transaction(workspace: &Path) -> Result<(), Box<dyn Error>> {
    let mut database = Database::open_dev(workspace)?;
    database.begin_transaction()?;
    database.query("INSERT INTO facts VALUES (-1, 0, 'uncommitted')")?;
    database.flush_now()?;
    // This helper intentionally bypasses destructors. Its parent measures the
    // next open restoring the synced rollback image.
    std::process::exit(0);
}

fn recovery_open_median_ms(workspace: &Path) -> Result<f64, Box<dyn Error>> {
    let mut samples = Vec::with_capacity(5);
    for sample in 0..6 {
        let status = Command::new(std::env::current_exe()?)
            .arg("--leave-uncommitted")
            .arg(workspace)
            .status()?;
        if !status.success() {
            return Err("recovery benchmark helper failed".into());
        }
        let started = Instant::now();
        black_box(Database::open_dev(workspace)?);
        let elapsed = started.elapsed().as_secs_f64() * 1_000.0;
        if sample > 0 {
            samples.push(elapsed);
        }
    }
    samples.sort_by(f64::total_cmp);
    Ok(samples[samples.len() / 2])
}

fn median_ms(mut operation: impl FnMut()) -> f64 {
    operation();
    let mut samples = Vec::with_capacity(5);
    for _ in 0..5 {
        let started = Instant::now();
        operation();
        samples.push(started.elapsed().as_secs_f64() * 1_000.0);
    }
    samples.sort_by(f64::total_cmp);
    samples[samples.len() / 2]
}
