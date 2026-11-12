//! Cancellation and progress (architecture §4.2).

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use crate::Cancelled;

/// A cheap, clonable cancellation flag shared between a frontend and a worker.
#[derive(Clone, Debug, Default)]
pub struct Cancel(Arc<AtomicBool>);

impl Cancel {
    pub fn new() -> Cancel {
        Cancel::default()
    }

    /// Requests cancellation. Every clone observes it.
    pub fn cancel(&self) {
        self.0.store(true, Ordering::Relaxed);
    }

    pub fn is_cancelled(&self) -> bool {
        self.0.load(Ordering::Relaxed)
    }

    /// `Err(Cancelled)` once cancelled, so `?` works at every chunk boundary.
    pub fn check(&self) -> Result<(), Cancelled> {
        if self.is_cancelled() {
            Err(Cancelled)
        } else {
            Ok(())
        }
    }
}

/// What a long operation is doing right now.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Phase {
    Preparing,
    Reading,
    Writing,
    Verifying,
    Publishing,
    Sending,
}

impl Phase {
    /// `Publishing` cannot be interrupted; every other phase can.
    pub const fn is_interruptible(self) -> bool {
        !matches!(self, Phase::Publishing)
    }
}

/// One progress event, reported on chunk boundaries only.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Progress<'a> {
    pub phase: Phase,
    /// Decoded/output bytes, never compressed input bytes.
    pub done: u64,
    /// `None` = indeterminate.
    pub total: Option<u64>,
    /// Current file or package.
    pub item: Option<&'a str>,
}

/// Receives progress from the worker thread. Must be cheap; frontends throttle.
pub trait ProgressSink: Sync {
    fn report(&self, progress: &Progress<'_>);
}

/// A sink that discards everything.
#[derive(Clone, Copy, Debug, Default)]
pub struct NoProgress;

impl ProgressSink for NoProgress {
    fn report(&self, _: &Progress<'_>) {}
}

/// What every long operation receives.
#[derive(Clone, Copy)]
pub struct Ctx<'a> {
    pub cancel: &'a Cancel,
    pub progress: &'a dyn ProgressSink,
}

impl<'a> Ctx<'a> {
    pub fn new(cancel: &'a Cancel, progress: &'a dyn ProgressSink) -> Ctx<'a> {
        Ctx { cancel, progress }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cancel_is_shared_across_clones() {
        let cancel = Cancel::new();
        let clone = cancel.clone();
        assert_eq!(clone.check(), Ok(()));
        cancel.cancel();
        assert_eq!(clone.check(), Err(Cancelled));
    }

    #[test]
    fn only_publishing_is_uninterruptible() {
        assert!(!Phase::Publishing.is_interruptible());
        assert!(Phase::Writing.is_interruptible());
    }
}
