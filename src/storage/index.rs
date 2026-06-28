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

use crate::core::types::RecordAddr;
use crate::core::value::Value;

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
}
