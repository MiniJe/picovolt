//! Cooperative cancellation shared by bounded queries and the native
//! `SharedDatabase` coordinator.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

use crate::{PvError, Result};

/// A clonable, thread-safe cancellation signal.
///
/// Cancellation is cooperative: queued shared-database work observes the flag
/// before it starts, and running bounded queries observe it at the same regular
/// checkpoints used for row, memory, and deadline limits.
#[derive(Clone, Debug, Default)]
pub struct CancellationToken {
    cancelled: Arc<AtomicBool>,
}

impl CancellationToken {
    /// Create a token in the non-cancelled state.
    pub fn new() -> Self {
        Self::default()
    }

    /// Request cancellation. Calling this more than once is harmless.
    pub fn cancel(&self) {
        self.cancelled.store(true, Ordering::Release);
    }

    /// Whether cancellation has been requested.
    pub fn is_cancelled(&self) -> bool {
        self.cancelled.load(Ordering::Acquire)
    }

    pub(crate) fn check(&self) -> Result<()> {
        if self.is_cancelled() {
            Err(PvError::Cancelled)
        } else {
            Ok(())
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn clones_share_cancellation_state() {
        let token = CancellationToken::new();
        let clone = token.clone();
        assert!(!clone.is_cancelled());
        token.cancel();
        assert!(clone.is_cancelled());
        assert!(matches!(clone.check(), Err(PvError::Cancelled)));
    }
}
