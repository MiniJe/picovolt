use picovolt::{CommitLogOptions, Database, QueryLimits, Value};

#[test]
fn logged_index_definitions_stay_small_and_rebuild_full_history() {
    let temp = tempfile::tempdir().unwrap();
    let mut db = Database::open_dev(temp.path()).unwrap();
    db.enable_commit_log(CommitLogOptions::default()).unwrap();
    db.query("CREATE TABLE t (id PRIMARY KEY, value)").unwrap();
    let rows = (0..2000)
        .map(|i| vec![Value::Int(i), Value::Int(i % 5)])
        .collect::<Vec<_>>();
    db.execute_many("INSERT INTO t VALUES (?,?)", &rows)
        .unwrap();
    db.query("CREATE INDEX ON t (value)").unwrap();
    db.query("CREATE INDEX ON t (id)").unwrap();
    let before = db.current_tx();
    db.query("DELETE FROM t WHERE id < 100").unwrap();
    let hash = db.verification_hash().unwrap();
    let manifest: serde_json::Value =
        serde_json::from_slice(&std::fs::read(temp.path().join("pv_manifest.json")).unwrap())
            .unwrap();
    let table = &manifest["tables"][0];
    assert!(table["indexes"].is_null());
    assert_eq!(table["indexed_columns"].as_array().unwrap().len(), 2);
    assert!(
        std::fs::metadata(temp.path().join("pv_manifest.json"))
            .unwrap()
            .len()
            < 8192
    );
    drop(db);
    let mut db = Database::open_dev(temp.path()).unwrap();
    assert_eq!(hash, db.verification_hash().unwrap());
    assert_eq!(
        db.query("SELECT id FROM t WHERE id = 1")
            .unwrap()
            .rows()
            .unwrap()
            .len(),
        0
    );
    assert_eq!(
        db.query(&format!("SELECT id FROM t WHERE id = 1 BEFORE {before}"))
            .unwrap()
            .rows()
            .unwrap(),
        &[vec![Value::Int(1)]]
    );
    assert!(db.query("INSERT INTO t VALUES (1999,0)").is_err());
    assert_eq!(
        db.query_with_limits(
            "SELECT id FROM t WHERE id >= 1995 AND id < 2000",
            &[],
            QueryLimits::new(5, 8192, 5, None)
        )
        .unwrap()
        .rows()
        .unwrap()
        .len(),
        5
    );
}

#[test]
fn streaming_aggregates_match_general_path_with_bounded_group_memory() {
    let mut db = Database::open_memory();
    db.query("CREATE TABLE t (g, v)").unwrap();
    for i in 0..2000 {
        db.insert(
            "t",
            vec![
                Value::Int(i % 5),
                if i % 7 == 0 {
                    Value::Null
                } else if i % 3 == 0 {
                    Value::Decimal(i as i128 * 1_000_000 + 500_000)
                } else {
                    Value::Int(i)
                },
            ],
        )
        .unwrap();
    }
    let before = db.current_tx();
    db.query("DELETE FROM t WHERE v < 500").unwrap();
    for suffix in [String::new(), format!(" BEFORE {before}")] {
        for projection in [
            "g, COUNT(*), COUNT(v), SUM(v), AVG(v), MIN(v), MAX(v)",
            "g, SUM(v) AS total",
        ] {
            let fast = format!("SELECT {projection} FROM t GROUP BY g{suffix} ORDER BY g");
            let reference =
                format!("SELECT {projection} FROM t WHERE g >= 0 GROUP BY g{suffix} ORDER BY g");
            assert_eq!(db.query(&fast).unwrap(), db.query(&reference).unwrap());
            assert!(db
                .query_with_limits(&fast, &[], QueryLimits::new(2100, 20_000, 5, None))
                .is_ok());
        }
    }
    db.query("CREATE TABLE empty (v)").unwrap();
    assert_eq!(
        db.query("SELECT COUNT(*),SUM(v),AVG(v),MIN(v),MAX(v) FROM empty")
            .unwrap()
            .rows()
            .unwrap(),
        &[vec![
            Value::Int(0),
            Value::Null,
            Value::Null,
            Value::Null,
            Value::Null
        ]]
    );
    db.query("INSERT INTO empty VALUES ('not numeric')")
        .unwrap();
    assert!(db.query("SELECT SUM(v) FROM empty").is_err());
}

#[test]
fn atomic_batches_validate_and_rollback_with_one_physical_commit() {
    let temp = tempfile::tempdir().unwrap();
    let mut db = Database::open_dev(temp.path()).unwrap();
    db.enable_commit_log(CommitLogOptions::default()).unwrap();
    db.query("CREATE TABLE t (id PRIMARY KEY, value)").unwrap();
    let rows = vec![
        vec![Value::Int(1), Value::Text("one".into())],
        vec![Value::Int(2), Value::Text("two".into())],
    ];
    assert_eq!(
        db.execute_many("INSERT INTO t (id,value) VALUES (?,?)", &rows)
            .unwrap(),
        2
    );
    assert_eq!(db.commit_log_status().unwrap().head_sequence, 2);
    let fail = vec![
        vec![Value::Int(3), Value::Null],
        vec![Value::Int(1), Value::Null],
    ];
    assert!(db
        .execute_many("INSERT INTO t VALUES (?,?)", &fail)
        .is_err());
    assert_eq!(db.row_count("t", None).unwrap(), 2);
    assert_eq!(db.commit_log_status().unwrap().head_sequence, 2);
    for sql in [
        "COMMIT",
        "BEGIN",
        "SELECT * FROM t",
        "CREATE TABLE bad (id)",
    ] {
        assert!(db.execute_many(sql, &[vec![]]).is_err());
    }
    assert_eq!(
        db.execute_many("DELETE FROM t WHERE id = ?", &[]).unwrap(),
        0
    );
    db.begin_transaction().unwrap();
    assert!(db
        .execute_many("DELETE FROM t WHERE id = ?", &[vec![Value::Int(1)]])
        .is_err());
    db.rollback_transaction().unwrap();
    db.prune_changes(2).unwrap();
    assert_eq!(db.commit_log_status().unwrap().retained_commits, 0);
}

#[test]
fn bounded_numeric_ranges_match_scan_including_mixed_types_and_extremes() {
    let mut db = Database::open_memory();
    db.query("CREATE TABLE indexed (id, value)").unwrap();
    db.query("CREATE TABLE plain (id, value)").unwrap();
    let values = [
        Value::Null,
        Value::Int(i64::MIN),
        Value::Int(-2),
        Value::Int(0),
        Value::Int(1),
        Value::Int(2),
        Value::Int(i64::MAX),
        Value::Decimal(i128::MIN),
        Value::Decimal(-1_500_000),
        Value::Decimal(1_000_000),
        Value::Decimal(1_500_000),
        Value::Decimal(i128::MAX),
        Value::Text("z".into()),
        Value::Blob(vec![1]),
    ];
    for (id, value) in values.iter().enumerate() {
        for table in ["indexed", "plain"] {
            db.insert(table, vec![Value::Int(id as i64), value.clone()])
                .unwrap();
        }
    }
    db.create_index("indexed", "value").unwrap();
    for value in &values {
        for op in ["=", "<", "<=", ">", ">="] {
            // Bind values through the low-level predicate API for i128 extremes
            // and blobs, which the SQL parameter literal bridge does not support.
            let op = match op {
                "=" => picovolt::engine::query::CompareOp::Eq,
                "<" => picovolt::engine::query::CompareOp::Lt,
                "<=" => picovolt::engine::query::CompareOp::Le,
                ">" => picovolt::engine::query::CompareOp::Gt,
                _ => picovolt::engine::query::CompareOp::Ge,
            };
            let pred = picovolt::engine::query::Predicate::Compare {
                column: "value".into(),
                op,
                value: value.clone(),
            };
            let (_, mut indexed) = db.select_filtered("indexed", Some(&pred), None).unwrap();
            let (_, mut plain) = db.select_filtered("plain", Some(&pred), None).unwrap();
            indexed.sort();
            plain.sort();
            assert_eq!(indexed, plain, "{op:?} {value:?}");
        }
    }
    let limits = QueryLimits::new(2, 100_000, 2, None);
    let result = db
        .query_with_limits(
            "SELECT id FROM indexed WHERE value >= 1 AND value < 1.25 ORDER BY id",
            &[],
            limits,
        )
        .unwrap();
    assert_eq!(
        result.rows().unwrap(),
        &[vec![Value::Int(4)], vec![Value::Int(9)]]
    );
    assert!(db
        .query_with_limits(
            "SELECT id FROM indexed WHERE value > 3 AND value < 2",
            &[],
            QueryLimits::new(0, 1000, 1, None)
        )
        .unwrap()
        .rows()
        .unwrap()
        .is_empty());
}

#[test]
fn join_source_predicate_pushdown_preserves_outer_join_and_or_semantics() {
    let mut db = Database::open_memory();
    db.query("CREATE TABLE events (id, bucket)").unwrap();
    db.query("CREATE TABLE labels (bucket, label)").unwrap();
    for i in 0..2000 {
        db.insert("events", vec![Value::Int(i), Value::Int(i % 20)])
            .unwrap();
    }
    db.query("INSERT INTO labels VALUES (1, 'one')").unwrap();
    db.create_index("events", "bucket").unwrap();
    db.create_index("labels", "bucket").unwrap();
    let query = "SELECT events.id FROM events LEFT JOIN labels ON events.bucket = labels.bucket WHERE events.bucket = 2 ORDER BY events.id";
    let limited = db
        .query_with_limits(query, &[], QueryLimits::new(110, 1_000_000, 100, None))
        .unwrap();
    assert_eq!(limited.rows().unwrap().len(), 100);
    assert_eq!(limited, db.query(query).unwrap());
    let result = db.query("SELECT events.id FROM events LEFT JOIN labels ON events.bucket = labels.bucket WHERE events.bucket = 2 OR labels.label = 'one'").unwrap();
    assert_eq!(result.rows().unwrap().len(), 200);
}

#[test]
fn synced_catalog_changes_reopen_and_indexes_cannot_mutate_read_only_handles() {
    let temp = tempfile::tempdir().unwrap();
    let mut db = Database::open_dev(temp.path()).unwrap();
    db.enable_commit_log(CommitLogOptions::default()).unwrap();
    db.query("CREATE TABLE t (id)").unwrap();
    db.query("INSERT INTO t VALUES (1)").unwrap();
    db.query("CREATE INDEX ON t (id)").unwrap();
    db.query("INSERT INTO t VALUES (2)").unwrap();
    assert!(db.create_index("t", "id").is_err()); // explicit tx required
    let bytes = db.bake_to_bytes().unwrap();
    drop(db);
    let mut reopened = Database::open_dev(temp.path()).unwrap();
    assert_eq!(
        reopened
            .query("SELECT id FROM t WHERE id >= 1 ORDER BY id")
            .unwrap()
            .rows()
            .unwrap()
            .len(),
        2
    );
    let image = temp.path().join("readonly.pvdb");
    std::fs::write(&image, bytes).unwrap();
    let mut readonly = Database::open_prod(&image).unwrap();
    assert!(readonly.create_index("t", "id").is_err());
}
