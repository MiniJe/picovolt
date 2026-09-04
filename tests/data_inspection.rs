use picovolt::{Database, QueryLimits, QueryResult, Value};

fn operations(result: QueryResult) -> Vec<String> {
    result
        .rows()
        .unwrap()
        .iter()
        .map(|r| r[1].as_text().unwrap().to_string())
        .collect()
}

#[test]
fn explain_matches_access_paths_without_reading_pages() {
    let mut db = Database::open_memory();
    db.query("CREATE TABLE t (id, name)").unwrap();
    db.query("INSERT INTO t VALUES (1, 'Ada'), (2, 'Lin')")
        .unwrap();
    db.create_index("t", "name").unwrap();
    db.create_index("t", "id").unwrap();
    assert!(operations(
        db.query("EXPLAIN SELECT * FROM t WHERE name = 'Ada'")
            .unwrap()
    )
    .contains(&"index lookup".into()));
    assert!(
        operations(db.explain("SELECT * FROM t WHERE name > 'A'").unwrap())
            .contains(&"index range scan".into())
    );
    assert!(
        operations(db.explain("SELECT * FROM t WHERE id > 0").unwrap())
            .contains(&"table scan".into())
    );
    assert!(operations(
        db.query_with_limits(
            "EXPLAIN SELECT * FROM t WHERE name > 'A'",
            &[],
            QueryLimits::new(0, 10000, 100, None)
        )
        .unwrap()
    )
    .contains(&"table scan".into()));
    let ordered = operations(db.explain("SELECT * FROM t ORDER BY id LIMIT 1").unwrap());
    assert!(ordered.contains(&"ordered index scan".into()));
    assert!(!ordered.contains(&"sort".into()));
    assert_eq!(
        operations(db.explain("SELECT COUNT(*) FROM t").unwrap()),
        ["snapshot", "count envelopes"]
    );
    assert!(operations(
        db.explain("SELECT a.id FROM t a LEFT JOIN t b ON a.id = b.id ORDER BY a.id LIMIT 1")
            .unwrap()
    )
    .contains(&"left equality join".into()));
    assert!(operations(
        db.explain("SELECT name, COUNT(*) FROM t GROUP BY name HAVING COUNT(*) > 0 ORDER BY name")
            .unwrap()
    )
    .contains(&"aggregate".into()));
    assert!(db.query("EXPLAIN DELETE FROM t WHERE id = 1").is_err());
    assert!(db.explain("SELECT missing FROM t").is_err());
    assert!(db.explain("SELECT LOWER(missing) FROM t").is_err());
    assert!(db
        .explain("SELECT SUM(missing) FROM t GROUP BY name")
        .is_err());
    assert!(db
        .query_with_limits(
            "EXPLAIN SELECT * FROM t",
            &[],
            QueryLimits::new(0, 1, 100, None)
        )
        .is_err());
    assert_eq!(operations(db.explain("SELECT name, COUNT(*) FROM t GROUP BY name HAVING COUNT(*) > 0 ORDER BY name LIMIT 2").unwrap()), ["snapshot", "table scan", "aggregate", "project", "having", "sort", "limit"]);
    assert_eq!(db.row_count("t", None).unwrap(), 2);
}

#[test]
fn inspection_counts_versions_pages_indexes_and_cas() {
    let mut db = Database::open_memory();
    db.create_table("t", vec!["id".into(), "payload".into()])
        .unwrap();
    for i in 0..500 {
        db.insert("t", vec![Value::Int(i), Value::Text("x".repeat(512))])
            .unwrap();
    }
    db.create_index("t", "id").unwrap();
    db.delete("t", "id", &Value::Int(0)).unwrap();
    db.set_cache_capacity(2).unwrap();
    let stats = db.inspect_stats().unwrap();
    let table = &stats.tables[0];
    assert_eq!(table.live_rows, 499);
    assert_eq!(table.row_versions, 500);
    assert!(table.pages > 2);
    assert_eq!(
        table.used_page_bytes + table.free_page_bytes,
        table.pages * 4096
    );
    assert_eq!(table.indexes[0].entries, 500);
    assert_eq!(stats.cas.blobs, 1);
    assert_eq!(stats.cas.stored_bytes, 512);
    assert_eq!(table.compression.compressed_pages, 0);
    assert!(stats.buffer_pool.resident_pages <= 2);
}

#[test]
fn streaming_bake_matches_bytes_and_resumes_truncated_prefix() {
    let temp = tempfile::tempdir().unwrap();
    let mut db = Database::open_dev(temp.path().join("workspace")).unwrap();
    db.set_autocommit(false);
    db.create_table("t", vec!["id".into(), "payload".into()])
        .unwrap();
    for i in 0..1500 {
        db.insert("t", vec![Value::Int(i), Value::Text("payload".repeat(80))])
            .unwrap();
    }
    db.create_index("t", "id").unwrap();
    db.set_cache_capacity(2).unwrap();
    let expected = db.bake_to_bytes().unwrap();
    let output = temp.path().join("data.pvdb");
    db.bake(&output).unwrap();
    assert_eq!(std::fs::read(&output).unwrap(), expected);
    let resumed = temp.path().join("resumed.pvdb");
    let partial = temp.path().join("resumed.pvdb.partial");
    std::fs::write(&partial, &expected[..expected.len() / 3 + 17]).unwrap();
    db.bake_resumable(&resumed).unwrap();
    assert!(!partial.exists());
    assert_eq!(std::fs::read(&resumed).unwrap(), expected);
    assert_eq!(
        Database::open_prod(&resumed)
            .unwrap()
            .row_count("t", None)
            .unwrap(),
        1500
    );
    let mut corrupt = expected[..100].to_vec();
    corrupt[50] ^= 1;
    std::fs::write(&partial, corrupt).unwrap();
    assert!(db.bake_resumable(&resumed).is_err());
    assert_eq!(std::fs::read(&resumed).unwrap(), expected);
}
