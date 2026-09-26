//! Two actors in a cycle of bounded channels, and the deadlock it is
//! waiting to become.
//!
//! Actor A forwards every message it receives to B; B forwards every one
//! back to A. Each inbox is a bounded [`MpscCore`]. While there is slack
//! somewhere the messages circulate forever. Feed in enough of them and
//! both inboxes fill: A parks in `send().await` towards B, B parks in
//! `send().await` towards A, and neither will ever `recv()` again — so
//! neither inbox will ever have room. Nothing panics and nothing errors.
//! The count-ups just keep counting.
//!
//! Cutting the cycle — B forwards to a sink instead of back to A — makes
//! the topology a DAG, and the same inputs simply drain away.
//!
//! Stepped by the tick contract: each step, each actor makes one move of
//! its loop, A then B.

use sim_channels::mpsc::{MpscCore, RecvPoll, SendOffer, SendPoll};
use sim_channels::WaiterId;

use crate::clock::Ticking;

/// Room in each inbox. Two slots and one message in each actor's hand make
/// six messages the most the cycle can hold before it seizes.
pub const CAPACITY: usize = 2;

/// Time between steps, in milliseconds: slow enough to follow.
pub const STEP_MS: f64 = 700.0;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Msg {
    pub id: u64,
    /// How many actors have forwarded it so far.
    pub hops: u32,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Actor {
    /// In `rx.recv().await`, with an empty inbox.
    Receiving,
    /// Received this message; forwards it on the next step.
    Handling(Msg),
    /// Parked in `send().await` on a full inbox, since `since`.
    Sending {
        msg: Msg,
        waiter: WaiterId,
        since: f64,
    },
}

impl Actor {
    pub fn parked_since(self) -> Option<f64> {
        match self {
            Actor::Sending { since, .. } => Some(since),
            _ => None,
        }
    }
}

/// Which actor, as an index into [`CycleSim::actors`] and the inboxes.
pub const A: usize = 0;
pub const B: usize = 1;

pub struct CycleSim {
    now: f64,
    /// When the next step is due, if anything can move.
    next_step: Option<f64>,
    pub inboxes: [MpscCore<Msg>; 2],
    pub actors: [Actor; 2],
    /// B forwards to a sink instead of back to A.
    pub cut: bool,
    /// Messages the sink has swallowed.
    pub sunk: u64,
    /// A message from outside parked on A's full inbox, since when.
    pub injecting: Option<(Msg, WaiterId, f64)>,
    next_msg: u64,
}

impl CycleSim {
    pub fn new(cut: bool) -> Self {
        Self {
            now: 0.0,
            next_step: None,
            inboxes: [MpscCore::new(CAPACITY), MpscCore::new(CAPACITY)],
            actors: [Actor::Receiving; 2],
            cut,
            sunk: 0,
            injecting: None,
            next_msg: 1,
        }
    }

    /// Send one fresh message into A's inbox from outside. Parks like any
    /// sender if the inbox is full.
    pub fn inject(&mut self) {
        if self.injecting.is_some() {
            return;
        }
        let msg = Msg {
            id: self.next_msg,
            hops: 0,
        };
        self.next_msg += 1;
        if let SendOffer::Blocked { waiter } = self.inboxes[A].offer_send(msg) {
            self.injecting = Some((msg, waiter, self.now));
        }
        self.schedule();
    }

    /// Both actors parked sending to each other: a deadlock, and since when
    /// it has been one.
    pub fn deadlocked_since(&self) -> Option<f64> {
        if self.cut || !self.stuck(A) || !self.stuck(B) {
            return None;
        }
        let a = self.actors[A].parked_since()?;
        let b = self.actors[B].parked_since()?;
        Some(a.max(b))
    }

    /// Messages anywhere in the cycle: queued, in hand, or parked.
    pub fn in_circulation(&self) -> usize {
        let held = self
            .actors
            .iter()
            .filter(|a| !matches!(a, Actor::Receiving))
            .count();
        self.inboxes.iter().map(|i| i.len()).sum::<usize>() + held
    }

    fn schedule(&mut self) {
        if self.next_step.is_none() && self.can_move() {
            self.next_step = Some(self.now + STEP_MS);
        }
    }

    /// Whether a step would change anything.
    fn can_move(&self) -> bool {
        (0..2).any(|i| match self.actors[i] {
            Actor::Receiving => !self.inboxes[i].is_empty(),
            Actor::Handling(_) => true,
            Actor::Sending { .. } => !self.stuck(i),
        }) || self
            .injecting
            .is_some_and(|(_, waiter, _)| !self.still_queued(A, waiter))
    }

    /// Whether `waiter`'s send into `inbox` is still waiting for a slot.
    /// Asked of the core: a receive on the far side may already have moved
    /// the value in, and only the next poll tells the sender so.
    fn still_queued(&self, inbox: usize, waiter: WaiterId) -> bool {
        self.inboxes[inbox].blocked().any(|(w, _)| w == waiter)
    }

    /// Actor `i` is parked in a send that no slot has taken yet.
    pub fn stuck(&self, i: usize) -> bool {
        match (self.actors[i], self.target(i)) {
            (Actor::Sending { waiter, .. }, Some(target)) => self.still_queued(target, waiter),
            _ => false,
        }
    }

    /// Where actor `i` forwards to; `None` is the sink.
    fn target(&self, i: usize) -> Option<usize> {
        match i {
            A => Some(B),
            _ if self.cut => None,
            _ => Some(A),
        }
    }

    /// One move of actor `i`'s loop.
    fn step_actor(&mut self, i: usize) {
        self.actors[i] = match self.actors[i] {
            Actor::Receiving => match self.inboxes[i].poll_recv() {
                RecvPoll::Value(msg) => Actor::Handling(msg),
                RecvPoll::Empty { .. } | RecvPoll::Disconnected => Actor::Receiving,
            },
            Actor::Handling(msg) => {
                let msg = Msg {
                    hops: msg.hops + 1,
                    ..msg
                };
                match self.target(i) {
                    None => {
                        self.sunk += 1;
                        Actor::Receiving
                    }
                    Some(target) => match self.inboxes[target].offer_send(msg) {
                        SendOffer::Blocked { waiter } => Actor::Sending {
                            msg,
                            waiter,
                            since: self.now,
                        },
                        SendOffer::Accepted | SendOffer::Rejected(_) => Actor::Receiving,
                    },
                }
            }
            Actor::Sending { msg, waiter, since } => {
                let target = self.target(i).unwrap_or(A);
                match self.inboxes[target].poll_send(waiter) {
                    SendPoll::Pending => Actor::Sending { msg, waiter, since },
                    _ => Actor::Receiving,
                }
            }
        };
    }

    fn step(&mut self) {
        self.step_actor(A);
        if let Some((_, waiter, _)) = self.injecting {
            if self.inboxes[A].poll_send(waiter) != SendPoll::Pending {
                self.injecting = None;
            }
        }
        self.step_actor(B);
    }
}

impl Ticking for CycleSim {
    type Event = ();

    fn sync_now(&mut self, now_ms: f64) {
        self.now = now_ms;
    }

    fn next_delay_ms(&self) -> Option<f64> {
        self.next_step.map(|due| (due - self.now).max(0.0))
    }

    fn poll_due(&mut self) -> Vec<()> {
        let mut stepped = Vec::new();
        while let Some(due) = self.next_step.filter(|due| *due <= self.now) {
            self.next_step = None;
            self.step();
            stepped.push(());
            if self.can_move() {
                self.next_step = Some(due + STEP_MS);
            }
        }
        stepped
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Step the clock until nothing is due, or `limit` steps.
    fn settle(sim: &mut CycleSim, limit: usize) {
        for _ in 0..limit {
            let Some(delay) = sim.next_delay_ms() else {
                return;
            };
            let now = sim.now + delay;
            sim.tick(now);
        }
    }

    #[test]
    fn a_few_messages_circulate_forever() {
        let mut sim = CycleSim::new(false);
        for _ in 0..3 {
            sim.inject();
        }
        settle(&mut sim, 200);
        assert!(
            sim.next_delay_ms().is_some(),
            "still moving after 200 steps"
        );
        assert_eq!(sim.deadlocked_since(), None);
        assert_eq!(sim.in_circulation(), 3, "nothing lost, nothing made");
    }

    #[test]
    fn six_messages_seize_the_cycle() {
        let mut sim = CycleSim::new(false);
        for _ in 0..6 {
            sim.inject();
            settle(&mut sim, 3);
        }
        settle(&mut sim, 200);
        assert!(sim.deadlocked_since().is_some());
        assert_eq!(sim.next_delay_ms(), None, "nothing can ever move again");
        assert!(sim.inboxes.iter().all(|inbox| inbox.len() == CAPACITY));
    }

    #[test]
    fn once_seized_the_outside_world_parks_too() {
        let mut sim = CycleSim::new(false);
        for _ in 0..6 {
            sim.inject();
            settle(&mut sim, 3);
        }
        settle(&mut sim, 200);
        sim.inject();
        settle(&mut sim, 50);
        assert!(sim.injecting.is_some(), "the send into A never completes");
    }

    #[test]
    fn cutting_the_cycle_drains_everything() {
        let mut sim = CycleSim::new(true);
        for _ in 0..10 {
            sim.inject();
            settle(&mut sim, 3);
        }
        settle(&mut sim, 200);
        assert_eq!(sim.deadlocked_since(), None);
        assert_eq!(sim.sunk, 10);
        assert_eq!(sim.in_circulation(), 0);
        assert_eq!(sim.next_delay_ms(), None, "quiet, not stuck");
    }

    #[test]
    fn an_idle_cycle_asks_for_no_clock() {
        let sim = CycleSim::new(false);
        assert_eq!(sim.next_delay_ms(), None);
    }
}
