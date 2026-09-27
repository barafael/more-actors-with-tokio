//! The watchdog actor from `barafael/watchdog`, running in real time.
//!
//! CONCEPT.md §5: the loop-select from the slide before, given somewhere to
//! run. `Watchdog::with_timeout(d).run()` spawns the actor and hands back
//! a `mpsc::Sender<Signal>` and a `oneshot::Receiver<Expired>`. Its loop
//! `select!`s over two branches:
//!
//! ```text
//! msg = reset_rx.recv()     => Reset: active, sleep re-armed
//!                              Stop:  active = false
//!                              None:  every sender gone — exit
//! () = sleep, if active     => send Expired on the oneshot — exit
//! ```
//!
//! The sim follows that loop exactly. A signal takes a moment to arrive
//! (`FLIGHT_MS`, so it can be seen travelling); time is the tick
//! contract's, so the sim never sleeps and tests step a number.

use sim_channels::mpsc::{MpscCore, TryRecvError};

use crate::clock::Ticking;
use crate::games::step::LEG_MS;

/// The watchdog's timeout.
pub const TIMEOUT_MS: f64 = 5_000.0;
/// How long a signal is in the channel before `recv()` takes it: one
/// flight on the board, so a signal lands as its flight does.
pub const FLIGHT_MS: f64 = LEG_MS;
/// `mpsc::channel(16)`, as in the crate.
pub const CAPACITY: usize = 16;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Signal {
    Reset,
    Stop,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Phase {
    /// `run()` has not been called.
    Idle,
    Running,
    /// The sleep won: `Expired` was sent and the loop exited.
    Expired,
    /// `recv()` returned `None`: every sender was dropped.
    Exited,
}

/// Which `select!` branch completed, for the view to light up.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Fired {
    Recv(Signal),
    Closed,
    Sleep,
}

pub struct WatchdogSim {
    now: f64,
    pub phase: Phase,
    /// The `if active` precondition on the sleep branch.
    pub active: bool,
    deadline: f64,
    /// The actor's inbox. Each signal carries the instant it can be
    /// received, so it is not seen before its flight lands.
    inbox: MpscCore<(f64, Signal)>,
    /// The last branch to fire, and when.
    pub last: Option<(Fired, f64)>,
    pub resets: u32,
}

impl Default for WatchdogSim {
    fn default() -> Self {
        Self::new()
    }
}

impl WatchdogSim {
    pub fn new() -> Self {
        Self {
            now: 0.0,
            phase: Phase::Idle,
            active: false,
            deadline: 0.0,
            inbox: MpscCore::new(CAPACITY),
            last: None,
            resets: 0,
        }
    }

    /// `Watchdog::with_timeout(TIMEOUT).run()`: a fresh actor, a fresh
    /// sender and a fresh oneshot. The sleep starts now.
    pub fn run(&mut self, now: f64) {
        *self = Self::new();
        self.now = now;
        self.phase = Phase::Running;
        self.active = true;
        self.deadline = now + TIMEOUT_MS;
    }

    /// `reset_tx.send(signal)`. False when it fails: the actor is gone and
    /// dropped its receiver, the sender was dropped, or the inbox is full.
    pub fn send(&mut self, signal: Signal) -> bool {
        self.phase == Phase::Running
            && !self.sender_dropped()
            && self.inbox.try_send((self.now + FLIGHT_MS, signal)).is_ok()
    }

    /// `drop(reset_tx)`.
    pub fn drop_sender(&mut self) {
        if self.phase == Phase::Running && !self.sender_dropped() {
            self.inbox.drop_sender();
        }
    }

    pub fn sender_dropped(&self) -> bool {
        self.inbox.sender_count() == 0
    }

    pub fn in_channel(&self) -> usize {
        self.inbox.len()
    }

    /// Time left on the sleep, if the sleep branch is live.
    pub fn remaining_ms(&self) -> Option<f64> {
        (self.phase == Phase::Running && self.active).then(|| (self.deadline - self.now).max(0.0))
    }

    /// The instant `recv()` next completes: the next signal's arrival, or
    /// — once the sender is dropped and the inbox drained — right away.
    fn next_recv(&self) -> Option<f64> {
        match self.inbox.buffer().next() {
            Some((at, _)) => Some(*at),
            None if self.sender_dropped() => Some(self.now),
            None => None,
        }
    }

    fn next_event(&self) -> Option<f64> {
        if self.phase != Phase::Running {
            return None;
        }
        let sleep = self.active.then_some(self.deadline);
        match (self.next_recv(), sleep) {
            (Some(a), Some(b)) => Some(a.min(b)),
            (a, b) => a.or(b),
        }
    }

    /// The loop's end: `Expired` sent (or the inbox closed), and the
    /// receiver dropped with the actor, so later sends fail.
    fn exit(&mut self, branch: Fired, at: f64) {
        self.phase = if branch == Fired::Sleep {
            Phase::Expired
        } else {
            Phase::Exited
        };
        self.inbox.drop_receiver();
        self.last = Some((branch, at));
    }
}

impl Ticking for WatchdogSim {
    type Event = Fired;

    fn sync_now(&mut self, now_ms: f64) {
        self.now = now_ms;
    }

    fn next_delay_ms(&self) -> Option<f64> {
        self.next_event().map(|at| (at - self.now).max(0.0))
    }

    fn poll_due(&mut self) -> Vec<Fired> {
        let mut fired = Vec::new();
        while self.phase == Phase::Running {
            let recv = self.next_recv().filter(|at| *at <= self.now);
            let sleep = (self.active && self.deadline <= self.now).then_some(self.deadline);
            // whichever became ready first wins this turn of the loop
            match (recv, sleep) {
                (Some(r), Some(s)) if s < r => {}
                (Some(_), _) => match self.inbox.try_recv() {
                    Ok((at, signal)) => {
                        match signal {
                            Signal::Reset => {
                                self.active = true;
                                self.deadline = at + TIMEOUT_MS;
                                self.resets += 1;
                            }
                            Signal::Stop => self.active = false,
                        }
                        self.last = Some((Fired::Recv(signal), at));
                        fired.push(Fired::Recv(signal));
                        continue;
                    }
                    Err(TryRecvError::Disconnected) => {
                        self.exit(Fired::Closed, self.now);
                        fired.push(Fired::Closed);
                        continue;
                    }
                    Err(TryRecvError::Empty) => break,
                },
                (None, Some(_)) => {}
                (None, None) => break,
            }
            // the sleep won
            self.exit(Fired::Sleep, self.deadline);
            fired.push(Fired::Sleep);
        }
        fired
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn running() -> WatchdogSim {
        let mut sim = WatchdogSim::new();
        sim.run(0.0);
        sim
    }

    #[test]
    fn left_alone_it_expires_after_the_timeout() {
        let mut sim = running();
        assert_eq!(sim.next_delay_ms(), Some(TIMEOUT_MS));
        assert!(sim.tick(TIMEOUT_MS - 1.0).is_empty());
        assert_eq!(sim.tick(TIMEOUT_MS), [Fired::Sleep]);
        assert_eq!(sim.phase, Phase::Expired);
        assert_eq!(sim.next_delay_ms(), None, "the loop has exited");
    }

    #[test]
    fn a_reset_rearms_the_sleep_from_when_it_arrives() {
        let mut sim = running();
        sim.sync_now(3_000.0);
        assert!(sim.send(Signal::Reset));
        assert_eq!(sim.tick(3_000.0 + FLIGHT_MS), [Fired::Recv(Signal::Reset)]);
        assert_eq!(sim.remaining_ms(), Some(TIMEOUT_MS));
        assert!(
            sim.tick(TIMEOUT_MS + 1.0).is_empty(),
            "survived the first deadline"
        );
    }

    #[test]
    fn stop_disables_the_sleep_branch_and_reset_brings_it_back() {
        let mut sim = running();
        sim.send(Signal::Stop);
        sim.tick(FLIGHT_MS);
        assert!(!sim.active);
        assert_eq!(sim.remaining_ms(), None);
        assert!(
            sim.tick(60_000.0).is_empty(),
            "a stopped watchdog never fires"
        );
        sim.send(Signal::Reset);
        sim.tick(60_000.0 + FLIGHT_MS);
        assert!(sim.active);
        assert_eq!(sim.tick(60_000.0 + FLIGHT_MS + TIMEOUT_MS), [Fired::Sleep]);
    }

    #[test]
    fn a_reset_still_in_flight_at_the_deadline_is_too_late() {
        let mut sim = running();
        sim.sync_now(TIMEOUT_MS - FLIGHT_MS / 2.0);
        sim.send(Signal::Reset);
        assert_eq!(sim.tick(TIMEOUT_MS + FLIGHT_MS), [Fired::Sleep]);
        assert_eq!(sim.resets, 0);
    }

    #[test]
    fn dropping_the_sender_ends_it_once_the_channel_drains() {
        let mut sim = running();
        sim.send(Signal::Reset);
        sim.drop_sender();
        assert!(!sim.send(Signal::Reset), "no sender left to send with");
        assert_eq!(
            sim.tick(FLIGHT_MS),
            [Fired::Recv(Signal::Reset), Fired::Closed]
        );
        assert_eq!(sim.phase, Phase::Exited);
    }

    #[test]
    fn nothing_is_sent_to_an_expired_watchdog() {
        let mut sim = running();
        sim.tick(TIMEOUT_MS);
        assert!(!sim.send(Signal::Reset));
    }

    #[test]
    fn run_starts_over_with_a_fresh_actor() {
        let mut sim = running();
        sim.tick(TIMEOUT_MS);
        sim.run(10_000.0);
        assert_eq!(sim.phase, Phase::Running);
        assert_eq!(sim.remaining_ms(), Some(TIMEOUT_MS));
        assert!(sim.last.is_none());
    }

    #[test]
    fn an_idle_watchdog_asks_for_no_clock() {
        assert_eq!(WatchdogSim::new().next_delay_ms(), None);
    }
}
