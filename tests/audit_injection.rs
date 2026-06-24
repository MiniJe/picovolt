use picovolt::core::value::Value;
use picovolt::Database;

#[test]
fn migrator_quoted_identifier_injects_quote() {
    let mut db = Database::open_memory();
    db.query("CREATE TABLE secret (a)").unwrap();
    db.query("CREATE TABLE t (a)").unwrap();
    // A quoted identifier whose CONTENT contains a single quote. unquote_double_quotes
    // strips the double quotes but leaves the apostrophe raw in the output, then the
    // statement is handed to query() which lexes the apostrophe as a string start.
    let dump = r#"INSERT INTO t VALUES ('a'); DROP TABLE "secret'); "#;
    println!("--- dump: {dump}");
    let report = db.import_sql(dump);
    println!("report = {report:?}");
    let still = db.query("SELECT COUNT(*) FROM secret");
    println!("secret table still queryable: {:?}", still.is_ok());
}

#[test]
fn migrator_quoted_identifier_with_apostrophe_breaks_lexing() {
    let mut db = Database::open_memory();
    // A double-quoted identifier containing an apostrophe. After unquoting, the bare
    // apostrophe corrupts subsequent statement splitting / lexing.
    let dump = r#"SELECT "o'brien" FROM t; SELECT 1 FROM t;"#;
    let report = db.import_sql(dump);
    println!("report = {report:?}");
}

#[test]
fn migrator_keyword_column_dropped() {
    let mut db = Database::open_memory();
    let dump = r#"CREATE TABLE t ("unique" TEXT, "check" TEXT, real_col INT);"#;
    let report = db.import_sql(dump);
    println!("report = {report:?}");
    let res = db.query("SELECT * FROM t");
    match res {
        Ok(r) => println!("cols = {:?}", r.columns().map(|c| c.to_vec())),
        Err(e) => println!("select err = {e}"),
    }
    // Also: an INSERT against the table now mismatches the surviving column count.
    let ins = db.query("INSERT INTO t VALUES ('x', 'y', 1)");
    println!(
        "insert 3-value: {:?}",
        ins.map(|_| "ok".to_string()).map_err(|e| e.to_string())
    );
}

#[test]
fn decimal_min_round_trip() {
    let mut db = Database::open_memory();
    db.query("CREATE TABLE t (d)").unwrap();
    let res = db.query_with("INSERT INTO t VALUES (?)", &[Value::Decimal(i128::MIN)]);
    println!(
        "i128::MIN decimal insert: {:?}",
        res.map(|_| "ok".to_string()).map_err(|e| e.to_string())
    );
}
