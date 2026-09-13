//! Shared plumbing: waiter identities and the lock/condvar pair the blocking
//! handle layer wraps around every core.

#[cfg(not(target_arch = "wasm32"))]
use std::sync::Condvar;
use std::sync::{Mutex, MutexGuard};

/// Identifies one parked operation inside a channel core: a blocked `send`
/// holding its value, a waiting `recv`, or a parked `changed()`.
///
/// Cores allocate ids monotonically, so they are also useful for asserting
/// FIFO wake order in tests.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct WaiterId(u64);

/// Monotonic allocator for [`WaiterId`]s, owned by each core.
#[derive(Debug, Default)]
pub(crate) struct WaiterIds {
    next: u64,
}

impl WaiterIds {
    pub(crate) fn alloc(&mut self) -> WaiterId {
        self.next += 1;
        WaiterId(self.next)
    }
}

/// A core behind a mutex, plus a condvar used as a "something changed" bell.
///
/// Wake decisions live in the core (its waiter queues pick exactly which
/// parked operation proceeds); the condvar is only broadcast to. Every
/// blocking method loops over a predicate, so spurious wakeups are harmless.
pub(crate) struct Lock<C> {
    core: Mutex<C>,
    #[cfg(not(target_arch = "wasm32"))]
    cond: Condvar,
}

impl<C> Lock<C> {
    pub(crate) fn new(core: C) -> Self {
        Self {
            core: Mutex::new(core),
            #[cfg(not(target_arch = "wasm32"))]
            cond: Condvar::new(),
        }
    }

    pub(crate) fn lock(&self) -> MutexGuard<'_, C> {
        self.core.lock().expect("channel core lock poisoned")
    }

    #[cfg(not(target_arch = "wasm32"))]
    pub(crate) fn wait<'a>(&self, guard: MutexGuard<'a, C>) -> MutexGuard<'a, C> {
        self.cond.wait(guard).expect("channel core lock poisoned")
    }

    /// Waking is harmless everywhere (on wasm nobody ever parks, since
    /// blocking methods are compiled out).
    pub(crate) fn notify_all(&self) {
        #[cfg(not(target_arch = "wasm32"))]
        self.cond.notify_all();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn waiter_ids_are_monotonic() {
        let mut ids = WaiterIds::default();
        assert_eq!(ids.alloc(), WaiterId(1));
        assert_eq!(ids.alloc(), WaiterId(2));
    }
}
