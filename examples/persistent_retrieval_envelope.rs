// PicoVolt 2.3 qualification; see legal/COMPONENT-SCOPE-2.3.md.
//! `cargo run --release --example persistent_retrieval_envelope -- --bench`
//! emits machine-readable same-run comparisons, not a cross-machine SLA.
//! `--fixture` creates the format-8 golden only when it does not already exist.
#[cfg(not(all(feature = "full-text", feature = "vector-search")))]
fn main() {
    eprintln!("This qualification example requires full-text and vector-search.");
}

#[cfg(all(feature = "full-text", feature = "vector-search"))]
fn main() -> Result<(), Box<dyn std::error::Error>> {
    qualification::run()
}

#[cfg(all(feature = "full-text", feature = "vector-search"))]
mod qualification {
    use picovolt::{Database, Value};
    use serde_json::{json, Value as Json};
    use std::io::Write;
    use std::path::Path;
    use std::time::Instant;

    fn vector(id: usize, dimensions: usize) -> String {
        serde_json::to_string(
            &(0..dimensions)
                .map(|dimension| ((id + dimension) % 11 + 1) as f32)
                .collect::<Vec<_>>(),
        )
        .unwrap()
    }

    fn populate(db: &mut Database, count: usize, dimensions: usize) -> picovolt::Result<()> {
        db.query("CREATE TABLE docs (id,tenant,title,body,embedding)")?;
        db.transaction(|db| {
            for id in 0..count {
                let body = format!(
                    "verified snapshot database commit history {}",
                    "bounded local retrieval tokenization vectors durable tenant ownership "
                        .repeat(5)
                );
                db.query_with(
                    "INSERT INTO docs VALUES (?,?,?,?,?)",
                    &[
                        Value::Int(id as i64),
                        Value::Text(format!("tenant-{}", id % 8)),
                        Value::Text(format!("document {id} verified")),
                        Value::Text(body),
                        Value::Text(vector(id, dimensions)),
                    ],
                )?;
            }
            Ok(())
        })
    }

    fn indexes(db: &mut Database, dimensions: usize) -> picovolt::Result<(f64, f64)> {
        let start = Instant::now();
        db.query("CREATE INDEX ft ON docs USING FULLTEXT (title,body) WITH (id_column='id')")?;
        let text = start.elapsed().as_secs_f64() * 1000.0;
        let start = Instant::now();
        db.query(&format!("CREATE INDEX vx ON docs USING VECTOR (embedding) WITH (id_column='id',metric='cosine',dimensions={dimensions})"))?;
        Ok((text, start.elapsed().as_secs_f64() * 1000.0))
    }

    fn request(kind: &str, filtered: bool, dimensions: usize) -> Json {
        let columns = match kind {
            "full_text" => "id,title,body",
            "vector" => "id,embedding",
            _ => "*",
        };
        let sql = format!(
            "SELECT {columns} FROM docs{}",
            if filtered {
                " WHERE tenant='tenant-0'"
            } else {
                ""
            }
        );
        let query = vec![1.0f32; dimensions];
        match kind {
            "full_text" => {
                json!({"kind":kind,"sql":sql,"id_column":"id","text_columns":["title","body"],"query":"verified snapshot","limit":10})
            }
            "vector" => {
                json!({"kind":kind,"sql":sql,"id_column":"id","vector_column":"embedding","metric":"cosine","query":query,"limit":10})
            }
            _ => {
                json!({"kind":"hybrid","sql":sql,"id_column":"id","text_columns":["title","body"],"vector_column":"embedding","query":"verified snapshot","vector_query":query,"metric":"cosine","text_weight":0.5,"candidate_limit":30,"limit":10})
            }
        }
    }

    fn named(mut request: Json) -> Json {
        match request["kind"].as_str().unwrap() {
            "full_text" => request["index"] = json!("ft"),
            "vector" => request["index"] = json!("vx"),
            _ => {
                request["text_index"] = json!("ft");
                request["vector_index"] = json!("vx");
            }
        }
        request
    }

    fn time_queries(db: &mut Database, request: &str, repetitions: usize) -> picovolt::Result<f64> {
        let start = Instant::now();
        for _ in 0..repetitions {
            std::hint::black_box(db.retrieve_json(request)?);
        }
        Ok(start.elapsed().as_secs_f64() * 1000.0 / repetitions as f64)
    }

    fn directory_bytes(path: &Path) -> std::io::Result<u64> {
        let mut total = 0;
        for entry in std::fs::read_dir(path)? {
            let path = entry?.path();
            total += if path.is_dir() {
                directory_bytes(&path)?
            } else {
                std::fs::metadata(path)?.len()
            };
        }
        Ok(total)
    }

    fn rss() -> Json {
        let status = std::fs::read_to_string("/proc/self/status").unwrap_or_default();
        let number = |key: &str| {
            status
                .lines()
                .find(|line| line.starts_with(key))
                .and_then(|line| line.split_whitespace().nth(1))
                .and_then(|value| value.parse::<u64>().ok())
        };
        json!({"process_rss_kib":number("VmRSS:"),"process_peak_rss_kib":number("VmHWM:"),
            "scope":"entire benchmark process, including source, indexed and reopened copies; not per-index attribution"})
    }

    fn corpus(count: usize, dimensions: usize) -> Result<Json, Box<dyn std::error::Error>> {
        let mut plain = Database::open_memory();
        populate(&mut plain, count, dimensions)?;
        let plain_bytes = plain.bake_to_bytes()?;
        let mut db = Database::import_bytes(&plain_bytes)?;
        let (text_build, vector_build) = indexes(&mut db, dimensions)?;
        let bytes = db.bake_to_bytes()?;
        let start = Instant::now();
        let mut reopened = Database::import_bytes(&bytes)?;
        let reopen = start.elapsed().as_secs_f64() * 1000.0;
        let start = Instant::now();
        let reference = Database::import_bytes(&plain_bytes)?;
        let plain_reopen = start.elapsed().as_secs_f64() * 1000.0;
        drop(reference);
        let repetitions = 15;
        let mut queries = Vec::new();
        for kind in ["full_text", "vector", "hybrid"] {
            for filtered in [false, true] {
                let request = request(kind, filtered, dimensions);
                let legacy = request.to_string();
                let named = named(request).to_string();
                assert_eq!(
                    reopened.retrieve_json(&legacy)?,
                    reopened.retrieve_json(&named)?,
                    "differential failure before measuring"
                );
                // Alternate order between the filtered and whole-corpus cases.
                let (ephemeral, persistent) = if filtered {
                    let persistent = time_queries(&mut reopened, &named, repetitions)?;
                    (
                        time_queries(&mut reopened, &legacy, repetitions)?,
                        persistent,
                    )
                } else {
                    let ephemeral = time_queries(&mut reopened, &legacy, repetitions)?;
                    (ephemeral, time_queries(&mut reopened, &named, repetitions)?)
                };
                queries.push(json!({"kind":kind,"filtered":filtered,"repetitions":repetitions,
                    "ephemeral_mean_ms":ephemeral,"persistent_mean_ms":persistent,"observed_speedup":ephemeral/persistent}));
            }
        }
        let stats = db.retrieval_indexes();
        let mut mutations = Vec::new();
        for (kind, sql) in [
            ("delete", format!("DELETE FROM docs WHERE id={}", count - 1)),
            ("insert", format!("INSERT INTO docs VALUES ({count},'tenant-0','verified new','snapshot replacement','{}')", vector(count, dimensions))),
            ("update_text", "UPDATE docs SET body='verified snapshot changed' WHERE id=0".into()),
            ("update_vector", format!("UPDATE docs SET embedding='{}' WHERE id=0", vector(77, dimensions))),
        ] {
            let start = Instant::now(); plain.query(&sql)?; let baseline = start.elapsed().as_secs_f64()*1000.0;
            let start = Instant::now(); db.query(&sql)?; let indexed = start.elapsed().as_secs_f64()*1000.0;
            mutations.push(json!({"operation":kind,"observations":1,"unindexed_ms":baseline,"indexed_ms":indexed}));
        }
        let temp = tempfile::tempdir()?;
        let root = temp.path().join("workspace");
        let mut workspace = Database::open_dev(&root)?;
        populate(&mut workspace, count, dimensions)?;
        let workspace_plain = directory_bytes(&root)?;
        indexes(&mut workspace, dimensions)?;
        let workspace_indexed = directory_bytes(&root)?;
        workspace.query("UPDATE docs SET body='verified snapshot changed' WHERE id=0")?;
        let workspace_after_update = directory_bytes(&root)?;
        drop(workspace);
        let start = Instant::now();
        let workspace = Database::open_dev(&root)?;
        let workspace_reopen = start.elapsed().as_secs_f64() * 1000.0;
        drop(workspace);
        Ok(
            json!({"documents":count,"vector_dimensions":dimensions,"text_create_ms":text_build,"vector_create_ms":vector_build,
            "unindexed_import_ms":plain_reopen,"indexed_import_with_source_verification_ms":reopen,
            "baked_unindexed_bytes":plain_bytes.len(),"baked_indexed_bytes":bytes.len(),"baked_overhead_bytes":bytes.len()-plain_bytes.len(),
            "workspace_unindexed_bytes":workspace_plain,"workspace_indexed_bytes":workspace_indexed,
            "workspace_after_one_update_bytes":workspace_after_update,"workspace_reopen_ms":workspace_reopen,
            "indexes":stats,"queries":queries,"mutations":mutations,"memory":rss()}),
        )
    }

    pub fn run() -> Result<(), Box<dyn std::error::Error>> {
        match std::env::args().nth(1).as_deref() {
            Some("--fixture") => {
                let path = Path::new("tests/fixtures/format_v8.pvdb");
                if path.exists() {
                    return Err("Refusing to overwrite an existing golden fixture".into());
                }
                let mut db = Database::open_memory();
                populate(&mut db, 3, 3)?;
                indexes(&mut db, 3)?;
                let bytes = db.bake_to_bytes()?;
                let mut reopened = Database::import_bytes(&bytes)?;
                for kind in ["full_text", "vector", "hybrid"] {
                    let legacy = request(kind, false, 3);
                    assert_eq!(
                        reopened.retrieve_json(&legacy.to_string())?,
                        reopened.retrieve_json(&named(legacy).to_string())?
                    );
                }
                let mut file = std::fs::OpenOptions::new()
                    .write(true)
                    .create_new(true)
                    .open(path)?;
                file.write_all(&bytes)?;
                file.sync_all()?;
                println!(
                    "Created {} ({} bytes, BLAKE3 {})",
                    path.display(),
                    bytes.len(),
                    blake3::hash(&bytes)
                );
            }
            Some("--bench") => {
                let corpora = vec![corpus(1_000, 128)?, corpus(10_000, 128)?];
                println!(
                    "{}",
                    serde_json::to_string_pretty(
                        &json!({"schema_version":1,"mandate":"PV-2.3-M-001",
                    "package_version":env!("CARGO_PKG_VERSION"),"revision":std::env::var("PV_BENCH_REVISION").ok(),
                    "os":std::env::consts::OS,"architecture":std::env::consts::ARCH,
                    "profile":"release recommended; caller records exact build command and runner",
                    "large_corpus":"50,000–100,000 documents are not admitted in 2.3; the explicit per-index cap is 10,000",
                    "notes":["same host/process/run and source corpus","queries retain SELECT materialization","open verifies decoded state against authoritative rows once",
                        "mutation numbers are individual observations, not percentiles","CAS generations are append-only; disk overhead includes retained generations"],"corpora":corpora})
                    )?
                );
            }
            _ => return Err("Use --fixture or --bench".into()),
        }
        Ok(())
    }
}
