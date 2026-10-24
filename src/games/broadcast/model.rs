//! The broadcast channel the broadcast game draws: one ring buffer, many
//! `Sender<T>` handles, many `Receiver<T>` handles each reading at its own
//! pace.
//!
//! Channel truth lives in [`BroadcastCore`]; this adds handle ids the view
//! can key on, what each receiver last saw, and the flights and notice the
//! view animates.
//!
//! - `send` never blocks: when the buffer is full the oldest value is
//!   evicted to make room.
//! - `subscribe()` starts at the tail, like tokio's: a new receiver sees
//!   only values sent after it subscribed, never buffered history.
//! - a receiver that falls behind the ring yields `Lagged(n)` once — the
//!   count of evicted values it missed — and resumes at the oldest retained
//!   value.
//! - `recv()` with nothing new arms the receiver (`waiting`); it completes on
//!   the next send.
//! - once every sender is gone the channel is closed, but values still
//!   retained stay receivable: `Closed` is only reported after they drain.

use std::collections::BTreeMap;

use sim_channels::broadcast::{BroadcastCore, BroadcastPoll};

/// How many values the ring buffer retains.
pub const CAPACITY: usize = 5;

/// Demo guard, not channel semantics: how many receivers the game holds at
/// once. Tokio's `subscribe()` is unbounded; the diagram is not.
pub const MAX_RX: usize = 8;

/// The handle `broadcast::channel` hands out; every other one is a clone.
pub const ORIGINAL: u64 = 0;

/// Flights still on screen are capped, so a burst on a slide whose
/// animations never get to end cannot grow the list without bound.
const MAX_FLIGHTS: usize = 32;

/// A char in the channel, tagged with the handle that sent it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Value {
    pub ch: char,
    pub from: u64,
}

/// What a receiver's last `recv()` yielded once it had fallen behind.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RecvError {
    /// `RecvError::Lagged(n)`: the receiver skipped `n` evicted values; its
    /// index was reset to the oldest buffered value.
    Lagged(u64),
    /// `RecvError::Closed`: every sender handle has been dropped.
    Closed,
}

/// One `Receiver<T>` handle. `next` is the seq of the next value it will
/// receive (values are numbered from 1). `lagged_total` accumulates every
/// missed value; `error` holds the pending `Lagged`/`Closed` outcome.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Receiver {
    pub id: u64,
    pub next: u64,
    pub lagged_total: u64,
    pub last: Option<Value>,
    pub error: Option<RecvError>,
    /// Parked in `recv()`: completes on the next send.
    pub waiting: bool,
}

/// What one `recv()` step did.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Received {
    Value(char),
    Lagged(u64),
    Closed,
    /// Nothing new: the receiver waits for the next send.
    Waiting,
}

/// A value flying from its sender into the buffer tail. Pure cosmetics: the
/// view keys the animation on `key` and lands it when the animation ends.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Flight {
    pub key: u64,
    pub value: Value,
}

pub struct BroadcastSim {
    core: BroadcastCore<Value>,
    /// Live sender handles, in the order they appeared.
    senders: Vec<u64>,
    /// Receiver id -> core slot id. Core slots are recycled on drop, so
    /// receiver ids (which the view renders and keys nodes by) are
    /// allocated separately and never reused.
    slots: BTreeMap<u64, usize>,
    receivers: Vec<Receiver>,
    next_receiver: u64,
    next_sender: u64,
    flights: Vec<Flight>,
    /// A refused send, said out loud — or the game looks broken: a send
    /// goes out and nothing anywhere moves.
    refused: Option<u64>,
    /// Keys for flights and the notice: one counter, so none collide.
    next_key: u64,
}

impl Default for BroadcastSim {
    fn default() -> Self {
        Self::new()
    }
}

impl BroadcastSim {
    /// A channel and its original sender, with no receivers yet.
    pub fn new() -> Self {
        Self {
            core: BroadcastCore::new(CAPACITY),
            senders: vec![ORIGINAL],
            slots: BTreeMap::new(),
            receivers: Vec::new(),
            next_receiver: 1,
            next_sender: ORIGINAL + 1,
            flights: Vec::new(),
            refused: None,
            next_key: 0,
        }
    }

    pub fn senders(&self) -> &[u64] {
        &self.senders
    }

    pub fn receivers(&self) -> &[Receiver] {
        &self.receivers
    }

    /// The retained values, oldest first.
    pub fn buffer(&self) -> Vec<Value> {
        self.core.ring().map(|(_, value)| *value).collect()
    }

    /// How many values were ever accepted. The buffer holds the last
    /// `min(tail, CAPACITY)` of them.
    pub fn tail(&self) -> u64 {
        self.core.next_seq()
    }

    /// Every sender is gone.
    pub fn closed(&self) -> bool {
        self.senders.is_empty()
    }

    pub fn can_subscribe(&self) -> bool {
        self.receivers.len() < MAX_RX
    }

    pub fn flights(&self) -> &[Flight] {
        &self.flights
    }

    /// The key of a refused send still worth announcing.
    pub fn refused(&self) -> Option<u64> {
        self.refused
    }

    /// A flight's animation ended.
    pub fn land(&mut self, key: u64) {
        self.flights.retain(|f| f.key != key);
    }

    /// The refusal has been on screen long enough. Keyed, so clearing an
    /// old one cannot wipe a newer one.
    pub fn clear_refused(&mut self, key: u64) {
        if self.refused == Some(key) {
            self.refused = None;
        }
    }

    fn key(&mut self) -> u64 {
        self.next_key += 1;
        self.next_key
    }

    pub fn contains_sender(&self, handle: u64) -> bool {
        self.senders.contains(&handle)
    }

    /// `tx.clone()`. The new handle, if `source` is live.
    pub fn clone_sender(&mut self, source: u64) -> Option<u64> {
        if !self.contains_sender(source) {
            return None;
        }
        let handle = self.next_sender;
        self.next_sender += 1;
        self.senders.push(handle);
        self.core.add_sender();
        Some(handle)
    }

    /// `drop(tx)`. Emptying the senders closes the channel.
    pub fn drop_sender(&mut self, handle: u64) {
        if !self.contains_sender(handle) {
            return;
        }
        self.senders.retain(|h| *h != handle);
        self.core.drop_sender();
    }

    /// `tx.subscribe()`: a new receiver at the tail. It sees every value
    /// sent from now on, and none of the history still in the buffer. Every
    /// call yields a fresh, independent receiver.
    pub fn subscribe(&mut self) -> Option<u64> {
        if !self.can_subscribe() {
            return None;
        }
        let id = self.next_receiver;
        self.next_receiver += 1;
        let slot = self.core.subscribe();
        self.slots.insert(id, slot);
        self.receivers.push(Receiver {
            id,
            next: self.core.next_seq() + 1,
            lagged_total: 0,
            last: None,
            error: None,
            waiting: false,
        });
        Some(id)
    }

    /// `drop(rx)`. The core slot may be handed to a future subscriber, so
    /// the mapping goes too.
    pub fn drop_receiver(&mut self, id: u64) {
        if let Some(slot) = self.slots.remove(&id) {
            self.core.drop_receiver(slot);
        }
        self.receivers.retain(|r| r.id != id);
    }

    /// `tx.send(ch)` from `handle`: appended at the tail, a full buffer
    /// evicts its oldest value, and parked receivers complete. Whether it
    /// was accepted; with no receiver left the value comes straight back.
    pub fn send(&mut self, handle: u64, ch: char) -> bool {
        if !self.contains_sender(handle) {
            return false;
        }
        let value = Value { ch, from: handle };
        if self.core.send(value).is_err() {
            let key = self.key();
            self.refused = Some(key);
            return false;
        }
        let key = self.key();
        self.flights.push(Flight { key, value });
        if self.flights.len() > MAX_FLIGHTS {
            self.flights.remove(0);
        }
        let waiting: Vec<u64> = self
            .receivers
            .iter()
            .filter(|r| r.waiting)
            .map(|r| r.id)
            .collect();
        for id in waiting {
            self.recv(id);
        }
        true
    }

    /// One `recv()` step for a receiver: a value, a lag report, closure, or
    /// parking it to complete on the next send.
    pub fn recv(&mut self, id: u64) -> Option<Received> {
        let slot = *self.slots.get(&id)?;
        let poll = self.core.poll_recv(slot);
        let cursor = self.core.receiver_seq(slot);
        let rx = self.receivers.iter_mut().find(|r| r.id == id)?;
        if let Some(cursor) = cursor {
            rx.next = cursor + 1;
        }
        rx.waiting = false;
        Some(match poll {
            BroadcastPoll::Value(value) => {
                rx.last = Some(value);
                rx.error = None;
                Received::Value(value.ch)
            }
            BroadcastPoll::Lagged { skipped } => {
                rx.lagged_total += skipped;
                rx.error = Some(RecvError::Lagged(skipped));
                Received::Lagged(skipped)
            }
            BroadcastPoll::Closed => {
                rx.error = Some(RecvError::Closed);
                Received::Closed
            }
            BroadcastPoll::Empty { .. } => {
                rx.waiting = true;
                Received::Waiting
            }
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rx(sim: &BroadcastSim, id: u64) -> Receiver {
        *sim.receivers().iter().find(|r| r.id == id).unwrap()
    }

    fn subscribe(sim: &mut BroadcastSim) -> u64 {
        sim.subscribe().expect("under the cap")
    }

    fn chars(sim: &BroadcastSim) -> Vec<char> {
        sim.buffer().iter().map(|v| v.ch).collect()
    }

    #[test]
    fn the_original_sender_exists_and_clones_are_new_handles() {
        let mut sim = BroadcastSim::new();
        assert!(sim.contains_sender(ORIGINAL));
        assert_eq!(sim.clone_sender(ORIGINAL), Some(1));
        assert!(sim.contains_sender(1));
        // a clone of a non-existent handle is ignored
        assert_eq!(sim.clone_sender(999), None);
    }

    #[test]
    fn send_never_blocks_and_evicts_oldest() {
        let mut sim = BroadcastSim::new();
        // tokio's send fails when no receiver is left, so the channel needs
        // one before values can flow
        subscribe(&mut sim);
        for ch in ('a'..).take(CAPACITY + 2) {
            assert!(sim.send(ORIGINAL, ch));
        }
        // 'a' and 'b' were evicted, in order
        assert_eq!(chars(&sim), ['c', 'd', 'e', 'f', 'g']);
        assert_eq!(sim.tail(), 7);
        assert_eq!(sim.flights().len(), 7, "every accepted send flies");
    }

    /// tokio's `subscribe` starts a receiver at the tail: it sees only what
    /// is sent afterwards, never the history still held in the ring.
    #[test]
    fn subscribe_mid_stream_starts_at_the_tail() {
        let mut sim = BroadcastSim::new();
        subscribe(&mut sim);
        for ch in 'a'..'d' {
            sim.send(ORIGINAL, ch);
        }
        let late = subscribe(&mut sim);
        assert_eq!(rx(&sim, late).next, 4, "past every buffered value");

        // nothing to read yet: the recv parks and waits for the next send
        assert_eq!(sim.recv(late), Some(Received::Waiting));
        assert!(rx(&sim, late).waiting);

        // the next send reaches it, and the buffered history never does
        sim.send(ORIGINAL, 'z');
        assert_eq!(rx(&sim, late).last.map(|v| v.ch), Some('z'));
        assert!(!rx(&sim, late).waiting);
    }

    #[test]
    fn recv_advances_index_and_reports_last_value() {
        let mut sim = BroadcastSim::new();
        let r = subscribe(&mut sim);
        for ch in 'a'..'d' {
            sim.send(ORIGINAL, ch);
        }
        assert_eq!(sim.recv(r), Some(Received::Value('a')));
        assert_eq!(rx(&sim, r).last, Some(Value { ch: 'a', from: 0 }));
        assert_eq!(rx(&sim, r).next, 2);

        assert_eq!(sim.recv(r), Some(Received::Value('b')));
        assert_eq!(rx(&sim, r).next, 3);
    }

    /// Lag arises the way it does in tokio: subscribe at the tail, then fall
    /// behind while the ring overwrites what you have not read.
    #[test]
    fn recv_yields_lagged_and_resumes_at_oldest() {
        let mut sim = BroadcastSim::new();
        let r = subscribe(&mut sim);

        // capacity + 3 sends while the receiver reads nothing: the three
        // oldest values are evicted out from under it
        for ch in ('a'..).take(CAPACITY + 3) {
            sim.send(ORIGINAL, ch);
        }

        assert_eq!(sim.recv(r), Some(Received::Lagged(3)));
        assert_eq!(rx(&sim, r).lagged_total, 3);
        assert_eq!(rx(&sim, r).error, Some(RecvError::Lagged(3)));

        // the next receive resumes at the oldest retained value, error cleared
        assert_eq!(sim.recv(r), Some(Received::Value('d')));
        assert!(rx(&sim, r).error.is_none());
    }

    #[test]
    fn a_waiting_recv_completes_on_next_send() {
        let mut sim = BroadcastSim::new();
        let r = subscribe(&mut sim);

        // nothing new to receive: the recv waits
        assert_eq!(sim.recv(r), Some(Received::Waiting));
        assert!(rx(&sim, r).waiting);

        // the next send completes it
        sim.send(ORIGINAL, 'x');
        let state = rx(&sim, r);
        assert!(!state.waiting);
        assert_eq!(
            state.next, 2,
            "parked at tail+1=1, received seq 1, next is 2"
        );
        assert_eq!(state.last, Some(Value { ch: 'x', from: 0 }));
    }

    #[test]
    fn every_subscribe_call_yields_an_independent_receiver() {
        let mut sim = BroadcastSim::new();
        // subscribe() is a method on Sender: calling it twice yields two
        // peers, both starting at the tail
        let first = subscribe(&mut sim);
        let second = subscribe(&mut sim);
        assert_ne!(first, second);
        assert_eq!(rx(&sim, first).next, rx(&sim, second).next);

        // fan-out: one send is deliverable to both, each at its own pace
        sim.send(ORIGINAL, 'a');
        assert_eq!(sim.recv(first), Some(Received::Value('a')));
        assert!(
            rx(&sim, second).last.is_none(),
            "the other has not recv()d yet"
        );
        assert_eq!(sim.recv(second), Some(Received::Value('a')));
    }

    #[test]
    fn the_receiver_cap_holds() {
        let mut sim = BroadcastSim::new();
        for _ in 0..MAX_RX {
            subscribe(&mut sim);
        }
        assert_eq!(sim.subscribe(), None);
        assert_eq!(sim.receivers().len(), MAX_RX);
    }

    #[test]
    fn dropping_a_receiver_removes_it() {
        let mut sim = BroadcastSim::new();
        let r = subscribe(&mut sim);
        sim.drop_receiver(r);
        assert!(sim.receivers().is_empty());
        assert_eq!(sim.recv(r), None, "a dropped receiver cannot recv");
    }

    /// A send with no receivers fails, like tokio's — and says so, or the
    /// game looks broken rather than strict.
    #[test]
    fn send_without_receivers_is_refused_out_loud() {
        let mut sim = BroadcastSim::new();
        assert!(!sim.send(ORIGINAL, 'a'));
        assert!(
            sim.refused().is_some(),
            "the refusal is announced, not silent"
        );
        assert!(sim.buffer().is_empty());
        assert!(sim.flights().is_empty(), "nothing flies");

        // with a receiver attached the same send lands
        subscribe(&mut sim);
        assert!(sim.send(ORIGINAL, 'a'));
    }

    /// Regression: closing used to be checked before the buffer, so a
    /// receiver holding unread values got `Closed` and the values were lost.
    /// tokio drains what is retained first and only then reports closure.
    #[test]
    fn closed_channel_drains_buffered_values_first() {
        let mut sim = BroadcastSim::new();
        let r = subscribe(&mut sim);
        sim.send(ORIGINAL, 'a');
        sim.send(ORIGINAL, 'b');

        sim.drop_sender(ORIGINAL);
        assert!(sim.closed());

        // the buffered values are still owed to the receiver
        assert_eq!(sim.recv(r), Some(Received::Value('a')));
        assert_eq!(sim.recv(r), Some(Received::Value('b')));

        // only once drained does the channel report itself closed
        assert_eq!(sim.recv(r), Some(Received::Closed));
        assert_eq!(rx(&sim, r).error, Some(RecvError::Closed));
    }

    #[test]
    fn the_channel_closes_only_when_the_last_sender_drops() {
        let mut sim = BroadcastSim::new();
        let clone = sim.clone_sender(ORIGINAL).unwrap();
        let r = subscribe(&mut sim);

        sim.drop_sender(ORIGINAL);
        assert!(!sim.closed(), "the clone keeps it open");
        assert!(sim.send(clone, 'a'));

        sim.drop_sender(clone);
        assert!(sim.closed());
        assert_eq!(sim.recv(r), Some(Received::Value('a')));
        assert_eq!(sim.recv(r), Some(Received::Closed));
    }

    #[test]
    fn a_refusal_is_cleared_only_by_its_own_key() {
        let mut sim = BroadcastSim::new();
        sim.send(ORIGINAL, 'a');
        let old = sim.refused().unwrap();
        sim.send(ORIGINAL, 'b');
        let new = sim.refused().unwrap();
        sim.clear_refused(old);
        assert_eq!(sim.refused(), Some(new), "the newer survives");
        sim.clear_refused(new);
        assert_eq!(sim.refused(), None);
    }

    /// The broadcast beats composed into one channel lifetime: a receiver
    /// that reads as the stream flows, a mid-stream subscriber, waiting
    /// recvs completing on the next send, lag and resume, a send that is
    /// visibly refused once nobody listens, and closure with retained
    /// values still readable until drained.
    #[test]
    fn broadcast_journey_composes_the_beats_in_one_channel() {
        let mut sim = BroadcastSim::new();
        let early = subscribe(&mut sim);

        // early reads as the stream flows and stays caught up
        for ch in 'a'..='c' {
            sim.send(ORIGINAL, ch);
            assert_eq!(sim.recv(early), Some(Received::Value(ch)));
        }

        // a late subscriber starts at the tail: history is not retroactive
        let late = subscribe(&mut sim);
        assert_eq!(rx(&sim, late).next, 4, "past the three sent values");

        // nothing new for either: recv waits; the next send completes both
        assert_eq!(sim.recv(early), Some(Received::Waiting));
        assert_eq!(sim.recv(late), Some(Received::Waiting));
        sim.send(ORIGINAL, 'd');
        assert_eq!(rx(&sim, early).last.map(|v| v.ch), Some('d'));
        assert_eq!(rx(&sim, late).last.map(|v| v.ch), Some('d'));

        // late stops reading while early keeps up; the ring overwrites
        // late's unread values
        for ch in 'e'..='k' {
            sim.send(ORIGINAL, ch);
            assert_eq!(sim.recv(early), Some(Received::Value(ch)));
        }
        assert_eq!(sim.recv(late), Some(Received::Lagged(2)));
        assert_eq!(
            sim.recv(late),
            Some(Received::Value('g')),
            "resume at the oldest retained value"
        );

        // the last listener leaves: the send comes back with a visible
        // refusal, and the stream position does not move
        sim.drop_receiver(early);
        sim.drop_receiver(late);
        assert!(!sim.send(ORIGINAL, 'l'));
        assert!(sim.refused().is_some());

        // a fresh subscriber starts at the unchanged tail, the channel
        // flows again, and then the last sender goes away
        let fresh = subscribe(&mut sim);
        assert_eq!(rx(&sim, fresh).next, 12, "the refused send never counted");
        sim.send(ORIGINAL, 'l');
        sim.send(ORIGINAL, 'm');
        sim.drop_sender(ORIGINAL);

        // retained values stay readable until drained, then closure shows
        assert_eq!(sim.recv(fresh), Some(Received::Value('l')));
        assert_eq!(sim.recv(fresh), Some(Received::Value('m')));
        assert_eq!(sim.recv(fresh), Some(Received::Closed));
    }

    /// Differential: a fresh subscriber must see the same values in the sim
    /// as in the bare core — notably none of the history already in the
    /// ring. The sim used to be a hand-written model of its own, and it
    /// drifted (it lost buffered values on close, and `subscribe` replayed
    /// history); this keeps it a thin adapter.
    #[test]
    fn the_sim_delivers_exactly_what_the_core_does() {
        let sent = ['a', 'b', 'c', 'd', 'e', 'f', 'g'];

        let mut core: BroadcastCore<char> = BroadcastCore::new(CAPACITY);
        let _seed = core.subscribe();
        let mut sim = BroadcastSim::new();
        subscribe(&mut sim);

        // both models take the same three sends
        for ch in sent.iter().take(3) {
            core.send(*ch).expect("receiver alive");
            sim.send(ORIGINAL, *ch);
        }

        // then both gain a late subscriber, at the same point in the stream
        let late = core.subscribe();
        let r = subscribe(&mut sim);

        // compare cursors before either side reads: this is where a
        // subscribe-at-oldest regression shows, and it disappears again once
        // both have drained the ring
        assert_eq!(
            rx(&sim, r).next,
            core.receiver_seq(late).expect("live receiver") + 1,
            "a fresh subscriber must start where the core puts it"
        );

        for ch in sent.iter().skip(3) {
            core.send(*ch).expect("receiver alive");
            sim.send(ORIGINAL, *ch);
        }

        let mut core_got = Vec::new();
        while let BroadcastPoll::Value(value) = core.poll_recv(late) {
            core_got.push(value);
        }
        let mut sim_got = Vec::new();
        while let Some(Received::Value(ch)) = sim.recv(r) {
            sim_got.push(ch);
        }
        assert_eq!(
            sim_got, core_got,
            "a late subscriber must see the same values in both models"
        );

        // What the view draws must agree with the core too: `next` is drawn
        // as "next #N" and positions the index line, so a cursor the core
        // does not actually hold is a lie on screen.
        assert_eq!(
            rx(&sim, r).next,
            core.receiver_seq(late).expect("live receiver") + 1,
            "the drawn cursor must be the core's, mapped 0-based to 1-based"
        );
        assert_eq!(
            chars(&sim),
            core.ring().map(|(_, value)| *value).collect::<Vec<_>>(),
            "the drawn buffer must be the core's ring"
        );
    }
}
