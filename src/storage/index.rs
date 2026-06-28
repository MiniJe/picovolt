//! In-memory ordered secondary index: column value → record addresses.
//!
//! Maps an indexed column's value to the [`RecordAddr`]s of every record version
//! that carries it (MVCC-style: tombstoned versions stay until vacuumed, and the
//! reader filters by visibility). A `BTreeMap` keyed on [`Value`]'s total order
//! turns both `WHERE col = value` *and* range predicates (`col < v`, `col >= v`,
//! …) into a keyed lookup / ordered scan plus a handful of page reads, instead of
//! a full table scan.
//!
//! The index is in-memory, rebuilt by a streaming scan when a table is opened; it
//! is not yet persisted as an on-disk B-tree.

use std::collections::BTreeMap;
use std::ops::RangeBounds;

use crate::core::errors::{PvError, Result};
use crate::core::types::RecordAddr;
use crate::core::value::Value;

// Tags for the self-contained index-region value codec. They mirror the record
// body tags for familiarity, but this codec always stores payloads inline (it
// never reaches into CAS) so a decoded index needs nothing but its own bytes.
const IV_NULL: u8 = 0x00;
const IV_INT: u8 = 0x01;
const IV_TEXT: u8 = 0x02;
const IV_BLOB: u8 = 0x03;
const IV_DECIMAL: u8 = 0x06;

/// An ordered index over one column.
#[derive(Default)]
pub struct SecondaryIndex {
    map: BTreeMap<Value, Vec<RecordAddr>>,
    entries: usize,
}

impl SecondaryIndex {
    /// An empty index.
    pub fn new() -> Self {
        Self::default()
    }

    /// Record that `value` occurs at `addr`.
    pub fn insert(&mut self, value: &Value, addr: RecordAddr) {
        self.map.entry(value.clone()).or_default().push(addr);
        self.entries += 1;
    }

    /// Addresses of records carrying exactly `value` (may include tombstoned
    /// versions, the caller filters by visibility).
    pub fn lookup(&self, value: &Value) -> &[RecordAddr] {
        self.map.get(value).map(Vec::as_slice).unwrap_or(&[])
    }

    /// Addresses whose key falls within `range`, in ascending key order. Powers
    /// range predicates; pass e.g. `(Bound::Excluded(v), Bound::Unbounded)` for
    /// `col > v`.
    pub fn range<R: RangeBounds<Value>>(&self, range: R) -> Vec<RecordAddr> {
        self.map
            .range(range)
            .flat_map(|(_, addrs)| addrs.iter().copied())
            .collect()
    }

    /// Every address in key order: ascending, or descending when `descending` is
    /// set. Lets a reader satisfy `ORDER BY indexed_col` without a sort.
    pub fn ordered_addrs(&self, descending: bool) -> Vec<RecordAddr> {
        if descending {
            self.map.values().rev().flatten().copied().collect()
        } else {
            self.map.values().flatten().copied().collect()
        }
    }

    /// Total number of (value, addr) entries indexed.
    pub fn len(&self) -> usize {
        self.entries
    }

    /// Whether the index holds no entries.
    pub fn is_empty(&self) -> bool {
        self.entries == 0
    }

    /// Number of distinct keys.
    pub fn distinct_keys(&self) -> usize {
        self.map.len()
    }

    /// Dump the index as `(key, addresses)` pairs in key order, for persistence.
    pub fn to_pairs(&self) -> Vec<(Value, Vec<RecordAddr>)> {
        self.map
            .iter()
            .map(|(k, v)| (k.clone(), v.clone()))
            .collect()
    }

    /// Rebuild an index from persisted `(key, addresses)` pairs, so it can be
    /// loaded from a `.pvdb` instead of rebuilt by a full table scan.
    pub fn from_pairs(pairs: Vec<(Value, Vec<RecordAddr>)>) -> Self {
        let mut map: BTreeMap<Value, Vec<RecordAddr>> = BTreeMap::new();
        let mut entries = 0;
        for (k, addrs) in pairs {
            entries += addrs.len();
            map.entry(k).or_default().extend(addrs);
        }
        Self { map, entries }
    }

    /// Serialize the index to a compact, self-contained binary blob for the
    /// `.pvdb` index region. Layout (little-endian):
    ///
    /// ```text
    /// key_count: u32
    /// repeated key_count times, in ascending key order:
    ///   key: tagged value (see IV_* tags)
    ///   addr_count: u32
    ///   addrs: u64 * addr_count
    /// ```
    ///
    /// This is far smaller and faster to parse than the JSON `pairs` form, and
    /// carries no external references, so a reader reconstructs the index from
    /// these bytes alone.
    pub fn encode_binary(&self) -> Vec<u8> {
        let mut out = Vec::new();
        out.extend_from_slice(&(self.map.len() as u32).to_le_bytes());
        for (key, addrs) in &self.map {
            encode_index_value(&mut out, key);
            out.extend_from_slice(&(addrs.len() as u32).to_le_bytes());
            for &addr in addrs {
                out.extend_from_slice(&addr.to_le_bytes());
            }
        }
        out
    }

    /// Reconstruct an index from the bytes written by [`encode_binary`]. The
    /// input is untrusted (it comes straight off disk / the network), so every
    /// length and extent is bounds-checked and a malformed blob yields an error,
    /// never a panic.
    pub fn decode_binary(bytes: &[u8]) -> Result<Self> {
        let mut pos = 0usize;
        let key_count = rd_u32(bytes, &mut pos)? as usize;
        let mut map: BTreeMap<Value, Vec<RecordAddr>> = BTreeMap::new();
        let mut entries = 0usize;
        for _ in 0..key_count {
            let key = decode_index_value(bytes, &mut pos)?;
            let addr_count = rd_u32(bytes, &mut pos)? as usize;
            let mut addrs = Vec::with_capacity(addr_count.min(1024));
            for _ in 0..addr_count {
                addrs.push(rd_u64(bytes, &mut pos)?);
            }
            entries += addrs.len();
            map.entry(key).or_default().extend(addrs);
        }
        Ok(Self { map, entries })
    }
}

fn encode_index_value(out: &mut Vec<u8>, value: &Value) {
    match value {
        Value::Null => out.push(IV_NULL),
        Value::Int(i) => {
            out.push(IV_INT);
            out.extend_from_slice(&i.to_le_bytes());
        }
        Value::Decimal(m) => {
            out.push(IV_DECIMAL);
            out.extend_from_slice(&m.to_le_bytes());
        }
        Value::Text(s) => {
            out.push(IV_TEXT);
            let b = s.as_bytes();
            out.extend_from_slice(&(b.len() as u32).to_le_bytes());
            out.extend_from_slice(b);
        }
        Value::Blob(b) => {
            out.push(IV_BLOB);
            out.extend_from_slice(&(b.len() as u32).to_le_bytes());
            out.extend_from_slice(b);
        }
    }
}

fn decode_index_value(buf: &[u8], pos: &mut usize) -> Result<Value> {
    let tag = *buf
        .get(*pos)
        .ok_or_else(|| PvError::Corruption("index: truncated at value tag".into()))?;
    *pos += 1;
    Ok(match tag {
        IV_NULL => Value::Null,
        IV_INT => Value::Int(rd_u64(buf, pos)? as i64),
        IV_DECIMAL => Value::Decimal(rd_i128(buf, pos)?),
        IV_TEXT => {
            let bytes = rd_bytes(buf, pos)?;
            Value::Text(
                String::from_utf8(bytes.to_vec())
                    .map_err(|_| PvError::Corruption("index: invalid utf-8 key".into()))?,
            )
        }
        IV_BLOB => Value::Blob(rd_bytes(buf, pos)?.to_vec()),
        other => {
            return Err(PvError::Corruption(format!(
                "index: bad value tag 0x{other:02X}"
            )))
        }
    })
}

fn rd_bytes<'a>(buf: &'a [u8], pos: &mut usize) -> Result<&'a [u8]> {
    // `len` is an untrusted u32; add it with a checked op so a hostile value cannot
    // overflow `pos` (which would panic in debug and could slip a bounds check on a
    // 32-bit target such as wasm32).
    let len = rd_u32(buf, pos)? as usize;
    let end = pos
        .checked_add(len)
        .filter(|&e| e <= buf.len())
        .ok_or_else(|| PvError::Corruption("index: truncated payload".into()))?;
    let slice = &buf[*pos..end];
    *pos = end;
    Ok(slice)
}

fn rd_u32(buf: &[u8], pos: &mut usize) -> Result<u32> {
    let slice = buf
        .get(*pos..*pos + 4)
        .ok_or_else(|| PvError::Corruption("index: truncated u32".into()))?;
    *pos += 4;
    Ok(u32::from_le_bytes(slice.try_into().unwrap()))
}

fn rd_u64(buf: &[u8], pos: &mut usize) -> Result<u64> {
    let slice = buf
        .get(*pos..*pos + 8)
        .ok_or_else(|| PvError::Corruption("index: truncated u64".into()))?;
    *pos += 8;
    Ok(u64::from_le_bytes(slice.try_into().unwrap()))
}

fn rd_i128(buf: &[u8], pos: &mut usize) -> Result<i128> {
    let slice = buf
        .get(*pos..*pos + 16)
        .ok_or_else(|| PvError::Corruption("index: truncated i128".into()))?;
    *pos += 16;
    Ok(i128::from_le_bytes(slice.try_into().unwrap()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::ops::Bound;

    #[test]
    fn groups_addresses_by_value() {
        let mut idx = SecondaryIndex::new();
        idx.insert(&Value::Int(1), 10);
        idx.insert(&Value::Int(1), 11);
        idx.insert(&Value::Int(2), 20);
        idx.insert(&Value::from("x"), 30);

        assert_eq!(idx.lookup(&Value::Int(1)), &[10, 11]);
        assert_eq!(idx.lookup(&Value::Int(2)), &[20]);
        assert_eq!(idx.lookup(&Value::from("x")), &[30]);
        assert_eq!(idx.lookup(&Value::Int(99)), &[] as &[RecordAddr]);
        assert_eq!(idx.len(), 4);
        assert_eq!(idx.distinct_keys(), 3);
    }

    #[test]
    fn distinguishes_types_with_same_bytes() {
        let mut idx = SecondaryIndex::new();
        idx.insert(&Value::Int(0), 1);
        idx.insert(&Value::Null, 2);
        // Int(0) and Null must not collide.
        assert_eq!(idx.lookup(&Value::Int(0)), &[1]);
        assert_eq!(idx.lookup(&Value::Null), &[2]);
    }

    #[test]
    fn range_scan_returns_keys_in_order() {
        let mut idx = SecondaryIndex::new();
        for (v, a) in [(10, 1), (30, 2), (20, 3), (30, 4), (40, 5)] {
            idx.insert(&Value::Int(v), a);
        }
        // col > 20  → keys 30, 30, 40, in ascending key order.
        assert_eq!(
            idx.range((Bound::Excluded(Value::Int(20)), Bound::Unbounded)),
            vec![2, 4, 5]
        );
        // col <= 20 → keys 10, 20.
        assert_eq!(
            idx.range((Bound::Unbounded, Bound::Included(Value::Int(20)))),
            vec![1, 3]
        );
        // a bounded window: 20 <= col < 40.
        assert_eq!(
            idx.range((
                Bound::Included(Value::Int(20)),
                Bound::Excluded(Value::Int(40))
            )),
            vec![3, 2, 4]
        );
    }

    #[test]
    fn ordered_addrs_walks_keys_in_order() {
        let mut idx = SecondaryIndex::new();
        for (v, a) in [(30, 1), (10, 2), (20, 3), (10, 4)] {
            idx.insert(&Value::Int(v), a);
        }
        // Ascending keys: 10 (a2, a4), 20 (a3), 30 (a1).
        assert_eq!(idx.ordered_addrs(false), vec![2, 4, 3, 1]);
        // Descending keys: 30 (a1), 20 (a3), 10 (a2, a4).
        assert_eq!(idx.ordered_addrs(true), vec![1, 3, 2, 4]);
    }

    #[test]
    fn binary_round_trips_every_value_kind() {
        let mut idx = SecondaryIndex::new();
        idx.insert(&Value::Null, 1);
        idx.insert(&Value::Int(-42), 2);
        idx.insert(&Value::Int(-42), 3); // duplicate key, two addrs
        idx.insert(&Value::Decimal(1_500_000), 4);
        idx.insert(&Value::from("crate"), 5);
        idx.insert(&Value::Blob(vec![0xDE, 0xAD, 0xBE, 0xEF]), 6);

        let blob = idx.encode_binary();
        let back = SecondaryIndex::decode_binary(&blob).unwrap();

        assert_eq!(back.len(), idx.len());
        assert_eq!(back.distinct_keys(), idx.distinct_keys());
        assert_eq!(back.lookup(&Value::Int(-42)), &[2, 3]);
        assert_eq!(back.lookup(&Value::Null), &[1]);
        assert_eq!(back.lookup(&Value::Decimal(1_500_000)), &[4]);
        assert_eq!(back.lookup(&Value::from("crate")), &[5]);
        assert_eq!(
            back.lookup(&Value::Blob(vec![0xDE, 0xAD, 0xBE, 0xEF])),
            &[6]
        );
        // Ordered traversal survives the round trip identically.
        assert_eq!(back.ordered_addrs(false), idx.ordered_addrs(false));
    }

    #[test]
    fn binary_decode_rejects_truncation() {
        let mut idx = SecondaryIndex::new();
        idx.insert(&Value::Int(7), 100);
        idx.insert(&Value::from("hello"), 200);
        let blob = idx.encode_binary();
        // Any prefix shorter than the whole must error, never panic.
        for cut in 0..blob.len() {
            assert!(SecondaryIndex::decode_binary(&blob[..cut]).is_err());
        }
        // The full blob still decodes.
        assert!(SecondaryIndex::decode_binary(&blob).is_ok());
    }
}
