//! Test-only fault injection: thread-local, so parallel tests stay independent.

use std::cell::RefCell;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Fault {
    /// Hard links fail as on FAT32/exFAT.
    NoHardLinks,
    /// The no-replace rename fails with a "not supported" error.
    NoNoReplaceRename,
    /// The process "dies" right before the no-replace rename.
    CrashBeforeRename,
}

thread_local! {
    static ARMED: RefCell<Vec<Fault>> = const { RefCell::new(Vec::new()) };
}

/// Arms `faults` until the returned guard drops.
pub(crate) fn arm(faults: &[Fault]) -> Guard {
    ARMED.with(|armed| armed.borrow_mut().extend_from_slice(faults));
    Guard
}

pub(crate) fn armed(fault: Fault) -> bool {
    ARMED.with(|armed| armed.borrow().contains(&fault))
}

pub(crate) struct Guard;

impl Drop for Guard {
    fn drop(&mut self) {
        ARMED.with(|armed| armed.borrow_mut().clear());
    }
}
