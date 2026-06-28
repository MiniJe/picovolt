//! Format-stability and corruption-injection tests for the 0.11.0 format freeze.
//!
//! Two guarantees are pinned here:
//!  1. A committed golden `.pvdb`, baked by this version, must keep opening and
//!     returning the same data — an accidental format change breaks this test.
//!  2. Crafted corruption (bit flips, truncation, bad version/magic) must produce
//!     a clean `Err`, never a panic and never a silently wrong answer.

use picovolt::Value;
use picovolt::{
    Database, PvError, FILE_HEADER_SIZE, FORMAT_VERSION, MAGIC_BYTES, PAGE_HEADER_SIZE,
};

const GOLDEN: &str = "tests/fixtures/golden_v0_11_0.pvdb";
const GOLDEN_V2: &str = "tests/fixtures/golden_v1_3_0.pvdb";

/// Bake the canonical sample dataset into `<dir>/sample.pvdb` and return its path.
fn bake_sample(dir: &std::path::Path) -> std::path::PathBuf {
    let ws = dir.join("ws");
    let mut db = Database::open_dev(&ws).unwrap();
    db.query("CREATE TABLE users (id, name, city)").unwrap();
    db.query("INSERT INTO users VALUES (1, 'alice', 'paris')")
        .unwrap();
    db.query("INSERT INTO users VALUES (2, 'bob', 'berlin')")
        .unwrap();
    db.query("INSERT INTO users VALUES (3, 'carol', 'cairo')")
        .unwrap();
    let out = dir.join("sample.pvdb");
    db.bake(&out).unwrap();
    out
}

// --- 1. The committed golden file must keep opening and matching --------------

#[test]
fn golden_file_opens_and_matches() {
    let mut db = Database::open_prod(GOLDEN).expect("golden .pvdb must still open");

    let users = db.query("SELECT * FROM users").unwrap();
    assert_eq!(users.rows().unwrap().len(), 3, "three users survive");

    // The UPDATE took effect: id 1's current city is the updated value.
    let city = db.query("SELECT city FROM users WHERE id = 1").unwrap();
    assert_eq!(city.rows().unwrap()[0][0], Value::Text("london".into()));

    // The 500-byte CAS-interned note round-trips intact.
    let note = db.query("SELECT body FROM notes WHERE id = 1").unwrap();
    match &note.rows().unwrap()[0][0] {
        Value::Text(s) => assert_eq!(s.len(), 500),
        other => panic!("expected a text body, got {other:?}"),
    }
}

#[test]
fn golden_file_header_carries_magic_and_version() {
    let bytes = std::fs::read(GOLDEN).unwrap();
    assert!(bytes.len() > FILE_HEADER_SIZE);
    assert_eq!(&bytes[0..4], &MAGIC_BYTES, "magic signature");
    let version = u16::from_le_bytes([bytes[4], bytes[5]]);
    // The golden is a frozen v0.11.0 file: it is version 1 and must stay readable
    // by every later build (whose FORMAT_VERSION only grows).
    assert!(
        (1..=FORMAT_VERSION).contains(&version),
        "version {version} outside the readable range 1..={FORMAT_VERSION}"
    );
    assert_eq!(version, 1, "the v0.11.0 golden is a version-1 file");
}

#[test]
fn golden_v2_carries_a_binary_index_region_and_reads_correctly() {
    let bytes = std::fs::read(GOLDEN_V2).unwrap();
    assert_eq!(&bytes[0..4], &MAGIC_BYTES, "magic signature");
    let version = u16::from_le_bytes([bytes[4], bytes[5]]);
    assert_eq!(version, 2, "the v1.3.0 golden carries an index region");

    // The persisted binary index drives an ordered query, without a rebuild scan.
    let mut db = Database::import_bytes(&bytes).unwrap();
    let top = db
        .query("SELECT name FROM crates ORDER BY downloads DESC LIMIT 2")
        .unwrap();
    let names: Vec<&str> = top
        .rows()
        .unwrap()
        .iter()
        .map(|r| match &r[0] {
            Value::Text(s) => s.as_str(),
            other => panic!("expected text name, got {other:?}"),
        })
        .collect();
    // id 1 (serde) was updated to 95000, the highest; tokio (80000) is next.
    assert_eq!(names, vec!["serde", "tokio"]);

    // MVCC history survived the bake: before serde's update it had 90000, so at the
    // earliest transaction tokio (80000) led only after serde's later bump. Confirm
    // time-travel still resolves an older snapshot without error.
    assert!(db
        .query("SELECT COUNT(*) FROM crates BEFORE 3")
        .unwrap()
        .rows()
        .is_some());
}

// --- 2. Corruption must error cleanly, never panic ----------------------------

#[test]
fn flipped_page_byte_is_caught_on_query() {
    let dir = tempfile::tempdir().unwrap();
    let out = bake_sample(dir.path());

    // Flip a byte inside the first page's body (page 0 holds the users table).
    let mut bytes = std::fs::read(&out).unwrap();
    let target = FILE_HEADER_SIZE + PAGE_HEADER_SIZE + 4;
    bytes[target] ^= 0xFF;
    std::fs::write(&out, &bytes).unwrap();

    // Opening still works (no scan yet); the corruption surfaces on the read.
    let mut db = Database::open_prod(&out).unwrap();
    let result = db.query("SELECT * FROM users");
    assert!(
        matches!(result, Err(PvError::Corruption(_))),
        "a flipped page byte must be caught by the checksum, got {result:?}"
    );
}

#[test]
fn truncated_monolith_errors_cleanly() {
    let dir = tempfile::tempdir().unwrap();
    let out = bake_sample(dir.path());

    // Keep the header but drop the body: the recorded offsets now run past EOF.
    let bytes = std::fs::read(&out).unwrap();
    std::fs::write(&out, &bytes[..FILE_HEADER_SIZE + 10]).unwrap();

    assert!(
        Database::open_prod(&out).is_err(),
        "a truncated file must error, not panic"
    );
}

#[test]
fn unsupported_header_version_is_rejected() {
    let dir = tempfile::tempdir().unwrap();
    let out = bake_sample(dir.path());

    let mut bytes = std::fs::read(&out).unwrap();
    bytes[4..6].copy_from_slice(&(FORMAT_VERSION + 1).to_le_bytes());
    std::fs::write(&out, &bytes).unwrap();

    let result = Database::open_prod(&out);
    assert!(
        matches!(result, Err(PvError::Corruption(_))),
        "a newer format version must be rejected"
    );
}

#[test]
fn corrupt_magic_is_rejected() {
    let dir = tempfile::tempdir().unwrap();
    let out = bake_sample(dir.path());

    let mut bytes = std::fs::read(&out).unwrap();
    bytes[1] ^= 0xFF; // corrupt the 'V' of "PVDB"
    std::fs::write(&out, &bytes).unwrap();

    let result = Database::open_prod(&out);
    assert!(
        matches!(result, Err(PvError::SignatureMismatch { .. })),
        "a corrupt magic must be rejected"
    );
}

#[test]
fn import_bytes_rejects_a_corrupt_image() {
    let dir = tempfile::tempdir().unwrap();
    let out = bake_sample(dir.path());

    let mut bytes = std::fs::read(&out).unwrap();
    bytes[4..6].copy_from_slice(&9999u16.to_le_bytes()); // impossible version
    let result = Database::import_bytes(&bytes);
    assert!(
        result.is_err(),
        "import must reject a corrupt image, not panic"
    );
}

#[test]
fn dev_workspace_without_format_version_is_rejected() {
    let dir = tempfile::tempdir().unwrap();
    let ws = dir.path().join("oldws");
    std::fs::create_dir_all(ws.join("chunks")).unwrap();

    // A pre-freeze (0.10.x) manifest has no `format_version` field; it deserializes
    // as 0 and must be refused rather than mis-read against the new page format.
    let manifest = r#"{"clock":0,"page_count":0,"tables":[],"cas_hashes":[]}"#;
    std::fs::write(ws.join("pv_manifest.json"), manifest).unwrap();

    let result = Database::open_dev(&ws);
    assert!(
        matches!(result, Err(PvError::Corruption(_))),
        "a pre-freeze workspace must be rejected"
    );
}
