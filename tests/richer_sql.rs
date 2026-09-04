//! End-to-end coverage for the 0.12.0 "richer SQL" features: `AS` aliases,
//! `SELECT DISTINCT`, `IN`/`NOT IN`, `BETWEEN`/`NOT BETWEEN`, `IS [NOT] NULL`,
//! `NOT LIKE`, multi-column `ORDER BY`, `HAVING`, and `AVG`/`SUM` over decimals.

use picovolt::{Database, QueryResult, Row, Value};

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

#[test]
fn ordinary_comparisons_and_case_use_sql_null_semantics() {
    let mut db = fixture();
    assert!(rows(&mut db, "SELECT id FROM t WHERE age = NULL").is_empty());
    assert!(rows(&mut db, "SELECT id FROM t WHERE age != NULL").is_empty());
    assert_eq!(ids(&mut db, "SELECT id FROM t WHERE age != 25"), vec![1, 3]);
    assert_eq!(
        rows(
            &mut db,
            "SELECT CASE WHEN age != 1 THEN 'known' ELSE 'unknown' END FROM t WHERE id = 4"
        ),
        vec![vec![Value::Text("unknown".into())]]
    );

    // An empty aggregate is NULL, so its HAVING comparison is UNKNOWN rather
    // than true merely because NULL is structurally different from an integer.
    assert!(rows(
        &mut db,
        "SELECT SUM(age) FROM t WHERE id = 999 HAVING SUM(age) != 1"
    )
    .is_empty());
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

#[test]
fn count_star_fast_path_matches_scan_including_time_travel() {
    let mut db = fixture(); // 5 rows, each inserted in its own transaction
                            // The bare-COUNT(*) fast path must agree with a full scan's visible-row count,
                            // at the latest snapshot and at every past transaction.
    for tx in [0u64, 1, 2, 3, 4, 5, 99] {
        let fast = match rows(&mut db, &format!("SELECT COUNT(*) FROM t BEFORE {tx}"))[0][0] {
            Value::Int(i) => i,
            ref v => panic!("expected int count, got {v:?}"),
        };
        let scanned = rows(&mut db, &format!("SELECT id FROM t BEFORE {tx}")).len() as i64;
        assert_eq!(
            fast, scanned,
            "COUNT(*) fast path != scan count at BEFORE {tx}"
        );
    }
    // Current count and alias still work.
    assert_eq!(
        rows(&mut db, "SELECT COUNT(*) FROM t"),
        vec![vec![Value::Int(5)]]
    );
    assert_eq!(
        cols(&mut db, "SELECT COUNT(*) AS n FROM t"),
        vec!["n".to_string()]
    );
}

#[test]
fn equality_inner_and_left_joins() {
    let mut db = Database::open_memory();
    db.query("CREATE TABLE users (id, name)").unwrap();
    db.query("CREATE TABLE orders (user_id, item)").unwrap();
    db.query("INSERT INTO users VALUES (1, 'Ada')").unwrap();
    db.query("INSERT INTO users VALUES (2, 'Lin')").unwrap();
    db.query("INSERT INTO orders VALUES (1, 'book')").unwrap();
    db.query("INSERT INTO orders VALUES (1, 'pen')").unwrap();

    assert_eq!(
        rows(
            &mut db,
            "SELECT * FROM users INNER JOIN orders ON id = user_id",
        ),
        vec![
            vec![
                Value::Int(1),
                Value::from("Ada"),
                Value::Int(1),
                Value::from("book")
            ],
            vec![
                Value::Int(1),
                Value::from("Ada"),
                Value::Int(1),
                Value::from("pen")
            ],
        ]
    );
    let result = db
        .query("SELECT * FROM users LEFT JOIN orders ON id = user_id")
        .unwrap();
    assert_eq!(
        result.columns().unwrap(),
        ["users.id", "users.name", "orders.user_id", "orders.item"]
    );
    assert_eq!(result.rows().unwrap().len(), 3);
    assert_eq!(result.rows().unwrap()[2][2], Value::Null);
}

#[test]
fn offset_multi_insert_and_join_projection_compose() {
    let mut db = Database::open_memory();
    db.query("CREATE TABLE users (id PRIMARY KEY, name)")
        .unwrap();
    db.query("INSERT INTO users VALUES (1, 'alice'), (2, 'bob'), (3, 'cara')")
        .unwrap();
    db.query("CREATE TABLE orders (user_id, item)").unwrap();
    db.query("INSERT INTO orders VALUES (1, 'book'), (2, 'pen'), (2, 'pad')")
        .unwrap();

    let QueryResult::Rows {
        columns,
        rows: joined_rows,
    } = db
        .query("SELECT name, item FROM users JOIN orders ON id = user_id")
        .unwrap()
    else {
        panic!("expected rows")
    };
    assert_eq!(columns, vec!["name", "item"]);
    assert_eq!(joined_rows.len(), 3);

    let QueryResult::Rows {
        rows: page_rows, ..
    } = db
        .query("SELECT * FROM users ORDER BY id LIMIT 1 OFFSET 1")
        .unwrap()
    else {
        panic!("expected rows")
    };
    assert_eq!(
        page_rows,
        vec![vec![Value::Int(2), Value::Text("bob".into())]]
    );

    let QueryResult::Rows {
        rows: distinct_rows,
        ..
    } = db
        .query("SELECT DISTINCT name FROM users JOIN orders ON id = user_id")
        .unwrap()
    else {
        panic!("expected rows")
    };
    assert_eq!(distinct_rows.len(), 2);

    let result = db
        .query(
            "SELECT users.name AS person, orders.item \
             FROM users JOIN orders ON users.id = orders.user_id \
             WHERE orders.item != 'book' ORDER BY orders.item DESC LIMIT 1 OFFSET 0",
        )
        .unwrap();
    assert_eq!(result.columns().unwrap(), ["person", "orders.item"]);
    assert_eq!(
        result.rows().unwrap(),
        &[vec![Value::Text("bob".into()), Value::Text("pen".into())]]
    );

    assert_eq!(
        rows(
            &mut db,
            "SELECT name FROM users LEFT JOIN orders ON users.id = orders.user_id \
             WHERE orders.item IS NULL",
        ),
        vec![vec![Value::Text("cara".into())]]
    );

    assert!(db
        .query("INSERT INTO users VALUES (4, 'dan'), (4, 'duplicate')")
        .is_err());
    assert!(rows(&mut db, "SELECT * FROM users WHERE id = 4").is_empty());

    db.query("CREATE TABLE decimal_keys (id, label)").unwrap();
    db.query("INSERT INTO decimal_keys VALUES (2.0, 'numeric match')")
        .unwrap();
    assert_eq!(
        rows(
            &mut db,
            "SELECT name, label FROM users JOIN decimal_keys ON id = id",
        ),
        vec![vec![
            Value::Text("bob".into()),
            Value::Text("numeric match".into()),
        ]]
    );
    assert!(db
        .query("SELECT id FROM users JOIN decimal_keys ON users.id = decimal_keys.id")
        .unwrap_err()
        .to_string()
        .contains("ambiguous column `id`"));
}

#[test]
fn table_aliases_make_self_joins_safe_and_accept_either_operand_order() {
    let mut db = Database::open_memory();
    db.query("CREATE TABLE employees (id PRIMARY KEY, manager_id, name)")
        .unwrap();
    db.query(
        "INSERT INTO employees VALUES \
         (1, NULL, 'Ada'), (2, 1, 'Lin'), (3, 1, 'Grace')",
    )
    .unwrap();

    assert_eq!(
        rows(&mut db, "SELECT e.name FROM employees AS e WHERE e.id = 2",),
        vec![vec![Value::Text("Lin".into())]]
    );

    // The new relation may appear first in ON (`manager.id = employee.manager_id`).
    let result = db
        .query(
            "SELECT e.name AS employee, m.name AS manager \
             FROM employees e LEFT JOIN employees AS m ON m.id = e.manager_id \
             ORDER BY e.id",
        )
        .unwrap();
    assert_eq!(result.columns().unwrap(), ["employee", "manager"]);
    assert_eq!(
        result.rows().unwrap(),
        &[
            vec![Value::Text("Ada".into()), Value::Null],
            vec![Value::Text("Lin".into()), Value::Text("Ada".into())],
            vec![Value::Text("Grace".into()), Value::Text("Ada".into())],
        ]
    );

    let duplicate = db
        .query("SELECT * FROM employees e JOIN employees e ON e.id = e.manager_id")
        .unwrap_err()
        .to_string();
    assert!(
        duplicate.contains("duplicate table qualifier `e`"),
        "{duplicate}"
    );

    let ambiguous = db
        .query("SELECT id FROM employees e JOIN employees m ON e.manager_id = m.id")
        .unwrap_err()
        .to_string();
    assert!(ambiguous.contains("ambiguous column `id`"), "{ambiguous}");
}

#[test]
fn n_table_inner_left_and_grouped_joins_compose() {
    let mut db = Database::open_memory();
    db.query("CREATE TABLE users (id PRIMARY KEY, name)")
        .unwrap();
    db.query("CREATE TABLE orders (id PRIMARY KEY, user_id)")
        .unwrap();
    db.query("CREATE TABLE items (order_id, label)").unwrap();
    db.query("INSERT INTO users VALUES (1, 'Ada'), (2, 'Lin'), (3, 'Moe')")
        .unwrap();
    db.query("INSERT INTO orders VALUES (10, 1), (20, 2)")
        .unwrap();
    db.query("INSERT INTO items VALUES (10, 'book'), (10, 'pen')")
        .unwrap();

    assert_eq!(
        rows(
            &mut db,
            "SELECT u.name, o.id, i.label FROM users u \
             JOIN orders o ON u.id = o.user_id \
             JOIN items i ON i.order_id = o.id ORDER BY i.label",
        ),
        vec![
            vec![
                Value::Text("Ada".into()),
                Value::Int(10),
                Value::Text("book".into()),
            ],
            vec![
                Value::Text("Ada".into()),
                Value::Int(10),
                Value::Text("pen".into()),
            ],
        ]
    );

    assert_eq!(
        rows(
            &mut db,
            "SELECT u.name, o.id, i.label FROM users u \
             LEFT JOIN orders o ON u.id = o.user_id \
             LEFT JOIN items i ON o.id = i.order_id ORDER BY u.id, i.label",
        ),
        vec![
            vec![
                Value::Text("Ada".into()),
                Value::Int(10),
                Value::Text("book".into()),
            ],
            vec![
                Value::Text("Ada".into()),
                Value::Int(10),
                Value::Text("pen".into()),
            ],
            vec![Value::Text("Lin".into()), Value::Int(20), Value::Null],
            vec![Value::Text("Moe".into()), Value::Null, Value::Null],
        ]
    );

    assert_eq!(
        rows(
            &mut db,
            "SELECT COUNT(*) AS n FROM users u JOIN orders o ON u.id = o.user_id",
        ),
        vec![vec![Value::Int(2)]]
    );
    assert_eq!(
        rows(
            &mut db,
            "SELECT u.name, COUNT(i.label) AS item_count FROM users u \
             LEFT JOIN orders o ON u.id = o.user_id \
             LEFT JOIN items i ON o.id = i.order_id \
             GROUP BY u.name HAVING COUNT(i.label) > 0 ORDER BY u.name",
        ),
        vec![vec![Value::Text("Ada".into()), Value::Int(2)]]
    );

    let ambiguous_on = db
        .query(
            "SELECT * FROM users u JOIN orders o ON u.id = o.user_id \
             JOIN items i ON id = i.order_id",
        )
        .unwrap_err()
        .to_string();
    assert!(
        ambiguous_on.contains("ambiguous join column `id`"),
        "{ambiguous_on}"
    );
}

#[test]
fn conditional_table_ddl_is_idempotent() {
    let mut db = Database::open_memory();
    db.query("CREATE TABLE IF NOT EXISTS cache (id PRIMARY KEY)")
        .unwrap();
    db.query("CREATE TABLE IF NOT EXISTS cache (ignored)")
        .unwrap();
    assert_eq!(db.column_names("cache").unwrap(), ["id"]);
    db.query("DROP TABLE IF EXISTS missing").unwrap();
    db.query("DROP TABLE IF EXISTS cache").unwrap();
    assert!(!db.table_names().contains(&"cache".to_string()));
}

#[test]
fn reusable_prepared_statement_validates_arity() {
    let mut db = Database::open_memory();
    db.query("CREATE TABLE users (id, name)").unwrap();
    let insert = db.prepare("INSERT INTO users VALUES (?, ?)").unwrap();
    assert_eq!(insert.parameter_count(), 2);
    insert
        .execute(&mut db, &[Value::Int(1), Value::from("Ada")])
        .unwrap();
    assert!(insert.execute(&mut db, &[Value::Int(2)]).is_err());
}

#[test]
fn case_and_scalar_functions_compose_in_projection() {
    let mut db = fixture();
    let result = db
        .query(
            "SELECT id, UPPER(TRIM(name)) AS display_name, LENGTH(name) AS chars, \
             ABS(age) AS magnitude, COALESCE(NULLIF(city, 'paris'), 'home') AS region, \
             CASE WHEN score >= 20 THEN 'high' WHEN score >= 10 THEN LOWER(city) \
                  ELSE 'low' END AS band \
             FROM t ORDER BY id",
        )
        .unwrap();
    let rows = result.rows().unwrap();
    assert_eq!(rows.len(), 5);
    assert_eq!(rows[0][1], Value::Text("ALICE".into()));
    assert_eq!(rows[0][2], Value::Int(5));
    assert_eq!(rows[0][3], Value::Int(30));
    assert_eq!(rows[0][4], Value::Text("home".into()));
    assert_eq!(rows[0][5], Value::Text("paris".into()));
    assert_eq!(rows[1][5], Value::Text("high".into()));
    assert_eq!(
        cols(&mut db, "SELECT LOWER(name), 7, NULL FROM t LIMIT 1"),
        vec!["lower(name)", "7", "NULL"]
    );
}

#[test]
fn scalar_functions_are_null_propagating_and_type_checked() {
    let mut db = fixture();
    assert_eq!(
        rows(
            &mut db,
            "SELECT CASE WHEN age > 100 THEN 'old' END, ABS(age) FROM t WHERE id = 4",
        ),
        vec![vec![Value::Null, Value::Null]]
    );
    assert_eq!(
        rows(
            &mut db,
            "SELECT COALESCE('safe', ABS('not numeric')) FROM t LIMIT 1",
        ),
        vec![vec![Value::Text("safe".into())]]
    );
    let error = db
        .query("SELECT LOWER(age) FROM t")
        .unwrap_err()
        .to_string();
    assert!(error.contains("expects text"), "{error}");
    let error = db
        .query("SELECT MYSTERY(name) FROM t")
        .unwrap_err()
        .to_string();
    assert!(
        error.contains("unsupported scalar function `MYSTERY`"),
        "{error}"
    );
    assert!(error.contains("line 1, column 8"), "{error}");
}

#[test]
fn primary_key_and_unique_constraints_are_enforced() {
    let mut db = Database::open_memory();
    db.query("CREATE TABLE users (id PRIMARY KEY, email UNIQUE, name NOT NULL)")
        .unwrap();
    db.query("INSERT INTO users VALUES (1, 'a@example.com', 'Ada')")
        .unwrap();
    assert!(db
        .query("INSERT INTO users VALUES (1, 'b@example.com', 'Lin')")
        .is_err());
    assert!(db
        .query("INSERT INTO users VALUES (2, 'a@example.com', 'Lin')")
        .is_err());
    assert!(db
        .query("INSERT INTO users VALUES (2, 'b@example.com', NULL)")
        .is_err());
    assert!(db.query("UPDATE users SET id = NULL WHERE id = 1").is_err());
    assert!(db
        .query("INSERT INTO users VALUES (1.0, 'c@example.com', 'Cat')")
        .is_err());
}

#[test]
fn schema_defaults_checks_and_named_inserts_compose() {
    let mut db = Database::open_memory();
    db.query(
        "CREATE TABLE jobs (\
            id INTEGER PRIMARY KEY, \
            state TEXT NOT NULL DEFAULT 'queued', \
            attempts INTEGER DEFAULT 0 CHECK (attempts >= 0), \
            note VARCHAR(40), \
            CHECK (state IN ('queued', 'running', 'done'))\
        )",
    )
    .unwrap();

    db.query("INSERT INTO jobs (id, note) VALUES (1, 'first')")
        .unwrap();
    db.query("INSERT INTO jobs VALUES (2, DEFAULT, DEFAULT, NULL)")
        .unwrap();
    assert_eq!(
        rows(
            &mut db,
            "SELECT id, state, attempts, note FROM jobs ORDER BY id"
        ),
        vec![
            vec![
                Value::Int(1),
                Value::Text("queued".into()),
                Value::Int(0),
                Value::Text("first".into()),
            ],
            vec![
                Value::Int(2),
                Value::Text("queued".into()),
                Value::Int(0),
                Value::Null,
            ],
        ]
    );

    db.query("UPDATE jobs SET state = 'running' WHERE id = 1")
        .unwrap();
    db.query("UPDATE jobs SET state = DEFAULT WHERE id = 1")
        .unwrap();
    assert_eq!(
        rows(&mut db, "SELECT state FROM jobs WHERE id = 1"),
        vec![vec![Value::Text("queued".into())]]
    );

    // A NULL comparison is UNKNOWN and therefore satisfies a SQL CHECK.
    db.query("INSERT INTO jobs (id, attempts) VALUES (3, NULL)")
        .unwrap();
    assert!(db
        .query("INSERT INTO jobs (id, attempts) VALUES (4, -1)")
        .unwrap_err()
        .to_string()
        .contains("CHECK constraint"));
}

#[test]
fn default_values_and_constraint_failures_are_atomic() {
    let mut db = Database::open_memory();
    db.query(
        "CREATE TABLE settings (enabled INTEGER DEFAULT 1, mode TEXT DEFAULT 'safe', \
         CHECK (enabled IN (0, 1)), CHECK (mode != 'broken'))",
    )
    .unwrap();
    db.query("INSERT INTO settings DEFAULT VALUES").unwrap();
    assert_eq!(
        rows(&mut db, "SELECT * FROM settings"),
        vec![vec![Value::Int(1), Value::Text("safe".into())]]
    );

    let error = db
        .query("INSERT INTO settings (enabled, mode) VALUES (0, 'ok'), (2, 'bad')")
        .unwrap_err()
        .to_string();
    assert!(error.contains("CHECK constraint"), "{error}");
    assert_eq!(rows(&mut db, "SELECT * FROM settings").len(), 1);

    let error = db
        .query("UPDATE settings SET mode = 'broken' WHERE enabled = 1")
        .unwrap_err()
        .to_string();
    assert!(error.contains("CHECK constraint"), "{error}");
    assert_eq!(
        rows(&mut db, "SELECT mode FROM settings"),
        vec![vec![Value::Text("safe".into())]]
    );
}

#[test]
fn schema_metadata_survives_workspace_and_baked_round_trips() {
    let temp = tempfile::tempdir().unwrap();
    let workspace = temp.path().join("schema.pv");
    {
        let mut db = Database::open_dev(&workspace).unwrap();
        db.query(
            "CREATE TABLE counters (id INTEGER PRIMARY KEY, value INTEGER DEFAULT 1 \
             CHECK (value BETWEEN 0 AND 10))",
        )
        .unwrap();
        db.query("INSERT INTO counters (id) VALUES (1)").unwrap();
    }
    let mut reopened = Database::open_dev(&workspace).unwrap();
    reopened
        .query("INSERT INTO counters (id) VALUES (2)")
        .unwrap();
    assert!(reopened
        .query("INSERT INTO counters (id, value) VALUES (3, 11)")
        .is_err());

    let image = reopened.bake_to_bytes().unwrap();
    assert_eq!(u16::from_le_bytes([image[4], image[5]]), 4);
    let mut imported = Database::import_bytes(&image).unwrap();
    imported
        .query("INSERT INTO counters (id) VALUES (3)")
        .unwrap();
    assert!(imported
        .query("UPDATE counters SET value = 12 WHERE id = 3")
        .is_err());
    assert_eq!(
        rows(&mut imported, "SELECT value FROM counters WHERE id = 3"),
        vec![vec![Value::Int(1)]]
    );
}

#[test]
fn in_memory_transactions_roll_back_on_error() {
    let mut db = Database::open_memory();
    db.query("CREATE TABLE events (id PRIMARY KEY)").unwrap();
    let result = db.transaction(|tx| {
        tx.query("INSERT INTO events VALUES (1)")?;
        tx.query("INSERT INTO events VALUES (1)")?;
        Ok(())
    });
    assert!(result.is_err());
    assert!(rows(&mut db, "SELECT * FROM events").is_empty());
}
