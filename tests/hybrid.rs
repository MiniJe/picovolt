#![cfg(all(feature = "full-text", feature = "vector-search"))]
use picovolt::Database;
use serde_json::{json, Value};
#[test]
fn hybrid_fuses_one_authorized_snapshot_and_explains_each_rank() {
    let mut db = Database::open_memory();
    db.query("CREATE TABLE docs(id,tenant,body,embedding)")
        .unwrap();
    for (id, tenant, body, embedding) in [
        (1, "a", "backup restore", "[0,1]"),
        (2, "a", "backup storage", "[1,0]"),
        (3, "b", "backup restore", "[1,0]"),
    ] {
        db.query_with(
            "INSERT INTO docs VALUES(?,?,?,?)",
            &[
                picovolt::Value::Int(id),
                tenant.into(),
                body.into(),
                embedding.into(),
            ],
        )
        .unwrap();
    }
    let mut request = json!({"kind":"hybrid","sql":"SELECT id,body,embedding FROM docs WHERE tenant=?","params":["a"],"id_column":"id","text_columns":["body"],"vector_column":"embedding","query":"restore","vector_query":[1,0],"metric":"cosine","text_weight":0.5,"candidate_limit":10,"limit":10});
    let hits: Vec<Value> =
        serde_json::from_str(&db.retrieve_json(&request.to_string()).unwrap()).unwrap();
    assert_eq!(hits[0]["id"], "1");
    assert_eq!(hits[0]["text_rank"], 1);
    assert_eq!(hits[0]["vector_rank"], 2);
    assert!(hits.iter().all(|h| h["id"] != "3"));
    request["text_weight"] = json!(0);
    let hits: Vec<Value> =
        serde_json::from_str(&db.retrieve_json(&request.to_string()).unwrap()).unwrap();
    assert_eq!(hits[0]["id"], "2");
    request["text_weight"] = json!(1);
    let hits: Vec<Value> =
        serde_json::from_str(&db.retrieve_json(&request.to_string()).unwrap()).unwrap();
    assert_eq!(hits.len(), 1);
    assert_eq!(hits[0]["id"], "1");
    request["text_weight"] = json!(1.1);
    assert!(db.retrieve_json(&request.to_string()).is_err());
    request["text_weight"] = json!(0.5);
    request["candidate_limit"] = json!(1);
    assert!(db.retrieve_json(&request.to_string()).is_err());
    request["candidate_limit"] = json!(10);
    request["sql"] = json!("DELETE FROM docs");
    assert!(db.retrieve_json(&request.to_string()).is_err());
}
