//! A limit on how much work one answer may do.
//!
//! A session can hold gigabytes of OS trace, and an agent's context window is small. Scans take a
//! [`Budget`] and stop when it says so; a command then either returns what it has marked
//! `truncated` with a cursor, or fails with the "over budget" exit code if a partial answer would
//! mislead.

use std::fmt;
use std::sync::Arc;
use std::sync::atomic::AtomicBool;
use std::sync::atomic::AtomicU64;
use std::sync::atomic::Ordering;
use std::time::Duration;
use std::time::Instant;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Exceeded {
    Time,
    Bytes,
    Cancelled,
}

impl fmt::Display for Exceeded {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::Time => "ran out of time",
            Self::Bytes => "read more data than the budget allows",
            Self::Cancelled => "was cancelled",
        })
    }
}

impl std::error::Error for Exceeded {}

#[derive(Debug, Clone)]
pub struct Budget {
    deadline: Option<Instant>,
    max_bytes: Option<u64>,
    spent_bytes: Arc<AtomicU64>,
    cancelled: Arc<AtomicBool>,
}

impl Default for Budget {
    fn default() -> Self {
        Self::unlimited()
    }
}

impl Budget {
    #[must_use]
    pub fn unlimited() -> Self {
        Self {
            deadline: None,
            max_bytes: None,
            spent_bytes: Arc::new(AtomicU64::new(0)),
            cancelled: Arc::new(AtomicBool::new(false)),
        }
    }

    #[must_use]
    pub fn within(mut self, time: Duration) -> Self {
        self.deadline = Some(Instant::now() + time);
        self
    }

    #[must_use]
    pub fn reading_at_most(mut self, bytes: u64) -> Self {
        self.max_bytes = Some(bytes);
        self
    }

    /// A handle that cancels this budget (and every clone of it) when set.
    #[must_use]
    pub fn cancel_handle(&self) -> Arc<AtomicBool> {
        self.cancelled.clone()
    }

    /// Fail if time ran out or the work was cancelled. Cheap enough to call per record.
    pub fn check(&self) -> Result<(), Exceeded> {
        if self.cancelled.load(Ordering::Relaxed) {
            return Err(Exceeded::Cancelled);
        }

        if self
            .deadline
            .is_some_and(|deadline| Instant::now() >= deadline)
        {
            return Err(Exceeded::Time);
        }

        Ok(())
    }

    /// Account for `bytes` read, then [`Budget::check`].
    pub fn charge(&self, bytes: u64) -> Result<(), Exceeded> {
        let spent = self.spent_bytes.fetch_add(bytes, Ordering::Relaxed) + bytes;
        if self.max_bytes.is_some_and(|max| spent > max) {
            return Err(Exceeded::Bytes);
        }

        self.check()
    }

    #[must_use]
    pub fn spent_bytes(&self) -> u64 {
        self.spent_bytes.load(Ordering::Relaxed)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_unlimited_budget_never_runs_out() {
        let budget = Budget::unlimited();
        for _ in 0..1000 {
            assert_eq!(budget.charge(1 << 30), Ok(()));
        }
    }

    #[test]
    fn bytes_are_counted_across_clones() {
        let budget = Budget::unlimited().reading_at_most(100);
        let clone = budget.clone();
        assert_eq!(budget.charge(60), Ok(()));
        assert_eq!(clone.charge(60), Err(Exceeded::Bytes));
        assert_eq!(budget.spent_bytes(), 120);
    }

    #[test]
    fn time_runs_out() {
        let budget = Budget::unlimited().within(Duration::ZERO);
        assert_eq!(budget.check(), Err(Exceeded::Time));
    }

    #[test]
    fn cancelling_a_handle_stops_every_clone() {
        let budget = Budget::unlimited();
        let clone = budget.clone();
        budget.cancel_handle().store(true, Ordering::Relaxed);
        assert_eq!(clone.check(), Err(Exceeded::Cancelled));
    }
}
