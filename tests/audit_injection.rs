use picovolt::core::value::Value;
use picovolt::Database;

#[test]
fn migrator_quoted_identifier_cannot_inject_a_statement() {
    let mut db = Database::open_memory();
    db.query("CREATE TABLE secret (a)").unwrap();
    db.query("CREATE TABLE t (a)").unwrap();
    // The semicolon and apostrophe are both identifier data. They must never
    // become statement syntax while the dump is split or rewritten.
    let dump = r#"INSERT INTO t VALUES ('a'); DROP TABLE "secret'); "#;
    let report = db.import_sql(dump);
    assert_eq!(report.executed, 1, "{report:?}");
    assert_eq!(report.errors.len(), 1, "{report:?}");
    assert!(db.query("SELECT COUNT(*) FROM secret").is_ok());
}

#[test]
fn migrator_preserves_a_quoted_identifier_with_an_apostrophe() {
    let mut db = Database::open_memory();
    let dump = r#"
CREATE TABLE "people" ("o'brien" TEXT);
INSERT INTO "people" ("o'brien") VALUES ('Ada');
"#;
    let report = db.import_sql(dump);
    assert_eq!(report.executed, 2, "{report:?}");
    assert!(report.errors.is_empty(), "{report:?}");
    let rows = db
        .query(r#"SELECT "o'brien" FROM "people""#)
        .unwrap()
        .rows()
        .unwrap()
        .to_vec();
    assert_eq!(rows, vec![vec![Value::Text("Ada".into())]]);
}

#[test]
fn migrator_preserves_quoted_keyword_columns() {
    let mut db = Database::open_memory();
    let dump = r#"CREATE TABLE t ("unique" TEXT, "check" TEXT, real_col INT);"#;
    let report = db.import_sql(dump);
    assert_eq!(report.executed, 1, "{report:?}");
    assert!(report.errors.is_empty(), "{report:?}");
    db.query("INSERT INTO t VALUES ('x', 'y', 1)").unwrap();
    let result = db
        .query(r#"SELECT "unique", "check", real_col FROM t"#)
        .unwrap();
    assert_eq!(
        result.columns().unwrap(),
        &[
            "unique".to_string(),
            "check".to_string(),
            "real_col".to_string()
        ]
    );
    assert_eq!(
        result.rows().unwrap(),
        &[vec![
            Value::Text("x".into()),
            Value::Text("y".into()),
            Value::Int(1)
        ]]
    );
}

#[test]
fn decimal_min_round_trip() {
    let mut db = Database::open_memory();
    db.query("CREATE TABLE t (d)").unwrap();
    db.query_with("INSERT INTO t VALUES (?)", &[Value::Decimal(i128::MIN)])
        .unwrap();
    assert_eq!(
        db.query("SELECT d FROM t").unwrap().rows().unwrap(),
        &[vec![Value::Decimal(i128::MIN)]]
    );
}
