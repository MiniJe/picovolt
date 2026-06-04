//! MVCC & time-travel (spec §5).
//!
//! PicoVolt never overwrites a live record: inserts allocate a fresh transaction
//! id, deletions stamp `tx_deleted`, and every read is filtered through a
//! [`Snapshot`]. A monotonically increasing clock supplies transaction ids, and
//! the visibility rule is exactly [`RecordEnvelope::is_visible`].

use crate::core::types::{RecordEnvelope, TxId};

/// Allocates transaction ids and hands out read snapshots.
///
/// The clock starts at `0` (the reserved "before anything" snapshot). Each write
/// transaction increments it; `current()` is the most recently committed id.
#[derive(Debug, Clone)]
pub struct TxManager {
    clock: TxId,
}

impl TxManager {
    /// A fresh manager whose clock is at `0`.
    pub fn new() -> Self {
        Self { clock: 0 }
    }

    /// Restore a manager from a persisted clock value.
    pub fn with_clock(clock: TxId) -> Self {
        Self { clock }
    }

    /// The most recently allocated transaction id.
    pub fn current(&self) -> TxId {
        self.clock
    }

    /// Allocate and return the next write transaction id.
    pub fn begin_write(&mut self) -> TxId {
        self.clock += 1;
        self.clock
    }

    /// A snapshot that sees everything committed up to and including [`current`].
    ///
    /// [`current`]: TxManager::current
    pub fn snapshot(&self) -> Snapshot {
        Snapshot { tx: self.clock }
    }
}

impl Default for TxManager {
    fn default() -> Self {
        Self::new()
    }
}

/// A read view of the database "as of" a particular transaction id.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Snapshot {
    tx: TxId,
}

impl Snapshot {
    /// A snapshot pinned at transaction `tx` (used for `... BEFORE tx` queries).
    pub fn as_of(tx: TxId) -> Self {
        Self { tx }
    }

    /// The transaction id this snapshot is pinned at.
    pub fn tx(&self) -> TxId {
        self.tx
    }

    /// Whether the given record version is visible in this snapshot.
    #[inline]
    pub fn sees(&self, envelope: &RecordEnvelope) -> bool {
        envelope.is_visible(self.tx)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn clock_is_monotonic() {
        let mut txm = TxManager::new();
        assert_eq!(txm.current(), 0);
        assert_eq!(txm.begin_write(), 1);
        assert_eq!(txm.begin_write(), 2);
        assert_eq!(txm.current(), 2);
        assert_eq!(txm.snapshot().tx(), 2);
    }

    #[test]
    fn snapshot_isolates_inserts_and_deletes() {
        // Inserted at tx 2, deleted at tx 5.
        let mut env = RecordEnvelope::new(2, 0);
        env.mark_deleted(5);

        assert!(!Snapshot::as_of(1).sees(&env)); // before insert
        assert!(Snapshot::as_of(2).sees(&env)); // at insert
        assert!(Snapshot::as_of(4).sees(&env)); // live window
        assert!(!Snapshot::as_of(5).sees(&env)); // at delete
        assert!(!Snapshot::as_of(9).sees(&env)); // after delete
    }
}
