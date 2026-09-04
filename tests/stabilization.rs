//! 1.9 stabilization gates: adaptive join access and cooperative cold pages.

use picovolt::{Database, PvError, QueryLimits, Value, FORMAT_VERSION_COLUMNAR};

#[test]
fn bounded_three_table_join_probes_right_indexes() {
    let mut db = Database::open_memory();
    db.query("CREATE TABLE users (id PRIMARY KEY, name)")
        .unwrap();
    db.query("CREATE TABLE orders (id PRIMARY KEY, user_id)")
        .unwrap();
    db.query("CREATE TABLE items (order_id, label)").unwrap();
    db.query("INSERT INTO users VALUES (7, 'Ada')").unwrap();
    for id in 0..1_000i64 {
        db.insert(
            "orders",
            vec![Value::Int(id), Value::Int(if id == 512 { 7 } else { -1 })],
        )
        .unwrap();
        db.insert(
            "items",
            vec![
                Value::Int(id),
                Value::Text(if id == 512 { "wanted" } else { "other" }.into()),
            ],
        )
        .unwrap();
    }
    db.query("CREATE INDEX ON orders (user_id)").unwrap();
    db.query("CREATE INDEX ON items (order_id)").unwrap();

    // Scanning either 1,000-row right relation would exceed this allowance. The
    // successful result therefore proves both right inputs used candidate probes.
    let result = db
        .query_with_limits(
            "SELECT u.name, i.label FROM users u \
             JOIN orders o ON u.id = o.user_id \
             JOIN items i ON o.id = i.order_id",
            &[],
            QueryLimits::new(16, 1_000_000, 10, None),
        )
        .unwrap();
    assert_eq!(
        result.rows().unwrap(),
        &[vec![
            Value::Text("Ada".into()),
            Value::Text("wanted".into())
        ]]
    );

    let plan = db
        .explain(
            "SELECT u.name FROM users u JOIN orders o ON u.id = o.user_id \
             JOIN items i ON o.id = i.order_id",
        )
        .unwrap();
    let operations = plan
        .rows()
        .unwrap()
        .iter()
        .filter_map(|row| row.get(1))
        .map(ToString::to_string)
        .collect::<Vec<_>>();
    assert_eq!(
        operations
            .iter()
            .filter(|operation| operation.contains("adaptive indexed"))
            .count(),
        2
    );
}

#[test]
fn indexed_join_preserves_numeric_null_and_historical_semantics() {
    let mut db = Database::open_memory();
    db.query("CREATE TABLE left_rows (id PRIMARY KEY, join_key)")
        .unwrap();
    db.query("CREATE TABLE right_rows (join_key, label)")
        .unwrap();
    db.query("INSERT INTO left_rows VALUES (1, 1.000000), (2, NULL), (3, 2);")
        .unwrap();
    db.query("INSERT INTO right_rows VALUES (1, 'old'), (2.000000, 'decimal')")
        .unwrap();
    db.query("CREATE INDEX ON right_rows (join_key)").unwrap();
    let historical = db.current_tx();
    db.query("UPDATE right_rows SET label = 'new' WHERE join_key = 1")
        .unwrap();

    let historical_sql = format!(
        "SELECT l.id, r.label FROM left_rows l \
         LEFT JOIN right_rows r ON l.join_key = r.join_key \
         BEFORE {historical} ORDER BY l.id"
    );
    let limits = QueryLimits::new(64, 1_000_000, 1_000, None);
    let historical_rows = db
        .query_with_limits(&historical_sql, &[], limits)
        .unwrap()
        .rows()
        .unwrap()
        .to_vec();
    assert_eq!(
        historical_rows,
        vec![
            vec![Value::Int(1), Value::Text("old".into())],
            vec![Value::Int(2), Value::Null],
            vec![Value::Int(3), Value::Text("decimal".into())],
        ]
    );

    let latest = db
        .query_with_limits(
            "SELECT l.id, r.label FROM left_rows l \
             LEFT JOIN right_rows r ON l.join_key = r.join_key ORDER BY l.id",
            &[],
            limits,
        )
        .unwrap();
    assert_eq!(
        latest.rows().unwrap(),
        &[
            vec![Value::Int(1), Value::Text("new".into())],
            vec![Value::Int(2), Value::Null],
            vec![Value::Int(3), Value::Text("decimal".into())],
        ]
    );
}

#[test]
fn indexed_self_join_matches_the_unindexed_build_plan() {
    let mut db = Database::open_memory();
    db.query("CREATE TABLE employees (id PRIMARY KEY, manager_id, name)")
        .unwrap();
    db.query(
        "INSERT INTO employees VALUES \
         (1, NULL, 'Ada'), (2, 1, 'Lin'), (3, 1, 'Grace')",
    )
    .unwrap();
    let sql = "SELECT e.name, m.name FROM employees e \
               LEFT JOIN employees m ON e.manager_id = m.id ORDER BY e.id";
    let expected = db.query(sql).unwrap().rows().unwrap().to_vec();

    db.query("CREATE INDEX ON employees (id)").unwrap();
    let indexed = db
        .query_with_limits(sql, &[], QueryLimits::new(32, 1_000_000, 100, None))
        .unwrap();
    assert_eq!(indexed.rows().unwrap(), expected);
}

#[test]
fn bounded_join_accounts_for_owned_probe_keys() {
    let mut db = Database::open_memory();
    db.query("CREATE TABLE left_rows (k)").unwrap();
    db.query("CREATE TABLE right_rows (k)").unwrap();
    let key = "x".repeat(1_000);
    db.insert("left_rows", vec![Value::Text(key.clone())])
        .unwrap();
    db.insert("right_rows", vec![Value::Text(key)]).unwrap();
    db.query("CREATE INDEX ON right_rows (k)").unwrap();

    let result = db.query_with_limits(
        "SELECT * FROM left_rows l JOIN right_rows r ON l.k = r.k",
        &[],
        QueryLimits::new(10, 1_500, 10, None),
    );
    assert!(matches!(result, Err(PvError::ResourceLimit(_))));
}

#[test]
fn cold_compaction_preserves_history_indexes_and_mutability() {
    let mut db = Database::open_memory();
    db.query("CREATE TABLE ledger (id PRIMARY KEY, state, amount)")
        .unwrap();
    for id in 0..600i64 {
        db.insert(
            "ledger",
            vec![
                Value::Int(id),
                Value::Text(if id % 2 == 0 { "open" } else { "closed" }.into()),
                Value::Decimal((id % 5) as i128 * 1_000_000),
            ],
        )
        .unwrap();
    }
    let snapshot = db.current_tx();
    let before_hash = db.verification_hash().unwrap();
    let report = db.compact_step(128).unwrap();
    assert!(report.compacted_pages > 0, "{report:?}");
    assert!(report.saved_bytes > 0, "{report:?}");
    assert_eq!(db.verification_hash().unwrap(), before_hash);

    let stats = db.inspect_stats().unwrap();
    assert_eq!(stats.format_version, FORMAT_VERSION_COLUMNAR);
    assert!(stats.tables[0].compression.compressed_pages > 0);
    assert!(stats.tables[0].compression.saved_bytes > 0);

    // Row 0 lives on an old page. Updating it patches the cold-page MVCC
    // envelope, appends the replacement to the hot tail, and keeps time travel.
    db.query("UPDATE ledger SET state = 'settled' WHERE id = 0")
        .unwrap();
    assert_eq!(
        db.query("SELECT state FROM ledger WHERE id = 0")
            .unwrap()
            .rows()
            .unwrap(),
        &[vec![Value::Text("settled".into())]]
    );
    assert_eq!(
        db.query(&format!(
            "SELECT state FROM ledger WHERE id = 0 BEFORE {snapshot}"
        ))
        .unwrap()
        .rows()
        .unwrap(),
        &[vec![Value::Text("open".into())]]
    );

    let image = db.bake_to_bytes().unwrap();
    let mut reopened = Database::import_bytes(&image).unwrap();
    assert_eq!(
        reopened
            .query("SELECT COUNT(*) FROM ledger")
            .unwrap()
            .rows()
            .unwrap(),
        &[vec![Value::Int(600)]]
    );
    reopened.query("DELETE FROM ledger WHERE id = 1").unwrap();
    assert_eq!(reopened.row_count("ledger", None).unwrap(), 599);
}

#[test]
fn incremental_compaction_persists_in_development_workspaces() {
    let temp = tempfile::tempdir().unwrap();
    let workspace = temp.path().join("cold.pv");
    {
        let mut database = Database::open_dev(&workspace).unwrap();
        database.set_autocommit(false);
        database
            .query("CREATE TABLE events (id PRIMARY KEY, kind)")
            .unwrap();
        for id in 0..700i64 {
            database
                .insert("events", vec![Value::Int(id), Value::Text("repeat".into())])
                .unwrap();
        }
        database.query("CREATE INDEX ON events (kind)").unwrap();
        database.flush_now().unwrap();
        let first = database.compact_step(1).unwrap();
        let second = database.compact_step(1).unwrap();
        assert_eq!(first.compacted_pages, 1, "{first:?}");
        assert_eq!(second.compacted_pages, 1, "{second:?}");
        database.flush_now().unwrap();
    }

    let mut reopened = Database::open_dev(&workspace).unwrap();
    let stats = reopened.inspect_stats().unwrap();
    assert_eq!(stats.format_version, FORMAT_VERSION_COLUMNAR);
    assert!(stats.tables[0].compression.compressed_pages >= 2);
    assert_eq!(
        reopened
            .query("SELECT COUNT(*) FROM events WHERE kind = 'repeat'")
            .unwrap()
            .rows()
            .unwrap(),
        &[vec![Value::Int(700)]]
    );
    reopened
        .query("UPDATE events SET kind = 'updated' WHERE id = 0")
        .unwrap();
    assert_eq!(
        reopened
            .query("SELECT kind FROM events WHERE id = 0")
            .unwrap()
            .rows()
            .unwrap(),
        &[vec![Value::Text("updated".into())]]
    );
}

#[test]
fn bounded_compaction_resumes_after_an_incompressible_page_and_reopen() {
    let temp = tempfile::tempdir().unwrap();
    let workspace = temp.path().join("resume.pv");
    {
        let mut database = Database::open_dev(&workspace).unwrap();
        database.set_autocommit(false);
        database.query("CREATE TABLE events (id, payload)").unwrap();
        for id in 0..100i64 {
            database
                .insert(
                    "events",
                    vec![
                        Value::Int(id),
                        Value::Text(format!("{}-{id}", "not-columnar-friendly".repeat(16))),
                    ],
                )
                .unwrap();
        }
        for id in 100..800i64 {
            database
                .insert("events", vec![Value::Int(id), Value::Text("repeat".into())])
                .unwrap();
        }
        database.flush_now().unwrap();
        let first = database.compact_step(1).unwrap();
        assert_eq!(first.compacted_pages, 0, "{first:?}");
        assert_eq!(first.skipped_pages, 1, "{first:?}");
        assert!(!database.in_transaction());
    }

    // The persisted cursor must advance beyond the skipped first page even when
    // a separate maintenance process opens the workspace for the next slice.
    let mut reopened = Database::open_dev(&workspace).unwrap();
    let mut compacted = 0;
    for _ in 0..4 {
        compacted += reopened.compact_step(1).unwrap().compacted_pages;
        if compacted > 0 {
            break;
        }
    }
    assert!(compacted > 0, "compaction kept retrying the leading page");
    assert_eq!(reopened.row_count("events", None).unwrap(), 800);
}
