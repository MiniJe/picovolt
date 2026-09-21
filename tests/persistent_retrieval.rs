// PicoVolt 2.3 qualification; see legal/COMPONENT-SCOPE-2.3.md.
use picovolt::{Database, Value};

const FT: &str = "CREATE INDEX ft ON docs USING FULLTEXT (title,body) WITH (id_column='id')";
const VX: &str = "CREATE INDEX vx ON docs USING VECTOR (embedding) WITH (id_column='id',metric='cosine',dimensions=3)";

fn populate(db: &mut Database) {
    db.query("CREATE TABLE docs (id,tenant,title,body,embedding,other)")
        .unwrap();
    for (id, tenant, title, body, vector) in [
        (
            1,
            "a",
            "apple apple banana",
            Some("verified snapshot"),
            "[1,0,0]",
        ),
        (2, "a", "apple banana banana", None, "[0,1,0]"),
        (
            3,
            "b",
            "Unicode Știință 東京",
            Some("snapshot database"),
            "[0,0,1]",
        ),
    ] {
        db.query_with(
            "INSERT INTO docs VALUES (?,?,?,?,?,0)",
            &[
                Value::Int(id),
                Value::Text(tenant.into()),
                Value::Text(title.into()),
                body.map_or(Value::Null, |value| Value::Text(value.into())),
                Value::Text(vector.into()),
            ],
        )
        .unwrap();
    }
}

fn database() -> Database {
    let mut db = Database::open_memory();
    populate(&mut db);
    db
}

#[test]
fn ddl_feature_gates_and_legacy_secondary_indexes() {
    let mut db = database();
    db.query("CREATE INDEX ON docs (tenant)").unwrap();
    #[cfg(feature = "full-text")]
    db.query(FT).unwrap();
    #[cfg(not(feature = "full-text"))]
    assert!(db.query(FT).is_err());
    #[cfg(feature = "vector-search")]
    db.query(VX).unwrap();
    #[cfg(not(feature = "vector-search"))]
    assert!(db.query(VX).is_err());
    assert_eq!(
        db.query("SELECT id FROM docs WHERE tenant='a'")
            .unwrap()
            .rows()
            .unwrap()
            .len(),
        2
    );
}

#[test]
fn malformed_ddl_is_positioned_and_does_not_register_a_definition() {
    let mut db = database();
    for sql in [
        "CREATE INDEX x ON docs USING FULLTEXT () WITH (id_column='id')",
        "CREATE INDEX x ON docs USING FULLTEXT (title) WITH (metric='cosine')",
        "CREATE INDEX x ON docs USING FULLTEXT (title) WITH (id_column='id',id_column='id')",
        "CREATE INDEX x ON docs USING FULLTEXT (title) WITH (id_column='id',unknown=1)",
        "CREATE INDEX x ON docs USING VECTOR (embedding) WITH (id_column='id',metric='bad',dimensions=3)",
        "CREATE INDEX x ON docs USING VECTOR (embedding) WITH (id_column='id',metric='cosine',dimensions=0)",
        "CREATE INDEX x ON docs USING VECTOR (embedding) WITH (id_column='id',metric='cosine',dimensions=4097)",
        "CREATE INDEX x ON docs USING VECTOR (embedding) WITH (id_column='id',metric='cosine',dimensions='3')",
        "CREATE INDEX x ON docs USING UNKNOWN (title) WITH (id_column='id')",
    ] {
        let error = db.query(sql).unwrap_err().to_string();
        assert!(error.contains("byte") || error.contains("position") || error.contains("offset") || (error.contains("line ") && error.contains("column ")), "unpositioned error: {error}");
        assert!(db.retrieval_indexes().is_empty());
    }
}

#[cfg(feature = "full-text")]
#[test]
fn named_catalog_conflicts_schema_validation_and_drop() {
    let mut db = database();
    for sql in [
        "CREATE INDEX bad ON absent USING FULLTEXT (title) WITH (id_column='id')",
        "CREATE INDEX bad ON docs USING FULLTEXT (absent) WITH (id_column='id')",
        "CREATE INDEX bad ON docs USING FULLTEXT (title) WITH (id_column='absent')",
        "CREATE INDEX bad ON docs USING FULLTEXT (title,title) WITH (id_column='id')",
    ] {
        assert!(db.query(sql).is_err(), "{sql}");
        assert!(db.retrieval_indexes().is_empty());
    }
    db.query(FT).unwrap();
    assert!(db.query(FT).is_err());
    db.query(&FT.replace("CREATE INDEX ft", "CREATE INDEX IF NOT EXISTS ft"))
        .unwrap();
    assert!(db
        .query("CREATE INDEX IF NOT EXISTS ft ON docs USING FULLTEXT (title) WITH (id_column='id')")
        .is_err());
    assert_eq!(db.retrieval_indexes().len(), 1);
    db.query("DROP INDEX IF EXISTS missing").unwrap();
    assert!(db.query("DROP INDEX missing").is_err());
    db.query("DROP INDEX ft").unwrap();
    assert!(db.retrieval_indexes().is_empty());
    assert_eq!(db.row_count("docs", None).unwrap(), 3);
}

#[cfg(all(feature = "full-text", feature = "vector-search"))]
mod both {
    use super::*;
    use serde_json::{json, Value as Json};

    fn indexed(db: &mut Database) {
        db.query(FT).unwrap();
        db.query(VX).unwrap();
        db.query("CREATE INDEX l2 ON docs USING VECTOR (embedding) WITH (id_column='id',metric='squared_euclidean',dimensions=3)").unwrap();
    }

    fn requests(sql: &str) -> Vec<Json> {
        vec![
            json!({"kind":"full_text","sql":sql,"id_column":"id","text_columns":["title","body"],"query":"apple banana","limit":10}),
            json!({"kind":"vector","sql":sql,"id_column":"id","vector_column":"embedding","query":[1,0,0],"metric":"cosine","limit":10}),
            json!({"kind":"vector","sql":sql,"id_column":"id","vector_column":"embedding","query":[1,0,0],"metric":"squared_euclidean","limit":10}),
            json!({"kind":"hybrid","sql":sql,"id_column":"id","text_columns":["title","body"],"vector_column":"embedding","query":"apple banana","vector_query":[1,0,0],"metric":"cosine","text_weight":0.4,"candidate_limit":10,"limit":10}),
        ]
    }

    fn named(mut request: Json) -> Json {
        match request["kind"].as_str().unwrap() {
            "full_text" => request["index"] = json!("ft"),
            "vector" => {
                request["index"] = if request["metric"] == "cosine" {
                    json!("vx")
                } else {
                    json!("l2")
                }
            }
            "hybrid" => {
                request["text_index"] = json!("ft");
                request["vector_index"] = json!("vx");
            }
            _ => unreachable!(),
        }
        request
    }

    fn equivalent(db: &mut Database, sql: &str) {
        for request in requests(sql) {
            let reference = db.retrieve_json(&request.to_string()).unwrap();
            let actual = db.retrieve_json(&named(request).to_string()).unwrap();
            assert_eq!(actual, reference, "{sql}");
        }
    }

    #[test]
    fn initial_build_and_all_storage_lifecycles() {
        let mut db = database();
        indexed(&mut db);
        equivalent(&mut db, "SELECT * FROM docs");
        let bytes = db.bake_to_bytes().unwrap();
        assert_eq!(u16::from_le_bytes([bytes[4], bytes[5]]), 8);
        let mut imported = Database::import_bytes(&bytes).unwrap();
        equivalent(&mut imported, "SELECT * FROM docs WHERE tenant='a'");
        let temp = tempfile::tempdir().unwrap();
        let image = temp.path().join("index.pvdb");
        db.bake(&image).unwrap();
        let mut prod = Database::open_prod(&image).unwrap();
        equivalent(&mut prod, "SELECT * FROM docs");
        assert!(prod.query("DROP INDEX ft").is_err());
        for logged in [false, true] {
            let root = temp.path().join(if logged { "logged" } else { "unlogged" });
            let mut dev = Database::open_dev(&root).unwrap();
            populate(&mut dev);
            if logged {
                dev.enable_commit_log(picovolt::CommitLogOptions::default())
                    .unwrap();
            }
            indexed(&mut dev);
            equivalent(&mut dev, "SELECT * FROM docs");
            drop(dev);
            let mut reopened = Database::open_dev(&root).unwrap();
            equivalent(&mut reopened, "SELECT * FROM docs");
            for info in reopened.retrieval_indexes() {
                assert_eq!(info.health, "healthy");
                assert!(!info.pending_write);
                assert!(info.persisted_bytes > 0);
                reopened
                    .verify_retrieval_index(&info.definition.name)
                    .unwrap();
            }
        }
    }

    #[test]
    fn insert_update_id_delete_and_unindexed_updates_stay_exact() {
        let mut db = database();
        indexed(&mut db);
        db.flush_now().unwrap();
        let generation = db.retrieval_indexes()[0].generation;
        db.query("UPDATE docs SET other=7 WHERE id=1").unwrap();
        assert_eq!(db.retrieval_indexes()[0].generation, generation);
        db.query("INSERT INTO docs VALUES (4,'a','apple banana','new','[1,1,0]',0)")
            .unwrap();
        db.query("UPDATE docs SET title='banana apple apple apple' WHERE id=2")
            .unwrap();
        db.query("UPDATE docs SET embedding='[1,0.25,0]' WHERE id=2")
            .unwrap();
        db.query("UPDATE docs SET id=9223372036854775807 WHERE id=4")
            .unwrap();
        db.query("DELETE FROM docs WHERE id=3").unwrap();
        equivalent(&mut db, "SELECT * FROM docs WHERE tenant='a'");
        for info in db.retrieval_indexes() {
            db.verify_retrieval_index(&info.definition.name).unwrap();
        }
        let mut reopened = Database::import_bytes(&db.bake_to_bytes().unwrap()).unwrap();
        equivalent(&mut reopened, "SELECT * FROM docs");
    }

    #[test]
    fn rollback_failed_statements_and_explicit_rebuild_preserve_state() {
        let mut db = database();
        indexed(&mut db);
        let before = db.verification_hash().unwrap();
        db.query("BEGIN").unwrap();
        db.query("DELETE FROM docs WHERE id=1").unwrap();
        db.query("UPDATE docs SET title='different' WHERE id=2")
            .unwrap();
        db.query("ROLLBACK").unwrap();
        assert_eq!(db.verification_hash().unwrap(), before);
        for sql in [
            "INSERT INTO docs VALUES (1,'a','duplicate',NULL,'[1,0,0]',0)",
            "UPDATE docs SET embedding='[0,0,0]' WHERE id=1",
            "UPDATE docs SET embedding='[1,0]' WHERE id=1",
            "UPDATE docs SET embedding='[1e100,0,0]' WHERE id=1",
            "UPDATE docs SET id=2 WHERE id=1",
            "UPDATE docs SET id=NULL WHERE id=1",
        ] {
            assert!(db.query(sql).is_err(), "{sql}");
            assert_eq!(db.verification_hash().unwrap(), before, "{sql}");
            equivalent(&mut db, "SELECT * FROM docs");
        }
        let aborted = db.transaction(|database| {
            database.query("DELETE FROM docs WHERE id=1")?;
            Err::<(), _>(picovolt::PvError::Query("abort".into()))
        });
        assert!(aborted.is_err());
        assert_eq!(db.verification_hash().unwrap(), before);
        db.rebuild_retrieval_index("ft").unwrap();
        assert_eq!(
            db.retrieval_indexes()
                .iter()
                .find(|info| info.definition.name == "ft")
                .unwrap()
                .rebuild_count,
            1
        );
        db.verify_retrieval_index("ft").unwrap();
        db.query("BEGIN").unwrap();
        db.query("DROP INDEX ft").unwrap();
        db.query("ROLLBACK").unwrap();
        equivalent(&mut db, "SELECT * FROM docs");
    }

    #[test]
    fn multi_statement_and_low_level_mutations_share_the_commit_boundary() {
        let mut db = database();
        indexed(&mut db);
        db.transaction(|database| {
            database.insert(
                "docs",
                vec![
                    Value::Int(5),
                    Value::Text("a".into()),
                    Value::Text("apple banana".into()),
                    Value::Null,
                    Value::Text("[1,1,1]".into()),
                    Value::Int(0),
                ],
            )?;
            database.update(
                "docs",
                "title",
                &Value::Text("apple".into()),
                "id",
                &Value::Int(1),
            )?;
            database.delete("docs", "id", &Value::Int(3))?;
            Ok(())
        })
        .unwrap();
        equivalent(&mut db, "SELECT * FROM docs");
        db.query("DROP TABLE docs").unwrap();
        assert!(db.retrieval_indexes().is_empty());
    }

    #[test]
    fn tenant_bm25_is_not_global_bm25_with_post_filtering() {
        let mut db = Database::open_memory();
        db.query("CREATE TABLE docs (id,tenant,title,body,embedding)")
            .unwrap();
        db.query("INSERT INTO docs VALUES (1,'a','apple apple banana','','[1,0,0]')")
            .unwrap();
        db.query("INSERT INTO docs VALUES (2,'a','apple banana banana','','[1,0,0]')")
            .unwrap();
        for id in 3..33 {
            db.query(&format!(
                "INSERT INTO docs VALUES ({id},'b','apple','','[0,1,0]')"
            ))
            .unwrap();
        }
        indexed(&mut db);
        let global = requests("SELECT * FROM docs").remove(0);
        let tenant = requests("SELECT * FROM docs WHERE tenant='a'").remove(0);
        let global: Json =
            serde_json::from_str(&db.retrieve_json(&global.to_string()).unwrap()).unwrap();
        let filtered: Json =
            serde_json::from_str(&db.retrieve_json(&tenant.to_string()).unwrap()).unwrap();
        assert_eq!(global[0]["id"], "2");
        assert_eq!(filtered[0]["id"], "1");
        equivalent(&mut db, "SELECT * FROM docs WHERE tenant='a'");
        equivalent(&mut db, "SELECT * FROM docs WHERE tenant='missing'");
    }

    #[test]
    fn historical_queries_fall_back_and_definition_mismatches_fail() {
        let mut db = database();
        indexed(&mut db);
        let previous = db.current_tx();
        db.query("UPDATE docs SET title='no longer matching' WHERE id=1")
            .unwrap();
        db.query("UPDATE docs SET embedding='[-1,0,0]' WHERE id=2")
            .unwrap();
        db.query("DELETE FROM docs WHERE id=3").unwrap();
        equivalent(&mut db, &format!("SELECT * FROM docs BEFORE {previous}"));
        equivalent(
            &mut db,
            "SELECT * FROM docs WHERE tenant='a' ORDER BY id DESC LIMIT 1",
        );
        let mut request = named(requests("SELECT * FROM docs").remove(0));
        for (field, value) in [
            ("index", json!("missing")),
            ("id_column", json!("other")),
            ("text_columns", json!(["title"])),
        ] {
            let old = request[field].clone();
            request[field] = value;
            assert!(db.retrieve_json(&request.to_string()).is_err());
            request[field] = old;
        }
        request["sql"] = json!("DELETE FROM docs WHERE id=1");
        assert!(db.retrieve_json(&request.to_string()).is_err());
    }

    #[test]
    fn generated_corpora_preserve_unicode_nulls_ties_and_both_vector_metrics() {
        let mut db = database();
        for id in 4..124 {
            let title = match id % 6 {
                0 => "apple apple banana".into(),
                1 => "APPLE banana".into(),
                2 => "Știință 東京 apple".into(),
                3 => String::new(),
                4 => format!("{} banana apple", "z".repeat(129)),
                _ => "banana banana apple".into(),
            };
            let vector = format!("[{}, {}, {}]", id % 7 + 1, id % 5, -(id % 3));
            db.query_with(
                "INSERT INTO docs VALUES (?,?,?,?,?,0)",
                &[
                    Value::Int(id),
                    Value::Text(if id % 2 == 0 { "a" } else { "b" }.into()),
                    Value::Text(title),
                    Value::Null,
                    Value::Text(vector),
                ],
            )
            .unwrap();
        }
        indexed(&mut db);
        for filter in [
            "",
            " WHERE tenant='a'",
            " WHERE tenant='b'",
            " WHERE id>90",
            " WHERE id<0",
        ] {
            equivalent(&mut db, &format!("SELECT * FROM docs{filter}"));
        }
        let mut reopened = Database::import_bytes(&db.bake_to_bytes().unwrap()).unwrap();
        equivalent(&mut reopened, "SELECT * FROM docs WHERE tenant='a'");
    }

    #[test]
    fn cli_retrieve_and_inspect_expose_persisted_indexes() {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("docs.pvdb");
        let request_path = temp.path().join("request.json");
        let mut db = database();
        indexed(&mut db);
        db.bake(&path).unwrap();
        let request = named(requests("SELECT * FROM docs WHERE tenant='a'").remove(0));
        std::fs::write(&request_path, request.to_string()).unwrap();
        let expected = db.retrieve_json(&request.to_string()).unwrap();
        let output = std::process::Command::new(env!("CARGO_BIN_EXE_pv"))
            .arg("retrieve")
            .arg(&path)
            .arg(request_path)
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        assert_eq!(String::from_utf8(output.stdout).unwrap().trim(), expected);
        assert_eq!(db.inspect_stats().unwrap().retrieval_indexes.len(), 3);
    }
}

#[test]
fn indexless_images_keep_their_minimum_format() {
    let mut db = database();
    let bytes = db.bake_to_bytes().unwrap();
    assert_eq!(u16::from_le_bytes([bytes[4], bytes[5]]), 1);
}
