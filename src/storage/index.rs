//! In-memory equality secondary index: column value → record addresses.
//!
//! Maps an indexed column's value to the [`RecordAddr`]s of every record version
//! that carries it (MVCC-style: tombstoned versions stay until vacuumed, and the
//! reader filters by visibility). This turns `WHERE col = value` from a full scan
//! into a hash lookup plus a handful of page reads.
//!
//! Scope: equality only. Range/ordered indexes (a persisted B-tree) are future
//! work; this index is rebuilt by a streaming scan when a table is opened.

use std::collections::HashMap;

use crate::core::types::RecordAddr;
use crate::core::value::Value;

/// An equality index over one column.
#[derive(Default)]
pub struct SecondaryIndex {
    map: HashMap<Vec<u8>, Vec<RecordAddr>>,
    entries: usize,
}

impl SecondaryIndex {
    /// An empty index.
    pub fn new() -> Self {
        Self::default()
    }

    /// Record that `value` occurs at `addr`.
    pub fn insert(&mut self, value: &Value, addr: RecordAddr) {
        self.map.entry(encode_key(value)).or_default().push(addr);
        self.entries += 1;
    }

    /// Addresses of records carrying `value` (may include tombstoned versions —
    /// the caller filters by visibility).
    pub fn lookup(&self, value: &Value) -> &[RecordAddr] {
        self.map
            .get(&encode_key(value))
            .map(Vec::as_slice)
            .unwrap_or(&[])
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
}

/// Encode a value into an order-agnostic equality key (tag + payload bytes).
fn encode_key(value: &Value) -> Vec<u8> {
    let mut out = Vec::new();
    match value {
        Value::Null => out.push(0),
        Value::Int(i) => {
            out.push(1);
            out.extend_from_slice(&i.to_le_bytes());
        }
        Value::Text(s) => {
            out.push(2);
            out.extend_from_slice(s.as_bytes());
        }
        Value::Blob(b) => {
            out.push(3);
            out.extend_from_slice(b);
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

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
}
