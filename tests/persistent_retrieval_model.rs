// PicoVolt 2.3 qualification; see legal/COMPONENT-SCOPE-2.3.md.
#![cfg(all(feature = "full-text", feature = "vector-search"))]
use picovolt::{Database, Value};
use serde_json::{json, Value as Json};
use std::collections::BTreeMap;

fn requests(sql: &str) -> Vec<Json> {
    vec![
        json!({"kind":"full_text","sql":sql,"id_column":"id","text_columns":["body"],"query":"apple","limit":100}),
        json!({"kind":"vector","sql":sql,"id_column":"id","vector_column":"embedding","query":[1,0],"metric":"cosine","limit":100}),
        json!({"kind":"hybrid","sql":sql,"id_column":"id","text_columns":["body"],"vector_column":"embedding","query":"apple","vector_query":[1,0],"metric":"cosine","text_weight":0.6,"candidate_limit":100,"limit":100}),
    ]
}

fn named(mut request: Json) -> Json {
    match request["kind"].as_str().unwrap() {
        "full_text" => request["index"] = json!("ft"),
        "vector" => request["index"] = json!("vx"),
        _ => {
            request["text_index"] = json!("ft");
            request["vector_index"] = json!("vx");
        }
    }
    request
}

#[test]
fn seeded_transaction_mutation_model_matches_rows_and_both_retrieval_oracles() {
    let mut db = Database::open_memory();
    db.query("CREATE TABLE docs (id,tenant,body,embedding)")
        .unwrap();
    db.query("CREATE INDEX ft ON docs USING FULLTEXT (body) WITH (id_column='id')")
        .unwrap();
    db.query("CREATE INDEX vx ON docs USING VECTOR (embedding) WITH (id_column='id',metric='cosine',dimensions=2)").unwrap();
    let mut model = BTreeMap::<i64, (String, String, String)>::new();
    let mut seed = 0x23_001_123_456u64;
    for step in 0..160 {
        let before = model.clone();
        db.begin_transaction().unwrap();
        for offset in 0..2 {
            seed ^= seed << 13;
            seed ^= seed >> 7;
            seed ^= seed << 17;
            let id = (seed % 24) as i64 - 12;
            let body = format!(
                "apple {} step {step} mutation {offset}",
                if id % 2 == 0 {
                    "apple 東京"
                } else {
                    "banana Știință"
                }
            );
            let vector = format!("[{},{}]", (seed % 5) + 1, (seed % 7) + 1);
            match (seed >> 12) % 4 {
                0 => {
                    db.query_with("DELETE FROM docs WHERE id=?", &[Value::Int(id)])
                        .unwrap();
                    model.remove(&id);
                }
                1 if model.contains_key(&id) => {
                    db.query_with(
                        "UPDATE docs SET body=? WHERE id=?",
                        &[Value::Text(body.clone()), Value::Int(id)],
                    )
                    .unwrap();
                    model.get_mut(&id).unwrap().1 = body;
                }
                2 if model.contains_key(&id) => {
                    db.query_with(
                        "UPDATE docs SET embedding=? WHERE id=?",
                        &[Value::Text(vector.clone()), Value::Int(id)],
                    )
                    .unwrap();
                    model.get_mut(&id).unwrap().2 = vector;
                }
                _ if !model.contains_key(&id) => {
                    let tenant = if id % 2 == 0 { "a" } else { "b" }.to_owned();
                    db.query_with(
                        "INSERT INTO docs VALUES (?,?,?,?)",
                        &[
                            Value::Int(id),
                            Value::Text(tenant.clone()),
                            Value::Text(body.clone()),
                            Value::Text(vector.clone()),
                        ],
                    )
                    .unwrap();
                    model.insert(id, (tenant, body, vector));
                }
                _ => {}
            }
        }
        if step % 11 == 0 {
            db.rollback_transaction().unwrap();
            model = before;
        } else {
            db.commit_transaction().unwrap();
        }
        if step % 7 == 0 {
            db = Database::import_bytes(&db.bake_to_bytes().unwrap()).unwrap();
        }
        let rows = db.query("SELECT * FROM docs ORDER BY id").unwrap();
        let expected: Vec<_> = model
            .iter()
            .map(|(id, (tenant, body, vector))| {
                vec![
                    Value::Int(*id),
                    Value::Text(tenant.clone()),
                    Value::Text(body.clone()),
                    Value::Text(vector.clone()),
                ]
            })
            .collect();
        assert_eq!(
            rows.rows().unwrap(),
            expected.as_slice(),
            "model rows at step {step}"
        );
        for sql in [
            "SELECT * FROM docs",
            "SELECT * FROM docs WHERE tenant='a'",
            "SELECT * FROM docs WHERE tenant='b'",
        ] {
            for request in requests(sql) {
                let reference = db.retrieve_json(&request.to_string()).unwrap();
                assert_eq!(
                    db.retrieve_json(&named(request).to_string()).unwrap(),
                    reference,
                    "model ranks at step {step}"
                );
            }
        }
        db.verify_retrieval_index("ft").unwrap();
        db.verify_retrieval_index("vx").unwrap();
    }
}

#[test]
fn format_eight_golden_reopens_both_persisted_index_kinds() {
    let bytes = include_bytes!("fixtures/format_v8.pvdb");
    assert_eq!(u16::from_le_bytes([bytes[4], bytes[5]]), 8);
    let mut db = Database::import_bytes(bytes).unwrap();
    assert_eq!(db.retrieval_indexes().len(), 2);
    for index in db.retrieval_indexes() {
        assert_eq!(index.document_count, 3);
        assert!(!index.pending_write);
        db.verify_retrieval_index(&index.definition.name).unwrap();
    }
    let mut request = json!({"kind":"full_text","sql":"SELECT * FROM docs","id_column":"id","text_columns":["title","body"],"query":"verified snapshot","limit":10});
    let expected = db.retrieve_json(&request.to_string()).unwrap();
    request["index"] = json!("ft");
    assert_eq!(db.retrieve_json(&request.to_string()).unwrap(), expected);
    let mut request = json!({"kind":"vector","sql":"SELECT * FROM docs","id_column":"id","vector_column":"embedding","query":[1,1,1],"metric":"cosine","limit":10});
    let expected = db.retrieve_json(&request.to_string()).unwrap();
    request["index"] = json!("vx");
    assert_eq!(db.retrieve_json(&request.to_string()).unwrap(), expected);
}

#[test]
fn invalid_initial_corpora_fail_without_registering_partial_indexes() {
    for value in [
        Value::Null,
        Value::Text("not json".into()),
        Value::Text("[1]".into()),
        Value::Text("[0,0]".into()),
        Value::Text("[1e100,0]".into()),
    ] {
        let mut db = Database::open_memory();
        db.query("CREATE TABLE docs (id,body,embedding)").unwrap();
        db.insert(
            "docs",
            vec![Value::Int(1), Value::Text("apple".into()), value],
        )
        .unwrap();
        let before = db.verification_hash().unwrap();
        assert!(db.query("CREATE INDEX vx ON docs USING VECTOR (embedding) WITH (id_column='id',metric='cosine',dimensions=2)").is_err());
        assert!(db.retrieval_indexes().is_empty());
        assert_eq!(db.verification_hash().unwrap(), before);
    }
    let mut db = Database::open_memory();
    db.query("CREATE TABLE docs (id,body)").unwrap();
    db.query("INSERT INTO docs VALUES (1,'a'),(1,'b')").unwrap();
    assert!(db
        .query("CREATE INDEX ft ON docs USING FULLTEXT (body) WITH (id_column='id')")
        .is_err());
    assert!(db.retrieval_indexes().is_empty());
    db.query("DELETE FROM docs WHERE id=1").unwrap();
    db.insert(
        "docs",
        vec![Value::Text("1".into()), Value::Text("a".into())],
    )
    .unwrap();
    assert!(db
        .query("CREATE INDEX ft ON docs USING FULLTEXT (body) WITH (id_column='id')")
        .is_err());
}

#[test]
fn creation_and_indexed_mutation_enforce_document_budgets_atomically() {
    let mut db = Database::open_memory();
    db.query("CREATE TABLE docs (id,body)").unwrap();
    db.transaction(|db| {
        for id in 0..=picovolt::persistent::MAX_RETRIEVAL_DOCUMENTS {
            db.insert(
                "docs",
                vec![Value::Int(id as i64), Value::Text("apple".into())],
            )?;
        }
        Ok(())
    })
    .unwrap();
    assert!(db
        .query("CREATE INDEX ft ON docs USING FULLTEXT (body) WITH (id_column='id')")
        .is_err());
    assert!(db.retrieval_indexes().is_empty());
    db.query("DELETE FROM docs WHERE id=10000").unwrap();
    db.query("CREATE INDEX ft ON docs USING FULLTEXT (body) WITH (id_column='id')")
        .unwrap();
    let before = db.verification_hash().unwrap();
    assert!(db.query("INSERT INTO docs VALUES (10000,'extra')").is_err());
    assert_eq!(db.verification_hash().unwrap(), before);
}

#[cfg(not(target_arch = "wasm32"))]
#[test]
fn retained_readers_keep_their_index_generation_across_later_commits() {
    use picovolt::{CancellationToken, QueryLimits, RequestOptions, SharedDatabase};
    let shared = SharedDatabase::open_memory().unwrap();
    shared
        .query("CREATE TABLE docs (id,tenant,body,embedding)")
        .unwrap();
    shared
        .query("INSERT INTO docs VALUES (1,'a','apple old','[1,0]')")
        .unwrap();
    shared
        .query("CREATE INDEX ft ON docs USING FULLTEXT (body) WITH (id_column='id')")
        .unwrap();
    shared.query("CREATE INDEX vx ON docs USING VECTOR (embedding) WITH (id_column='id',metric='cosine',dimensions=2)").unwrap();
    let mut old = shared.begin_read().unwrap();
    let inputs: Vec<_> = requests("SELECT * FROM docs")
        .into_iter()
        .map(named)
        .collect();
    let old_results: Vec<_> = inputs
        .iter()
        .map(|request| old.retrieve_json(&request.to_string()).unwrap())
        .collect();
    let mut writer = shared.begin_write().unwrap();
    writer
        .query("UPDATE docs SET body='banana new' WHERE id=1")
        .unwrap();
    writer
        .query("UPDATE docs SET embedding='[0,1]' WHERE id=1")
        .unwrap();
    writer
        .query("INSERT INTO docs VALUES (2,'a','apple new','[1,0]')")
        .unwrap();
    writer.commit().unwrap();
    let mut new = shared.begin_read().unwrap();
    for (request, expected) in inputs.iter().zip(&old_results) {
        assert_eq!(old.retrieve_json(&request.to_string()).unwrap(), *expected);
        assert_ne!(new.retrieve_json(&request.to_string()).unwrap(), *expected);
    }
    let mut future = inputs[0].clone();
    future["sql"] = json!(format!(
        "SELECT * FROM docs BEFORE {}",
        old.snapshot_tx() + 1
    ));
    assert!(old.retrieve_json(&future.to_string()).is_err());
    let limits = QueryLimits::new(0, 16 * 1024 * 1024, 10_000, None);
    let mut limited = shared
        .begin_read_with_options(RequestOptions::new(limits))
        .unwrap();
    assert!(limited.retrieve_json(&inputs[0].to_string()).is_err());
    let token = CancellationToken::new();
    let mut cancelled = shared
        .begin_read_with_options(RequestOptions::default().with_cancellation(token.clone()))
        .unwrap();
    token.cancel();
    assert!(cancelled.retrieve_json(&inputs[0].to_string()).is_err());
    assert_eq!(
        shared.retrieve_json(&inputs[0].to_string()).unwrap(),
        new.retrieve_json(&inputs[0].to_string()).unwrap()
    );
}
