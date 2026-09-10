#![cfg(not(target_arch = "wasm32"))]

use picovolt::{
    CommitLogOptions, Database, PvError, QueryResult, SharedDatabase, SharedDatabaseOptions, Value,
};
use std::sync::{mpsc, Arc, Barrier};
use std::thread;
use std::time::Duration;

fn count(result: QueryResult) -> i64 {
    let Value::Int(n) = result.rows().unwrap()[0][0] else {
        panic!("expected count")
    };
    n
}

#[test]
fn readers_preserve_catalog_pages_indexes_and_blobs_while_writer_runs() {
    let temp = tempfile::tempdir().unwrap();
    let db = SharedDatabase::open_dev(temp.path()).unwrap();
    db.query("CREATE TABLE t (id, value)").unwrap();
    db.query("CREATE INDEX ON t (id)").unwrap();
    db.query("INSERT INTO t VALUES (1, 'original long content addressed payload')")
        .unwrap();
    let mut first = db.begin_read().unwrap();
    let second = db.begin_read().unwrap();
    let before = first.query("SELECT * FROM t WHERE id = 1").unwrap();
    let mut writer = db.begin_write().unwrap();
    writer
        .query("UPDATE t SET value = 'replacement long payload' WHERE id = 1")
        .unwrap();
    let (sent, received) = mpsc::channel();
    let barrier = Arc::new(Barrier::new(3));
    let mut workers = Vec::new();
    for mut reader in [first, second] {
        let sent = sent.clone();
        let barrier = barrier.clone();
        workers.push(thread::spawn(move || {
            barrier.wait();
            sent.send(reader.query("SELECT * FROM t WHERE id = 1").unwrap())
                .unwrap();
            reader
        }));
    }
    barrier.wait();
    for _ in 0..2 {
        assert_eq!(
            received
                .recv_timeout(Duration::from_secs(5))
                .unwrap()
                .rows(),
            before.rows()
        );
    }
    writer.query("DROP TABLE t").unwrap();
    writer.commit().unwrap();
    assert!(db.query("SELECT * FROM t").is_err());
    for worker in workers {
        let mut reader = worker.join().unwrap();
        assert_eq!(
            reader.query("SELECT * FROM t WHERE id = 1").unwrap().rows(),
            before.rows()
        );
        reader.close().unwrap();
    }
}

#[test]
fn snapshot_limits_reject_without_poisoning_database() {
    let db = SharedDatabase::open_memory_with_options(
        SharedDatabaseOptions::new(8).with_snapshot_limits(1, 1024 * 1024),
    )
    .unwrap();
    db.query("CREATE TABLE t (id)").unwrap();
    let read = db.begin_read().unwrap();
    assert!(matches!(db.begin_read(), Err(PvError::Busy(_))));
    db.query("INSERT INTO t VALUES (1)").unwrap();
    read.close().unwrap();
    for _ in 0..20 {
        db.begin_read().unwrap().close().unwrap();
    }
    let db = SharedDatabase::open_memory_with_options(
        SharedDatabaseOptions::new(8).with_snapshot_limits(1, 1),
    )
    .unwrap();
    assert!(matches!(db.begin_read(), Err(PvError::ResourceLimit(_))));
    db.query("CREATE TABLE t (id)").unwrap();
}

#[test]
fn durable_stream_groups_transactions_survives_reopen_and_prunes_explicitly() {
    let temp = tempfile::tempdir().unwrap();
    let db = SharedDatabase::open_dev(temp.path()).unwrap();
    db.query("CREATE TABLE t (id, value)").unwrap();
    let mut write = db.begin_write().unwrap();
    write
        .query("INSERT INTO t VALUES (1, 'a long content addressed value')")
        .unwrap();
    write
        .query("INSERT INTO t VALUES (2, 'another long content addressed value')")
        .unwrap();
    write.commit().unwrap();
    let mut failed = db.begin_write().unwrap();
    failed.query("DELETE FROM t WHERE id = 1").unwrap();
    failed.rollback().unwrap();
    let changes = db.changes_since(0, 10).unwrap();
    assert_eq!(changes.len(), 2);
    assert_eq!(changes[1].sequence, 2);
    assert_eq!(changes[1].blobs.len(), 2);
    assert!(!changes[1].pages.is_empty());
    assert_eq!(changes[1].after_tx, db.current_tx().unwrap());
    db.prune_changes(1).unwrap();
    assert!(db.changes_since(0, 1).is_err());
    assert_eq!(db.changes_since(1, 1).unwrap(), changes[1..]);
    db.prune_changes(2).unwrap();
    db.query("INSERT INTO t VALUES (3, 'next')").unwrap();
    assert_eq!(db.changes_since(2, 1).unwrap()[0].sequence, 3);
    let reopened = Database::open_dev(temp.path()).unwrap();
    assert_eq!(reopened.changes_since(2, 10).unwrap()[0].sequence, 3);
}

#[test]
fn retained_log_backpressure_recovers_after_acknowledgement() {
    let temp = tempfile::tempdir().unwrap();
    let options = SharedDatabaseOptions::new(8).with_commit_log(CommitLogOptions {
        max_retained_commits: 2,
        ..CommitLogOptions::default()
    });
    let db = SharedDatabase::open_dev_with_options(temp.path(), options).unwrap();
    db.query("CREATE TABLE t (id)").unwrap();
    db.query("INSERT INTO t VALUES (1)").unwrap();
    assert!(matches!(
        db.query("INSERT INTO t VALUES (2)"),
        Err(PvError::ResourceLimit(_))
    ));
    assert_eq!(count(db.query("SELECT COUNT(*) FROM t").unwrap()), 1);
    db.prune_changes(2).unwrap();
    db.query("INSERT INTO t VALUES (2)").unwrap();
    assert_eq!(count(db.query("SELECT COUNT(*) FROM t").unwrap()), 2);
}

#[test]
fn byte_limit_rolls_back_writes_and_keeps_stream_gap_free() {
    let temp = tempfile::tempdir().unwrap();
    let options = SharedDatabaseOptions::new(8).with_commit_log(CommitLogOptions {
        max_transaction_bytes: 24000,
        ..CommitLogOptions::default()
    });
    let db = SharedDatabase::open_dev_with_options(temp.path(), options).unwrap();
    db.query("CREATE TABLE t (id, data)").unwrap();
    assert!(matches!(
        db.query_with(
            "INSERT INTO t VALUES (?, ?)",
            &[Value::Int(1), Value::Text("x".repeat(30000))]
        ),
        Err(PvError::ResourceLimit(_))
    ));
    assert_eq!(count(db.query("SELECT COUNT(*) FROM t").unwrap()), 0);
    assert_eq!(db.changes_since(0, 10).unwrap().len(), 1);
    db.query("INSERT INTO t VALUES (2, 'ok')").unwrap();
    assert_eq!(db.changes_since(0, 10).unwrap().len(), 2);
}

#[test]
fn deterministic_transaction_model_matches_commit_stream() {
    let temp = tempfile::tempdir().unwrap();
    let db = SharedDatabase::open_dev(temp.path()).unwrap();
    db.query("CREATE TABLE t (id)").unwrap();
    let mut model = 0;
    let mut commits = 1;
    let mut random = 91u64;
    for id in 0..80 {
        let mut before = db.begin_read().unwrap();
        let prior = model;
        let mut write = db.begin_write().unwrap();
        write
            .query_with("INSERT INTO t VALUES (?)", &[Value::Int(id)])
            .unwrap();
        random = random.wrapping_mul(6364136223846793005).wrapping_add(1);
        if random >> 63 == 0 {
            write.commit().unwrap();
            model += 1;
            commits += 1;
        } else {
            write.rollback().unwrap();
        }
        assert_eq!(
            count(before.query("SELECT COUNT(*) FROM t").unwrap()),
            prior
        );
        before.close().unwrap();
        assert_eq!(count(db.query("SELECT COUNT(*) FROM t").unwrap()), model);
        assert_eq!(db.changes_since(0, 100).unwrap().len(), commits);
    }
}

#[test]
fn log_format_prevents_legacy_recovery_bypass_and_sql_reopen_preserves_stream() {
    let temp = tempfile::tempdir().unwrap();
    let db = SharedDatabase::open_dev(temp.path()).unwrap();
    db.query("CREATE TABLE t (id)").unwrap();
    let manifest: serde_json::Value =
        serde_json::from_slice(&std::fs::read(temp.path().join(picovolt::MANIFEST_FILE)).unwrap())
            .unwrap();
    assert_eq!(
        manifest["format_version"],
        picovolt::FORMAT_VERSION_COMMIT_ANCHOR
    );
    // The worker is idle; the public contract prohibits using these two live
    // handles concurrently. The reopened sequential API detects the log.
    let mut sequential = Database::open_dev(temp.path()).unwrap();
    sequential.query("INSERT INTO t VALUES (1)").unwrap();
    assert_eq!(sequential.changes_since(0, 10).unwrap().len(), 2);
    assert!(matches!(
        sequential.insert("t", vec![Value::Int(2)]),
        Err(PvError::Transaction(_))
    ));
}

#[test]
fn physical_change_stream_reconstructs_a_verified_database() {
    use std::io::{Seek, SeekFrom, Write};
    struct Replica(std::path::PathBuf);
    impl picovolt::ChangeSink for Replica {
        fn accept(&mut self, commit: &picovolt::ChangeCommit) -> picovolt::Result<()> {
            for page in &commit.pages {
                let per_chunk = picovolt::storage::vle::PAGES_PER_CHUNK;
                let mut file = std::fs::OpenOptions::new()
                    .write(true)
                    .create(true)
                    .truncate(false)
                    .open(
                        self.0
                            .join("chunks")
                            .join(format!("chunk_{:05}.pvd", page.page_id / per_chunk)),
                    )?;
                file.seek(SeekFrom::Start(
                    page.page_id % per_chunk * picovolt::PAGE_SIZE as u64,
                ))?;
                file.write_all(&page.bytes)?;
            }
            for blob in &commit.blobs {
                assert_eq!(blake3::hash(&blob.bytes).to_hex().as_str(), blob.hash);
                let parent = self.0.join("blobs").join(&blob.hash[..2]);
                std::fs::create_dir_all(&parent)?;
                std::fs::write(parent.join(&blob.hash), &blob.bytes)?;
            }
            std::fs::write(self.0.join(picovolt::MANIFEST_FILE), &commit.manifest)?;
            // This offline test sink does not retain upstream log records.
            // Persist its applied cursor as the retention floor before open.
            // A production sink must publish these files atomically/durably.
            let log = self.0.join(picovolt::COMMIT_LOG_DIR);
            std::fs::create_dir_all(&log)?;
            let cursor = commit.sequence.to_le_bytes();
            let mut checkpoint = blake3::hash(&cursor).as_bytes().to_vec();
            checkpoint.extend_from_slice(&cursor);
            std::fs::write(log.join("checkpoint"), checkpoint)?;
            Ok(())
        }
    }
    let source = tempfile::tempdir().unwrap();
    let replica = tempfile::tempdir().unwrap();
    std::fs::create_dir(replica.path().join("chunks")).unwrap();
    let db = SharedDatabase::open_dev(source.path()).unwrap();
    db.query("CREATE TABLE t (id PRIMARY KEY, payload)")
        .unwrap();
    db.query("INSERT INTO t VALUES (1, 'a content addressed payload')")
        .unwrap();
    db.query("UPDATE t SET payload = 'updated content addressed payload' WHERE id = 1")
        .unwrap();
    let mut sink = Replica(replica.path().to_owned());
    assert_eq!(db.visit_changes(0, 10, &mut sink).unwrap(), 3);
    let original = Database::open_dev(source.path()).unwrap();
    let copy = Database::open_dev(replica.path()).unwrap();
    assert_eq!(
        original.verification_hash().unwrap(),
        copy.verification_hash().unwrap()
    );
}

#[test]
fn checkpoint_cursor_and_maintenance_are_serialized_with_writes() {
    let temp = tempfile::tempdir().unwrap();
    let workspace = temp.path().join("workspace");
    let db = SharedDatabase::open_dev(&workspace).unwrap();
    db.query("CREATE TABLE t (id)").unwrap();
    db.query("INSERT INTO t VALUES (1)").unwrap();
    let mut read = db.begin_read().unwrap();
    let output = temp.path().join("base.pvdb");
    let checkpoint = db.export_checkpoint(&output).unwrap();
    assert_eq!(checkpoint.sequence, 2);
    assert_eq!(
        Database::open_prod(&output)
            .unwrap()
            .verification_hash()
            .unwrap(),
        checkpoint.verification_hash
    );
    assert!(db.export_checkpoint(&output).is_err());
    assert!(db.export_checkpoint(workspace.join("image.pvdb")).is_err());
    db.query("INSERT INTO t VALUES (2)").unwrap();
    assert_eq!(
        db.changes_since(checkpoint.sequence, 10).unwrap()[0].sequence,
        3
    );
    db.compact_step(1).unwrap();
    assert_eq!(count(read.query("SELECT COUNT(*) FROM t").unwrap()), 1);
    read.close().unwrap();
    assert_eq!(count(db.query("SELECT COUNT(*) FROM t").unwrap()), 2);
}
