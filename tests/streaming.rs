//! The streamed (`open_streamed`) read path: opening fetches only the header and
//! tail, pages are fetched on demand, and results match an in-memory open.

use std::cell::Cell;
use std::rc::Rc;

use picovolt::storage::vle::RangeReader;
use picovolt::{Database, PvError, Result};

/// A `RangeReader` over an in-memory image that counts how many ranges it serves
/// and reports an out-of-range read as an error (as a real HTTP reader would).
struct CountingReader {
    data: Vec<u8>,
    calls: Rc<Cell<usize>>,
}

impl RangeReader for CountingReader {
    fn read_at(&self, offset: u64, len: usize) -> Result<Vec<u8>> {
        self.calls.set(self.calls.get() + 1);
        let o = offset as usize;
        let end = o
            .checked_add(len)
            .filter(|&e| e <= self.data.len())
            .ok_or_else(|| PvError::Corruption("range past end of image".into()))?;
        Ok(self.data[o..end].to_vec())
    }
}

fn sample_bytes() -> Vec<u8> {
    let mut db = Database::open_memory();
    db.query("CREATE TABLE t (id, kind)").unwrap();
    for i in 0..200 {
        let kind = if i % 2 == 0 { "even" } else { "odd" };
        db.query(&format!("INSERT INTO t VALUES ({i}, '{kind}')"))
            .unwrap();
    }
    db.bake_to_bytes().unwrap()
}

#[test]
fn streamed_open_is_lazy_and_matches_in_memory() {
    let bytes = sample_bytes();
    let calls = Rc::new(Cell::new(0usize));
    let reader = Box::new(CountingReader {
        data: bytes.clone(),
        calls: calls.clone(),
    });
    let mut streamed = Database::open_streamed(reader, bytes.len() as u64).unwrap();

    // Opening reads only the header and the tail (CAS pool + manifest), never the
    // page-data block. This is the whole point: a huge file opens instantly.
    assert_eq!(calls.get(), 2, "open should fetch header + tail only");

    let mut mem = Database::import_bytes(&bytes).unwrap();
    for sql in [
        "SELECT COUNT(*) FROM t",
        "SELECT kind, COUNT(*) AS n FROM t GROUP BY kind ORDER BY kind",
        "SELECT id FROM t WHERE id < 5 ORDER BY id",
    ] {
        assert_eq!(
            streamed.query(sql).unwrap(),
            mem.query(sql).unwrap(),
            "streamed result differs from in-memory for: {sql}"
        );
    }

    // Running queries fetched pages on demand, so more ranges were served.
    assert!(calls.get() > 2, "queries should fetch pages lazily");
}

#[test]
fn streamed_time_travel_matches_in_memory() {
    let bytes = sample_bytes();
    let reader = Box::new(CountingReader {
        data: bytes.clone(),
        calls: Rc::new(Cell::new(0)),
    });
    let mut streamed = Database::open_streamed(reader, bytes.len() as u64).unwrap();
    let mut mem = Database::import_bytes(&bytes).unwrap();

    // Time-travel works over the streamed backend exactly as in memory.
    for q in [
        "SELECT COUNT(*) FROM t BEFORE 10",
        "SELECT COUNT(*) FROM t BEFORE 50",
    ] {
        assert_eq!(streamed.query(q).unwrap(), mem.query(q).unwrap(), "{q}");
    }
}

#[test]
fn streamed_rejects_a_truncated_image() {
    let bytes = sample_bytes();
    let reader = Box::new(CountingReader {
        data: bytes.clone(),
        calls: Rc::new(Cell::new(0)),
    });
    // Claim the image is larger than it is: the tail read runs past the data and
    // must surface as a clean error, not a panic.
    assert!(Database::open_streamed(reader, bytes.len() as u64 + 4096).is_err());
}
