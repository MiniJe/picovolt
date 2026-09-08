use picovolt::{CommitLogOptions, Database, QueryLimits, Value, COMMIT_LOG_DIR, MANIFEST_FILE};

fn logged(root: &std::path::Path) -> Database {
    let mut db = Database::open_dev(root).unwrap();
    db.enable_commit_log(CommitLogOptions::default()).unwrap();
    db.query("CREATE TABLE t (id PRIMARY KEY, name UNIQUE)")
        .unwrap();
    db.query("INSERT INTO t VALUES (1, 'first')").unwrap();
    db.query("INSERT INTO t VALUES (2, 'second')").unwrap();
    db
}

#[test]
fn missing_history_never_reuses_acknowledged_sequences() {
    for missing in ["00000000000000000003", "00000000000000000002", "all"] {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().join("workspace");
        let mut db = logged(&root);
        let before = std::fs::read(root.join(MANIFEST_FILE)).unwrap();
        let log = root.join(COMMIT_LOG_DIR);
        let source = if missing == "all" {
            log
        } else {
            log.join(missing)
        };
        std::fs::rename(source, temp.path().join("preserved-missing-history")).unwrap();
        assert!(db.changes_since(3, 10).is_err());
        assert!(db.query("INSERT INTO t VALUES (3, 'third')").is_err());
        drop(db);
        assert!(Database::open_dev(&root).is_err());
        assert_eq!(std::fs::read(root.join(MANIFEST_FILE)).unwrap(), before);
    }
}

#[test]
fn pruning_rollback_and_reopen_preserve_the_anchor() {
    let temp = tempfile::tempdir().unwrap();
    let mut db = logged(temp.path());
    assert!(db.changes_since(4, 10).is_err());
    db.prune_changes(3).unwrap();
    drop(db);
    let mut db = Database::open_dev(temp.path()).unwrap();
    db.begin_transaction().unwrap();
    db.query("INSERT INTO t VALUES (3, 'rolled back')").unwrap();
    db.rollback_transaction().unwrap();
    assert!(db.changes_since(3, 10).unwrap().is_empty());
    db.query("INSERT INTO t VALUES (3, 'third')").unwrap();
    assert_eq!(db.changes_since(3, 10).unwrap()[0].sequence, 4);
    drop(db);
    let db = Database::open_dev(temp.path()).unwrap();
    assert_eq!(db.changes_since(3, 10).unwrap()[0].sequence, 4);
}

#[test]
fn trigger_bodies_are_skipped_whole_and_comments_preserve_following_sql() {
    for qualifier in ["", "TEMP ", "TEMPORARY "] {
        let mut db = Database::open_memory();
        let dump = format!("-- SQLite dump\nBEGIN TRANSACTION;\nCREATE TABLE t(id INTEGER, name TEXT);\n/* before data */ INSERT INTO t VALUES(1, 'original; -- /*');\nCREATE {qualifier}TRIGGER t_log AFTER INSERT ON t BEGIN\n INSERT INTO t VALUES(2, CASE WHEN 1=1 THEN 'END;' ELSE 'BEGIN' END);\n UPDATE t SET name='triggered' WHERE id=1;\nEND;\nCOMMIT;");
        let report = db.import_sql(&dump);
        assert!(report.errors.is_empty(), "{report:?}");
        assert_eq!(report.executed, 2);
        assert_eq!(report.skipped.len(), 3);
        assert_eq!(
            db.query("SELECT name FROM t").unwrap().rows().unwrap(),
            &[vec![Value::Text("original; -- /*".into())]]
        );
    }
}

#[test]
fn incomplete_dump_is_rejected_before_any_mutations() {
    for suffix in [
        "CREATE TRIGGER tr AFTER INSERT ON t BEGIN INSERT INTO t VALUES(2);",
        "INSERT INTO t VALUES('unterminated",
        "/* unterminated",
    ] {
        let mut db = Database::open_memory();
        let report = db.import_sql(&format!("CREATE TABLE t(id); {suffix}"));
        assert_eq!(report.executed, 0);
        assert_eq!(report.errors.len(), 1);
        assert!(db.query("SELECT * FROM t").is_err());
    }
}

#[test]
fn constraint_indexes_survive_reopen_and_preserve_constraint_semantics() {
    let temp = tempfile::tempdir().unwrap();
    let mut db = Database::open_dev(temp.path()).unwrap();
    db.query("CREATE TABLE t(id PRIMARY KEY, name UNIQUE)")
        .unwrap();
    for id in 0..100 {
        db.query(&format!("INSERT INTO t VALUES({id}, NULL)"))
            .unwrap();
    }
    drop(db);
    let mut db = Database::open_dev(temp.path()).unwrap();
    let limits = QueryLimits::new(3, usize::MAX, usize::MAX, None);
    assert_eq!(
        db.query_with_limits("SELECT id FROM t WHERE id=50", &[], limits)
            .unwrap()
            .rows()
            .unwrap(),
        &[vec![Value::Int(50)]]
    );
    assert!(db.query("INSERT INTO t VALUES(50.0, 'duplicate')").is_err());
    db.query("DELETE FROM t WHERE id=50").unwrap();
    db.query("INSERT INTO t VALUES(50, 'replacement')").unwrap();
    assert!(db.query("UPDATE t SET id=50 WHERE id=51").is_err());
    assert_eq!(db.row_count("t", None).unwrap(), 100);
}

#[test]
fn legacy_workspace_rebuilds_missing_constraint_indexes() {
    let temp = tempfile::tempdir().unwrap();
    let mut db = Database::open_dev(temp.path()).unwrap();
    db.query("CREATE TABLE t(id PRIMARY KEY)").unwrap();
    for id in 0..100 {
        db.query(&format!("INSERT INTO t VALUES({id})")).unwrap();
    }
    drop(db);
    let path = temp.path().join(MANIFEST_FILE);
    let mut manifest: serde_json::Value =
        serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
    let table = manifest["tables"][0].as_object_mut().unwrap();
    table.remove("indexes");
    table.remove("indexed_columns");
    std::fs::write(&path, serde_json::to_vec(&manifest).unwrap()).unwrap();
    let mut db = Database::open_dev(temp.path()).unwrap();
    assert!(db
        .query_with_limits(
            "SELECT * FROM t WHERE id=50",
            &[],
            QueryLimits::new(3, usize::MAX, usize::MAX, None)
        )
        .is_ok());
    assert!(db.query("INSERT INTO t VALUES(50)").is_err());
}

#[test]
fn batch_unique_checks_are_bounded_and_numeric_aware() {
    let mut db = Database::open_memory();
    db.query("CREATE TABLE t(id PRIMARY KEY, name UNIQUE)")
        .unwrap();
    let rows = (0..100)
        .map(|i| format!("({i}, NULL)"))
        .collect::<Vec<_>>()
        .join(",");
    db.query_with_limits(
        &format!("INSERT INTO t VALUES {rows}"),
        &[],
        QueryLimits::new(150, usize::MAX, usize::MAX, None),
    )
    .unwrap();
    assert!(db
        .query("INSERT INTO t VALUES(101, 'x'), (101.0, 'y')")
        .is_err());
    assert!(db
        .query("INSERT INTO t(id, name) VALUES(102, 'x'), (103, 'x')")
        .is_err());
    assert_eq!(db.row_count("t", None).unwrap(), 100);
}
