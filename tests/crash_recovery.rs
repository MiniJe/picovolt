//! Cross-process probes for the filesystem transaction recovery contract.
//!
//! The child exits without unwinding after it has synced dirty pages. This is
//! deliberately different from dropping a `Database`: no Rust destructor gets
//! an opportunity to tidy the transaction marker or rollback image.

use std::fs::{self, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, ExitStatus};

use picovolt::{Database, Value};

const CHILD_MODE: &str = "PICOVOLT_CRASH_CHILD";
const CHILD_ROOT: &str = "PICOVOLT_CRASH_ROOT";
const CHILD_ID: &str = "PICOVOLT_CRASH_ID";
const CHILD_COMMIT: &str = "PICOVOLT_CRASH_COMMIT";
const CHILD_READY: &str = "PICOVOLT_CRASH_READY";
const SIMULATED_CRASH_EXIT: i32 = 86;

#[test]
fn crash_child() {
    if std::env::var_os(CHILD_MODE).is_none() {
        return;
    }

    let root = PathBuf::from(std::env::var_os(CHILD_ROOT).expect("child root"));
    let id = std::env::var(CHILD_ID)
        .expect("child id")
        .parse::<i64>()
        .expect("numeric child id");
    let commit = std::env::var(CHILD_COMMIT).as_deref() == Ok("1");
    let ready = PathBuf::from(std::env::var_os(CHILD_READY).expect("ready path"));

    let mut db = Database::open_dev(root).expect("open child workspace");
    db.begin_transaction().expect("begin child transaction");
    db.query(&format!("INSERT INTO crash_rows VALUES ({id}, 'child')"))
        .expect("write child row");
    if commit {
        db.commit_transaction().expect("commit child transaction");
    } else {
        db.flush_now().expect("sync uncommitted child transaction");
    }

    let mut signal = OpenOptions::new()
        .create(true)
        .truncate(true)
        .write(true)
        .open(ready)
        .expect("create ready signal");
    signal.write_all(b"ready\n").expect("write ready signal");
    signal.sync_all().expect("sync ready signal");

    // Bypass unwinding and all destructors to model abrupt process loss.
    std::process::exit(SIMULATED_CRASH_EXIT);
}

#[test]
fn randomized_cross_process_crashes_are_atomic() {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path().join("workspace");
    let mut db = Database::open_dev(&root).unwrap();
    db.query("CREATE TABLE crash_rows (id PRIMARY KEY, source)")
        .unwrap();
    db.query("INSERT INTO crash_rows VALUES (0, 'baseline')")
        .unwrap();
    drop(db);

    // Fixed seed makes failures reproducible while exercising mixed commit and
    // rollback decisions. Override the count for an extended release soak.
    let cycles = std::env::var("PICOVOLT_CRASH_CYCLES")
        .ok()
        .and_then(|value| value.parse().ok())
        .unwrap_or(32usize);
    let mut random = 0x9E37_79B9_7F4A_7C15u64;
    let mut expected = 1usize;

    for cycle in 1..=cycles {
        random = random
            .wrapping_mul(6_364_136_223_846_793_005)
            .wrapping_add(1_442_695_040_888_963_407);
        let commit = random & 1 == 0;
        let status = run_crash_child(&root, cycle, commit, temp.path());
        assert_eq!(
            status.code(),
            Some(SIMULATED_CRASH_EXIT),
            "crash child failed before reaching its simulated crash in cycle {cycle}"
        );

        if commit {
            expected += 1;
        }
        let mut reopened = Database::open_dev(&root).unwrap_or_else(|error| {
            panic!("cycle {cycle} failed recovery after commit={commit}: {error}")
        });
        assert_eq!(row_count(&mut reopened), expected, "cycle {cycle}");

        let row = reopened
            .query(&format!("SELECT id FROM crash_rows WHERE id = {cycle}"))
            .unwrap();
        assert_eq!(
            !row.rows().unwrap().is_empty(),
            commit,
            "cycle {cycle} exposed a partially applied transaction"
        );
        drop(reopened);
    }
}

fn run_crash_child(root: &Path, id: usize, commit: bool, temp: &Path) -> ExitStatus {
    let ready = temp.join(format!("ready-{id}"));
    let output = Command::new(std::env::current_exe().unwrap())
        .arg("--exact")
        .arg("crash_child")
        .arg("--test-threads=1")
        .env(CHILD_MODE, "1")
        .env(CHILD_ROOT, root)
        .env(CHILD_ID, id.to_string())
        .env(CHILD_COMMIT, if commit { "1" } else { "0" })
        .env(CHILD_READY, &ready)
        .output()
        .unwrap();
    assert!(
        ready.is_file(),
        "child did not finish its transaction step\nstdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    fs::remove_file(ready).unwrap();
    output.status
}

fn row_count(db: &mut Database) -> usize {
    let result = db.query("SELECT COUNT(*) FROM crash_rows").unwrap();
    match &result.rows().unwrap()[0][0] {
        Value::Int(value) => usize::try_from(*value).unwrap(),
        other => panic!("expected integer count, got {other:?}"),
    }
}
