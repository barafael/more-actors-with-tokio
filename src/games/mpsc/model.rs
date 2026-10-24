//! The bounded mpsc channel the mpsc game draws: sender handles, buffered
//! values, values in flight, and parked sends.
//!
//! Channel truth lives in [`MpscCore`]; this adds the handle registry, the
//! flight clock and the outstanding receives.

use std::collections::{BTreeMap, VecDeque};

use sim_channels::mpsc::{MpscCore, RecvPoll, SendOffer, SendPoll};
use sim_channels::WaiterId;

use crate::clock::Ticking;

pub const CAPACITY: usize = 5;

/// How long a value spends in flight, in milliseconds: the sim schedules
/// landings with it and the stylesheet animates over the same span.
pub const FLIGHT_MS: f64 = 700.0;

/// The handle `mpsc::channel` hands out; every other one is a clone.
pub const ORIGINAL: u64 = 0;

/// A char in the channel, tagged with the handle that sent it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Value {
    pub ch: char,
    pub from: u64,
}

/// One live `Sender<T>` handle, and since when its send has been parked on a
/// full queue. There is no timeout: a parked send waits until a receive
/// frees a slot, however long that takes.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Sender {
    pub handle: u64,
    pub parked_since: Option<f64>,
}

/// A value on its way from its sender to the channel.
///
/// `key` is what identifies a flight: two identical chars sent from one
/// handle are distinct flights, and the view keys their animations on it.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Flight {
    pub key: u64,
    pub value: Value,
    due: f64,
}

/// A value is "in flight" while its animation plays. Flights already occupy
/// channel capacity — the core buffers the value at send time and the flight
/// is pure cosmetics on top — so a full channel blocks the next sender even
/// while values are still mid-air, exactly as `Sender::send` would.
///
/// Parked senders wake in FIFO order, like tokio's.
#[derive(Debug)]
pub struct MpscSim {
    core: MpscCore<Value>,
    /// Live handles, in the order they appeared.
    senders: Vec<u64>,
    /// Parked sends, with the value each carries (so a woken send can fly
    /// with the right char).
    parked: Vec<(WaiterId, Value)>,
    /// When each parked handle's send parked, on this sim's clock.
    parked_at: BTreeMap<u64, f64>,
    /// Identity of the single consumer while it is parked in `recv()`.
    waiting: Option<WaiterId>,
    /// How many `recv()` calls are outstanding. The game lets the player
    /// click Receive repeatedly; each click is a separate awaited recv, and
    /// every one of them must eventually be served.
    pending_receives: usize,
    flights: VecDeque<Flight>,
    next_flight: u64,
    next_handle: u64,
    last_received: Option<Value>,
    flight_ms: f64,
    now: f64,
}

impl MpscSim {
    /// A channel and its original sender.
    pub fn new(flight_ms: f64) -> Self {
        Self {
            // the core counts the sender `channel()` returns: that is ORIGINAL
            core: MpscCore::new(CAPACITY),
            senders: vec![ORIGINAL],
            parked: Vec::new(),
            parked_at: BTreeMap::new(),
            waiting: None,
            pending_receives: 0,
            flights: VecDeque::new(),
            next_flight: 0,
            next_handle: ORIGINAL + 1,
            last_received: None,
            flight_ms,
            now: 0.0,
        }
    }

    pub fn senders(&self) -> Vec<Sender> {
        self.senders
            .iter()
            .map(|&handle| Sender {
                handle,
                parked_since: self.parked_at.get(&handle).copied(),
            })
            .collect()
    }

    /// The values that have landed in the buffer, oldest first.
    ///
    /// A value in flight already occupies a buffer slot in the core, but the
    /// view draws it mid-air; hide the newest `in_flight` values so each is
    /// shown — and counted — exactly once.
    pub fn buffer(&self) -> Vec<Value> {
        let buffered: Vec<Value> = self.core.buffer().copied().collect();
        let settled = buffered.len().saturating_sub(self.flights.len());
        buffered[..settled].to_vec()
    }

    /// Sends parked waiting for capacity, in FIFO wake order.
    pub fn parked_sends(&self) -> Vec<Value> {
        self.core.blocked().map(|(_, value)| *value).collect()
    }

    pub fn flights(&self) -> impl Iterator<Item = &Flight> {
        self.flights.iter()
    }

    /// Slots taken, counting values still in the air.
    pub fn occupancy(&self) -> usize {
        self.core.buffer().count()
    }

    pub fn waiting_receive(&self) -> bool {
        self.pending_receives > 0
    }

    pub fn last_received(&self) -> Option<Value> {
        self.last_received
    }

    /// Every sender is gone: once the buffer drains, `recv()` yields `None`.
    pub fn closed(&self) -> bool {
        self.senders.is_empty()
    }

    pub fn contains_sender(&self, handle: u64) -> bool {
        self.senders.contains(&handle)
    }

    /// `tx.clone()`: a new handle, its own sender. Clones are peers, not
    /// children — nothing about one is shared with or owned by the handle
    /// it came from. The new handle, if `source` is live.
    pub fn clone_sender(&mut self, source: u64) -> Option<u64> {
        if !self.contains_sender(source) {
            return None;
        }
        let handle = self.next_handle;
        self.next_handle += 1;
        self.senders.push(handle);
        self.core.add_sender();
        Some(handle)
    }

    /// `drop(tx)`: a send it had parked is cancelled, handing the value back
    /// the way a dropped `send` future would.
    pub fn drop_sender(&mut self, handle: u64) {
        if !self.contains_sender(handle) {
            return;
        }
        self.senders.retain(|h| *h != handle);
        self.parked_at.remove(&handle);
        let cancelled: Vec<WaiterId> = self
            .parked
            .iter()
            .filter(|(_, value)| value.from == handle)
            .map(|(waiter, _)| *waiter)
            .collect();
        for waiter in cancelled {
            self.core.cancel_send(waiter);
            self.parked.retain(|(parked, _)| *parked != waiter);
        }
        self.core.drop_sender();
        self.settle();
    }

    /// `tx.send(ch).await` from `handle`. Whether the value took off at
    /// once; otherwise the send is parked on the full queue.
    pub fn send(&mut self, handle: u64, ch: char) -> bool {
        if !self.contains_sender(handle) {
            return false;
        }
        let value = Value { ch, from: handle };
        match self.core.offer_send(value) {
            SendOffer::Accepted => {
                self.take_off(value);
                true
            }
            SendOffer::Blocked { waiter } => {
                self.parked.push((waiter, value));
                self.parked_at.entry(handle).or_insert(self.now);
                false
            }
            // the receiver never drops in this game
            SendOffer::Rejected(_) => false,
        }
    }

    /// One awaited `rx.recv()`. Queuing them means a second receive on an
    /// empty channel is still owed a value instead of being swallowed by a
    /// single "waiting" flag.
    pub fn receive(&mut self) {
        self.pending_receives += 1;
        self.settle();
    }

    fn take_off(&mut self, value: Value) {
        self.flights.push_back(Flight {
            key: self.next_flight,
            value,
            due: self.now + self.flight_ms,
        });
        self.next_flight += 1;
    }

    /// Re-check everything parked after a state change: woken sends take off,
    /// and a receiver blocked on an empty channel takes the next value.
    fn settle(&mut self) {
        // Receives and parked sends feed each other: a served receive frees
        // a slot, which wakes a parked send, whose value a later receive can
        // take. Iterate until neither side moves.
        loop {
            self.drain_receives();
            if !self.wake_parked_sends() {
                break;
            }
        }
    }

    /// Re-check parked sends. The core wakes them in FIFO order, so a value
    /// parked before another always lands first. Whether any woke.
    fn wake_parked_sends(&mut self) -> bool {
        let mut woke = false;
        for (waiter, value) in self.parked.clone() {
            let outcome = self.core.poll_send(waiter);
            if matches!(outcome, SendPoll::Pending) {
                continue;
            }
            woke = true;
            self.parked.retain(|(parked, _)| *parked != waiter);
            if !self.parked.iter().any(|(_, v)| v.from == value.from) {
                self.parked_at.remove(&value.from);
            }
            // an accepted send flies like any other; the value is already in
            // the buffer it will appear to land in
            if matches!(outcome, SendPoll::Accepted) {
                self.take_off(value);
            }
        }
        woke
    }

    /// Serve as many outstanding `recv()` calls as there are values. The
    /// consumer is single, so at most one can be parked in the core at a
    /// time; the rest stay owed and are served as values arrive.
    fn drain_receives(&mut self) {
        while self.pending_receives > 0 {
            let poll = match self.waiting {
                Some(waiter) => self.core.poll_recv_wait(waiter),
                None => self.core.poll_recv(),
            };
            match poll {
                RecvPoll::Value(value) => {
                    self.waiting = None;
                    self.pending_receives -= 1;
                    self.last_received = Some(value);
                }
                RecvPoll::Empty { waiter } => {
                    self.waiting = Some(waiter);
                    break;
                }
                RecvPoll::Disconnected => {
                    self.waiting = None;
                    self.pending_receives = 0;
                    break;
                }
            }
        }
    }
}

/// The mpsc game is clock-driven: its flights land on a deadline.
///
/// This is the case the [`Ticking`] contract was modelled on — the three
/// methods below were inherent methods here first, and the trait exists to
/// state their shape once rather than to describe only the newer games.
impl Ticking for MpscSim {
    /// A flight that landed.
    type Event = Flight;

    fn sync_now(&mut self, now_ms: f64) {
        self.now = now_ms;
    }

    fn next_delay_ms(&self) -> Option<f64> {
        self.flights
            .front()
            .map(|flight| (flight.due - self.now).max(0.0))
    }

    /// Land every flight whose deadline has passed. Landing is cosmetic: the
    /// value already sits in the channel, so this only ends the animation and
    /// lets a waiting receiver pick the value up.
    fn poll_due(&mut self) -> Vec<Flight> {
        let mut landed = Vec::new();
        while let Some(flight) = self.flights.front().filter(|f| f.due <= self.now) {
            landed.push(*flight);
            self.flights.pop_front();
        }
        if !landed.is_empty() {
            self.settle();
        }
        landed
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const FLIGHT: f64 = 100.0;

    /// The original sender (0) and one clone (1).
    fn sim_with_two_senders() -> MpscSim {
        let mut sim = MpscSim::new(FLIGHT);
        assert_eq!(sim.clone_sender(ORIGINAL), Some(1));
        sim
    }

    fn land(sim: &mut MpscSim) -> Vec<char> {
        let now = sim.now + FLIGHT;
        sim.tick(now).iter().map(|f| f.value.ch).collect()
    }

    fn flying(sim: &MpscSim) -> Vec<char> {
        sim.flights().map(|f| f.value.ch).collect()
    }

    fn fill(sim: &mut MpscSim, handle: u64) {
        for ch in ('a'..).take(CAPACITY) {
            sim.send(handle, ch);
        }
        land(sim);
    }

    fn parked(sim: &MpscSim, handle: u64) -> bool {
        sim.senders()
            .iter()
            .any(|s| s.handle == handle && s.parked_since.is_some())
    }

    #[test]
    fn send_flies_then_lands_into_buffer() {
        let mut sim = sim_with_two_senders();
        assert!(sim.send(0, 'a'));
        assert_eq!(flying(&sim), ['a']);
        // not landed yet
        assert!(sim.poll_due().is_empty());
        assert!(sim.buffer().is_empty());
        assert_eq!(sim.occupancy(), 1, "a value in the air holds its slot");

        assert_eq!(land(&mut sim), ['a']);
        assert_eq!(sim.buffer(), [Value { ch: 'a', from: 0 }]);
        assert!(flying(&sim).is_empty());
    }

    #[test]
    fn full_buffer_blocks_sender_until_a_receive_wakes_it() {
        let mut sim = sim_with_two_senders();
        for (i, ch) in ('a'..).take(CAPACITY).enumerate() {
            sim.send((i % 2) as u64, ch);
        }
        assert_eq!(land(&mut sim).len(), CAPACITY);
        assert_eq!(sim.buffer().len(), CAPACITY);

        // the sixth send parks: nothing flies, the handle shows it
        assert!(!sim.send(0, 'z'));
        assert_eq!(sim.parked_sends(), [Value { ch: 'z', from: 0 }]);
        assert!(parked(&sim, 0));

        // a receive frees a slot and wakes exactly one parked send
        sim.receive();
        assert_eq!(flying(&sim), ['z']);
        assert_eq!(land(&mut sim).len(), 1, "one woken send lands");
        assert!(sim.parked_sends().is_empty());
        assert!(!parked(&sim, 0));
        assert_eq!(sim.buffer().len(), CAPACITY);
        assert!(sim.last_received().is_some());
    }

    #[test]
    fn unblock_wake_order_is_fifo() {
        let mut sim = MpscSim::new(FLIGHT);
        let one = sim.clone_sender(ORIGINAL).unwrap();
        let two = sim.clone_sender(ORIGINAL).unwrap();
        fill(&mut sim, ORIGINAL);

        // two sends park, in this order
        sim.send(one, 'x');
        sim.send(two, 'y');
        assert_eq!(
            sim.parked_sends().iter().map(|v| v.ch).collect::<Vec<_>>(),
            ['x', 'y'],
        );

        // tokio wakes parked senders in park order: 'x' before 'y'
        let mut woken = Vec::new();
        for _ in 0..2 {
            // the wake happens inside the receive that frees the slot
            sim.receive();
            woken.extend(flying(&sim));
            land(&mut sim);
        }
        assert_eq!(woken, ['x', 'y'], "parked sends wake FIFO, never randomly");
    }

    #[test]
    fn receive_on_empty_channel_waits_and_bypasses_buffer() {
        let mut sim = sim_with_two_senders();
        sim.receive();
        assert!(sim.waiting_receive());

        sim.send(0, 'a');
        assert_eq!(land(&mut sim), ['a']);
        assert!(
            sim.buffer().is_empty(),
            "waiting receiver takes it directly"
        );
        assert!(!sim.waiting_receive());
        assert_eq!(sim.last_received(), Some(Value { ch: 'a', from: 0 }));
    }

    /// Regression: a second `recv()` while one is already parked used to
    /// overwrite a single `waiting` flag, so one of the two receives was
    /// silently lost. The core keeps the waiter, so both are served.
    #[test]
    fn concurrent_receives_are_queued_not_dropped() {
        let mut sim = sim_with_two_senders();
        sim.receive();
        sim.receive();
        assert!(sim.waiting_receive());

        sim.send(0, 'a');
        sim.send(0, 'b');
        land(&mut sim);

        // 'a' goes to the receive that was parked first (FIFO); 'b' is
        // taken by the second, which the old single-flag sim had discarded.
        assert_eq!(sim.last_received(), Some(Value { ch: 'b', from: 0 }));
        assert!(
            sim.buffer().is_empty(),
            "neither value is stranded behind a lost receive"
        );
        assert!(!sim.waiting_receive(), "both receives completed");
    }

    #[test]
    fn clone_requires_a_live_handle() {
        let mut sim = sim_with_two_senders();
        assert_eq!(sim.clone_sender(1), Some(2));
        assert!(sim.contains_sender(2));
        assert_eq!(sim.clone_sender(99), None);
    }

    #[test]
    fn a_parked_send_counts_its_wait_and_never_times_out() {
        let mut sim = sim_with_two_senders();
        fill(&mut sim, 0);
        sim.sync_now(1_000.0);
        sim.send(1, 'z');
        assert_eq!(
            sim.senders()[1].parked_since,
            Some(1_000.0),
            "the wait starts at the send"
        );

        // nothing frees a parked sender but a receive
        sim.sync_now(3_500.0);
        assert!(sim.poll_due().is_empty());
        assert!(parked(&sim, 1));

        // a receive frees a slot, the parked send takes off, the clock stops
        sim.receive();
        assert_eq!(sim.senders()[1].parked_since, None);
    }

    #[test]
    fn a_clone_is_an_autonomous_sender() {
        let mut sim = sim_with_two_senders();
        let clone = sim.clone_sender(1).unwrap();

        // a clone is a peer, not a child: dropping the handle it was
        // cloned from leaves it untouched
        sim.drop_sender(1);
        assert!(sim.contains_sender(clone));
        assert!(!sim.contains_sender(1));

        // it sends under its own identity
        assert!(sim.send(clone, 'x'));

        // and parks under its own identity ('x' already took one slot, so
        // CAPACITY - 1 fills the buffer and the clone's next send parks)
        for ch in ('a'..).take(CAPACITY - 1) {
            sim.send(0, ch);
        }
        land(&mut sim);
        sim.send(clone, 'y');
        assert!(parked(&sim, clone));
        assert_eq!(
            sim.parked_sends(),
            [Value {
                ch: 'y',
                from: clone
            }]
        );
    }

    #[test]
    fn dropping_a_sender_cancels_its_parked_send() {
        let mut sim = sim_with_two_senders();
        fill(&mut sim, 0);
        sim.send(1, 'z');
        assert_eq!(sim.parked_sends().len(), 1);

        sim.drop_sender(1);
        assert!(sim.parked_sends().is_empty(), "parked send discarded");
        assert!(!sim.contains_sender(1));
    }

    #[test]
    fn dropping_the_last_sender_closes_the_channel_after_it_drains() {
        let mut sim = sim_with_two_senders();
        sim.send(0, 'a');
        land(&mut sim);
        sim.drop_sender(0);
        sim.drop_sender(1);
        assert!(sim.closed());

        // what was sent before the close is still delivered
        sim.receive();
        assert_eq!(sim.last_received(), Some(Value { ch: 'a', from: 0 }));
        // and then recv() yields None rather than waiting forever
        sim.receive();
        assert!(
            !sim.waiting_receive(),
            "a closed channel does not park recv()"
        );
    }

    #[test]
    fn a_receive_parked_on_an_empty_channel_ends_when_the_last_sender_drops() {
        let mut sim = MpscSim::new(FLIGHT);
        sim.receive();
        assert!(sim.waiting_receive());
        sim.drop_sender(ORIGINAL);
        assert!(!sim.waiting_receive(), "recv() returned None");
    }

    /// The beats composed into one channel lifetime: fill the buffer, park
    /// two sends, wake them FIFO, park two receives, serve both, then drop a
    /// sender whose send is still parked. Cross-beat state — queues that
    /// must drain completely before the channel behaves again — is what
    /// one-beat-per-test isolations cannot see.
    #[test]
    fn mpsc_journey_composes_the_beats_in_one_channel() {
        let mut sim = sim_with_two_senders();

        // fill the buffer and land every flight so occupancy is settled
        fill(&mut sim, 0);

        // two sends park holding their values, in park order
        sim.send(1, 'x');
        sim.send(1, 'y');
        assert_eq!(
            sim.parked_sends().iter().map(|v| v.ch).collect::<Vec<_>>(),
            ['x', 'y']
        );

        // each receive drains one buffered value and wakes one parked
        // send — FIFO, never random
        let mut woken = Vec::new();
        for _ in 0..2 {
            sim.receive();
            woken.extend(flying(&sim));
            land(&mut sim);
        }
        assert_eq!(woken, ['x', 'y']);
        assert!(sim.parked_sends().is_empty(), "queue drained");

        // drain what is left of the buffer: three originals never read
        // plus the two woken values landed into it
        for _ in 0..5 {
            sim.receive();
            land(&mut sim);
        }
        assert!(sim.buffer().is_empty(), "buffer fully drained");

        // two receives park on the drained channel and are served in
        // order, straight past the buffer
        sim.receive();
        sim.receive();
        assert!(sim.waiting_receive());
        sim.send(0, 'p');
        sim.send(0, 'q');
        land(&mut sim);
        assert_eq!(sim.last_received(), Some(Value { ch: 'q', from: 0 }));
        assert!(
            sim.buffer().is_empty(),
            "waiting receives bypass the buffer"
        );
        assert!(!sim.waiting_receive(), "both receives served");

        // refill and park once more; dropping the parked sender cancels its
        // send — the value never lands
        fill(&mut sim, 0);
        sim.send(1, 'z');
        assert_eq!(sim.parked_sends(), [Value { ch: 'z', from: 1 }]);

        sim.drop_sender(1);
        assert!(sim.parked_sends().is_empty(), "cancelled, not stranded");
        assert!(!sim.contains_sender(1));
        assert_eq!(
            sim.last_received(),
            Some(Value { ch: 'q', from: 0 }),
            "the cancelled value never arrived"
        );
    }
}

/// Differential tests: drive the game sim and the bare `sim-channels` core
/// through the same operation sequence and assert they still agree.
///
/// The sims used to be a second, hand-written model of the same channels,
/// and it drifted: mpsc woke parked senders at random. The sim is a thin
/// adapter now, and these tests are what keeps it thin — they fail if it
/// ever starts deciding channel behaviour for itself instead of asking the
/// core.
#[cfg(test)]
mod differential_tests {
    use super::*;

    #[derive(Clone, Copy, Debug)]
    enum Op {
        Send(char),
        Receive,
    }

    /// Every value a sequence of sends and receives hands to the consumer,
    /// in order, according to the core alone.
    fn core_deliveries(ops: &[Op]) -> Vec<char> {
        let mut core: MpscCore<char> = MpscCore::new(CAPACITY);
        core.add_sender();
        let mut parked = Vec::new();
        let mut owed = 0usize;
        let mut got = Vec::new();
        for op in ops {
            match op {
                Op::Send(ch) => match core.offer_send(*ch) {
                    SendOffer::Blocked { waiter } => parked.push(waiter),
                    SendOffer::Accepted | SendOffer::Rejected(_) => {}
                },
                Op::Receive => owed += 1,
            }
            // drain in the same order the sim does
            loop {
                let mut moved = false;
                while owed > 0 {
                    match core.poll_recv() {
                        RecvPoll::Value(value) => {
                            got.push(value);
                            owed -= 1;
                            moved = true;
                        }
                        RecvPoll::Empty { .. } | RecvPoll::Disconnected => break,
                    }
                }
                parked.retain(|waiter| matches!(core.poll_send(*waiter), SendPoll::Pending));
                if !moved {
                    break;
                }
            }
        }
        got
    }

    /// Run the same ops against the game sim, collecting what the receiver
    /// was handed.
    fn sim_deliveries(ops: &[Op]) -> Vec<char> {
        // zero flight time: landings are cosmetics and would only add delay
        let mut sim = MpscSim::new(0.0);
        let mut got = Vec::new();
        let mut last = None;
        for op in ops {
            match op {
                Op::Send(ch) => {
                    sim.send(ORIGINAL, *ch);
                }
                Op::Receive => sim.receive(),
            }
            sim.poll_due();
            let now = sim.last_received();
            if now != last {
                if let Some(value) = now {
                    got.push(value.ch);
                }
                last = now;
            }
        }
        got
    }

    #[test]
    fn the_sim_delivers_exactly_what_the_core_does() {
        // a sequence that fills the buffer, parks sends, and interleaves
        // receives so wake order is observable
        let mut ops = Vec::new();
        for ch in ('a'..).take(CAPACITY + 3) {
            ops.push(Op::Send(ch));
        }
        for _ in 0..3 {
            ops.push(Op::Receive);
        }
        ops.push(Op::Send('x'));
        ops.extend([Op::Receive; 3]);

        assert_eq!(
            sim_deliveries(&ops),
            core_deliveries(&ops),
            "the sim must not decide delivery order for itself"
        );
    }

    #[test]
    fn the_sim_matches_the_core_when_receives_run_ahead() {
        let ops = [
            Op::Receive,
            Op::Receive,
            Op::Send('a'),
            Op::Send('b'),
            Op::Receive,
            Op::Send('c'),
        ];
        assert_eq!(sim_deliveries(&ops), core_deliveries(&ops));
    }
}
