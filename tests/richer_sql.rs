//! End-to-end coverage for the 0.12.0 "richer SQL" features: `AS` aliases,
//! `SELECT DISTINCT`, `IN`/`NOT IN`, `BETWEEN`/`NOT BETWEEN`, `IS [NOT] NULL`,
//! `NOT LIKE`, multi-column `ORDER BY`, `HAVING`, and `AVG`/`SUM` over decimals.

use picovolt::{Database, Row, Value};

/// A small fixture: `t (id, name, city, age, score)` with a null age and decimal
/// scores, plus duplicate names/cities for DISTINCT and GROUP BY/HAVING.
fn fixture() -> Database {
    let mut db = Database::open_memory();
    db.query("CREATE TABLE t (id, name, city, age, score)")
        .unwrap();
    let rows = [
        "(1, 'alice', 'paris', 30, 10.50)",
        "(2, 'bob', 'berlin', 25, 20.50)",
        "(3, 'carol', 'paris', 40, 30.00)",
        "(4, 'dave', 'cairo', NULL, 5.00)",
        "(5, 'alice', 'berlin', 25, 15.00)",
    ];
    for r in rows {
        db.query(&format!("INSERT INTO t VALUES {r}")).unwrap();
    }
    db
}

fn rows(db: &mut Database, sql: &str) -> Vec<Row> {
    db.query(sql).unwrap().rows().unwrap().to_vec()
}

fn cols(db: &mut Database, sql: &str) -> Vec<String> {
    db.query(sql).unwrap().columns().unwrap().to_vec()
}

/// The `id` column of a result, sorted, for set comparisons.
fn ids(db: &mut Database, sql: &str) -> Vec<i64> {
    let mut out: Vec<i64> = rows(db, sql)
        .iter()
        .map(|r| match r[0] {
            Value::Int(i) => i,
            ref v => panic!("expected an int id, got {v:?}"),
        })
        .collect();
    out.sort_unstable();
    out
}

// --- AS aliases ---------------------------------------------------------------

#[test]
fn aliases_rename_output_columns() {
    let mut db = fixture();
    let c = cols(&mut db, "SELECT id AS uid, name AS who FROM t WHERE id = 1");
    assert_eq!(c, vec!["uid".to_string(), "who".to_string()]);
    let r = rows(&mut db, "SELECT id AS uid, name AS who FROM t WHERE id = 1");
    assert_eq!(r, vec![vec![Value::Int(1), Value::Text("alice".into())]]);
}

#[test]
fn aliases_name_aggregates() {
    let mut db = fixture();
    assert_eq!(
        cols(&mut db, "SELECT COUNT(*) AS n FROM t"),
        vec!["n".to_string()]
    );
    assert_eq!(
        rows(&mut db, "SELECT COUNT(*) AS n FROM t"),
        vec![vec![Value::Int(5)]]
    );
}

// --- DISTINCT -----------------------------------------------------------------

#[test]
fn distinct_dedups_rows() {
    let mut db = fixture();
    assert_eq!(rows(&mut db, "SELECT DISTINCT name FROM t").len(), 4); // alice once
    assert_eq!(rows(&mut db, "SELECT DISTINCT city FROM t").len(), 3);
    assert_eq!(rows(&mut db, "SELECT DISTINCT name, city FROM t").len(), 5);
    // DISTINCT plays with ORDER BY.
    let cities = rows(&mut db, "SELECT DISTINCT city FROM t ORDER BY city");
    assert_eq!(
        cities,
        vec![
            vec![Value::Text("berlin".into())],
            vec![Value::Text("cairo".into())],
            vec![Value::Text("paris".into())],
        ]
    );
}

// --- IN / NOT IN --------------------------------------------------------------

#[test]
fn in_and_not_in() {
    let mut db = fixture();
    assert_eq!(
        ids(
            &mut db,
            "SELECT id FROM t WHERE city IN ('paris', 'berlin')"
        ),
        vec![1, 2, 3, 5]
    );
    assert_eq!(
        ids(
            &mut db,
            "SELECT id FROM t WHERE city NOT IN ('paris', 'berlin')"
        ),
        vec![4]
    );
    assert_eq!(
        ids(&mut db, "SELECT id FROM t WHERE id IN (1, 3, 99)"),
        vec![1, 3]
    );
    // A null column value matches neither IN nor NOT IN.
    assert_eq!(
        ids(&mut db, "SELECT id FROM t WHERE age IN (25)"),
        vec![2, 5]
    );
    assert_eq!(
        ids(&mut db, "SELECT id FROM t WHERE age NOT IN (25)"),
        vec![1, 3] // not 4 (null age)
    );
}

// --- BETWEEN / NOT BETWEEN ----------------------------------------------------

#[test]
fn between_and_not_between() {
    let mut db = fixture();
    assert_eq!(
        ids(&mut db, "SELECT id FROM t WHERE age BETWEEN 25 AND 30"),
        vec![1, 2, 5] // 40 out of range, null excluded
    );
    // The key null-correctness case: NOT BETWEEN must NOT match the null-age row.
    assert_eq!(
        ids(&mut db, "SELECT id FROM t WHERE age NOT BETWEEN 25 AND 30"),
        vec![3] // age 40 only; id 4 (null) excluded
    );
    // BETWEEN composes with AND without the inner AND breaking precedence.
    assert_eq!(
        ids(
            &mut db,
            "SELECT id FROM t WHERE age BETWEEN 25 AND 40 AND city = 'paris'"
        ),
        vec![1, 3]
    );
}

// --- IS NULL / IS NOT NULL ----------------------------------------------------

#[test]
fn is_null_and_is_not_null() {
    let mut db = fixture();
    assert_eq!(ids(&mut db, "SELECT id FROM t WHERE age IS NULL"), vec![4]);
    assert_eq!(
        ids(&mut db, "SELECT id FROM t WHERE age IS NOT NULL"),
        vec![1, 2, 3, 5]
    );
}

// --- NOT LIKE -----------------------------------------------------------------

#[test]
fn like_and_not_like() {
    let mut db = fixture();
    assert_eq!(
        ids(&mut db, "SELECT id FROM t WHERE name LIKE 'a%'"),
        vec![1, 5]
    );
    assert_eq!(
        ids(&mut db, "SELECT id FROM t WHERE name NOT LIKE 'a%'"),
        vec![2, 3, 4]
    );
}

// --- multi-column ORDER BY ----------------------------------------------------

#[test]
fn multi_column_order_by() {
    let mut db = fixture();
    // city ASC, then id DESC within each city (no ties → deterministic).
    let got: Vec<i64> = rows(&mut db, "SELECT id FROM t ORDER BY city ASC, id DESC")
        .iter()
        .map(|r| match r[0] {
            Value::Int(i) => i,
            ref v => panic!("{v:?}"),
        })
        .collect();
    // berlin: 5,2 | cairo: 4 | paris: 3,1
    assert_eq!(got, vec![5, 2, 4, 3, 1]);
}

// --- HAVING -------------------------------------------------------------------

#[test]
fn having_filters_groups() {
    let mut db = fixture();
    // Direct aggregate reference.
    let r = rows(
        &mut db,
        "SELECT city, COUNT(*) FROM t GROUP BY city HAVING COUNT(*) > 1",
    );
    assert_eq!(
        r,
        vec![
            vec![Value::Text("berlin".into()), Value::Int(2)],
            vec![Value::Text("paris".into()), Value::Int(2)],
        ]
    );
    // Alias reference.
    let r2 = rows(
        &mut db,
        "SELECT city, COUNT(*) AS n FROM t GROUP BY city HAVING n > 1",
    );
    assert_eq!(r2.len(), 2);
    // The complement.
    let r3 = rows(
        &mut db,
        "SELECT city FROM t GROUP BY city HAVING COUNT(*) = 1",
    );
    assert_eq!(r3, vec![vec![Value::Text("cairo".into())]]);
    // HAVING on a group column.
    let r4 = rows(
        &mut db,
        "SELECT city, COUNT(*) FROM t GROUP BY city HAVING city = 'paris'",
    );
    assert_eq!(r4, vec![vec![Value::Text("paris".into()), Value::Int(2)]]);
}

// --- AVG / SUM over decimals (the correctness fix) ----------------------------

#[test]
fn avg_and_sum_over_decimals() {
    let mut db = fixture();
    // scores: 10.50 + 20.50 + 30.00 + 5.00 + 15.00 = 81.00; /5 = 16.20.
    assert_eq!(
        rows(&mut db, "SELECT AVG(score) FROM t"),
        vec![vec![Value::Decimal(16_200_000)]]
    );
    assert_eq!(
        rows(&mut db, "SELECT SUM(score) FROM t"),
        vec![vec![Value::Decimal(81_000_000)]]
    );
    // Grouped decimal averages, by group-key order (berlin, cairo, paris).
    let g = rows(&mut db, "SELECT city, AVG(score) FROM t GROUP BY city");
    assert_eq!(
        g,
        vec![
            vec![Value::Text("berlin".into()), Value::Decimal(17_750_000)],
            vec![Value::Text("cairo".into()), Value::Decimal(5_000_000)],
            vec![Value::Text("paris".into()), Value::Decimal(20_250_000)],
        ]
    );
}

#[test]
fn integer_avg_and_sum_unchanged() {
    let mut db = fixture();
    // ages: 30,25,40,(null),25 → sum 120 over 4 non-null → avg 30.00.
    assert_eq!(
        rows(&mut db, "SELECT AVG(age) FROM t"),
        vec![vec![Value::Decimal(30_000_000)]]
    );
    // A pure-integer SUM stays an integer.
    assert_eq!(
        rows(&mut db, "SELECT SUM(age) FROM t"),
        vec![vec![Value::Int(120)]]
    );
}

#[test]
fn mixed_int_and_decimal_column() {
    let mut db = Database::open_memory();
    db.query("CREATE TABLE m (v)").unwrap();
    db.query("INSERT INTO m VALUES (10)").unwrap(); // int
    db.query("INSERT INTO m VALUES (20.00)").unwrap(); // decimal
                                                       // (10 + 20.00) / 2 = 15.00; the presence of any decimal makes the result decimal.
    assert_eq!(
        rows(&mut db, "SELECT AVG(v) FROM m"),
        vec![vec![Value::Decimal(15_000_000)]]
    );
    assert_eq!(
        rows(&mut db, "SELECT SUM(v) FROM m"),
        vec![vec![Value::Decimal(30_000_000)]]
    );
}

// --- combinations & rejections ------------------------------------------------

#[test]
fn features_compose() {
    let mut db = fixture();
    let r = rows(
        &mut db,
        "SELECT id AS uid FROM t WHERE city = 'paris' ORDER BY id DESC LIMIT 1",
    );
    assert_eq!(r, vec![vec![Value::Int(3)]]);
    assert_eq!(
        cols(&mut db, "SELECT id AS uid FROM t WHERE city = 'paris'"),
        vec!["uid".to_string()]
    );
}

#[test]
fn having_can_filter_an_unselected_aggregate() {
    let mut db = fixture();
    // SUM(age) is computed per group even though it is not in the SELECT list.
    // Every group's SUM(age) is below 1000, so all groups are filtered out.
    assert_eq!(
        rows(
            &mut db,
            "SELECT city FROM t GROUP BY city HAVING SUM(age) > 1000"
        )
        .len(),
        0
    );
    // paris ages 30 + 40 = 70; berlin 25 + 25 = 50; cairo null → 0 (sum of empty).
    let r = rows(
        &mut db,
        "SELECT city FROM t GROUP BY city HAVING SUM(age) > 60",
    );
    assert_eq!(r, vec![vec![Value::Text("paris".into())]]);
}

#[test]
fn rejects_unparseable_and_unknown_columns() {
    let mut db = fixture();
    // HAVING referencing a column that is neither grouped nor aliased.
    assert!(db
        .query("SELECT city FROM t GROUP BY city HAVING bogus > 1")
        .is_err());
    // An aggregate over a non-existent column in HAVING.
    assert!(db
        .query("SELECT city FROM t GROUP BY city HAVING SUM(nope) > 1")
        .is_err());
    // A bad NOT form is a parse error, not a panic.
    assert!(db.query("SELECT * FROM t WHERE name NOT = 'x'").is_err());
    // SELECT * with HAVING (no grouping) is rejected, not silently ignored.
    assert!(db.query("SELECT * FROM t HAVING COUNT(*) > 1").is_err());
}

// --- regression fixes surfaced by the adversarial review ----------------------

#[test]
fn decimal_vs_int_comparisons_are_numeric() {
    let mut db = fixture();
    // scores: 10.50, 20.50, 30.00, 5.00, 15.00 (ids 1..5). Plain integer literals
    // must compare by magnitude, not by Value's variant tag.
    assert_eq!(
        ids(&mut db, "SELECT id FROM t WHERE score > 16"),
        vec![2, 3]
    );
    assert_eq!(
        ids(&mut db, "SELECT id FROM t WHERE score < 16"),
        vec![1, 4, 5]
    );
    assert_eq!(ids(&mut db, "SELECT id FROM t WHERE score = 30"), vec![3]);
    assert_eq!(
        ids(&mut db, "SELECT id FROM t WHERE score >= 15"),
        vec![2, 3, 5]
    );
    assert_eq!(
        ids(&mut db, "SELECT id FROM t WHERE score BETWEEN 10 AND 20"),
        vec![1, 5]
    );
    assert_eq!(
        ids(&mut db, "SELECT id FROM t WHERE score IN (30, 5)"),
        vec![3, 4]
    );
    // The symmetric direction: an Int column against a Decimal literal.
    assert_eq!(
        ids(&mut db, "SELECT id FROM t WHERE age > 24.5"),
        vec![1, 2, 3, 5]
    );
}

#[test]
fn having_decimal_aggregate_vs_int_literal() {
    let mut db = fixture();
    // berlin avg 17.75, cairo 5.00, paris 20.25. HAVING AVG(score) > 16 must keep
    // berlin and paris and DROP cairo (5.00) — the cross-type comparison bug.
    let r = rows(
        &mut db,
        "SELECT city, AVG(score) FROM t GROUP BY city HAVING AVG(score) > 16",
    );
    assert_eq!(
        r,
        vec![
            vec![Value::Text("berlin".into()), Value::Decimal(17_750_000)],
            vec![Value::Text("paris".into()), Value::Decimal(20_250_000)],
        ]
    );
}

#[test]
fn not_in_with_null_in_list_returns_nothing() {
    let mut db = fixture();
    // SQL three-valued logic: a NULL in a NOT IN list makes the predicate UNKNOWN
    // for every row, so the result is empty.
    assert_eq!(
        rows(&mut db, "SELECT id FROM t WHERE city NOT IN ('zzz', NULL)").len(),
        0
    );
    // IN with a NULL still matches a real member.
    assert_eq!(
        ids(&mut db, "SELECT id FROM t WHERE city IN ('paris', NULL)"),
        vec![1, 3]
    );
}

#[test]
fn distinct_named_column_is_selectable() {
    let mut db = Database::open_memory();
    db.query("CREATE TABLE k (a, distinct)").unwrap();
    db.query("INSERT INTO k VALUES (1, 7)").unwrap();
    db.query("INSERT INTO k VALUES (1, 7)").unwrap();
    // `distinct` as the leading selected column is a column, not the keyword.
    assert_eq!(
        rows(&mut db, "SELECT distinct FROM k"),
        vec![vec![Value::Int(7)], vec![Value::Int(7)]]
    );
    // Real SELECT DISTINCT still dedups.
    assert_eq!(rows(&mut db, "SELECT DISTINCT a FROM k").len(), 1);
}

#[test]
fn reserved_word_alias_is_rejected() {
    let mut db = fixture();
    assert!(db.query("SELECT id AS from FROM t").is_err());
    assert!(db.query("SELECT id AS where FROM t").is_err());
    assert_eq!(
        cols(&mut db, "SELECT id AS thing FROM t WHERE id = 1"),
        vec!["thing".to_string()]
    );
}

#[test]
fn large_decimal_sum_errors_rather_than_panicking() {
    let mut db = Database::open_memory();
    db.query("CREATE TABLE big (v)").unwrap();
    // Each mantissa (value * 10^6) is ~1e38, under i128::MAX (~1.7e38) so it parses;
    // their sum (~2e38) overflows, and must error cleanly rather than panic/wrap.
    let huge = "100000000000000000000000000000000.0"; // 1e32 → mantissa 1e38
    db.query(&format!("INSERT INTO big VALUES ({huge})"))
        .unwrap();
    db.query(&format!("INSERT INTO big VALUES ({huge})"))
        .unwrap();
    assert!(db.query("SELECT SUM(v) FROM big").is_err());
    assert!(db.query("SELECT AVG(v) FROM big").is_err());
}
