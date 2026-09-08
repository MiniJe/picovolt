use picovolt::{CommitLogOptions, Database, QueryLimits, Value};

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
