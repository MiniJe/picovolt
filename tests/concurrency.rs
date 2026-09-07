#![cfg(not(target_arch = "wasm32"))]

use std::sync::{mpsc, Arc, Barrier};
use std::thread;
use std::time::{Duration, Instant};

use picovolt::{
    CancellationToken, Database, PvError, QueryLimits, QueryResult, RequestOptions, SharedDatabase,
    SharedDatabaseOptions, Value, TRANSACTION_BACKUP_DIR, TRANSACTION_MARKER_FILE,
};

fn count(result: QueryResult) -> i64 {
    match result {
        QueryResult::Rows { rows, .. } => match &rows[0][0] {
            Value::Int(value) => *value,
            other => panic!("expected integer count, got {other:?}"),
        },
        other => panic!("expected rows, got {other:?}"),
    }
}

fn assert_send_sync<T: Send + Sync>() {}
fn assert_send<T: Send>() {}

#[test]
fn handle_is_send_sync_and_transactions_are_send() {
    assert_send_sync::<SharedDatabase>();
    assert_send::<picovolt::ReadTransaction>();
    assert_send::<picovolt::WriteTransaction>();
    assert_eq!(SharedDatabaseOptions::new(0).queue_capacity(), 1);
}

#[test]
fn explicit_transactions_commit_and_enforce_read_only_access() {
    let database = SharedDatabase::open_memory().unwrap();
    database.query("CREATE TABLE t (id, name)").unwrap();

    let mut write = database.begin_write().unwrap();
    write
        .query_with(
            "INSERT INTO t VALUES (?, ?)",
            &[Value::Int(1), Value::Text("alice".into())],
        )
        .unwrap();
    write.query("INSERT INTO t VALUES (2, 'bob')").unwrap();
    assert_eq!(count(write.query("SELECT COUNT(*) FROM t").unwrap()), 2);
    write.commit().unwrap();

    let committed_tx = database.current_tx().unwrap();
    let mut read = database.begin_read().unwrap();
    assert_eq!(read.snapshot_tx(), committed_tx);
    assert!(matches!(
        read.query("INSERT INTO t VALUES (3, 'carol')"),
        Err(PvError::Transaction(_))
    ));
    assert_eq!(count(read.query("SELECT COUNT(*) FROM t").unwrap()), 2);
    read.close().unwrap();
}

#[test]
fn writer_excludes_readers_until_commit() {
    let database = SharedDatabase::open_memory().unwrap();
    database.query("CREATE TABLE t (id)").unwrap();
    let mut write = database.begin_write().unwrap();
    write.query("INSERT INTO t VALUES (1)").unwrap();

    let reader_database = database.clone();
    let (result_sender, result_receiver) = mpsc::channel();
    let reader = thread::spawn(move || {
        let result = reader_database.query("SELECT COUNT(*) FROM t");
        result_sender.send(result).unwrap();
    });
    assert!(matches!(
        result_receiver.recv_timeout(Duration::from_millis(75)),
        Err(mpsc::RecvTimeoutError::Timeout)
    ));

    write.commit().unwrap();
    assert_eq!(
        count(
            result_receiver
                .recv_timeout(Duration::from_secs(2))
                .unwrap()
                .unwrap()
        ),
        1
    );
    reader.join().unwrap();
}

#[test]
fn dropping_write_rolls_back_before_queued_work_runs() {
    let database = SharedDatabase::open_memory().unwrap();
    database.query("CREATE TABLE t (id)").unwrap();
    let mut write = database.begin_write().unwrap();
    write.query("INSERT INTO t VALUES (1)").unwrap();

    let queued_database = database.clone();
    let queued = thread::spawn(move || queued_database.query("SELECT COUNT(*) FROM t"));
    drop(write);

    assert_eq!(count(queued.join().unwrap().unwrap()), 0);
    assert_eq!(count(database.query("SELECT COUNT(*) FROM t").unwrap()), 0);
}

#[test]
fn statement_error_aborts_the_complete_write_transaction() {
    let database = SharedDatabase::open_memory().unwrap();
    database.query("CREATE TABLE t (id)").unwrap();
    let mut write = database.begin_write().unwrap();
    write.query("INSERT INTO t VALUES (1)").unwrap();
    assert!(write.query("INSERT INTO missing VALUES (2)").is_err());
    drop(write);

    assert_eq!(count(database.query("SELECT COUNT(*) FROM t").unwrap()), 0);
}

#[test]
fn bounded_queue_returns_busy_without_blocking() {
    let database = SharedDatabase::open_memory_with_options(SharedDatabaseOptions::new(1)).unwrap();
    database.query("CREATE TABLE t (id)").unwrap();
    let read = database.begin_read().unwrap();

    let barrier = Arc::new(Barrier::new(3));
    let (sender, receiver) = mpsc::channel();
    let mut workers = Vec::new();
    for _ in 0..2 {
        let database = database.clone();
        let barrier = Arc::clone(&barrier);
        let sender = sender.clone();
        workers.push(thread::spawn(move || {
            barrier.wait();
            sender
                .send(database.query("SELECT COUNT(*) FROM t"))
                .unwrap();
        }));
    }
    barrier.wait();

    let first = receiver.recv_timeout(Duration::from_secs(2)).unwrap();
    assert!(matches!(first, Err(PvError::Busy(_))));
    drop(read);
    assert_eq!(
        count(
            receiver
                .recv_timeout(Duration::from_secs(2))
                .unwrap()
                .unwrap()
        ),
        0
    );
    for worker in workers {
        worker.join().unwrap();
    }
}

#[test]
fn queued_cancellation_never_mutates() {
    let database = SharedDatabase::open_memory().unwrap();
    database.query("CREATE TABLE t (id)").unwrap();
    let read = database.begin_read().unwrap();
    let cancellation = CancellationToken::new();
    let options = RequestOptions::default().with_cancellation(cancellation.clone());

    let queued_database = database.clone();
    let queued = thread::spawn(move || {
        queued_database.query_with_options("INSERT INTO t VALUES (1)", &[], options)
    });
    cancellation.cancel();
    drop(read);

    assert!(matches!(queued.join().unwrap(), Err(PvError::Cancelled)));
    assert_eq!(count(database.query("SELECT COUNT(*) FROM t").unwrap()), 0);
}

#[test]
fn expired_write_session_rolls_back_and_releases_worker() {
    let database = SharedDatabase::open_memory().unwrap();
    database.query("CREATE TABLE t (id)").unwrap();
    let limits = QueryLimits::new(
        usize::MAX,
        usize::MAX,
        usize::MAX,
        Some(Instant::now() + Duration::from_millis(75)),
    );
    let mut write = database
        .begin_write_with_options(RequestOptions::new(limits))
        .unwrap();
    write.query("INSERT INTO t VALUES (1)").unwrap();
    thread::sleep(Duration::from_millis(125));
    assert!(matches!(write.commit(), Err(PvError::ResourceLimit(_))));
    assert_eq!(count(database.query("SELECT COUNT(*) FROM t").unwrap()), 0);
}

#[test]
fn transaction_control_is_owned_by_the_shared_api() {
    let database = SharedDatabase::open_memory().unwrap();
    assert!(matches!(
        database.query("BEGIN"),
        Err(PvError::Transaction(_))
    ));
    database.query("CREATE TABLE t (id)").unwrap();

    let mut write = database.begin_write().unwrap();
    write.query("INSERT INTO t VALUES (1)").unwrap();
    assert!(matches!(
        write.query("COMMIT"),
        Err(PvError::Transaction(_))
    ));
    drop(write);
    assert_eq!(count(database.query("SELECT COUNT(*) FROM t").unwrap()), 0);
}

#[test]
fn last_database_handle_can_drop_before_live_sessions() {
    let mut read = {
        let database = SharedDatabase::open_memory().unwrap();
        database.query("CREATE TABLE t (id)").unwrap();
        let read = database.begin_read().unwrap();
        drop(database);
        read
    };
    assert_eq!(count(read.query("SELECT COUNT(*) FROM t").unwrap()), 0);
    read.close().unwrap();

    let temporary = tempfile::tempdir().unwrap();
    let workspace = temporary.path().join("shared.pv");
    let mut write = {
        let database = SharedDatabase::open_dev(&workspace).unwrap();
        database.query("CREATE TABLE t (id)").unwrap();
        let mut write = database.begin_write().unwrap();
        write.query("INSERT INTO t VALUES (7)").unwrap();
        drop(database);
        write
    };
    write.query("INSERT INTO t VALUES (8)").unwrap();
    write.commit().unwrap();

    let mut reopened = Database::open_dev(&workspace).unwrap();
    assert_eq!(count(reopened.query("SELECT COUNT(*) FROM t").unwrap()), 2);
}

#[test]
fn dropping_last_database_and_writer_returns_promptly_then_rolls_back() {
    let temporary = tempfile::tempdir().unwrap();
    let workspace = temporary.path().join("detached-rollback.pv");
    let write = {
        let database = SharedDatabase::open_dev(&workspace).unwrap();
        database.query("CREATE TABLE t (id)").unwrap();
        let mut write = database.begin_write().unwrap();
        write.query("INSERT INTO t VALUES (7)").unwrap();
        drop(database);
        write
    };

    let started = Instant::now();
    drop(write);
    assert!(started.elapsed() < Duration::from_millis(250));

    let deadline = Instant::now() + Duration::from_secs(3);
    let mut reopened = loop {
        match Database::open_dev(&workspace) {
            Ok(database) => break database,
            Err(_) if Instant::now() < deadline => thread::sleep(Duration::from_millis(10)),
            Err(error) => panic!("background rollback did not release the workspace: {error}"),
        }
    };
    assert_eq!(count(reopened.query("SELECT COUNT(*) FROM t").unwrap()), 0);
}

#[test]
fn cloned_handles_do_not_lose_writes() {
    let database = SharedDatabase::open_memory().unwrap();
    database.query("CREATE TABLE t (id PRIMARY KEY)").unwrap();
    let barrier = Arc::new(Barrier::new(5));
    let mut workers = Vec::new();
    for worker_id in 0..4 {
        let database = database.clone();
        let barrier = Arc::clone(&barrier);
        workers.push(thread::spawn(move || {
            barrier.wait();
            for offset in 0..10 {
                let id = worker_id * 10 + offset;
                database
                    .query_with("INSERT INTO t VALUES (?)", &[Value::Int(id)])
                    .unwrap();
            }
        }));
    }
    barrier.wait();
    for worker in workers {
        worker.join().unwrap();
    }
    assert_eq!(count(database.query("SELECT COUNT(*) FROM t").unwrap()), 40);
}

#[test]
fn failed_begin_never_removes_preexisting_recovery_evidence() {
    let temporary = tempfile::tempdir().unwrap();
    let workspace = temporary.path().join("existing-marker.pv");
    let mut database = Database::open_dev(&workspace).unwrap();
    database.query("CREATE TABLE t (id)").unwrap();

    let backup = workspace.join(TRANSACTION_BACKUP_DIR);
    std::fs::create_dir(&backup).unwrap();
    std::fs::write(backup.join("owner"), b"preexisting").unwrap();
    std::fs::write(workspace.join(TRANSACTION_MARKER_FILE), b"PVTX1\n").unwrap();

    assert!(database.begin_transaction().is_err());
    assert!(workspace.join(TRANSACTION_MARKER_FILE).is_file());
    assert_eq!(std::fs::read(backup.join("owner")).unwrap(), b"preexisting");
}

#[test]
fn transient_writer_lock_conflict_does_not_close_coordinator() {
    let temporary = tempfile::tempdir().unwrap();
    let workspace = temporary.path().join("lock-conflict.pv");
    let first = SharedDatabase::open_dev(&workspace).unwrap();
    first.query("CREATE TABLE t (id)").unwrap();
    let second = SharedDatabase::open_dev(&workspace).unwrap();

    let first_write = first.begin_write().unwrap();
    assert!(matches!(second.begin_write(), Err(PvError::Transaction(_))));
    first_write.rollback().unwrap();

    let mut second_write = second.begin_write().unwrap();
    second_write.query("INSERT INTO t VALUES (1)").unwrap();
    second_write.commit().unwrap();
    assert_eq!(count(second.query("SELECT COUNT(*) FROM t").unwrap()), 1);
}

#[test]
fn read_only_and_rejected_begins_leave_coordinator_usable() {
    let temporary = tempfile::tempdir().unwrap();
    let image = temporary.path().join("readonly.pvdb");
    let mut source = Database::open_memory();
    source.query("CREATE TABLE t (id)").unwrap();
    source.query("INSERT INTO t VALUES (1)").unwrap();
    source.bake(&image).unwrap();

    let production = SharedDatabase::open_prod(&image).unwrap();
    assert!(matches!(production.begin_write(), Err(PvError::ReadOnly)));
    assert_eq!(
        count(production.query("SELECT COUNT(*) FROM t").unwrap()),
        1
    );

    let database = SharedDatabase::open_memory().unwrap();
    let cancellation = CancellationToken::new();
    cancellation.cancel();
    let cancelled = RequestOptions::default().with_cancellation(cancellation);
    assert!(matches!(
        database.begin_read_with_options(cancelled.clone()),
        Err(PvError::Cancelled)
    ));
    assert!(matches!(
        database.begin_write_with_options(cancelled),
        Err(PvError::Cancelled)
    ));

    let expired = RequestOptions::new(QueryLimits::new(
        usize::MAX,
        usize::MAX,
        usize::MAX,
        Some(Instant::now()),
    ));
    assert!(matches!(
        database.begin_read_with_options(expired.clone()),
        Err(PvError::ResourceLimit(_))
    ));
    assert!(matches!(
        database.begin_write_with_options(expired),
        Err(PvError::ResourceLimit(_))
    ));
    database.query("CREATE TABLE still_usable (id)").unwrap();
}

#[test]
fn transaction_limits_cannot_be_weakened_and_future_snapshots_are_rejected() {
    let database = SharedDatabase::open_memory().unwrap();
    database.query("CREATE TABLE t (id)").unwrap();
    for id in 0..3 {
        database
            .query_with("INSERT INTO t VALUES (?)", &[Value::Int(id)])
            .unwrap();
    }

    let restrictive = RequestOptions::new(QueryLimits::new(1, usize::MAX, usize::MAX, None));
    let mut read = database.begin_read_with_options(restrictive).unwrap();
    assert!(matches!(
        read.query_with_limits(
            "SELECT * FROM t",
            &[],
            QueryLimits::new(usize::MAX, usize::MAX, usize::MAX, None)
        ),
        Err(PvError::ResourceLimit(_))
    ));
    let future = read.snapshot_tx() + 1;
    assert!(matches!(
        read.query(&format!("SELECT * FROM t BEFORE {future}")),
        Err(PvError::Transaction(_))
    ));
    read.close().unwrap();
}

#[test]
fn malformed_reads_are_recoverable_but_write_errors_abort_the_session() {
    let database = SharedDatabase::open_memory().unwrap();
    database.query("CREATE TABLE t (id)").unwrap();
    assert!(database.query("SELECT FROM").is_err());
    assert_eq!(count(database.query("SELECT COUNT(*) FROM t").unwrap()), 0);

    let mut read = database.begin_read().unwrap();
    assert!(read.query("SELECT FROM").is_err());
    assert_eq!(count(read.query("SELECT COUNT(*) FROM t").unwrap()), 0);
    read.close().unwrap();

    let mut write = database.begin_write().unwrap();
    write.query("INSERT INTO t VALUES (1)").unwrap();
    assert!(write.query("SELECT FROM").is_err());
    assert!(matches!(write.commit(), Err(PvError::TransactionClosed)));
    assert_eq!(count(database.query("SELECT COUNT(*) FROM t").unwrap()), 0);
}

#[test]
fn rollback_failure_is_outcome_unknown_and_closes_the_coordinator() {
    let temporary = tempfile::tempdir().unwrap();
    let workspace = temporary.path().join("rollback-failure.pv");
    let database = SharedDatabase::open_dev(&workspace).unwrap();
    database.query("CREATE TABLE t (id)").unwrap();

    let mut write = database.begin_write().unwrap();
    std::fs::remove_dir_all(workspace.join(TRANSACTION_BACKUP_DIR)).unwrap();
    assert!(matches!(
        write.query("INSERT INTO missing VALUES (1)"),
        Err(PvError::TransactionOutcomeUnknown(_))
    ));
    assert!(matches!(
        database.query("SELECT COUNT(*) FROM t"),
        Err(PvError::DatabaseClosed)
    ));
}
