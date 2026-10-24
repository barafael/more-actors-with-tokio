//! The watch channel the watch game draws: ONE shared cell of state, a
//! single sender, and receivers that each remember the last version they
//! saw.
//!
//! Channel truth lives in [`WatchCore`]; this adds receiver ids the view can
//! key on, the borrow guards, and the flights and notices the view animates.
//!
//! Teaching beats mirrored from tokio:
//! - `Receiver::borrow()` returns a read guard; `send` waits on the write
//!   lock, so a send issued while any receiver is borrowing is parked and
//!   commits once the last guard drops.
//! - a `changed()` that is already stale (`version < current`) resolves
//!   instantly; otherwise it waits for the next accepted send.
//! - sending a value equal to the current one changes nothing (dedup).
//! - sending with zero receivers fails (`SendError`).

use std::collections::{BTreeMap, BTreeSet};

use sim_channels::watch::{BorrowRelease, ChangePoll, SendOffer, WatchCore};
use sim_channels::WaiterId;

/// Demo guard, not channel semantics: how many receivers the game holds at
/// once. Tokio's `subscribe()` is unbounded; the diagram is not.
pub const MAX_RX: usize = 6;

/// Flights still on screen are capped, so a burst of clicks on a slide whose
/// animations never get to end cannot grow the list without bound.
const MAX_FLIGHTS: usize = 32;

/// One live `Receiver<T>` handle. `version` is the highest version it has
/// observed so far (its `changed()` checkpoint).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Receiver {
    pub id: u64,
    pub version: u64,
    /// Blocked in `changed()`, waiting for the next *different* value.
    pub awaiting: bool,
    /// Holding a `borrow()` read guard; while any guard is held a send is
    /// parked.
    pub borrowing: bool,
}

/// What a flight animates: a changed value fanning out from the cell, or a
/// gray borrow/subscribe transfer.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FlightKind {
    Value,
    Borrow,
}

/// A value travelling from the cell to receiver `rx`. Pure cosmetics: the
/// view keys the animation on `key` and lands it when the animation ends.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Flight {
    pub key: u64,
    pub kind: FlightKind,
    pub rx: u64,
    pub ch: char,
}

/// A send that changed nothing visible, said out loud so it does not look
/// like the game ignored the click.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Notice {
    /// The value equals the current one: nothing changes, nobody wakes.
    Same,
    /// No receiver is left: `send` returns `SendError`.
    Refused,
    /// A borrow is held: the send is parked behind the read guard.
    Blocked,
}

impl Notice {
    pub fn label(self) -> &'static str {
        match self {
            Notice::Same => "same",
            Notice::Refused => "error",
            Notice::Blocked => "blocked",
        }
    }
}

/// What a `send` did.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SendOutcome {
    /// The value changed and the version moved on.
    Changed,
    Same,
    Refused,
    /// Parked behind a borrow; it commits when the last guard drops.
    Blocked,
    /// A send is already parked. One at a time, so the blocked-send beat
    /// stays legible on stage.
    Ignored,
}

pub struct WatchSim {
    core: WatchCore<char>,
    /// Receiver id -> core receiver id. Core ids are recycled on drop; these
    /// ids are rendered by the view and never reused.
    ids: BTreeMap<u64, u64>,
    /// Receivers in the order they appeared.
    order: Vec<u64>,
    /// Receivers currently holding a `borrow()` read guard.
    borrowing: BTreeSet<u64>,
    /// The one send parked behind outstanding borrows, if any. The core
    /// keeps its queue entry after the send resolves and only `poll_send`
    /// retires it, so the waiter id is kept alongside the value.
    pending_send: Option<(WaiterId, char)>,
    next_rx: u64,
    flights: Vec<Flight>,
    notice: Option<(Notice, u64)>,
    /// Keys for flights and notices: one counter, so no two ever collide.
    next_key: u64,
}

impl WatchSim {
    /// `watch::channel(init)`, with `receivers` subscribed already — the
    /// slide should show the mechanism, value cell and fan of cursors,
    /// before anyone clicks.
    pub fn new(init: char, receivers: usize) -> Self {
        let mut core = WatchCore::new(init);
        // `WatchCore::new` subscribes receiver 1 as tokio's `channel` hands
        // one back; the ids here are allocated separately, so drop it and
        // subscribe the game's own
        core.drop_receiver(1);
        let mut sim = Self {
            core,
            ids: BTreeMap::new(),
            order: Vec::new(),
            borrowing: BTreeSet::new(),
            pending_send: None,
            next_rx: 1,
            flights: Vec::new(),
            notice: None,
            next_key: 0,
        };
        for _ in 0..receivers {
            sim.subscribe();
        }
        sim.flights.clear();
        sim
    }

    pub fn value(&self) -> char {
        *self.core.value()
    }

    /// Bumps on every accepted (non-dedup) send.
    #[cfg(test)]
    pub fn version(&self) -> u64 {
        self.core.version()
    }

    /// A send parked behind borrows.
    pub fn pending_send(&self) -> Option<char> {
        self.pending_send.map(|(_, ch)| ch)
    }

    pub fn receivers(&self) -> Vec<Receiver> {
        self.order
            .iter()
            .filter_map(|id| {
                let core_id = *self.ids.get(id)?;
                Some(Receiver {
                    id: *id,
                    version: self.core.receiver_version(core_id).unwrap_or_default(),
                    awaiting: self.core.is_awaiting(core_id),
                    borrowing: self.borrowing.contains(id),
                })
            })
            .collect()
    }

    pub fn can_subscribe(&self) -> bool {
        self.order.len() < MAX_RX
    }

    pub fn flights(&self) -> &[Flight] {
        &self.flights
    }

    pub fn notice(&self) -> Option<(Notice, u64)> {
        self.notice
    }

    /// A flight's animation ended.
    pub fn land(&mut self, key: u64) {
        self.flights.retain(|f| f.key != key);
    }

    /// A notice has been on screen long enough. Keyed, so clearing an old
    /// notice cannot wipe a newer one.
    pub fn clear_notice(&mut self, key: u64) {
        if self.notice.is_some_and(|(_, k)| k == key) {
            self.notice = None;
        }
    }

    fn key(&mut self) -> u64 {
        self.next_key += 1;
        self.next_key
    }

    fn fly(&mut self, kind: FlightKind, rx: u64) {
        let key = self.key();
        let ch = self.value();
        self.flights.push(Flight { key, kind, rx, ch });
        if self.flights.len() > MAX_FLIGHTS {
            self.flights.remove(0);
        }
    }

    fn say(&mut self, notice: Notice) {
        let key = self.key();
        self.notice = Some((notice, key));
    }

    /// Register a fresh receiver under a new id; it instantly sees the
    /// current value.
    fn register(&mut self, core_id: u64) -> u64 {
        let id = self.next_rx;
        self.next_rx += 1;
        self.ids.insert(id, core_id);
        self.order.push(id);
        self.fly(FlightKind::Borrow, id);
        id
    }

    /// `tx.subscribe()`: a brand-new receiver at the current value.
    pub fn subscribe(&mut self) -> Option<u64> {
        if !self.can_subscribe() {
            return None;
        }
        let core_id = self.core.subscribe();
        Some(self.register(core_id))
    }

    /// `rx.clone()`: keeps the source's seen-version but starts fresh (not
    /// awaiting, not borrowing).
    pub fn clone_receiver(&mut self, rx: u64) -> Option<u64> {
        let source = *self.ids.get(&rx)?;
        if !self.can_subscribe() {
            return None;
        }
        let core_id = self.core.clone_receiver(source)?;
        Some(self.register(core_id))
    }

    /// Toggle `changed().await` on a receiver: start it, or cancel it.
    pub fn toggle_changed(&mut self, rx: u64) {
        let Some(&core_id) = self.ids.get(&rx) else {
            return;
        };
        if self.core.is_awaiting(core_id) {
            self.core.cancel_changed(core_id);
            return;
        }
        // already stale: changed() completes at once, and the value reaches
        // that one receiver without any send happening
        if self.core.poll_changed(core_id) == ChangePoll::Immediate {
            self.fly(FlightKind::Value, rx);
        }
    }

    /// Toggle a `borrow()` read guard on a receiver.
    pub fn toggle_borrow(&mut self, rx: u64) {
        if !self.ids.contains_key(&rx) {
            return;
        }
        if self.borrowing.remove(&rx) {
            let release = self.core.end_borrow();
            self.settle_release(release);
            return;
        }
        self.borrowing.insert(rx);
        self.core.begin_borrow();
        self.fly(FlightKind::Borrow, rx);
    }

    /// `drop(rx)`. A guard it held drops with it.
    pub fn drop_receiver(&mut self, rx: u64) {
        let Some(core_id) = self.ids.remove(&rx) else {
            return;
        };
        self.order.retain(|id| *id != rx);
        let was_borrowing = self.borrowing.remove(&rx);
        self.core.drop_receiver(core_id);
        if was_borrowing {
            let release = self.core.end_borrow();
            self.settle_release(release);
        }
    }

    /// `tx.send(ch)`.
    pub fn send(&mut self, ch: char) -> SendOutcome {
        if self.pending_send.is_some() {
            return SendOutcome::Ignored;
        }
        let outcome = if self.core.receiver_count() == 0 {
            SendOutcome::Refused
        } else if *self.core.value() == ch {
            SendOutcome::Same
        } else {
            match self.core.send(ch) {
                SendOffer::Sent => SendOutcome::Changed,
                SendOffer::Blocked { waiter } => {
                    self.pending_send = Some((waiter, ch));
                    SendOutcome::Blocked
                }
                SendOffer::Rejected(_) => SendOutcome::Refused,
            }
        };
        match outcome {
            SendOutcome::Changed => self.fan_out(),
            SendOutcome::Same => self.say(Notice::Same),
            SendOutcome::Refused => self.say(Notice::Refused),
            SendOutcome::Blocked => self.say(Notice::Blocked),
            SendOutcome::Ignored => {}
        }
        outcome
    }

    /// The new value flies from the cell to every receiver.
    fn fan_out(&mut self) {
        for rx in self.order.clone() {
            self.fly(FlightKind::Value, rx);
        }
    }

    /// A parked send resolves once the last read guard drops.
    ///
    /// The core keeps the resolved entry in its queue — `poll_send` is what
    /// retires it, the same recheck a live sender thread runs in its own
    /// loop. Skipping the poll would leave the queue non-empty and park
    /// every later `send` behind nothing.
    fn settle_release(&mut self, release: BorrowRelease) {
        if release == BorrowRelease::Released {
            return;
        }
        if let Some((waiter, _)) = self.pending_send.take() {
            self.core.poll_send(waiter);
        }
        match release {
            BorrowRelease::Resolved => self.fan_out(),
            BorrowRelease::Refused => self.say(Notice::Refused),
            BorrowRelease::Released => {}
        }
        // Invariant: the core's send queue only holds entries while a send
        // is parked behind borrows.
        debug_assert!(
            self.core.pending_waiters().is_empty(),
            "no send is parked, so the core's send queue must be empty"
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use sim_channels::watch::SendPoll;

    /// A channel holding 'c', with no receivers yet.
    fn sim() -> WatchSim {
        WatchSim::new('c', 0)
    }

    fn rx(sim: &mut WatchSim) -> u64 {
        sim.subscribe().expect("under the cap")
    }

    fn receiver(sim: &WatchSim, id: u64) -> Receiver {
        sim.receivers().into_iter().find(|r| r.id == id).unwrap()
    }

    #[test]
    fn a_new_channel_holds_its_initial_value_and_its_receivers() {
        let sim = WatchSim::new('c', 2);
        assert_eq!(sim.value(), 'c');
        assert_eq!(sim.version(), 0);
        assert_eq!(sim.receivers().len(), 2);
        assert!(sim.flights().is_empty(), "the opening frame is still");
    }

    #[test]
    fn send_changes_value_and_bumps_version() {
        let mut sim = sim();
        let r = rx(&mut sim);
        assert_eq!(sim.send('d'), SendOutcome::Changed);
        assert_eq!(sim.value(), 'd');
        assert_eq!(sim.version(), 1);
        assert!(!receiver(&sim, r).awaiting);
        assert!(
            sim.flights()
                .iter()
                .any(|f| f.rx == r && f.kind == FlightKind::Value && f.ch == 'd'),
            "the new value flies to the receiver"
        );
    }

    #[test]
    fn send_without_receivers_is_refused_out_loud() {
        let mut sim = sim();
        assert_eq!(sim.send('d'), SendOutcome::Refused);
        assert_eq!(sim.value(), 'c');
        assert_eq!(sim.notice().map(|(n, _)| n), Some(Notice::Refused));
    }

    #[test]
    fn equal_value_dedups_and_does_not_wake() {
        let mut sim = sim();
        let r = rx(&mut sim);
        sim.toggle_changed(r);
        assert!(receiver(&sim, r).awaiting);

        assert_eq!(sim.send('c'), SendOutcome::Same);
        assert_eq!(sim.version(), 0, "dedup does not bump version");
        assert!(receiver(&sim, r).awaiting, "nobody wakes on a dedup send");
        assert_eq!(sim.notice().map(|(n, _)| n), Some(Notice::Same));
    }

    #[test]
    fn changed_waiting_wakes_on_next_different_value() {
        let mut sim = sim();
        let r1 = rx(&mut sim);
        let r2 = rx(&mut sim);
        sim.toggle_changed(r1);
        sim.toggle_changed(r2);
        assert!(sim.receivers().iter().all(|r| r.awaiting));

        sim.send('d');
        assert!(sim.receivers().iter().all(|r| !r.awaiting));
        assert!(sim.receivers().iter().all(|r| r.version == sim.version()));
    }

    #[test]
    fn changed_completes_immediately_when_already_stale() {
        let mut sim = sim();
        let r = rx(&mut sim);
        sim.send('d');
        let flights_before = sim.flights().len();
        sim.toggle_changed(r);
        assert!(
            !receiver(&sim, r).awaiting,
            "stale changed() resolves at once"
        );
        assert_eq!(receiver(&sim, r).version, sim.version());
        // the completion is drawn, not left for the eye to infer from a
        // version number
        assert_eq!(sim.flights().len(), flights_before + 1);
    }

    #[test]
    fn changed_can_be_cancelled() {
        let mut sim = sim();
        let r = rx(&mut sim);
        sim.toggle_changed(r);
        assert!(receiver(&sim, r).awaiting);
        sim.toggle_changed(r);
        assert!(!receiver(&sim, r).awaiting);
    }

    #[test]
    fn send_blocks_while_borrowing_then_flushes_on_release() {
        let mut sim = sim();
        let r = rx(&mut sim);
        sim.toggle_borrow(r);
        assert!(receiver(&sim, r).borrowing);

        assert_eq!(sim.send('d'), SendOutcome::Blocked);
        assert_eq!(sim.pending_send(), Some('d'));
        assert_eq!(sim.value(), 'c', "not committed while borrowed");

        sim.toggle_borrow(r);
        assert_eq!(sim.value(), 'd');
        assert_eq!(sim.version(), 1);
        assert_eq!(sim.pending_send(), None);
    }

    #[test]
    fn multiple_borrows_delay_send_until_all_released() {
        let mut sim = sim();
        let r1 = rx(&mut sim);
        let r2 = rx(&mut sim);
        sim.toggle_borrow(r1);
        sim.toggle_borrow(r2);
        sim.send('d');
        assert_eq!(sim.pending_send(), Some('d'));

        // releasing only one keeps it pending
        sim.toggle_borrow(r1);
        assert_eq!(sim.pending_send(), Some('d'));

        sim.toggle_borrow(r2);
        assert_eq!(sim.pending_send(), None);
        assert_eq!(sim.value(), 'd');
    }

    #[test]
    fn dropping_last_borrower_flushes_pending_send() {
        let mut sim = sim();
        // a second, non-borrowing receiver survives the drop so the pending
        // send can resolve against remaining receivers
        let _survivor = rx(&mut sim);
        let r = rx(&mut sim);
        sim.toggle_borrow(r);
        sim.send('d');
        sim.drop_receiver(r);
        assert_eq!(sim.value(), 'd');
        assert_eq!(sim.receivers().len(), 1);
    }

    #[test]
    fn pending_send_refused_when_all_receivers_disappear() {
        let mut sim = sim();
        let r = rx(&mut sim);
        sim.toggle_borrow(r);
        sim.send('d');
        sim.drop_receiver(r);
        assert_eq!(sim.value(), 'c');
        assert_eq!(sim.pending_send(), None);
        assert_eq!(sim.notice().map(|(n, _)| n), Some(Notice::Refused));
    }

    #[test]
    fn send_while_pending_is_ignored() {
        let mut sim = sim();
        let r = rx(&mut sim);
        sim.toggle_borrow(r);
        sim.send('d');
        assert_eq!(sim.send('e'), SendOutcome::Ignored, "second send dropped");
        assert_eq!(sim.pending_send(), Some('d'));
    }

    #[test]
    fn clone_copies_seen_version_but_not_awaiting() {
        let mut sim = sim();
        let r = rx(&mut sim);
        // awaiting at the *current* version (not stale), so it blocks
        sim.toggle_changed(r);
        assert!(receiver(&sim, r).awaiting);
        let id = sim.clone_receiver(r).expect("a live source");
        let clone = receiver(&sim, id);
        assert_eq!(
            clone.version,
            sim.version(),
            "clone starts at the source's seen version"
        );
        assert!(!clone.awaiting);
        assert!(!clone.borrowing);

        // a send wakes the source receiver only; the clone stays behind until
        // *it* calls changed() again
        sim.send('d');
        let src = receiver(&sim, r);
        assert!(!src.awaiting, "source was waiting on changed() and woke");
        assert_eq!(src.version, sim.version());
        assert_eq!(receiver(&sim, id).version, 0, "clone has not caught up");

        // cloning an unknown receiver does nothing
        assert_eq!(sim.clone_receiver(999), None);
    }

    #[test]
    fn the_receiver_cap_holds() {
        let mut sim = sim();
        for _ in 0..MAX_RX {
            assert!(sim.subscribe().is_some());
        }
        assert_eq!(sim.subscribe(), None);
        let first = sim.receivers()[0].id;
        assert_eq!(sim.clone_receiver(first), None, "cloning counts too");
        assert_eq!(sim.receivers().len(), MAX_RX);
    }

    #[test]
    fn a_notice_is_cleared_only_by_its_own_key() {
        let mut sim = sim();
        sim.send('d');
        let (_, old) = sim.notice().unwrap();
        let r = rx(&mut sim);
        sim.send('c');
        let (notice, new) = sim.notice().unwrap();
        assert_eq!(notice, Notice::Same);
        sim.clear_notice(old);
        assert_eq!(
            sim.notice(),
            Some((Notice::Same, new)),
            "the newer survives"
        );
        sim.clear_notice(new);
        assert_eq!(sim.notice(), None);
        let _ = r;
    }

    #[test]
    fn landing_removes_exactly_that_flight() {
        let mut sim = sim();
        let a = rx(&mut sim);
        let b = rx(&mut sim);
        let keys: Vec<u64> = sim.flights().iter().map(|f| f.key).collect();
        assert_eq!(keys.len(), 2);
        sim.land(keys[0]);
        assert_eq!(sim.flights().iter().map(|f| f.rx).collect::<Vec<_>>(), [b]);
        let _ = a;
    }

    /// Differential: the sim against the bare core through the
    /// borrow-blocked-send beats. The core keeps a resolved send in its
    /// queue until `poll_send` retires it — the reference model plays the
    /// sender thread and polls. A sim that stops retiring entries drifts at
    /// exactly the next send: its send parks behind a stale entry while the
    /// core's commits.
    #[test]
    fn the_sim_matches_the_core_through_blocked_sends() {
        let mut sim = sim();
        let mut core = WatchCore::new('c');
        core.drop_receiver(1);
        let assert_agree = |sim: &WatchSim, core: &WatchCore<char>, at: &str| {
            assert_eq!(sim.value(), *core.value(), "{at}: value");
            assert_eq!(sim.version(), core.version(), "{at}: version");
            assert_eq!(
                sim.pending_send().is_some(),
                core.pending_waiters().len() == 1,
                "{at}: parked-send state"
            );
        };

        let r = rx(&mut sim);
        let core_r = core.subscribe();
        assert_agree(&sim, &core, "after subscribe");

        // a plain send commits in both models
        assert_eq!(sim.send('d'), SendOutcome::Changed);
        assert_eq!(core.send('d'), SendOffer::Sent);
        assert_agree(&sim, &core, "after plain send");

        // a borrow parks the send in both models
        sim.toggle_borrow(r);
        core.begin_borrow();
        assert_eq!(sim.send('e'), SendOutcome::Blocked);
        let waiter = match core.send('e') {
            SendOffer::Blocked { waiter } => waiter,
            other => panic!("core should park the send, got {other:?}"),
        };
        assert_agree(&sim, &core, "while parked");

        // release: both commit, and the reference retires the resolved
        // entry the way a live sender thread would
        sim.toggle_borrow(r);
        core.end_borrow();
        core.poll_send(waiter);
        assert_agree(&sim, &core, "after release");

        // the regression beat: nobody is borrowing, so the next send must
        // commit — a stale queue entry would park it forever
        assert_eq!(sim.send('f'), SendOutcome::Changed);
        assert_eq!(core.send('f'), SendOffer::Sent);
        assert_agree(&sim, &core, "after the send following a blocked one");

        // refusal must also drain: park a send, then the last receiver
        // leaves while the borrow is out
        sim.toggle_borrow(r);
        core.begin_borrow();
        assert_eq!(sim.send('g'), SendOutcome::Blocked);
        let waiter = match core.send('g') {
            SendOffer::Blocked { waiter } => waiter,
            other => panic!("core should park the send, got {other:?}"),
        };
        sim.drop_receiver(r);
        core.drop_receiver(core_r);
        core.end_borrow();
        assert_eq!(core.poll_send(waiter), SendPoll::Failed('g'));
        assert_agree(&sim, &core, "after refusal");

        // the channel still works for a fresh receiver afterwards
        rx(&mut sim);
        core.subscribe();
        assert_eq!(sim.send('h'), SendOutcome::Changed);
        assert_eq!(core.send('h'), SendOffer::Sent);
        assert_agree(&sim, &core, "after refusal, new receiver");
    }
}
