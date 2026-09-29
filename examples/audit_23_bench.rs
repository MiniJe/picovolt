//! Reproducible, single-host PicoVolt 2.3 audit matrix.
//! Run: cargo run --release --locked --example audit_23_bench -- 1000 5000 10000
use picovolt::{Database, Value};
use serde_json::{json, Value as Json};
use std::error::Error;
use std::hint::black_box;
use std::path::Path;
use std::process::Command;
use std::time::Instant;

fn ms(f: impl FnOnce() -> Result<(), Box<dyn Error>>) -> Result<f64, Box<dyn Error>> {
    let start = Instant::now();
    f()?;
    Ok(start.elapsed().as_secs_f64() * 1000.0)
}

fn size(path: &Path) -> std::io::Result<u64> {
    if path.is_file() {
        return Ok(path.metadata()?.len());
    }
    let mut total = 0;
    for entry in std::fs::read_dir(path)? {
        total += size(&entry?.path())?;
    }
    Ok(total)
}

fn request(kind: &str, named: bool) -> String {
    let mut value = match kind {
        "full_text" => {
            json!({"kind":"full_text","sql":"SELECT id,body,embedding FROM docs","id_column":"id","text_columns":["body"],"query":"apple 東京","limit":10})
        }
        "vector" => {
            json!({"kind":"vector","sql":"SELECT id,body,embedding FROM docs","id_column":"id","vector_column":"embedding","query":[1,0,0],"metric":"cosine","limit":10})
        }
        _ => {
            json!({"kind":"hybrid","sql":"SELECT id,body,embedding FROM docs","id_column":"id","text_columns":["body"],"vector_column":"embedding","query":"apple 東京","vector_query":[1,0,0],"metric":"cosine","text_weight":0.5,"candidate_limit":20,"limit":10})
        }
    };
    if named {
        match kind {
            "full_text" => value["index"] = json!("ft"),
            "vector" => value["index"] = json!("vx"),
            _ => {
                value["text_index"] = json!("ft");
                value["vector_index"] = json!("vx");
            }
        }
    }
    value.to_string()
}

fn run(n: usize) -> Result<Json, Box<dyn Error>> {
    let temp = tempfile::tempdir()?;
    let workspace = temp.path().join("audit.pv");
    let image = temp.path().join("audit.pvdb");
    let mut db = Database::open_dev(&workspace)?;
    db.set_autocommit(false);
    db.query("CREATE TABLE docs (id,tenant,body,embedding)")?;
    let insert_bulk_ms = ms(|| {
        for id in 0..n {
            db.insert(
                "docs",
                vec![
                    Value::Int(id as i64),
                    Value::Int((id % 10) as i64),
                    Value::Text(format!(
                        "apple 東京 item {id} {}",
                        if id % 3 == 0 { "banana" } else { "pear" }
                    )),
                    Value::Text(format!("[1,{},{}]", id % 7, id % 11)),
                ],
            )?;
        }
        Ok(())
    })?;
    let flush_ms = ms(|| {
        db.flush_now()?;
        Ok(())
    })?;
    let secondary_index_build_ms = ms(|| {
        db.query("CREATE INDEX ON docs (id)")?;
        Ok(())
    })?;
    let text_index_build_ms = ms(|| {
        db.query("CREATE INDEX ft ON docs USING FULLTEXT (body) WITH (id_column='id')")?;
        Ok(())
    })?;
    let vector_index_build_ms = ms(|| {
        db.query("CREATE INDEX vx ON docs USING VECTOR (embedding) WITH (id_column='id',metric='cosine',dimensions=3)")?;
        Ok(())
    })?;
    db.flush_now()?;
    let index_bytes: usize = db
        .retrieval_indexes()
        .iter()
        .map(|entry| entry.persisted_bytes)
        .sum();
    let workspace_bytes = size(&workspace)?;
    let bake_ms = ms(|| {
        db.bake(&image)?;
        Ok(())
    })?;
    let image_bytes = size(&image)?;
    let mut opened = None;
    let open_ms = ms(|| {
        opened = Some(Database::open_prod(&image)?);
        Ok(())
    })?;
    let mut prod = opened.unwrap();
    let point_ms = ms(|| {
        black_box(prod.query_with(
            "SELECT body FROM docs WHERE id=?",
            &[Value::Int((n / 2) as i64)],
        )?);
        Ok(())
    })?;
    let indexed_lookup_ms = ms(|| {
        black_box(prod.select_where("docs", "id", &Value::Int((n / 2) as i64), None)?);
        Ok(())
    })?;
    let scan_ms = ms(|| {
        black_box(prod.query("SELECT id,body FROM docs")?);
        Ok(())
    })?;
    let mut retrieval = serde_json::Map::new();
    for kind in ["full_text", "vector", "hybrid"] {
        let legacy = request(kind, false);
        let named = request(kind, true);
        let fallback_ms = ms(|| {
            black_box(prod.retrieve_json(&legacy)?);
            Ok(())
        })?;
        let named_ms = ms(|| {
            black_box(prod.retrieve_json(&named)?);
            Ok(())
        })?;
        if prod.retrieve_json(&legacy)? != prod.retrieve_json(&named)? {
            return Err(format!("{kind} named index diverged from full scan").into());
        }
        retrieval.insert(
            kind.into(),
            json!({"fallback_ms":fallback_ms,"named_ms":named_ms}),
        );
    }
    db.set_autocommit(true);
    // Free one document before timing insertion at the 10,000-document cap.
    let delete_ms = ms(|| {
        db.query_with("DELETE FROM docs WHERE id=?", &[Value::Int((n - 1) as i64)])?;
        Ok(())
    })?;
    let insert_ms = ms(|| {
        db.query_with(
            "INSERT INTO docs VALUES (?,?,?,?)",
            &[
                Value::Int(n as i64),
                Value::Int(0),
                Value::Text("apple 東京 new".into()),
                Value::Text("[1,0,0]".into()),
            ],
        )?;
        Ok(())
    })?;
    let update_ms = ms(|| {
        db.query_with(
            "UPDATE docs SET body=? WHERE id=?",
            &[
                Value::Text("apple 東京 updated".into()),
                Value::Int((n / 2) as i64),
            ],
        )?;
        Ok(())
    })?;
    let commit_ms = ms(|| {
        db.begin_transaction()?;
        db.query_with(
            "UPDATE docs SET body=? WHERE id=?",
            &[
                Value::Text("apple 東京 committed".into()),
                Value::Int((n / 2) as i64),
            ],
        )?;
        db.commit_transaction()?;
        Ok(())
    })?;
    let workspace_after_crud_bytes = size(&workspace)?;
    drop(db);
    let crash = Command::new(std::env::current_exe()?)
        .arg("--crash-workspace")
        .arg(&workspace)
        .status()?;
    if crash.code() != Some(91) {
        return Err(format!("crash helper exited with {crash}").into());
    }
    let recovery_ms = ms(|| {
        let mut recovered = Database::open_dev(&workspace)?;
        let restored = recovered.query("SELECT body FROM docs WHERE id=0")?;
        if restored
            .rows()
            .is_none_or(|rows| rows.len() != 1 || rows[0][0] == Value::Text("uncommitted".into()))
        {
            return Err("recovery exposed the uncommitted update".into());
        }
        black_box(restored);
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
    let peak_rss_kb = memory("VmHWM:");
    let resident_rss_kb = memory("VmRSS:");
    Ok(
        json!({"rows":n,"bulk_insert_ms":insert_bulk_ms,"flush_ms":flush_ms,"secondary_index_build_ms":secondary_index_build_ms,"text_index_build_ms":text_index_build_ms,"vector_index_build_ms":vector_index_build_ms,"bake_ms":bake_ms,"open_ms":open_ms,"point_ms":point_ms,"indexed_lookup_ms":indexed_lookup_ms,"scan_ms":scan_ms,"retrieval":retrieval,"insert_ms":insert_ms,"update_ms":update_ms,"delete_ms":delete_ms,"commit_ms":commit_ms,"recovery_ms":recovery_ms,"workspace_bytes":workspace_bytes,"workspace_after_crud_bytes":workspace_after_crud_bytes,"image_bytes":image_bytes,"retrieval_index_bytes":index_bytes,"process_peak_rss_kb":peak_rss_kb,"process_resident_rss_kb":resident_rss_kb}),
    )
}

fn main() -> Result<(), Box<dyn Error>> {
    let args = std::env::args().skip(1).collect::<Vec<_>>();
    if args.first().is_some_and(|arg| arg == "--crash-workspace") {
        let mut db = Database::open_dev(args.get(1).ok_or("missing workspace")?)?;
        db.begin_transaction()?;
        db.query("UPDATE docs SET body='uncommitted' WHERE id=0")?;
        std::process::exit(91);
    }
    let sizes = if args.is_empty() {
        vec![1000, 5000, 10000]
    } else {
        args.iter()
            .map(|arg| arg.parse())
            .collect::<Result<Vec<usize>, _>>()?
    };
    let mut results = Vec::new();
    for n in sizes {
        if !(2..=10_000).contains(&n) {
            return Err("rows must be in 2..=10000 (persistent retrieval document cap)".into());
        }
        let result = run(n)?;
        eprintln!("AUDIT_RUN {result}");
        results.push(result);
    }
    let migration_source = include_bytes!("../tests/fixtures/golden_v1_9_0.pvdb");
    let migration_open_ms = ms(|| {
        black_box(Database::import_bytes(migration_source)?);
        Ok(())
    })?;
    println!(
        "{}",
        json!({"source":"v2.3.0","mode":if cfg!(debug_assertions) {"debug"} else {"release"},"runs":results,"legacy_v1_9_import_ms":migration_open_ms})
    );
    Ok(())
}
