#![cfg(all(feature = "full-text", feature = "vector-search"))]
use picovolt::vector::{Metric, VectorError, VectorIndex};
use picovolt::{Database, Value};
use serde_json::json;

#[test]
fn vector_exact_ranking_updates_and_extreme_values() {
    for metric in [Metric::Cosine, Metric::SquaredEuclidean] {
        let mut index = VectorIndex::new(2, metric).unwrap();
        index.upsert(4, &[1.0, 0.0]).unwrap();
        index.upsert(2, &[1.0, 0.0]).unwrap();
        index.upsert(7, &[0.0, 1.0]).unwrap();
        assert_eq!(
            index
                .search(&[1.0, 0.0], 2)
                .unwrap()
                .iter()
                .map(|h| h.id)
                .collect::<Vec<_>>(),
            vec![2, 4]
        );
        assert_eq!(
            index.upsert(2, &[f32::NAN, 0.0]),
            Err(VectorError::InvalidVector)
        );
        assert_eq!(index.search(&[1.0, 0.0], 1).unwrap()[0].id, 2);
        index.upsert(2, &[-1.0, 0.0]).unwrap();
        assert_eq!(index.search(&[1.0, 0.0], 1).unwrap()[0].id, 4);
        assert!(index.remove(4));
        assert!(!index.remove(4));
        assert_eq!(index.search(&[1.0], 2), Err(VectorError::InvalidVector));
        assert_eq!(index.search(&[1.0, 0.0], 101), Err(VectorError::Limit));
        index.upsert(99, &[f32::MAX, 1.0]).unwrap();
        assert!(index
            .search(&[f32::MAX, 1.0], 3)
            .unwrap()
            .iter()
            .all(|h| h.distance.is_finite()));
    }
    let mut cosine = VectorIndex::new(2, Metric::Cosine).unwrap();
    assert_eq!(
        cosine.upsert(1, &[0.0, 0.0]),
        Err(VectorError::InvalidVector)
    );
    cosine.upsert(1, &[f32::from_bits(1), 0.0]).unwrap();
    assert_eq!(
        cosine.search(&[f32::from_bits(1), 0.0], 1).unwrap()[0].distance,
        0.0
    );
}

fn database() -> Database {
    let mut db = Database::open_memory();
    db.query("CREATE TABLE docs (id, tenant, title, embedding)")
        .unwrap();
    for (id, tenant, title, vector) in [
        (1, "a", "SQL database transactions", "[1,0]"),
        (2, "b", "SQL SQL database transactions", "[1,0]"),
        (3, "a", "Backups and recovery", "[0,1]"),
    ] {
        db.query_with(
            "INSERT INTO docs VALUES (?,?,?,?)",
            &[
                Value::Int(id),
                Value::Text(tenant.into()),
                Value::Text(title.into()),
                Value::Text(vector.into()),
            ],
        )
        .unwrap();
    }
    db
}

#[test]
fn retrieval_filters_before_ranking_and_rejects_all_mutations() {
    let mut db = database();
    let mut request = json!({"kind":"full_text","sql":"SELECT id,title FROM docs WHERE tenant=?","params":["a"],"id_column":"id","text_columns":["title"],"query":"sql database","limit":10});
    let result: serde_json::Value =
        serde_json::from_str(&db.retrieve_json(&request.to_string()).unwrap()).unwrap();
    assert_eq!(result[0]["id"], "1");
    assert_eq!(result.as_array().unwrap().len(), 1);
    for sql in [
        "DELETE FROM docs WHERE id=1",
        "DROP TABLE docs",
        "BEGIN",
        "SELECT * FROM docs; DELETE FROM docs WHERE id=1",
    ] {
        request["sql"] = json!(sql);
        request["params"] = json!([]);
        assert!(db.retrieve_json(&request.to_string()).is_err());
    }
    assert_eq!(
        db.query("SELECT * FROM docs")
            .unwrap()
            .rows()
            .unwrap()
            .len(),
        3
    );
    let vector = json!({"kind":"vector","sql":"SELECT id,embedding FROM docs WHERE tenant=?","params":["a"],"id_column":"id","vector_column":"embedding","query":[1,0],"metric":"cosine","limit":2});
    let results: serde_json::Value =
        serde_json::from_str(&db.retrieve_json(&vector.to_string()).unwrap()).unwrap();
    assert_eq!(results[0]["id"], "1");
    assert_eq!(results[1]["id"], "3");
}

#[test]
fn retrieval_preserves_large_ids_and_baked_compatibility() {
    let dir = tempfile::tempdir().unwrap();
    let mut db = database();
    db.query("UPDATE docs SET id=9223372036854775807 WHERE id=1")
        .unwrap();
    let request = json!({"kind":"full_text","sql":"SELECT id,title FROM docs WHERE tenant='a'","id_column":"id","text_columns":["title"],"query":"sql","limit":10});
    let before = db.retrieve_json(&request.to_string()).unwrap();
    assert!(before.contains("\"9223372036854775807\""));
    let path = dir.path().join("docs.pvdb");
    db.bake(&path).unwrap();
    let mut baked = Database::open_prod(&path).unwrap();
    assert_eq!(before, baked.retrieve_json(&request.to_string()).unwrap());
    let result = std::process::Command::new(env!("CARGO_BIN_EXE_pv"))
        .arg("--version")
        .output()
        .unwrap();
    assert!(String::from_utf8(result.stdout).unwrap().contains("2.1.0"));
    let request_path = dir.path().join("request.json");
    std::fs::write(&request_path, request.to_string()).unwrap();
    let cli = std::process::Command::new(env!("CARGO_BIN_EXE_pv"))
        .args([
            "retrieve",
            path.to_str().unwrap(),
            request_path.to_str().unwrap(),
        ])
        .output()
        .unwrap();
    assert!(cli.status.success());
    assert_eq!(String::from_utf8(cli.stdout).unwrap().trim(), before);
}

#[test]
fn vector_admission_bounds_do_not_mutate_prior_index() {
    assert!(VectorIndex::new(0, Metric::Cosine).is_err());
    assert!(VectorIndex::new(4097, Metric::Cosine).is_err());
    let mut index = VectorIndex::new(4096, Metric::Cosine).unwrap();
    let vector = vec![1.0; 4096];
    for id in 0..1024 {
        index.upsert(id, &vector).unwrap();
    }
    assert_eq!(index.upsert(1024, &vector), Err(VectorError::Limit));
    assert_eq!(index.len(), 1024);
    index.upsert(1, &vector).unwrap();
}
