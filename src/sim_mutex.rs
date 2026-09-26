//! The hook: `Arc<Mutex<u64>>`, shared by the whole room.
//!
//! CONCEPT.md §1. Every phone is a task holding a clone of the `Arc`. One of
//! them locks it; everyone who tries next parks in `lock().await` and can do
//! nothing else until the guard drops. The mutex buys safety — only the
//! holder can touch the value — and nothing more: no backpressure, no say
//! over how long the holder keeps it, no lifecycle beyond "whoever holds it,
//! holds it".
//!
//! Modelled on `tokio::sync::Mutex`, which is fair: waiters acquire in the
//! order they called `lock()`. A task that goes away drops what it held — a
//! guard releases the lock, a pending `lock()` future leaves the queue —
//! exactly as a cancelled task would.

use std::collections::VecDeque;

use crate::protocol::{MutexSnapshot, MutexTask, MutexWire};

#[derive(Debug, Default)]
pub struct MutexSim {
    now: f64,
    value: u64,
    /// Present tasks and who owns each, in the order they joined.
    tasks: Vec<(u64, u64)>,
    /// The guard's holder, and since when.
    holder: Option<(u64, f64)>,
    /// Parked in `lock().await`, and since when. FIFO.
    waiters: VecDeque<(u64, f64)>,
}

impl MutexSim {
    pub fn new() -> Self {
        Self::default()
    }

    /// Tell the sim what time it is, in monotonic milliseconds.
    pub fn sync_now(&mut self, now: f64) {
        self.now = now;
    }

    pub fn has_task(&self, task: u64) -> bool {
        self.tasks.iter().any(|(t, _)| *t == task)
    }

    /// A task appears holding a clone of the `Arc`.
    pub fn add_task(&mut self, task: u64, owner: u64) {
        if !self.has_task(task) {
            self.tasks.push((task, owner));
        }
    }

    /// A task goes away, dropping whatever it held.
    pub fn remove_task(&mut self, task: u64) {
        self.tasks.retain(|(t, _)| *t != task);
        self.waiters.retain(|(t, _)| *t != task);
        if self.holding(task) {
            self.release();
        }
    }

    fn holding(&self, task: u64) -> bool {
        self.holder.is_some_and(|(t, _)| t == task)
    }

    /// The guard drops: the longest waiter, if any, acquires right now.
    fn release(&mut self) {
        let now = self.now;
        self.holder = self.waiters.pop_front().map(|(task, _)| (task, now));
    }

    pub fn handle(&mut self, task: u64, wire: MutexWire) {
        if !self.has_task(task) {
            return;
        }
        match wire {
            MutexWire::Lock => {
                let waiting = self.waiters.iter().any(|(t, _)| *t == task);
                if self.holding(task) || waiting {
                    // A task parked in `lock()` cannot call anything else,
                    // and a holder locking again would deadlock itself.
                    return;
                }
                if self.holder.is_none() {
                    self.holder = Some((task, self.now));
                } else {
                    self.waiters.push_back((task, self.now));
                }
            }
            MutexWire::Increment if self.holding(task) => self.value += 1,
            MutexWire::Unlock if self.holding(task) => self.release(),
            MutexWire::Increment | MutexWire::Unlock => {}
        }
    }

    pub fn snapshot(&self) -> MutexSnapshot {
        let owner = |task: u64| {
            self.tasks
                .iter()
                .find(|(t, _)| *t == task)
                .map(|(_, owner)| *owner)
                .unwrap_or(task)
        };
        let view = |(task, since): (u64, f64)| MutexTask {
            task,
            owner: owner(task),
            for_ms: (self.now - since).max(0.0),
        };
        let busy = |task: u64| self.holding(task) || self.waiters.iter().any(|(t, _)| *t == task);
        MutexSnapshot {
            value: self.value,
            holder: self.holder.map(view),
            waiters: self.waiters.iter().copied().map(view).collect(),
            idle: self
                .tasks
                .iter()
                .filter(|(task, _)| !busy(*task))
                .map(|&(task, owner)| MutexTask {
                    task,
                    owner,
                    for_ms: 0.0,
                })
                .collect(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn room(tasks: &[u64]) -> MutexSim {
        let mut sim = MutexSim::new();
        for &task in tasks {
            sim.add_task(task, task);
        }
        sim
    }

    fn holder(sim: &MutexSim) -> Option<u64> {
        sim.snapshot().holder.map(|h| h.task)
    }

    fn waiters(sim: &MutexSim) -> Vec<u64> {
        sim.snapshot().waiters.iter().map(|w| w.task).collect()
    }

    #[test]
    fn the_first_lock_acquires_and_the_rest_park_in_order() {
        let mut sim = room(&[1, 2, 3]);
        sim.handle(2, MutexWire::Lock);
        sim.handle(3, MutexWire::Lock);
        sim.handle(1, MutexWire::Lock);
        assert_eq!(holder(&sim), Some(2));
        assert_eq!(waiters(&sim), [3, 1]);
        assert!(sim.snapshot().idle.is_empty());
    }

    #[test]
    fn unlocking_hands_the_guard_to_the_longest_waiter() {
        let mut sim = room(&[1, 2, 3]);
        for task in [1, 2, 3] {
            sim.handle(task, MutexWire::Lock);
        }
        sim.handle(1, MutexWire::Unlock);
        assert_eq!(holder(&sim), Some(2), "fair: first come, first served");
        assert_eq!(waiters(&sim), [3]);
        assert_eq!(sim.snapshot().idle.len(), 1);
    }

    #[test]
    fn only_the_holder_touches_the_value_or_the_lock() {
        let mut sim = room(&[1, 2]);
        sim.handle(1, MutexWire::Lock);
        sim.handle(2, MutexWire::Lock);
        sim.handle(2, MutexWire::Increment);
        sim.handle(2, MutexWire::Unlock);
        assert_eq!(sim.snapshot().value, 0);
        assert_eq!(holder(&sim), Some(1));
        sim.handle(1, MutexWire::Increment);
        sim.handle(1, MutexWire::Increment);
        assert_eq!(sim.snapshot().value, 2);
    }

    #[test]
    fn a_task_cannot_queue_twice_or_relock_its_own_guard() {
        let mut sim = room(&[1, 2]);
        sim.handle(1, MutexWire::Lock);
        sim.handle(1, MutexWire::Lock);
        sim.handle(2, MutexWire::Lock);
        sim.handle(2, MutexWire::Lock);
        assert_eq!(holder(&sim), Some(1));
        assert_eq!(waiters(&sim), [2]);
    }

    #[test]
    fn a_departing_holder_drops_its_guard() {
        let mut sim = room(&[1, 2]);
        sim.handle(1, MutexWire::Lock);
        sim.handle(2, MutexWire::Lock);
        sim.remove_task(1);
        assert_eq!(holder(&sim), Some(2));
        assert!(waiters(&sim).is_empty());
    }

    #[test]
    fn a_departing_waiter_leaves_the_queue_without_acquiring() {
        let mut sim = room(&[1, 2, 3]);
        for task in [1, 2, 3] {
            sim.handle(task, MutexWire::Lock);
        }
        sim.remove_task(2);
        sim.handle(1, MutexWire::Unlock);
        assert_eq!(holder(&sim), Some(3));
    }

    #[test]
    fn waits_count_up_from_the_lock_call_and_holds_from_the_acquire() {
        let mut sim = room(&[1, 2]);
        sim.sync_now(1_000.0);
        sim.handle(1, MutexWire::Lock);
        sim.sync_now(2_000.0);
        sim.handle(2, MutexWire::Lock);

        sim.sync_now(9_000.0);
        let snap = sim.snapshot();
        assert_eq!(snap.holder.map(|h| h.for_ms), Some(8_000.0));
        assert_eq!(snap.waiters[0].for_ms, 7_000.0, "no timeout: it only grows");

        sim.handle(1, MutexWire::Unlock);
        let snap = sim.snapshot();
        assert_eq!(snap.holder.map(|h| (h.task, h.for_ms)), Some((2, 0.0)));
    }

    #[test]
    fn strangers_are_ignored() {
        let mut sim = room(&[1]);
        sim.handle(9, MutexWire::Lock);
        assert_eq!(holder(&sim), None);
    }
}
