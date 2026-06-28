//! The version-2 on-disk format: secondary indexes stored in a compact binary
//! region. Covers version stamping, in-memory and production round-trips through
//! the region, and back-compat for index-less files.

use picovolt::Database;

fn format_version(image: &[u8]) -> u16 {
    u16::from_le_bytes([image[4], image[5]])
}

/// Build a small table with a unique-valued, indexed column so an
/// `ORDER BY col ... LIMIT` is fully deterministic and uses the index path.
fn indexed_db(rows: u64) -> Database {
    let mut db = Database::open_memory();
    db.query("CREATE TABLE t (id, score)").unwrap();
    db.query("CREATE INDEX ON t (score)").unwrap();
    for i in 0..rows {
        db.query(&format!("INSERT INTO t VALUES ({i}, {})", i * 2))
            .unwrap();
    }
    db
}

#[test]
fn indexed_monolith_is_version_2_and_round_trips_via_region() {
    let bytes = indexed_db(500).bake_to_bytes().unwrap();
    assert_eq!(
        format_version(&bytes),
        2,
        "a monolith carrying a binary index region must be format version 2"
    );

    // Reopening reconstructs the index from the binary region (no rebuild scan),
    // and the index drives the same answer as a freshly rebuilt index would.
    let mut reopened = Database::import_bytes(&bytes).unwrap();
    let got = reopened
        .query("SELECT id FROM t ORDER BY score DESC LIMIT 5")
        .unwrap();
    // score = id*2 is unique and monotonic, so the top 5 are ids 499..=495.
    let ids: Vec<i64> = got
        .rows()
        .unwrap()
        .iter()
        .map(|r| match r[0] {
            picovolt::Value::Int(i) => i,
            _ => panic!("expected int id"),
        })
        .collect();
    assert_eq!(ids, vec![499, 498, 497, 496, 495]);
}

#[test]
fn index_less_monolith_stays_version_1() {
    let mut db = Database::open_memory();
    db.query("CREATE TABLE t (id, score)").unwrap();
    for i in 0..50u64 {
        db.query(&format!("INSERT INTO t VALUES ({i}, {i})"))
            .unwrap();
    }
    let bytes = db.bake_to_bytes().unwrap();
    assert_eq!(
        format_version(&bytes),
        1,
        "an index-less monolith stays version 1 so older builds can still read it"
    );
    assert!(Database::import_bytes(&bytes).is_ok());
}

#[test]
fn open_prod_loads_the_binary_index() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("t.pvdb");
    indexed_db(300).bake(&path).unwrap();

    let mut db = Database::open_prod(&path).unwrap();
    let res = db
        .query("SELECT id FROM t ORDER BY score DESC LIMIT 3")
        .unwrap();
    let ids: Vec<i64> = res
        .rows()
        .unwrap()
        .iter()
        .map(|r| match r[0] {
            picovolt::Value::Int(i) => i,
            _ => panic!("expected int id"),
        })
        .collect();
    assert_eq!(ids, vec![299, 298, 297]);
}

#[test]
fn binary_index_region_overhead_is_compact_and_bounded() {
    const ROWS: u64 = 2000;
    let indexed = indexed_db(ROWS).bake_to_bytes().unwrap();
    assert_eq!(format_version(&indexed), 2);
    assert!(Database::import_bytes(&indexed).is_ok());

    // The same data with no index, for a baseline.
    let mut plain = Database::open_memory();
    plain.query("CREATE TABLE t (id, score)").unwrap();
    for i in 0..ROWS {
        plain
            .query(&format!("INSERT INTO t VALUES ({i}, {})", i * 2))
            .unwrap();
    }
    let plain_bytes = plain.bake_to_bytes().unwrap();

    // The region encodes each of the 2000 unique keys as roughly a constant number
    // of bytes (tag + 8-byte key + 4-byte addr count + 8-byte addr ≈ 21). Assert
    // the per-key overhead stays in a tight band: small enough to prove the packing
    // is compact, large enough to prove the region is actually present.
    let overhead = indexed.len() - plain_bytes.len();
    let per_key = overhead as f64 / ROWS as f64;
    assert!(
        (12.0..40.0).contains(&per_key),
        "index region overhead {per_key:.1} bytes/key is outside the expected band"
    );
}
