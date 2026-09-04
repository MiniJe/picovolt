#![cfg(all(feature = "data-tools", not(target_arch = "wasm32")))]

#[path = "../src/bin/data_tools/mod.rs"]
mod data;
use data::{dataset, export_parquet, import_parquet, import_sqlite};
use picovolt::{Database, Value};
use std::fs;

#[test]
fn cli_data_pipeline_and_failed_export_preserve_files() {
    use std::process::Command;
    let temp = tempfile::tempdir().unwrap();
    let run = |args: &[&str]| {
        let output = Command::new(env!("CARGO_BIN_EXE_pv"))
            .current_dir(temp.path())
            .args(args)
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "{args:?}: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        String::from_utf8(output.stdout).unwrap()
    };
    run(&["query", "source", "CREATE TABLE t (id, name)"]);
    run(&[
        "query",
        "source",
        "INSERT INTO t VALUES (1, 'Ada'), (2, NULL)",
    ]);
    run(&["export", "source", "t", "rows.parquet"]);
    run(&["import", "copy", "rows.parquet", "--table", "t"]);
    let stats: serde_json::Value =
        serde_json::from_str(&run(&["inspect", "copy", "--json"])).unwrap();
    assert_eq!(stats["tables"][0]["live_rows"], 2);
    assert!(run(&["explain", "copy", "SELECT * FROM t"]).contains("table scan"));
    run(&["bake", "copy", "data.pvdb", "--resume"]);
    let public = run(&["dataset", "keygen", "key.secret"]);
    run(&[
        "dataset",
        "sign",
        "data.pvdb",
        "--key",
        "key.secret",
        "--name",
        "cli-test",
        "--output",
        "manifest.json",
    ]);
    run(&[
        "dataset",
        "verify",
        "data.pvdb",
        "manifest.json",
        "--public-key",
        public.trim(),
    ]);
    fs::write(temp.path().join("keep.csv"), b"keep me").unwrap();
    for options in [vec!["--format", "invalid"], vec!["--before", "1"]] {
        let output = Command::new(env!("CARGO_BIN_EXE_pv"))
            .current_dir(temp.path())
            .args(["export", "source", "t", "keep.csv"])
            .args(options)
            .output()
            .unwrap();
        assert!(!output.status.success());
        assert_eq!(fs::read(temp.path().join("keep.csv")).unwrap(), b"keep me");
    }
}

#[test]
fn public_iris_dataset_differential_release_gate() {
    let temp = tempfile::tempdir().unwrap();
    let sqlite = temp.path().join("iris.sqlite");
    let mut connection = rusqlite::Connection::open(&sqlite).unwrap();
    connection.execute_batch("CREATE TABLE iris (sepal_length REAL, sepal_width REAL, petal_length REAL, petal_width REAL, species TEXT)").unwrap();
    let transaction = connection.transaction().unwrap();
    let source = include_str!("fixtures/iris.data");
    assert_eq!(source.lines().filter(|l| !l.trim().is_empty()).count(), 150);
    for _ in 0..20 {
        for line in source.lines().filter(|l| !l.trim().is_empty()) {
            let fields = line.split(',').collect::<Vec<_>>();
            transaction
                .execute(
                    "INSERT INTO iris VALUES (?1, ?2, ?3, ?4, ?5)",
                    rusqlite::params![
                        fields[0].parse::<f64>().unwrap(),
                        fields[1].parse::<f64>().unwrap(),
                        fields[2].parse::<f64>().unwrap(),
                        fields[3].parse::<f64>().unwrap(),
                        fields[4]
                    ],
                )
                .unwrap();
        }
    }
    transaction.commit().unwrap();
    let sql = "SELECT species, COUNT(*) FROM iris WHERE sepal_length >= 5.0 GROUP BY species ORDER BY species";
    let expected = connection
        .prepare(sql)
        .unwrap()
        .query_map([], |r| {
            Ok(vec![Value::Text(r.get(0)?), Value::Int(r.get(1)?)])
        })
        .unwrap()
        .collect::<rusqlite::Result<Vec<_>>>()
        .unwrap();
    let mut db = Database::open_dev(temp.path().join("workspace")).unwrap();
    db.set_cache_capacity(2).unwrap();
    assert_eq!(
        import_sqlite(&mut db, &sqlite, "iris", "iris").unwrap(),
        3000
    );
    assert_eq!(db.query(sql).unwrap().rows().unwrap(), expected);
    let parquet = temp.path().join("iris.parquet");
    export_parquet(&db, "iris", &parquet, None).unwrap();
    let mut restored = Database::open_dev(temp.path().join("restored")).unwrap();
    restored.set_cache_capacity(2).unwrap();
    import_parquet(&mut restored, &parquet, "iris").unwrap();
    assert_eq!(
        restored.select("iris", None).unwrap(),
        db.select("iris", None).unwrap()
    );
    assert_eq!(restored.query(sql).unwrap().rows().unwrap(), expected);
    let image = temp.path().join("iris.pvdb");
    restored.bake_resumable(&image).unwrap();
    let mut baked = Database::open_prod(image).unwrap();
    baked.set_cache_capacity(2).unwrap();
    assert_eq!(baked.query(sql).unwrap().rows().unwrap(), expected);
    assert!(baked.inspect_stats().unwrap().allocated_pages > 2);
}

#[test]
fn sqlite_parquet_roundtrip_with_null_blob_decimal_and_multiple_batches() {
    let temp = tempfile::tempdir().unwrap();
    let sqlite = temp.path().join("input.sqlite");
    let mut connection = rusqlite::Connection::open(&sqlite).unwrap();
    connection
        .execute_batch(
            "CREATE TABLE source (id INTEGER, amount REAL, name TEXT, payload BLOB, optional TEXT)",
        )
        .unwrap();
    let transaction = connection.transaction().unwrap();
    for i in 0..2500 {
        transaction
            .execute(
                "INSERT INTO source VALUES (?1, ?2, ?3, ?4, NULL)",
                rusqlite::params![
                    i,
                    1.25,
                    format!("row,{i}\nquoted\""),
                    vec![0u8, 255, (i % 256) as u8]
                ],
            )
            .unwrap();
    }
    transaction.commit().unwrap();
    let before_bytes = fs::read(&sqlite).unwrap();
    let mut db = Database::open_dev(temp.path().join("workspace")).unwrap();
    db.set_cache_capacity(2).unwrap();
    assert_eq!(
        import_sqlite(&mut db, &sqlite, "source", "items").unwrap(),
        2500
    );
    assert_eq!(fs::read(&sqlite).unwrap(), before_bytes);
    let expected = db.select("items", None).unwrap();
    assert_eq!(expected.1[0][1], Value::Decimal(1_250_000));
    let parquet = temp.path().join("items.parquet");
    assert_eq!(export_parquet(&db, "items", &parquet, None).unwrap(), 2500);
    let mut restored = Database::open_dev(temp.path().join("restored")).unwrap();
    restored.set_cache_capacity(2).unwrap();
    assert_eq!(
        import_parquet(&mut restored, &parquet, "items").unwrap(),
        2500
    );
    assert_eq!(restored.select("items", None).unwrap(), expected);
    assert!(restored.inspect_stats().unwrap().allocated_pages > 2);
    // An existing destination table is never silently appended or replaced.
    assert!(import_parquet(&mut restored, &parquet, "items").is_err());
    assert_eq!(restored.row_count("items", None).unwrap(), 2500);
}

#[test]
fn imports_roll_back_and_failed_export_preserves_existing_destination() {
    let temp = tempfile::tempdir().unwrap();
    let sqlite = temp.path().join("bad.sqlite");
    let connection = rusqlite::Connection::open(&sqlite).unwrap();
    connection.execute_batch("CREATE TABLE source (value); INSERT INTO source VALUES (1); INSERT INTO source VALUES (1e999)").unwrap();
    let mut db = Database::open_memory();
    assert!(import_sqlite(&mut db, &sqlite, "source", "bad").is_err());
    assert!(db.table_names().is_empty());
    db.create_table("mixed", vec!["v".into()]).unwrap();
    db.insert("mixed", vec![Value::Int(1)]).unwrap();
    db.insert("mixed", vec![Value::Text("text".into())])
        .unwrap();
    let output = temp.path().join("existing.parquet");
    fs::write(&output, b"keep me").unwrap();
    assert!(export_parquet(&db, "mixed", &output, None).is_err());
    assert_eq!(fs::read(&output).unwrap(), b"keep me");
    fs::write(&output, b"PAR1truncated").unwrap();
    assert!(import_parquet(&mut db, &output, "bad").is_err());
    assert_eq!(db.table_names(), ["mixed"]);
}

#[test]
fn parquet_empty_and_snapshot_exports_preserve_schema_and_visibility() {
    let temp = tempfile::tempdir().unwrap();
    let path = temp.path().join("snapshot.parquet");
    let mut db = Database::open_memory();
    db.create_table("t", vec!["id".into(), "name".into()])
        .unwrap();
    export_parquet(&db, "t", &path, None).unwrap();
    let mut copy = Database::open_memory();
    import_parquet(&mut copy, &path, "empty").unwrap();
    assert_eq!(copy.column_names("empty").unwrap(), ["id", "name"]);
    db.insert("t", vec![Value::Int(1), Value::Null]).unwrap();
    let tx = db.current_tx();
    db.insert("t", vec![Value::Int(2), Value::Text("later".into())])
        .unwrap();
    export_parquet(&db, "t", &path, Some(tx)).unwrap();
    import_parquet(&mut copy, &path, "snapshot").unwrap();
    assert_eq!(
        copy.select("snapshot", None).unwrap().1,
        db.select("t", Some(tx)).unwrap().1
    );
}

#[test]
fn signed_manifests_reject_wrong_key_changed_metadata_and_changed_artifact() {
    let temp = tempfile::tempdir().unwrap();
    let image = temp.path().join("data.pvdb");
    let key = temp.path().join("key");
    let public = dataset::generate_key(&key).unwrap();
    assert!(dataset::generate_key(&key).is_err());
    let mut db = Database::open_memory();
    db.create_table("t", vec!["id".into()]).unwrap();
    db.bake(&image).unwrap();
    let signed = dataset::sign(&image, "test-dataset", &key).unwrap();
    let manifest = temp.path().join("manifest.json");
    fs::write(&manifest, serde_json::to_vec(&signed).unwrap()).unwrap();
    assert_eq!(
        dataset::verify(&image, &manifest, &public).unwrap().name,
        "test-dataset"
    );
    let other = dataset::generate_key(&temp.path().join("other-key")).unwrap();
    assert!(dataset::verify(&image, &manifest, &other).is_err());
    let mut changed = signed.clone();
    changed.manifest.name = "substitution".into();
    fs::write(&manifest, serde_json::to_vec(&changed).unwrap()).unwrap();
    assert!(dataset::verify(&image, &manifest, &public).is_err());
    fs::write(&manifest, serde_json::to_vec(&signed).unwrap()).unwrap();
    let mut bytes = fs::read(&image).unwrap();
    bytes.push(0);
    fs::write(&image, bytes).unwrap();
    assert!(dataset::verify(&image, &manifest, &public).is_err());
}
