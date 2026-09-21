// PicoVolt 2.3 qualification; see legal/COMPONENT-SCOPE-2.3.md.
#![cfg(all(feature = "full-text", feature = "vector-search", not(target_arch = "wasm32")))]
use picovolt::Database;
use serde_json::json;
use std::process::Command;

const CHILD: &str = "PV23_CRASH_CHILD";

fn check(db: &mut Database) {
    for (kind, name) in [("full_text", "ft"), ("vector", "vx")] {
        let mut request = if kind == "full_text" {
            json!({"kind":kind,"sql":"SELECT * FROM docs","id_column":"id","text_columns":["body"],"query":"commit","limit":100})
        } else {
            json!({"kind":kind,"sql":"SELECT * FROM docs","id_column":"id","vector_column":"embedding","metric":"cosine","query":[1,0],"limit":100})
        };
        let reference = db.retrieve_json(&request.to_string()).unwrap();
        request["index"] = json!(name);
        assert_eq!(db.retrieve_json(&request.to_string()).unwrap(), reference);
        db.verify_retrieval_index(name).unwrap();
    }
}

#[test]
fn crash_child() {
    let Ok(root) = std::env::var(CHILD) else { return; };
    let cycle: i64 = std::env::var("PV23_CRASH_ID").unwrap().parse().unwrap();
    let commit = std::env::var("PV23_CRASH_COMMIT").unwrap() == "yes";
    let ready = std::env::var("PV23_CRASH_READY").unwrap();
    let mut db = Database::open_dev(root).unwrap();
    db.begin_transaction().unwrap();
    db.query(&format!("INSERT INTO docs VALUES ({cycle},'commit candidate','[1,1]')")).unwrap();
    db.query(&format!("UPDATE docs SET body='commit cycle {cycle}' WHERE id=0")).unwrap();
    db.query("UPDATE docs SET embedding='[0,1]' WHERE id=0").unwrap();
    if commit { db.commit_transaction().unwrap(); } else { db.flush_now().unwrap(); }
    let mut signal = std::fs::File::create(ready).unwrap();
    std::io::Write::write_all(&mut signal, b"ready").unwrap(); signal.sync_all().unwrap();
    std::process::exit(86);
}

#[test]
fn abrupt_exit_keeps_table_and_indexes_on_one_committed_state() {
    let temp = tempfile::tempdir().unwrap();
    for logged in [false, true] {
        let root = temp.path().join(if logged { "logged" } else { "unlogged" });
        let mut db = Database::open_dev(&root).unwrap();
        db.query("CREATE TABLE docs (id,body,embedding)").unwrap();
        db.query("INSERT INTO docs VALUES (0,'commit baseline','[1,0]')").unwrap();
        if logged { db.enable_commit_log(picovolt::CommitLogOptions::default()).unwrap(); }
        db.query("CREATE INDEX ft ON docs USING FULLTEXT (body) WITH (id_column='id')").unwrap();
        db.query("CREATE INDEX vx ON docs USING VECTOR (embedding) WITH (id_column='id',metric='cosine',dimensions=2)").unwrap();
        let mut expected = db.verification_hash().unwrap();
        drop(db);
        let mut rows = 1;
        for cycle in 1..=12 {
            let commit = cycle % 3 == 0;
            let ready = temp.path().join(format!("ready-{logged}-{cycle}"));
            let output = Command::new(std::env::current_exe().unwrap())
                .args(["--exact", "crash_child", "--test-threads=1"])
                .env(CHILD, &root).env("PV23_CRASH_ID", cycle.to_string())
                .env("PV23_CRASH_COMMIT", if commit { "yes" } else { "no" })
                .env("PV23_CRASH_READY", &ready).output().unwrap();
            assert!(ready.is_file(), "child failed: {} {}", String::from_utf8_lossy(&output.stdout), String::from_utf8_lossy(&output.stderr));
            assert_eq!(output.status.code(), Some(86));
            let mut db = Database::open_dev(&root).unwrap();
            if commit { rows += 1; expected = db.verification_hash().unwrap(); }
            else { assert_eq!(db.verification_hash().unwrap(), expected, "uncommitted cycle {cycle} leaked"); }
            assert_eq!(db.row_count("docs", None).unwrap(), rows);
            assert_eq!(db.row_count("docs", None).unwrap() as usize, db.retrieval_indexes()[0].document_count);
            check(&mut db);
            drop(db);
        }
    }
}

#[test]
fn dropping_an_explicit_writer_rolls_back_retrieval_catalog_and_rows() {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path().join("drop");
    let mut db = Database::open_dev(&root).unwrap();
    db.query("CREATE TABLE docs (id,body,embedding)").unwrap();
    db.query("INSERT INTO docs VALUES (1,'commit baseline','[1,0]')").unwrap();
    db.query("CREATE INDEX ft ON docs USING FULLTEXT (body) WITH (id_column='id')").unwrap();
    db.query("CREATE INDEX vx ON docs USING VECTOR (embedding) WITH (id_column='id',metric='cosine',dimensions=2)").unwrap();
    let before = db.verification_hash().unwrap();
    db.begin_transaction().unwrap();
    db.query("DELETE FROM docs WHERE id=1").unwrap();
    db.query("DROP INDEX ft").unwrap();
    drop(db);
    let mut reopened = Database::open_dev(&root).unwrap();
    assert_eq!(before, reopened.verification_hash().unwrap());
    check(&mut reopened);
}
