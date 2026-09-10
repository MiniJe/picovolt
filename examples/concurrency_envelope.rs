//! Reproducible local contention measurements; timings are observations, not SLAs.
use picovolt::{SharedDatabase, Value};
use std::sync::{Arc, Barrier};
use std::time::Instant;

fn main() -> picovolt::Result<()> {
    let root = tempfile::tempdir()?;
    let database = SharedDatabase::open_dev(root.path())?;
    let mut write = database.begin_write()?;
    write.query("CREATE TABLE events (id, payload)")?;
    for id in 0..2000 {
        write.query_with(
            "INSERT INTO events VALUES (?, ?)",
            &[
                Value::Int(id),
                Value::Text(format!("event payload {id:06}")),
            ],
        )?;
    }
    write.commit()?;
    let image_pages = database.changes_since(0, 1)?[0].pages.len();
    let started = Instant::now();
    let mut readers = Vec::new();
    for _ in 0..4 {
        readers.push(database.begin_read()?);
    }
    let snapshot_ms = started.elapsed().as_secs_f64() * 1000.0;
    let barrier = Arc::new(Barrier::new(5));
    let mut workers = Vec::new();
    for mut reader in readers {
        let barrier = barrier.clone();
        workers.push(std::thread::spawn(move || -> picovolt::Result<Vec<f64>> {
            barrier.wait();
            let mut times = Vec::new();
            for _ in 0..40 {
                let start = Instant::now();
                let result = reader.query("SELECT COUNT(*) FROM events")?;
                assert_eq!(result.rows().unwrap()[0][0], Value::Int(2000));
                times.push(start.elapsed().as_secs_f64() * 1000.0);
            }
            reader.close()?;
            Ok(times)
        }));
    }
    barrier.wait();
    let mut writes = Vec::new();
    for id in 2000..2040 {
        let start = Instant::now();
        database.query_with(
            "INSERT INTO events VALUES (?, ?)",
            &[Value::Int(id), Value::Text("later event".into())],
        )?;
        writes.push(start.elapsed().as_secs_f64() * 1000.0);
    }
    let mut reads = Vec::new();
    for worker in workers {
        reads.extend(worker.join().expect("reader worker")?);
    }
    reads.sort_by(f64::total_cmp);
    writes.sort_by(f64::total_cmp);
    let latest = database.changes_since(40, 1)?;
    let output = serde_json::json!({
        "rows": 2000, "readers": 4, "read_operations": reads.len(), "write_operations": writes.len(),
        "snapshot_total_ms": snapshot_ms,
        "read_p50_ms": reads[reads.len()/2], "read_p95_ms": reads[reads.len()*95/100],
        "write_p50_ms": writes[writes.len()/2], "write_p95_ms": writes[writes.len()*95/100],
        "initial_changed_pages": image_pages, "final_insert_changed_pages": latest[0].pages.len(),
        "platform": std::env::consts::OS, "arch": std::env::consts::ARCH
    });
    println!("{}", serde_json::to_string_pretty(&output)?);
    Ok(())
}
