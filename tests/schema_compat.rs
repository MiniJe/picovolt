//! Adversarial compatibility coverage for the 1.7 schema surface.
//!
//! These tests intentionally exercise validation failures and persisted metadata,
//! where a permissive parser or a partially-applied mutation would be especially
//! costly to correct after the v4 format ships.

use std::path::Path;

use picovolt::{
    Database, PvError, QueryLimits, Row, Value, FORMAT_VERSION, FORMAT_VERSION_CONSTRAINTS,
    MANIFEST_FILE,
};

const GOLDEN_V3: &str = "tests/fixtures/golden_v1_4_0.pvdb";

fn rows(db: &mut Database, sql: &str) -> Vec<Row> {
    db.query(sql).unwrap().rows().unwrap().to_vec()
}

fn assert_error_contains(error: PvError, expected: &str) {
    let rendered = error.to_string();
    assert!(
        rendered.contains(expected),
        "expected error containing {expected:?}, got {rendered:?}"
    );
}

fn open_dev_error(path: &Path) -> PvError {
    match Database::open_dev(path) {
        Ok(_) => panic!("malformed workspace unexpectedly opened"),
        Err(error) => error,
    }
}

fn assert_corruption(error: PvError, expected: &str) {
    match error {
        PvError::Corruption(message) => assert!(
            message.contains(expected),
            "expected corruption containing {expected:?}, got {message:?}"
        ),
        other => panic!("expected corruption, got {other}"),
    }
}

fn create_rich_workspace(path: &Path) {
    let mut db = Database::open_dev(path).unwrap();
    db.query(
        "CREATE TABLE guarded (\
            id INTEGER PRIMARY KEY DEFAULT 1, \
            score INTEGER DEFAULT 0 CHECK (score BETWEEN 0 AND 10)\
        )",
    )
    .unwrap();
}

fn edit_manifest(path: &Path, edit: impl FnOnce(&mut serde_json::Value)) {
    let manifest_path = path.join(MANIFEST_FILE);
    let mut manifest: serde_json::Value =
        serde_json::from_slice(&std::fs::read(&manifest_path).unwrap()).unwrap();
    edit(&mut manifest);
    std::fs::write(manifest_path, serde_json::to_vec(&manifest).unwrap()).unwrap();
}

#[test]
fn named_insert_rejects_duplicate_unknown_and_mismatched_targets_without_writes() {
    let mut db = Database::open_memory();
    db.query(
        "CREATE TABLE targets (left_value INTEGER DEFAULT 10, right_value INTEGER DEFAULT 20)",
    )
    .unwrap();

    assert_error_contains(
        db.query("INSERT INTO targets (left_value, left_value) VALUES (1, 2)")
            .unwrap_err(),
        "duplicate INSERT target column `left_value`",
    );
    assert_error_contains(
        db.query("INSERT INTO targets (left_value, missing) VALUES (1, 2)")
            .unwrap_err(),
        "no column `missing`",
    );
    assert_error_contains(
        db.query("INSERT INTO targets (left_value, right_value) VALUES (1)")
            .unwrap_err(),
        "target list has 2 columns but row has 1 values",
    );
    assert!(rows(&mut db, "SELECT * FROM targets").is_empty());

    db.query("INSERT INTO targets (right_value, left_value) VALUES (DEFAULT, 7)")
        .unwrap();
    assert_eq!(
        rows(&mut db, "SELECT * FROM targets"),
        vec![vec![Value::Int(7), Value::Int(20)]]
    );
}

#[test]
fn defaults_still_obey_unique_and_not_null_constraints() {
    let mut db = Database::open_memory();
    db.query(
        "CREATE TABLE principals (\
            id INTEGER PRIMARY KEY, \
            alias TEXT UNIQUE DEFAULT 'guest', \
            owner TEXT NOT NULL DEFAULT 'system'\
        )",
    )
    .unwrap();

    db.query("INSERT INTO principals (id) VALUES (1)").unwrap();
    db.query("INSERT INTO principals (id, alias, owner) VALUES (2, 'named', DEFAULT)")
        .unwrap();

    assert_error_contains(
        db.query("INSERT INTO principals (id) VALUES (3)")
            .unwrap_err(),
        "duplicate value for unique column `alias`",
    );
    assert_error_contains(
        db.query("INSERT INTO principals (id, alias, owner) VALUES (3, 'third', NULL)")
            .unwrap_err(),
        "column `owner` may not be NULL",
    );

    // DEFAULT resolves before uniqueness validation. The failed update must not
    // replace row 2's distinct value with the default already held by row 1.
    assert_error_contains(
        db.query("UPDATE principals SET alias = DEFAULT WHERE id = 2")
            .unwrap_err(),
        "duplicate value for unique column `alias`",
    );
    assert_eq!(
        rows(
            &mut db,
            "SELECT id, alias, owner FROM principals ORDER BY id"
        ),
        vec![
            vec![
                Value::Int(1),
                Value::Text("guest".into()),
                Value::Text("system".into()),
            ],
            vec![
                Value::Int(2),
                Value::Text("named".into()),
                Value::Text("system".into()),
            ],
        ]
    );

    db.query(
        "CREATE TABLE null_default (id INTEGER PRIMARY KEY, label TEXT NOT NULL DEFAULT NULL)",
    )
    .unwrap();
    assert_error_contains(
        db.query("INSERT INTO null_default (id) VALUES (1)")
            .unwrap_err(),
        "column `label` may not be NULL",
    );
    assert!(rows(&mut db, "SELECT * FROM null_default").is_empty());
}

#[test]
fn check_constraints_use_sql_three_valued_logic_for_boolean_and_set_predicates() {
    let mut db = Database::open_memory();

    db.query("CREATE TABLE check_and (a INTEGER, b INTEGER, CHECK (a > 0 AND b > 0))")
        .unwrap();
    db.query("INSERT INTO check_and VALUES (1, NULL), (NULL, 1)")
        .unwrap();
    assert!(db.query("INSERT INTO check_and VALUES (-1, NULL)").is_err());
    assert!(db.query("INSERT INTO check_and VALUES (NULL, -1)").is_err());
    assert_eq!(rows(&mut db, "SELECT * FROM check_and").len(), 2);

    db.query("CREATE TABLE check_or (a INTEGER, b INTEGER, CHECK (a > 0 OR b > 0))")
        .unwrap();
    db.query("INSERT INTO check_or VALUES (-1, NULL), (NULL, NULL), (1, -1)")
        .unwrap();
    assert!(db.query("INSERT INTO check_or VALUES (-1, -1)").is_err());
    assert_eq!(rows(&mut db, "SELECT * FROM check_or").len(), 3);

    // A non-match remains UNKNOWN when the IN list contains NULL, while the same
    // non-match is FALSE against a list without NULL.
    db.query("CREATE TABLE check_in_nullable (value TEXT CHECK (value IN ('ok', NULL)))")
        .unwrap();
    db.query("INSERT INTO check_in_nullable VALUES ('ok'), ('other'), (NULL)")
        .unwrap();
    assert_eq!(rows(&mut db, "SELECT * FROM check_in_nullable").len(), 3);

    db.query("CREATE TABLE check_in_strict (value TEXT CHECK (value IN ('ok', 'ready')))")
        .unwrap();
    db.query("INSERT INTO check_in_strict VALUES ('ok'), (NULL)")
        .unwrap();
    assert!(db
        .query("INSERT INTO check_in_strict VALUES ('other')")
        .is_err());

    db.query("CREATE TABLE check_between (value INTEGER CHECK (value BETWEEN 1 AND 3))")
        .unwrap();
    db.query("INSERT INTO check_between VALUES (1), (3), (NULL)")
        .unwrap();
    assert!(db.query("INSERT INTO check_between VALUES (0)").is_err());
    assert!(db.query("INSERT INTO check_between VALUES (4)").is_err());
    assert_eq!(rows(&mut db, "SELECT * FROM check_between").len(), 3);
}

#[test]
fn failed_multi_row_insert_and_multi_row_update_are_atomic() {
    let mut db = Database::open_memory();
    db.query(
        "CREATE TABLE batches (\
            id INTEGER PRIMARY KEY, \
            score INTEGER DEFAULT 5 CHECK (score BETWEEN 0 AND 10)\
        )",
    )
    .unwrap();
    db.query("INSERT INTO batches (id, score) VALUES (1, 4), (2, 6)")
        .unwrap();
    let before = rows(&mut db, "SELECT id, score FROM batches ORDER BY id");

    assert_error_contains(
        db.query("INSERT INTO batches (id, score) VALUES (3, 7), (4, 11), (5, 8)")
            .unwrap_err(),
        "CHECK constraint",
    );
    assert_eq!(
        rows(&mut db, "SELECT id, score FROM batches ORDER BY id"),
        before,
        "a late invalid row must roll back the entire VALUES list"
    );

    assert_error_contains(
        db.query("UPDATE batches SET score = -1 WHERE id BETWEEN 1 AND 2")
            .unwrap_err(),
        "CHECK constraint",
    );
    assert_eq!(
        rows(&mut db, "SELECT id, score FROM batches ORDER BY id"),
        before,
        "all replacements must validate before any old version is tombstoned"
    );
}

#[test]
fn malformed_dev_schema_metadata_and_versions_are_rejected() {
    let temp = tempfile::tempdir().unwrap();

    let unknown_default = temp.path().join("unknown-default");
    create_rich_workspace(&unknown_default);
    edit_manifest(&unknown_default, |manifest| {
        manifest["tables"][0]["defaults"]["missing"] = serde_json::json!({"Int": 7});
    });
    assert_corruption(
        open_dev_error(&unknown_default),
        "DEFAULT references unknown column `missing`",
    );

    let unknown_check = temp.path().join("unknown-check");
    create_rich_workspace(&unknown_check);
    edit_manifest(&unknown_check, |manifest| {
        manifest["tables"][0]["checks"][0]["Between"]["column"] = serde_json::json!("missing");
    });
    assert_corruption(
        open_dev_error(&unknown_check),
        "CHECK references unknown column `missing`",
    );

    let understated = temp.path().join("understated-version");
    create_rich_workspace(&understated);
    edit_manifest(&understated, |manifest| {
        manifest["format_version"] = serde_json::json!(FORMAT_VERSION_CONSTRAINTS);
    });
    assert_corruption(
        open_dev_error(&understated),
        "understates persisted features",
    );

    let future = temp.path().join("future-version");
    create_rich_workspace(&future);
    edit_manifest(&future, |manifest| {
        manifest["format_version"] = serde_json::json!(FORMAT_VERSION + 1);
    });
    assert_corruption(
        open_dev_error(&future),
        "unsupported workspace format version",
    );
}

#[test]
fn malformed_manifest_table_identity_and_page_anchors_are_rejected() {
    let temp = tempfile::tempdir().unwrap();

    let duplicate = temp.path().join("duplicate-table");
    create_rich_workspace(&duplicate);
    edit_manifest(&duplicate, |manifest| {
        let duplicate_table = manifest["tables"][0].clone();
        manifest["tables"]
            .as_array_mut()
            .unwrap()
            .push(duplicate_table);
    });
    assert_corruption(
        open_dev_error(&duplicate),
        "manifest contains duplicate table `guarded`",
    );

    let empty_name = temp.path().join("empty-table-name");
    create_rich_workspace(&empty_name);
    edit_manifest(&empty_name, |manifest| {
        manifest["tables"][0]["name"] = serde_json::json!("");
    });
    assert_corruption(open_dev_error(&empty_name), "table with an empty name");

    let missing_tail = temp.path().join("missing-tail");
    create_rich_workspace(&missing_tail);
    {
        let mut db = Database::open_dev(&missing_tail).unwrap();
        db.query("INSERT INTO guarded VALUES (1, 5)").unwrap();
    }
    edit_manifest(&missing_tail, |manifest| {
        manifest["tables"][0]["tail_id"] = serde_json::Value::Null;
    });
    assert_corruption(
        open_dev_error(&missing_tail),
        "must define both first_page and tail_id",
    );

    let out_of_range = temp.path().join("out-of-range-tail");
    create_rich_workspace(&out_of_range);
    {
        let mut db = Database::open_dev(&out_of_range).unwrap();
        db.query("INSERT INTO guarded VALUES (1, 5)").unwrap();
    }
    edit_manifest(&out_of_range, |manifest| {
        let page_count = manifest["page_count"].as_u64().unwrap();
        manifest["tables"][0]["tail_id"] = serde_json::json!(page_count);
    });
    assert_corruption(open_dev_error(&out_of_range), "invalid head/tail page ids");
}

#[test]
fn v3_golden_remains_readable_and_writable_when_imported() {
    let bytes = std::fs::read(GOLDEN_V3).unwrap();
    assert_eq!(
        u16::from_le_bytes([bytes[4], bytes[5]]),
        FORMAT_VERSION_CONSTRAINTS
    );

    let mut disk = Database::open_prod(GOLDEN_V3).unwrap();
    assert_eq!(
        rows(&mut disk, "SELECT id, email, name FROM accounts"),
        vec![vec![
            Value::Int(1),
            Value::Text("ada@example.com".into()),
            Value::Text("Ada".into()),
        ]]
    );

    let mut imported = Database::import_bytes(&bytes).unwrap();
    assert_error_contains(
        imported
            .query("INSERT INTO accounts VALUES (1, 'lin@example.com', 'Lin')")
            .unwrap_err(),
        "duplicate value for unique column `id`",
    );
    assert_error_contains(
        imported
            .query("INSERT INTO accounts VALUES (2, 'lin@example.com', NULL)")
            .unwrap_err(),
        "column `name` may not be NULL",
    );
    imported
        .query("INSERT INTO accounts VALUES (2, 'lin@example.com', 'Lin')")
        .unwrap();
    assert_eq!(rows(&mut imported, "SELECT * FROM accounts").len(), 2);
}

#[test]
fn rich_insert_respects_read_only_handles() {
    let temp = tempfile::tempdir().unwrap();
    let image = temp.path().join("readonly.pvdb");
    let mut source = Database::open_memory();
    source
        .query("CREATE TABLE settings (id INTEGER PRIMARY KEY, mode TEXT DEFAULT 'safe')")
        .unwrap();
    source.bake(&image).unwrap();

    let mut readonly = Database::open_prod(&image).unwrap();
    assert!(matches!(
        readonly.query("INSERT INTO settings (id) VALUES (1)"),
        Err(PvError::ReadOnly)
    ));
    assert_eq!(readonly.row_count("settings", None).unwrap(), 0);
}

#[test]
fn oversized_batch_and_update_fail_before_any_row_mutation() {
    // 220 NULL fields fit easily, while 220 maximum-inline text fields exceed a
    // page record. The late invalid VALUES row must not leave the first one live.
    let mut batch = Database::open_memory();
    let columns = (0..220).map(|index| format!("c{index}")).collect();
    batch.create_table("batch", columns).unwrap();
    let nulls = vec!["NULL"; 220].join(", ");
    let inline_text = vec!["'1234567890abcdef'"; 220].join(", ");
    let error = batch
        .query(&format!(
            "INSERT INTO batch VALUES ({nulls}), ({inline_text})"
        ))
        .unwrap_err();
    assert_error_contains(error, "exceeds page capacity");
    assert_eq!(batch.row_count("batch", None).unwrap(), 0);

    // A nearly-full valid row must remain visible when its replacement would no
    // longer fit. In particular, UPDATE must validate before tombstoning it.
    let mut update = Database::open_memory();
    let columns = (0..4030).map(|index| format!("c{index}")).collect();
    update.create_table("wide", columns).unwrap();
    update.insert("wide", vec![Value::Null; 4030]).unwrap();
    let error = update
        .update_where(
            "wide",
            "c0",
            &Value::Text("1234567890abcdef".into()),
            &picovolt::engine::query::Predicate::IsNull {
                column: "c1".into(),
                negated: false,
            },
        )
        .unwrap_err();
    assert_error_contains(error, "exceeds page capacity");
    assert_eq!(update.row_count("wide", None).unwrap(), 1);
    assert_eq!(
        rows(&mut update, "SELECT c0 FROM wide"),
        vec![vec![Value::Null]]
    );
}

#[test]
fn unique_update_uses_numeric_equality_consistently() {
    let mut db = Database::open_memory();
    db.query("CREATE TABLE numeric_key (id INTEGER PRIMARY KEY, label TEXT)")
        .unwrap();
    db.query("INSERT INTO numeric_key VALUES (1, 'one')")
        .unwrap();

    db.query("UPDATE numeric_key SET id = 1.0 WHERE label = 'one'")
        .unwrap();
    assert_eq!(
        rows(&mut db, "SELECT id FROM numeric_key"),
        vec![vec![Value::Decimal(1_000_000)]]
    );

    db.query("CREATE INDEX ON numeric_key (id)").unwrap();
    assert_eq!(
        rows(&mut db, "SELECT label FROM numeric_key WHERE id = 1"),
        vec![vec![Value::Text("one".into())]]
    );
    assert_error_contains(
        db.query("INSERT INTO numeric_key VALUES (1, 'duplicate')")
            .unwrap_err(),
        "duplicate value for unique column `id`",
    );

    db.query("CREATE TABLE numeric_range (id INTEGER PRIMARY KEY, value)")
        .unwrap();
    db.query("INSERT INTO numeric_range VALUES (1, 2), (2, 1.0), (3, 3.0)")
        .unwrap();
    db.query("CREATE INDEX ON numeric_range (value)").unwrap();
    assert_eq!(
        rows(
            &mut db,
            "SELECT id FROM numeric_range WHERE value > 1.5 ORDER BY id"
        ),
        vec![vec![Value::Int(1)], vec![Value::Int(3)]]
    );
}

#[test]
fn check_metadata_has_a_total_complexity_bound() {
    let temp = tempfile::tempdir().unwrap();
    let workspace = temp.path().join("too-many-checks");
    create_rich_workspace(&workspace);
    edit_manifest(&workspace, |manifest| {
        let check = manifest["tables"][0]["checks"][0].clone();
        manifest["tables"][0]["checks"] = serde_json::Value::Array(vec![check; 257]);
    });
    assert_corruption(
        open_dev_error(&workspace),
        "CHECK expression exceeds the 32-level/256-node limit",
    );

    let mut db = Database::open_memory();
    let candidates = (0..257)
        .map(|value| value.to_string())
        .collect::<Vec<_>>()
        .join(", ");
    assert_error_contains(
        db.query(&format!(
            "CREATE TABLE bounded (id, CHECK (id IN ({candidates})))"
        ))
        .unwrap_err(),
        "CHECK expression exceeds",
    );
}

#[test]
fn indexed_reads_reject_manifest_row_arity_mismatches_without_panicking() {
    let temp = tempfile::tempdir().unwrap();
    let workspace = temp.path().join("row-arity");
    {
        let mut db = Database::open_dev(&workspace).unwrap();
        db.query("CREATE TABLE indexed (id, name)").unwrap();
        db.query("INSERT INTO indexed VALUES (1, 'Ada')").unwrap();
        db.query("CREATE INDEX ON indexed (id)").unwrap();
    }
    edit_manifest(&workspace, |manifest| {
        manifest["tables"][0]["columns"]
            .as_array_mut()
            .unwrap()
            .push(serde_json::json!("missing"));
    });

    let mut db = Database::open_dev(&workspace).unwrap();
    assert_corruption(
        db.query("SELECT missing FROM indexed WHERE id = 1")
            .unwrap_err(),
        "record field count does not match table columns",
    );
}

#[test]
fn bounded_queries_account_for_default_expansion_before_writing() {
    let mut db = Database::open_memory();
    let large_default = "x".repeat(1_024);
    db.query(&format!(
        "CREATE TABLE expanded (id INTEGER PRIMARY KEY, payload TEXT DEFAULT '{large_default}')"
    ))
    .unwrap();

    let limits = QueryLimits::new(10_000, 1_500, 10_000, None);
    let error = db
        .query_with_limits("INSERT INTO expanded (id) VALUES (1), (2)", &[], limits)
        .unwrap_err();
    assert!(matches!(error, PvError::ResourceLimit(_)), "{error}");
    assert_eq!(db.row_count("expanded", None).unwrap(), 0);
}
