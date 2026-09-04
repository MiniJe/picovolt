//! Deterministic model-based stress test. Increase `PICOVOLT_STRESS_STEPS` for
//! scheduled or dedicated-runner soak jobs without slowing ordinary CI.

use std::collections::BTreeMap;

use picovolt::{Database, Value};

#[test]
fn randomized_mvcc_compaction_and_reopen_match_reference_model() {
    let steps = std::env::var("PICOVOLT_STRESS_STEPS")
        .ok()
        .and_then(|value| value.parse().ok())
        .unwrap_or(500usize);
    let mut random = 0xA076_1D64_78BD_642Fu64;
    let mut database = Database::open_memory();
    database
        .query("CREATE TABLE model (id PRIMARY KEY, value, bucket)")
        .unwrap();
    database.query("CREATE INDEX ON model (bucket)").unwrap();
    let mut expected = BTreeMap::<i64, (i64, i64)>::new();
    let mut snapshots = Vec::<(u64, BTreeMap<i64, (i64, i64)>)>::new();
    snapshots.push((database.current_tx(), expected.clone()));

    for step in 0..steps {
        random = random
            .wrapping_mul(6_364_136_223_846_793_005)
            .wrapping_add(1_442_695_040_888_963_407);
        let id = ((random >> 17) % 64) as i64;
        let value = ((random >> 29) % 10_000) as i64;
        let bucket = value % 8;
        match random % 3 {
            0 if !expected.contains_key(&id) => {
                database
                    .insert(
                        "model",
                        vec![Value::Int(id), Value::Int(value), Value::Int(bucket)],
                    )
                    .unwrap();
                expected.insert(id, (value, bucket));
            }
            1 if expected.contains_key(&id) => {
                database
                    .query(&format!("UPDATE model SET value = {value} WHERE id = {id}"))
                    .unwrap();
                // Keep the independently indexed bucket stable: this operation
                // exercises an update without changing unrelated cells.
                expected.get_mut(&id).unwrap().0 = value;
            }
            2 if expected.contains_key(&id) => {
                database
                    .query(&format!("DELETE FROM model WHERE id = {id}"))
                    .unwrap();
                expected.remove(&id);
            }
            _ => {}
        }
        snapshots.push((database.current_tx(), expected.clone()));

        if step % 37 == 0 {
            assert_latest(&mut database, &expected);
            let pick = (random as usize) % snapshots.len();
            let (transaction, state) = &snapshots[pick];
            assert_snapshot(&database, *transaction, state);
        }
        if step > 0 && step % 100 == 0 {
            let before = database.verification_hash().unwrap();
            let _ = database.compact_step(4).unwrap();
            assert_eq!(database.verification_hash().unwrap(), before);
            let image = database.bake_to_bytes().unwrap();
            database = Database::import_bytes(&image).unwrap();
            assert_latest(&mut database, &expected);
        }
    }
    assert_latest(&mut database, &expected);
}

fn assert_latest(database: &mut Database, expected: &BTreeMap<i64, (i64, i64)>) {
    let rows = database
        .query("SELECT id, value, bucket FROM model ORDER BY id")
        .unwrap()
        .rows()
        .unwrap()
        .to_vec();
    assert_eq!(rows, model_rows(expected));
    for bucket in 0..8i64 {
        let actual = database
            .query(&format!(
                "SELECT id FROM model WHERE bucket = {bucket} ORDER BY id"
            ))
            .unwrap()
            .rows()
            .unwrap()
            .len();
        let wanted = expected
            .values()
            .filter(|(_, expected_bucket)| *expected_bucket == bucket)
            .count();
        assert_eq!(actual, wanted, "bucket {bucket}");
    }
}

fn assert_snapshot(database: &Database, transaction: u64, expected: &BTreeMap<i64, (i64, i64)>) {
    let (_, mut rows) = database.select("model", Some(transaction)).unwrap();
    rows.sort();
    assert_eq!(rows, model_rows(expected), "transaction {transaction}");
}

fn model_rows(expected: &BTreeMap<i64, (i64, i64)>) -> Vec<Vec<Value>> {
    expected
        .iter()
        .map(|(id, (value, bucket))| vec![Value::Int(*id), Value::Int(*value), Value::Int(*bucket)])
        .collect()
}
