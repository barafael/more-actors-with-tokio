//! The hook: `Arc<Mutex<Rgb>>`, shared by every task.
//!
//! CONCEPT.md §1. Each task holds a clone of the `Arc`. One of
//! them locks it; everyone who tries next parks in `lock().await` and can do
//! nothing else until the guard drops. The mutex buys safety — only the
//! holder can touch the value — and nothing more: no backpressure, no say
//! over how long the holder keeps it, no lifecycle beyond "whoever holds it,
//! holds it".
//!
//! The value is a colour the holder picks from a palette of six, not a
//! counter: a struct of three fields, where a number would invite "just use
//! an atomic".
//!
//! Modelled on `tokio::sync::Mutex`, which is fair: waiters acquire in the
//! order they called `lock()`.

use std::collections::VecDeque;

/// The tasks holding a clone of the `Arc`: three is the fewest that makes a
/// queue behind a holder.
pub const TASKS: [u64; 3] = [1, 2, 3];

/// The protected value: a colour, one byte per channel.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Rgb {
    pub r: u8,
    pub g: u8,
    pub b: u8,
}

impl Default for Rgb {
    /// Mid grey: not in the palette, so the first pick always shows.
    fn default() -> Self {
        Self { r: 128, g: 128, b: 128 }
    }
}

impl Rgb {
    /// `#rrggbb`, for the hexagon's fills.
    pub fn hex(self) -> String {
        format!("#{:02x}{:02x}{:02x}", self.r, self.g, self.b)
    }
}

/// The six colours the holder can pick: once round the colour wheel, in
/// the deck's muted paper tones.
pub const PALETTE: [Rgb; 6] = [
    Rgb { r: 192, g: 87, b: 79 },
    Rgb { r: 217, g: 138, b: 61 },
    Rgb { r: 226, g: 194, b: 91 },
    Rgb { r: 95, g: 143, b: 90 },
    Rgb { r: 91, g: 132, b: 168 },
    Rgb { r: 139, g: 106, b: 166 },
];

/// What one task is doing.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Standing {
    Idle,
    /// Parked in `lock().await`: 1-based place in the queue, and since when.
    Waiting {
        place: usize,
        since: f64,
    },
    /// Holding the guard, since when.
    Holding {
        since: f64,
    },
}

#[derive(Debug, Default)]
pub struct MutexSim {
    now: f64,
    value: Rgb,
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

    /// The protected value: only the guard's holder may change it.
    pub fn value(&self) -> Rgb {
        self.value
    }

    /// The guard's holder, and since when.
    pub fn holder(&self) -> Option<(u64, f64)> {
        self.holder
    }

    /// Parked in `lock().await`, in the order they will acquire.
    pub fn waiters(&self) -> impl Iterator<Item = (u64, f64)> + '_ {
        self.waiters.iter().copied()
    }

    pub fn standing(&self, task: u64) -> Standing {
        if let Some((_, since)) = self.holder.filter(|(t, _)| *t == task) {
            return Standing::Holding { since };
        }
        self.waiters
            .iter()
            .position(|(t, _)| *t == task)
            .map(|place| Standing::Waiting {
                place: place + 1,
                since: self.waiters[place].1,
            })
            .unwrap_or(Standing::Idle)
    }

    fn holding(&self, task: u64) -> bool {
        self.holder.is_some_and(|(t, _)| t == task)
    }

    /// `let guard = mutex.lock().await;`
    pub fn lock(&mut self, task: u64) {
        // A task parked in `lock()` cannot call anything else, and a holder
        // locking again would deadlock itself.
        if !TASKS.contains(&task) || self.standing(task) != Standing::Idle {
            return;
        }
        if self.holder.is_none() {
            self.holder = Some((task, self.now));
        } else {
            self.waiters.push_back((task, self.now));
        }
    }

    /// `*guard = colour;` — only while holding the guard.
    pub fn paint(&mut self, task: u64, colour: Rgb) {
        if self.holding(task) {
            self.value = colour;
        }
    }

    /// `drop(guard);` — the longest waiter, if any, acquires right now.
    pub fn unlock(&mut self, task: u64) {
        if self.holding(task) {
            let now = self.now;
            self.holder = self.waiters.pop_front().map(|(task, _)| (task, now));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn holder(sim: &MutexSim) -> Option<u64> {
        sim.holder().map(|(task, _)| task)
    }

    fn waiters(sim: &MutexSim) -> Vec<u64> {
        sim.waiters().map(|(task, _)| task).collect()
    }

    #[test]
    fn the_first_lock_acquires_and_the_rest_park_in_order() {
        let mut sim = MutexSim::new();
        sim.lock(2);
        sim.lock(3);
        sim.lock(1);
        assert_eq!(holder(&sim), Some(2));
        assert_eq!(waiters(&sim), [3, 1]);
        assert!(TASKS.iter().all(|t| sim.standing(*t) != Standing::Idle));
    }

    #[test]
    fn unlocking_hands_the_guard_to_the_longest_waiter() {
        let mut sim = MutexSim::new();
        for task in [1, 2, 3] {
            sim.lock(task);
        }
        sim.unlock(1);
        assert_eq!(holder(&sim), Some(2), "fair: first come, first served");
        assert_eq!(waiters(&sim), [3]);
        assert_eq!(sim.standing(1), Standing::Idle);
    }

    #[test]
    fn only_the_holder_touches_the_value_or_the_lock() {
        let mut sim = MutexSim::new();
        sim.lock(1);
        sim.lock(2);
        sim.paint(2, PALETTE[3]);
        sim.unlock(2);
        assert_eq!(sim.value(), Rgb::default());
        assert_eq!(holder(&sim), Some(1));
        sim.paint(1, PALETTE[3]);
        assert_eq!(sim.value(), PALETTE[3]);
    }

    #[test]
    fn the_palette_is_six_distinct_colours_none_of_them_the_start() {
        for (i, colour) in PALETTE.iter().enumerate() {
            assert_ne!(*colour, Rgb::default());
            assert!(!PALETTE[..i].contains(colour));
        }
        assert_eq!(PALETTE[0].hex(), "#c0574f");
    }

    #[test]
    fn a_task_cannot_queue_twice_or_relock_its_own_guard() {
        let mut sim = MutexSim::new();
        sim.lock(1);
        sim.lock(1);
        sim.lock(2);
        sim.lock(2);
        assert_eq!(holder(&sim), Some(1));
        assert_eq!(waiters(&sim), [2]);
    }

    #[test]
    fn waits_count_up_from_the_lock_call_and_holds_from_the_acquire() {
        let mut sim = MutexSim::new();
        sim.sync_now(1_000.0);
        sim.lock(1);
        sim.sync_now(2_000.0);
        sim.lock(2);
        assert_eq!(sim.standing(1), Standing::Holding { since: 1_000.0 });
        assert_eq!(
            sim.standing(2),
            Standing::Waiting {
                place: 1,
                since: 2_000.0
            }
        );

        sim.sync_now(9_000.0);
        sim.unlock(1);
        assert_eq!(
            sim.standing(2),
            Standing::Holding { since: 9_000.0 },
            "the hold counts from the acquire, not the lock call"
        );
    }

    #[test]
    fn strangers_are_ignored() {
        let mut sim = MutexSim::new();
        sim.lock(9);
        assert_eq!(holder(&sim), None);
    }
}
