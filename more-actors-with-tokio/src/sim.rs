//! Pure game state machines shared by the server actors (multiplayer) and the
//! browser client (single-player). No tokio, no transport — logic only.
use std::collections::{BTreeMap, VecDeque};

#[cfg(not(target_arch = "wasm32"))]
use std::sync::OnceLock;
#[cfg(not(target_arch = "wasm32"))]
use std::time::Instant;

use sim_channels::broadcast::{BroadcastCore, BroadcastPoll};
use sim_channels::mpsc::{MpscCore, RecvPoll, SendOffer, SendPoll};
use sim_channels::oneshot::OneshotCore;
use sim_channels::WaiterId;

use crate::protocol::{
    BroadcastError, BroadcastEvent, BroadcastSnapshot, BroadcastWire, BufferChar, ButtonEvent,
    ButtonState, ButtonWire, MpscEvent, MpscSnapshot, MpscWire, RxInfo, RxState, SenderInfo,
    Color, WatchEvent, WatchSnapshot, WatchWire, BROADCAST_CAPACITY, MPSC_CAPACITY,
};

/// Connection id used by the local (single-player) client.
pub const LOCAL_CONN: u64 = 0;

/// Monotonic milliseconds, valid on native and wasm.
pub fn now_ms() -> f64 {
    #[cfg(target_arch = "wasm32")]
    {
        web_sys::window()
            .expect("window")
            .performance()
            .expect("performance")
            .now()
    }
    #[cfg(not(target_arch = "wasm32"))]
    {
        static EPOCH: OnceLock<Instant> = OnceLock::new();
        EPOCH.get_or_init(Instant::now).elapsed().as_secs_f64() * 1000.0
    }
}

/// The button future: a `oneshot` channel whose value is the colour that
/// resolved it. Activating creates the channel, pressing a button sends the
/// one value it will ever carry.
///
/// `state` is a projection of the core for `ButtonState` consumers; the
/// channel itself is the truth.
pub struct ButtonSim {
    pending: Option<OneshotCore<Color>>,
    pub state: ButtonState,
}

impl Default for ButtonSim {
    fn default() -> Self {
        Self::new()
    }
}

impl ButtonSim {
    pub fn new() -> Self {
        Self {
            pending: None,
            state: ButtonState::Idle,
        }
    }

    pub fn handle(&mut self, wire: &ButtonWire) -> Vec<ButtonEvent> {
        match wire {
            // a resolved future is consumed; activating creates a new one
            ButtonWire::Activate
                if matches!(self.state, ButtonState::Idle | ButtonState::Ready(_)) =>
            {
                self.pending = Some(OneshotCore::new());
                self.state = ButtonState::Pending;
                vec![ButtonEvent::Activated]
            }
            // one value moves exactly once: a second press finds the slot
            // full and the send fails, leaving the resolved colour intact
            ButtonWire::Press { color } => {
                let Some(core) = self.pending.as_mut() else {
                    return Vec::new();
                };
                if core.send(*color).is_err() {
                    return Vec::new();
                }
                self.state = ButtonState::Ready(*color);
                vec![ButtonEvent::Resolved { color: *color }]
            }
            ButtonWire::Activate => Vec::new(),
        }
    }
}

/// Bounded mpsc channel: live senders, buffered values, in-flight values and
/// blocked senders. Channel truth lives in [`MpscCore`]; this driver owns the
/// sender registry, the flight clock and the protocol mapping.
///
/// A value is "in flight" while its animation plays. Flights already occupy
/// channel capacity — the core buffers the value at send time and the flight
/// is pure cosmetics on top — so a full channel blocks the next sender even
/// while values are still mid-air, exactly as `Sender::send` would.
///
/// Blocked senders wake in FIFO order, like tokio's.
pub struct MpscSim {
    core: MpscCore<BufferChar>,
    senders: Vec<SenderInfo>,
    /// Parked sends, mapped back to the handle that issued them and the value
    /// they carry (so a woken send can fly with the right char).
    parked: Vec<(WaiterId, BufferChar)>,
    /// Identity of the single consumer while it is parked in `recv()`.
    waiting: Option<WaiterId>,
    /// How many `recv()` calls are outstanding. The game lets the presenter
    /// click Receive repeatedly; each click is a separate awaited recv, and
    /// every one of them must eventually be served.
    pending_receives: usize,
    /// Values mid-animation: (due, conn, ch). Cosmetics only.
    landings: VecDeque<(f64, u64, char)>,
    in_flight: usize,
    last_received: Option<BufferChar>,
    flight_ms: f64,
    now: f64,
}

impl MpscSim {
    pub fn new(flight_ms: f64) -> Self {
        Self {
            core: MpscCore::new(MPSC_CAPACITY),
            senders: Vec::new(),
            parked: Vec::new(),
            waiting: None,
            pending_receives: 0,
            landings: VecDeque::new(),
            in_flight: 0,
            last_received: None,
            flight_ms,
            now: 0.0,
        }
    }

    /// Sync the sim clock with the wall clock (drivers call this before
    /// handling wires and polling landings).
    pub fn sync_now(&mut self, now_ms: f64) {
        self.now = now_ms;
    }

    /// Advance the sim clock (tests only).
    pub fn advance(&mut self, ms: f64) {
        self.now += ms;
    }

    pub fn contains_sender(&self, conn: u64) -> bool {
        self.senders.iter().any(|s| s.conn == conn)
    }

    /// A new `Sender<T>` handle appears: a freshly connected client
    /// (`owner == conn`) or a clone (`owner` = the handle cloned from).
    pub fn add_sender(&mut self, conn: u64, owner: u64) -> Vec<MpscEvent> {
        if self.contains_sender(conn) {
            return Vec::new();
        }
        self.senders.push(SenderInfo {
            conn,
            owner,
            blocked: false,
        });
        self.core.add_sender();
        vec![MpscEvent::SenderJoined { conn, owner }]
    }

    /// Dropping the handle: the client disconnects (clones die with their
    /// owner's connection). Sends it had parked are cancelled, handing the
    /// values back the way a dropped `Sender` would.
    pub fn remove_sender(&mut self, conn: u64) -> Vec<MpscEvent> {
        if !self.contains_sender(conn) {
            return Vec::new();
        }
        self.senders.retain(|s| s.conn != conn);
        let cancelled: Vec<WaiterId> = self
            .parked
            .iter()
            .filter(|(_, value)| value.conn == conn)
            .map(|(waiter, _)| *waiter)
            .collect();
        for waiter in cancelled {
            self.core.cancel_send(waiter);
            self.parked.retain(|(parked, _)| *parked != waiter);
        }
        self.core.drop_sender();
        let mut events = vec![MpscEvent::SenderLeft { conn }];
        events.extend(self.settle());
        events
    }

    pub fn handle(&mut self, wire: &MpscWire, conn: u64) -> Vec<MpscEvent> {
        match wire {
            // cloning requires an existing handle; `conn` is the new id
            MpscWire::CloneSender { conn: new_conn } if self.contains_sender(conn) => {
                self.add_sender(*new_conn, conn)
            }
            MpscWire::Send { conn: sender, ch } if self.contains_sender(*sender) => {
                let value = BufferChar {
                    ch: *ch,
                    conn: *sender,
                };
                match self.core.offer_send(value) {
                    SendOffer::Accepted => {
                        self.take_off(*sender, *ch);
                        vec![MpscEvent::InFlight {
                            conn: *sender,
                            ch: *ch,
                        }]
                    }
                    SendOffer::Blocked { waiter } => {
                        self.parked.push((waiter, value));
                        self.set_blocked(*sender, true);
                        Vec::new()
                    }
                    // the game never closes the channel
                    SendOffer::Rejected(_) => Vec::new(),
                }
            }
            // Each click is one awaited `recv()`. Queuing them means a
            // second receive on an empty channel is still owed a value
            // instead of being swallowed by a single "waiting" flag.
            MpscWire::Receive => {
                self.pending_receives += 1;
                self.settle()
            }
            _ => Vec::new(),
        }
    }

    /// Milliseconds until the next landing is due, if any.
    pub fn next_delay_ms(&self) -> Option<f64> {
        self.landings
            .front()
            .map(|(due, _, _)| (due - self.now).max(0.0))
    }

    /// Land every flight whose deadline has passed. Landing is cosmetic: the
    /// value already sits in the channel, so this only ends the animation and
    /// lets a waiting receiver pick the value up.
    pub fn poll_due(&mut self) -> Vec<MpscEvent> {
        let mut events = Vec::new();
        while self
            .landings
            .front()
            .is_some_and(|(due, _, _)| *due <= self.now)
        {
            let (_, conn, ch) = self.landings.pop_front().expect("checked non-empty");
            self.in_flight -= 1;
            events.push(MpscEvent::Consumed { conn, ch });
        }
        if !events.is_empty() {
            events.extend(self.settle());
        }
        events
    }

    /// Start a value's flight animation.
    fn take_off(&mut self, conn: u64, ch: char) {
        self.in_flight += 1;
        let due = self.now + self.flight_ms;
        self.landings.push_back((due, conn, ch));
    }

    fn set_blocked(&mut self, conn: u64, blocked: bool) {
        for sender in self.senders.iter_mut().filter(|s| s.conn == conn) {
            sender.blocked = blocked;
        }
    }

    /// Re-check everything parked after a state change: woken sends take off,
    /// and a receiver blocked on an empty channel takes the next value.
    ///
    /// The core wakes parked sends in FIFO order, so a value parked before
    /// another always lands first.
    fn settle(&mut self) -> Vec<MpscEvent> {
        let mut events = Vec::new();
        // Receives and parked sends feed each other: a served receive frees
        // a slot, which wakes a parked send, whose value a later receive can
        // take. Iterate until neither side moves.
        loop {
            self.drain_receives();
            let woken = self.wake_parked_sends();
            if woken.is_empty() {
                break;
            }
            events.extend(woken);
        }
        events
    }

    /// Re-check parked sends. The core wakes them in FIFO order, so a value
    /// parked before another always lands first.
    fn wake_parked_sends(&mut self) -> Vec<MpscEvent> {
        let mut events = Vec::new();
        for (waiter, value) in self.parked.clone() {
            let outcome = self.core.poll_send(waiter);
            if matches!(outcome, SendPoll::Pending) {
                continue;
            }
            self.parked.retain(|(parked, _)| *parked != waiter);
            if !self.parked.iter().any(|(_, v)| v.conn == value.conn) {
                self.set_blocked(value.conn, false);
            }
            // an accepted send flies like any other; the value is already in
            // the buffer it will appear to land in
            if matches!(outcome, SendPoll::Accepted) {
                self.take_off(value.conn, value.ch);
                events.push(MpscEvent::InFlight {
                    conn: value.conn,
                    ch: value.ch,
                });
            }
        }
        events
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
                RecvPoll::Value(owned) => {
                    self.waiting = None;
                    self.pending_receives -= 1;
                    self.last_received = Some(owned);
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

    pub fn snapshot(&self) -> MpscSnapshot {
        // A value in flight already occupies a buffer slot in the core, but
        // the UI draws it mid-air and adds `in_flight` to the buffer length
        // for occupancy. Hide the newest `in_flight` values so a value is
        // shown — and counted — exactly once.
        let buffered: Vec<BufferChar> = self.core.buffer().copied().collect();
        let settled = buffered.len().saturating_sub(self.in_flight);
        MpscSnapshot {
            senders: self.senders.clone(),
            buffer: buffered[..settled].to_vec(),
            blocked_sends: self.core.blocked().map(|(_, value)| *value).collect(),
            in_flight: self.in_flight,
            waiting_receive: self.pending_receives > 0,
            last_received: self.last_received,
        }
    }
}
/// Late-config watch channel: ONE shared cell of state and a single fixed
/// sender (the presenter), created with an initial value so receivers are born
/// with a known `created` phase. Unlike mpsc there is no buffer and no clock:
/// the value moves atomically at send time, so flights are pure client
/// cosmetics and this sim needs no driver polling.
///
/// Teaching beats mirrored from tokio:
/// - `Receiver::borrow()` returns a read guard; `send` waits on the write
///   lock, so a send issued while any receiver is `borrowing` is queued in
///   `pending_send` and commits once the last guard drops.
/// - a `changed()` that is already stale (`version < current`) resolves
///   instantly; otherwise it waits for the next accepted send.
/// - sending a value equal to the current one changes nothing (dedup).
/// - sending with zero receivers fails (`SendError`).
pub struct WatchSim {
    created: bool,
    value: Option<char>,
    version: u64,
    pending_send: Option<char>,
    presenter: Option<u64>,
    next_rx: u64,
    receivers: Vec<RxInfo>,
}

impl Default for WatchSim {
    fn default() -> Self {
        Self::new()
    }
}

impl WatchSim {
    pub fn new() -> Self {
        Self {
            created: false,
            value: None,
            version: 0,
            pending_send: None,
            presenter: None,
            next_rx: 1,
            receivers: Vec::new(),
        }
    }

    pub fn set_presenter(&mut self, conn: Option<u64>) {
        self.presenter = conn;
    }

    pub fn presenter(&self) -> Option<u64> {
        self.presenter
    }

    pub fn has_channel(&self) -> bool {
        self.created
    }

    pub fn owner_of(&self, rx: u64) -> Option<u64> {
        self.receivers.iter().find(|r| r.id == rx).map(|r| r.owner)
    }

    pub fn owned_by(&self, conn: u64) -> Vec<u64> {
        self.receivers
            .iter()
            .filter(|r| r.owner == conn)
            .map(|r| r.id)
            .collect()
    }

    fn commit_send(&mut self, ch: char) -> Vec<WatchEvent> {
        if self.value == Some(ch) {
            return vec![WatchEvent::SameValue];
        }
        self.version += 1;
        self.value = Some(ch);
        for rx in &mut self.receivers {
            if rx.awaiting {
                rx.awaiting = false;
                rx.version = self.version;
            }
        }
        vec![WatchEvent::Changed {
            version: self.version,
            ch,
        }]
    }

    /// A send blocked by borrows resolves once the last read guard drops:
    /// re-check receiver count (SendError), then commit (or dedup).
    fn resolve_pending_if_clear(&mut self) -> Vec<WatchEvent> {
        if self.pending_send.is_none() || self.receivers.iter().any(|r| r.borrowing) {
            return Vec::new();
        }
        let ch = self.pending_send.take().expect("checked pending");
        if self.receivers.is_empty() {
            vec![WatchEvent::SendRefused]
        } else {
            self.commit_send(ch)
        }
    }

    fn push_receiver(&mut self, owner: u64, version: u64) -> Vec<WatchEvent> {
        let id = self.next_rx;
        self.next_rx += 1;
        self.receivers.push(RxInfo {
            id,
            owner,
            version,
            awaiting: false,
            borrowing: false,
        });
        vec![WatchEvent::ReceiverAdded { id, owner }]
    }

    pub fn handle(&mut self, wire: &WatchWire, conn: u64) -> Vec<WatchEvent> {
        match wire {
            WatchWire::Create { init } => {
                if self.created || self.presenter != Some(conn) {
                    return Vec::new();
                }
                self.created = true;
                self.value = *init;
                self.version = 0;
                vec![WatchEvent::Created]
            }
            WatchWire::Send { ch } => {
                if !self.created || self.presenter != Some(conn) {
                    return Vec::new();
                }
                if self.pending_send.is_some() {
                    return Vec::new();
                }
                if self.receivers.is_empty() {
                    return vec![WatchEvent::SendRefused];
                }
                if self.receivers.iter().any(|r| r.borrowing) {
                    self.pending_send = Some(*ch);
                    return vec![WatchEvent::SendBlocked];
                }
                self.commit_send(*ch)
            }
            WatchWire::NewReceiver => {
                if !self.created {
                    return Vec::new();
                }
                self.push_receiver(conn, self.version)
            }
            // cloning copies the source's seen-version but starts fresh; the
            // driver/actor authorizes the request (owner or presenter)
            WatchWire::CloneReceiver { rx } => {
                if !self.created {
                    return Vec::new();
                }
                let Some(src) = self.receivers.iter().find(|r| r.id == *rx) else {
                    return Vec::new();
                };
                self.push_receiver(conn, src.version)
            }
            WatchWire::AwaitChange { rx } => {
                let Some(r) = self.receivers.iter_mut().find(|r| r.id == *rx) else {
                    return Vec::new();
                };
                if r.owner != conn {
                    return Vec::new();
                }
                if r.awaiting {
                    // cancel an outstanding changed()
                    r.awaiting = false;
                } else if r.version < self.version {
                    // already stale: changed() completes immediately
                    r.version = self.version;
                } else {
                    r.awaiting = true;
                }
                Vec::new()
            }
            WatchWire::LookInside { rx } => {
                let Some(r) = self.receivers.iter_mut().find(|r| r.id == *rx) else {
                    return Vec::new();
                };
                if r.owner != conn {
                    return Vec::new();
                }
                if r.borrowing {
                    r.borrowing = false;
                    return self.resolve_pending_if_clear();
                }
                r.borrowing = true;
                vec![WatchEvent::BorrowFlight { rx: *rx }]
            }
            WatchWire::DropReceiver { rx } => {
                let Some(r) = self.receivers.iter().find(|r| r.id == *rx) else {
                    return Vec::new();
                };
                if r.owner != conn {
                    return Vec::new();
                }
                let was_borrowing = r.borrowing;
                self.receivers.retain(|x| x.id != *rx);
                let mut events = vec![WatchEvent::ReceiverRemoved { id: *rx }];
                if was_borrowing {
                    events.extend(self.resolve_pending_if_clear());
                }
                events
            }
            // presenter slot is actor state; sim never sees the wire directly
            WatchWire::ClaimPresenter => Vec::new(),
        }
    }

    pub fn snapshot(&self) -> WatchSnapshot {
        WatchSnapshot {
            created: self.created,
            value: self.value,
            version: self.version,
            pending_send: self.pending_send,
            presenter: self.presenter,
            receivers: self.receivers.clone(),
        }
    }
}

/// Broadcast channel: one ring buffer, many `Sender<T>` handles (the host
/// seeds one; anyone may clone any of them), one `Receiver<T>` per
/// subscribing connection. Channel truth lives in [`BroadcastCore`]; this
/// driver owns the handle registries and the protocol mapping.
///
/// - `send` never blocks: when the buffer is full the oldest value is
///   evicted to make room.
/// - `subscribe()` starts at the tail, like tokio's: a new receiver sees
///   only values sent after it subscribed, never buffered history.
/// - a receiver that falls behind the ring yields `Lagged(n)` once — the
///   count of evicted values it missed — and resumes at the oldest retained
///   value.
/// - `recv()` with nothing new arms the receiver (`waiting`); it completes on
///   the next send.
/// - once every sender is gone the channel is closed, but values still
///   retained stay receivable: `Closed` is only reported after they drain.
/// - the host seed sender (`conn == 0`) is presenter-owned once a presenter
///   claims the slot; ownership transfers to them.
pub struct BroadcastSim {
    core: BroadcastCore<BufferChar>,
    senders: Vec<SenderInfo>,
    /// Protocol receiver id -> core slot id. Core slots are recycled on drop,
    /// so protocol ids (which the UI renders and keys DOM nodes by) are
    /// allocated separately and never reused.
    slots: BTreeMap<u64, usize>,
    receivers: Vec<RxState>,
    next_receiver: u64,
    next_sender: u64,
    presenter: Option<u64>,
}

/// The seed sender's reserved handle id; its owner transfers to whoever
/// claims the presenter slot.
pub const BROADCAST_HOST_CONN: u64 = 0;

impl Default for BroadcastSim {
    fn default() -> Self {
        Self::new()
    }
}

impl BroadcastSim {
    pub fn new() -> Self {
        Self {
            core: BroadcastCore::new(BROADCAST_CAPACITY),
            senders: vec![SenderInfo {
                conn: BROADCAST_HOST_CONN,
                owner: BROADCAST_HOST_CONN,
                blocked: false,
            }],
            slots: BTreeMap::new(),
            receivers: Vec::new(),
            next_receiver: 1,
            next_sender: 1,
            presenter: None,
        }
    }

    pub fn presenter(&self) -> Option<u64> {
        self.presenter
    }

    fn contains_sender(&self, conn: u64) -> bool {
        self.senders.iter().any(|s| s.conn == conn)
    }

    pub fn owner_of_sender(&self, conn: u64) -> Option<u64> {
        self.senders
            .iter()
            .find(|s| s.conn == conn)
            .map(|s| s.owner)
    }

    pub fn owned_senders(&self, conn: u64) -> Vec<u64> {
        self.senders
            .iter()
            .filter(|s| s.owner == conn || s.conn == conn)
            .map(|s| s.conn)
            .collect()
    }

    pub fn owned_senders_all(&self) -> Vec<(u64, u64)> {
        self.senders.iter().map(|s| (s.conn, s.owner)).collect()
    }

    pub fn owned_receivers(&self, conn: u64) -> Vec<u64> {
        self.receivers
            .iter()
            .filter(|r| r.owner == conn)
            .map(|r| r.receiver)
            .collect()
    }

    /// A fresh handle cloned from `source`, owned by `requester`.
    fn clone_sender(&mut self, source: u64, requester: u64) -> Vec<BroadcastEvent> {
        if !self.contains_sender(source) {
            return Vec::new();
        }
        let conn = self.next_sender;
        self.next_sender += 1;
        self.senders.push(SenderInfo {
            conn,
            owner: requester,
            blocked: false,
        });
        self.core.add_sender();
        vec![BroadcastEvent::SenderJoined {
            conn,
            owner: requester,
        }]
    }

    /// A new receiver starts at the tail (tokio's `subscribe`): it sees every
    /// value sent from now on, and none of the history still in the buffer.
    /// One receiver per connection.
    fn subscribe(&mut self, conn: u64) -> Vec<BroadcastEvent> {
        if self
            .receivers
            .iter()
            .any(|r| r.owner == conn || r.conn == conn)
        {
            return Vec::new();
        }
        let receiver = self.next_receiver;
        self.next_receiver += 1;
        let slot = self.core.subscribe();
        self.slots.insert(receiver, slot);
        self.receivers.push(RxState {
            receiver,
            conn,
            owner: conn,
            next: self.core.next_seq() + 1,
            lagged_total: 0,
            last: None,
            error: None,
            waiting: false,
        });
        vec![BroadcastEvent::ReceiverAdded {
            receiver,
            owner: conn,
        }]
    }

    fn unsubscribe(&mut self, receiver: u64, conn: u64) -> Vec<BroadcastEvent> {
        let Some(rx) = self.receivers.iter().find(|r| r.receiver == receiver) else {
            return Vec::new();
        };
        if rx.owner != conn {
            return Vec::new();
        }
        self.drop_receiver(receiver);
        vec![BroadcastEvent::ReceiverRemoved { receiver }]
    }

    /// Release a receiver's core slot and forget its protocol id. The slot
    /// may be handed to a future subscriber, so the mapping must go too.
    fn drop_receiver(&mut self, receiver: u64) {
        if let Some(slot) = self.slots.remove(&receiver) {
            self.core.drop_receiver(slot);
        }
        self.receivers.retain(|r| r.receiver != receiver);
    }

    /// A value is appended at the tail; a full buffer evicts its oldest
    /// value. Armed (waiting) receivers complete immediately.
    fn send(&mut self, conn: u64, ch: char) -> Vec<BroadcastEvent> {
        if !self.contains_sender(conn) {
            return Vec::new();
        }
        // remember the value at risk of eviction before the send lands
        let oldest_before = self.core.ring().next().map(|(seq, value)| (seq, *value));
        if self.core.send(BufferChar { ch, conn }).is_err() {
            return Vec::new();
        }
        let mut events = vec![BroadcastEvent::InFlight { conn, ch }];
        // the ring's front moved on: the value it held was evicted
        let oldest_now = self.core.ring().next().map(|(seq, _)| seq);
        if let (Some((before, evicted)), Some(now)) = (oldest_before, oldest_now) {
            if now > before {
                events.push(BroadcastEvent::Evicted { ch: evicted.ch });
            }
        }
        let armed: Vec<(u64, u64)> = self
            .receivers
            .iter()
            .filter(|r| r.waiting)
            .map(|r| (r.receiver, r.owner))
            .collect();
        for (receiver, owner) in armed {
            events.extend(self.recv(receiver, owner));
        }
        events
    }

    /// One `recv()` step for a receiver: a value, a lag report, closure, or
    /// arming it to complete on the next send.
    fn recv(&mut self, receiver: u64, conn: u64) -> Vec<BroadcastEvent> {
        let Some(rx) = self.receivers.iter().find(|r| r.receiver == receiver) else {
            return Vec::new();
        };
        if rx.owner != conn {
            return Vec::new();
        }
        let Some(slot) = self.slots.get(&receiver).copied() else {
            return Vec::new();
        };
        let poll = self.core.poll_recv(slot);
        let cursor = self.core.receiver_seq(slot);
        let rx = self
            .receivers
            .iter_mut()
            .find(|r| r.receiver == receiver)
            .expect("checked above");
        if let Some(cursor) = cursor {
            rx.next = cursor + 1;
        }
        match poll {
            BroadcastPoll::Value(owned) => {
                rx.last = Some(owned);
                rx.error = None;
                rx.waiting = false;
                vec![BroadcastEvent::Received {
                    receiver,
                    ch: owned.ch,
                }]
            }
            BroadcastPoll::Lagged { skipped } => {
                rx.lagged_total += skipped;
                rx.waiting = false;
                rx.error = Some(BroadcastError::Lagged(skipped));
                vec![BroadcastEvent::Lagged {
                    receiver,
                    n: skipped,
                }]
            }
            BroadcastPoll::Closed => {
                rx.error = Some(BroadcastError::Closed);
                rx.waiting = false;
                Vec::new()
            }
            // nothing new: the recv blocks until the next send
            BroadcastPoll::Empty { .. } => {
                rx.waiting = true;
                Vec::new()
            }
        }
    }

    /// Claiming the presenter slot transfers ownership of host-owned senders
    /// (so they die with the presenter's connection) — and re-seeds the host
    /// sender when the channel had gone fully closed.
    fn claim(&mut self, conn: u64) -> Vec<BroadcastEvent> {
        self.presenter = Some(conn);
        let mut events = Vec::new();
        if !self.senders.iter().any(|s| s.conn == BROADCAST_HOST_CONN) {
            self.senders.push(SenderInfo {
                conn: BROADCAST_HOST_CONN,
                owner: conn,
                blocked: false,
            });
            self.core.add_sender();
            events.push(BroadcastEvent::SenderJoined {
                conn: BROADCAST_HOST_CONN,
                owner: conn,
            });
        }
        // host-owned senders transfer to the new presenter
        for s in &mut self.senders {
            if s.owner == BROADCAST_HOST_CONN {
                s.owner = conn;
            }
        }
        events
    }

    /// A connection left: its handles (and clones it owns) drop, its receiver
    /// is removed. Emptying the senders closes the channel.
    pub fn drop_connection(&mut self, conn: u64) -> Vec<BroadcastEvent> {
        let mut events = Vec::new();
        let dropped: Vec<u64> = self
            .senders
            .iter()
            .filter(|s| s.conn == conn || s.owner == conn)
            .map(|s| s.conn)
            .collect();
        for handle in dropped {
            self.senders.retain(|s| s.conn != handle);
            self.core.drop_sender();
            events.push(BroadcastEvent::SenderLeft { conn: handle });
        }
        let receivers: Vec<u64> = self
            .receivers
            .iter()
            .filter(|r| r.owner == conn)
            .map(|r| r.receiver)
            .collect();
        for receiver in receivers {
            self.drop_receiver(receiver);
            events.push(BroadcastEvent::ReceiverRemoved { receiver });
        }
        if self.presenter == Some(conn) {
            self.presenter = None;
        }
        events
    }

    pub fn handle(&mut self, wire: &BroadcastWire, conn: u64) -> Vec<BroadcastEvent> {
        match wire {
            // `wire.conn` is the handle being sent from; it must be owned by
            // the requester (`conn`)
            BroadcastWire::Send { conn: sender, ch } => {
                if self.contains_sender(*sender) && self.owner_of_sender(*sender) == Some(conn) {
                    self.send(*sender, *ch)
                } else {
                    Vec::new()
                }
            }
            BroadcastWire::CloneSender { source } => self.clone_sender(*source, conn),
            BroadcastWire::Subscribe => self.subscribe(conn),
            BroadcastWire::Unsubscribe { receiver } => self.unsubscribe(*receiver, conn),
            BroadcastWire::Receive { receiver } => self.recv(*receiver, conn),
            BroadcastWire::ClaimPresenter => self.claim(conn),
        }
    }

    pub fn snapshot(&self) -> BroadcastSnapshot {
        BroadcastSnapshot {
            senders: self.senders.clone(),
            buffer: self.core.ring().map(|(_, value)| *value).collect(),
            tail: self.core.next_seq(),
            receivers: self.receivers.clone(),
            presenter: self.presenter,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::protocol::Color;

    const FLIGHT: f64 = 100.0;
    const CAP: usize = MPSC_CAPACITY;

    fn sim_with_two_senders() -> MpscSim {
        let mut sim = MpscSim::new(FLIGHT);
        sim.add_sender(1, 1);
        sim.add_sender(2, 1);
        sim
    }

    fn send(sim: &mut MpscSim, conn: u64, ch: char) -> Vec<MpscEvent> {
        sim.handle(&MpscWire::Send { conn, ch }, conn)
    }

    fn receive(sim: &mut MpscSim) -> Vec<MpscEvent> {
        sim.handle(&MpscWire::Receive, 0)
    }

    #[test]
    fn button_future_lifecycle() {
        let mut sim = ButtonSim::new();
        assert_eq!(sim.state, ButtonState::Idle);
        assert_eq!(
            sim.handle(&ButtonWire::Activate),
            vec![ButtonEvent::Activated]
        );
        assert_eq!(sim.state, ButtonState::Pending);
        assert_eq!(
            sim.handle(&ButtonWire::Press {
                color: Color::Green
            }),
            vec![ButtonEvent::Resolved {
                color: Color::Green
            }]
        );
        assert_eq!(sim.state, ButtonState::Ready(Color::Green));
        // a resolved future is consumed; activating creates a new one
        assert_eq!(
            sim.handle(&ButtonWire::Activate),
            vec![ButtonEvent::Activated]
        );
        // pressing while idle does nothing
        let mut idle = ButtonSim::new();
        assert!(idle
            .handle(&ButtonWire::Press { color: Color::Red })
            .is_empty());
    }

    #[test]
    fn send_flies_then_lands_into_buffer() {
        let mut sim = sim_with_two_senders();
        let events = send(&mut sim, 1, 'a');
        assert_eq!(events, vec![MpscEvent::InFlight { conn: 1, ch: 'a' }]);
        // not landed yet
        assert!(sim.poll_due().is_empty());
        assert_eq!(sim.snapshot().in_flight, 1);
        assert!(sim.snapshot().buffer.is_empty());

        sim.advance(FLIGHT);
        let events = sim.poll_due();
        assert_eq!(events, vec![MpscEvent::Consumed { conn: 1, ch: 'a' }]);
        let snap = sim.snapshot();
        assert_eq!(snap.buffer, vec![BufferChar { ch: 'a', conn: 1 }]);
        assert_eq!(snap.in_flight, 0);
    }

    #[test]
    fn full_buffer_blocks_sender_until_a_receive_wakes_it() {
        let mut sim = sim_with_two_senders();
        for (i, ch) in ('a'..).take(CAP).enumerate() {
            let conn = if i % 2 == 0 { 1 } else { 2 };
            send(&mut sim, conn, ch);
        }
        sim.advance(FLIGHT);
        let landed = sim.poll_due();
        assert_eq!(landed.len(), CAP);
        assert_eq!(sim.snapshot().buffer.len(), CAP);

        // sixth send blocks: no cosmetic event, state shows it
        let events = send(&mut sim, 1, 'z');
        assert!(events.is_empty());
        let snap = sim.snapshot();
        assert_eq!(snap.blocked_sends, vec![BufferChar { ch: 'z', conn: 1 }]);
        assert!(snap.senders.iter().find(|s| s.conn == 1).unwrap().blocked);

        // receive frees a slot and wakes exactly one blocked send
        let _ = receive(&mut sim);
        sim.advance(FLIGHT);
        let events = sim.poll_due();
        assert_eq!(events.len(), 1, "one unblocked send lands");

        let snap = sim.snapshot();
        assert!(snap.blocked_sends.is_empty());
        assert!(!snap.senders.iter().find(|s| s.conn == 1).unwrap().blocked);
        assert_eq!(snap.buffer.len(), CAP);
        assert!(snap.last_received.is_some());
    }

    #[test]
    fn unblock_wake_order_is_fifo() {
        let mut sim = MpscSim::new(FLIGHT);
        sim.add_sender(1, 1);
        sim.add_sender(2, 1);
        sim.add_sender(3, 1);
        for i in 0..CAP {
            let ch = char::from_u32('a' as u32 + i as u32).unwrap();
            send(&mut sim, 1, ch);
        }
        sim.advance(FLIGHT);
        let _ = sim.poll_due();

        // two sends park, in this order
        send(&mut sim, 2, 'x');
        send(&mut sim, 3, 'y');
        assert_eq!(
            sim.snapshot()
                .blocked_sends
                .iter()
                .map(|b| b.ch)
                .collect::<Vec<_>>(),
            ['x', 'y'],
        );

        // tokio wakes parked senders in park order: 'x' before 'y'
        let mut woken = Vec::new();
        for _ in 0..2 {
            // the wake happens inside the receive that frees the slot
            for event in receive(&mut sim) {
                if let MpscEvent::InFlight { ch, .. } = event {
                    woken.push(ch);
                }
            }
            sim.advance(FLIGHT);
            let _ = sim.poll_due();
        }
        assert_eq!(woken, ['x', 'y'], "parked sends wake FIFO, never randomly");
    }

    #[test]
    fn receive_on_empty_channel_waits_and_bypasses_buffer() {
        let mut sim = sim_with_two_senders();
        let events = receive(&mut sim);
        assert!(events.is_empty());
        assert!(sim.snapshot().waiting_receive);

        send(&mut sim, 1, 'a');
        sim.advance(FLIGHT);
        let events = sim.poll_due();
        assert_eq!(events, vec![MpscEvent::Consumed { conn: 1, ch: 'a' }]);
        let snap = sim.snapshot();
        assert!(snap.buffer.is_empty(), "waiting receiver takes it directly");
        assert!(!snap.waiting_receive);
        assert_eq!(snap.last_received, Some(BufferChar { ch: 'a', conn: 1 }));
    }

    /// Regression: a second `recv()` while one is already parked used to
    /// overwrite a single `waiting` flag, so one of the two receives was
    /// silently lost. The core keeps the waiter, so both are served.
    #[test]
    fn concurrent_receives_are_queued_not_dropped() {
        let mut sim = sim_with_two_senders();
        receive(&mut sim);
        receive(&mut sim);
        assert!(sim.snapshot().waiting_receive);

        send(&mut sim, 1, 'a');
        send(&mut sim, 1, 'b');
        sim.advance(FLIGHT);
        let _ = sim.poll_due();

        // 'a' goes to the receive that was parked first (FIFO); 'b' is
        // taken by the second, which the old single-flag sim had discarded.
        let snap = sim.snapshot();
        assert_eq!(snap.last_received, Some(BufferChar { ch: 'b', conn: 1 }));
        assert!(
            snap.buffer.is_empty(),
            "neither value is stranded behind a lost receive"
        );
        assert!(!snap.waiting_receive, "both receives completed");
    }

    #[test]
    fn clone_requires_a_handle_and_records_owner() {
        let mut sim = sim_with_two_senders();
        let events = sim.handle(&MpscWire::CloneSender { conn: 7 }, 1);
        assert_eq!(events, vec![MpscEvent::SenderJoined { conn: 7, owner: 1 }]);
        let snap = sim.snapshot();
        let clone = snap.senders.iter().find(|s| s.conn == 7).unwrap();
        assert_eq!(clone.owner, 1);

        // cloning from a connection that owns no handle is ignored
        assert!(sim
            .handle(&MpscWire::CloneSender { conn: 8 }, 99)
            .is_empty());
        assert!(!sim.contains_sender(8));
    }

    #[test]
    fn disconnect_drops_handle_and_its_blocked_send() {
        let mut sim = sim_with_two_senders();
        for i in 0..CAP {
            send(&mut sim, 1, char::from_u32('a' as u32 + i as u32).unwrap());
        }
        sim.advance(FLIGHT);
        let _ = sim.poll_due();
        send(&mut sim, 2, 'z');
        assert_eq!(sim.snapshot().blocked_sends.len(), 1);

        let events = sim.remove_sender(2);
        assert_eq!(events, vec![MpscEvent::SenderLeft { conn: 2 }]);
        let snap = sim.snapshot();
        assert!(snap.blocked_sends.is_empty(), "blocked send discarded");
        assert!(!sim.contains_sender(2));
    }
}

#[cfg(test)]
mod watch_tests {
    use super::*;
    use crate::protocol::{WatchEvent, WatchWire};

    /// Two-player sim: presenter is conn 1, a player is conn 2.
    fn sim() -> WatchSim {
        let mut sim = WatchSim::new();
        sim.set_presenter(Some(1));
        sim
    }

    fn create(sim: &mut WatchSim) {
        assert_eq!(
            sim.handle(&WatchWire::Create { init: Some('c') }, 1),
            vec![WatchEvent::Created]
        );
    }

    /// Subscribes a receiver for `owner` and returns its id.
    fn rx(sim: &mut WatchSim, owner: u64) -> u64 {
        match &sim.handle(&WatchWire::NewReceiver, owner)[..] {
            [WatchEvent::ReceiverAdded { id, .. }] => *id,
            other => panic!("expected ReceiverAdded, got {other:?}"),
        }
    }

    fn value(sim: &WatchSim) -> Option<char> {
        sim.snapshot().value
    }

    #[test]
    fn create_phase_holds_until_presenter_creates_with_initial_value() {
        let mut sim = sim();
        assert!(!sim.snapshot().created);
        // no receivers before the channel exists
        assert!(sim.handle(&WatchWire::NewReceiver, 2).is_empty());
        // only the presenter may create
        assert!(sim
            .handle(&WatchWire::Create { init: Some('x') }, 2)
            .is_empty());
        create(&mut sim);
        let snap = sim.snapshot();
        assert!(snap.created);
        assert_eq!(snap.value, Some('c'));
        assert_eq!(snap.version, 0);
        // the channel is created once; a second create is ignored
        assert!(sim
            .handle(&WatchWire::Create { init: Some('y') }, 1)
            .is_empty());
    }

    #[test]
    fn send_changes_value_and_bumps_version() {
        let mut sim = sim();
        create(&mut sim);
        let r = rx(&mut sim, 2);
        let events = sim.handle(&WatchWire::Send { ch: 'd' }, 1);
        assert_eq!(
            events,
            vec![WatchEvent::Changed {
                version: 1,
                ch: 'd'
            }]
        );
        let snap = sim.snapshot();
        assert_eq!(snap.value, Some('d'));
        assert_eq!(snap.version, 1);
        assert!(!snap.receivers.iter().find(|x| x.id == r).unwrap().awaiting);
    }

    #[test]
    fn send_without_receivers_is_refused() {
        let mut sim = sim();
        create(&mut sim);
        assert_eq!(
            sim.handle(&WatchWire::Send { ch: 'd' }, 1),
            vec![WatchEvent::SendRefused]
        );
        assert_eq!(value(&sim), Some('c'));
        let events = sim.handle(&WatchWire::Send { ch: 'd' }, 1);
        assert_eq!(events, vec![WatchEvent::SendRefused]);
    }

    #[test]
    fn send_is_presenter_only() {
        let mut sim = sim();
        create(&mut sim);
        rx(&mut sim, 2);
        assert!(sim.handle(&WatchWire::Send { ch: 'd' }, 2).is_empty());
        assert_eq!(value(&sim), Some('c'));
    }

    #[test]
    fn equal_value_dedups_and_does_not_wake() {
        let mut sim = sim();
        create(&mut sim);
        let r = rx(&mut sim, 2);
        sim.handle(&WatchWire::AwaitChange { rx: r }, 2);
        assert!(sim.snapshot().receivers[0].awaiting);

        assert_eq!(
            sim.handle(&WatchWire::Send { ch: 'c' }, 1),
            vec![WatchEvent::SameValue]
        );
        let snap = sim.snapshot();
        assert_eq!(snap.version, 0, "dedup does not bump version");
        assert!(snap.receivers[0].awaiting, "nobody wakes on a dedup send");
    }

    #[test]
    fn changed_waiting_wakes_on_next_different_value() {
        let mut sim = sim();
        create(&mut sim);
        let r1 = rx(&mut sim, 2);
        let r2 = rx(&mut sim, 3);
        sim.handle(&WatchWire::AwaitChange { rx: r1 }, 2);
        sim.handle(&WatchWire::AwaitChange { rx: r2 }, 3);
        assert!(sim.snapshot().receivers.iter().all(|r| r.awaiting));

        sim.handle(&WatchWire::Send { ch: 'd' }, 1);
        let snap = sim.snapshot();
        assert!(snap.receivers.iter().all(|r| !r.awaiting));
        assert!(snap.receivers.iter().all(|r| r.version == snap.version));
    }

    #[test]
    fn changed_completes_immediately_when_already_stale() {
        let mut sim = sim();
        create(&mut sim);
        let r = rx(&mut sim, 2);
        sim.handle(&WatchWire::Send { ch: 'd' }, 1);
        sim.handle(&WatchWire::AwaitChange { rx: r }, 2);
        let snap = sim.snapshot();
        assert!(
            !snap.receivers[0].awaiting,
            "stale changed() resolves at once"
        );
        assert_eq!(snap.receivers[0].version, snap.version);
    }

    #[test]
    fn changed_can_be_cancelled() {
        let mut sim = sim();
        create(&mut sim);
        let r = rx(&mut sim, 2);
        sim.handle(&WatchWire::AwaitChange { rx: r }, 2);
        assert!(sim.snapshot().receivers[0].awaiting);
        sim.handle(&WatchWire::AwaitChange { rx: r }, 2);
        assert!(!sim.snapshot().receivers[0].awaiting);
    }

    #[test]
    fn send_blocks_while_borrowing_then_flushes_on_release() {
        let mut sim = sim();
        create(&mut sim);
        let r = rx(&mut sim, 2);
        sim.handle(&WatchWire::LookInside { rx: r }, 2);
        assert!(sim.snapshot().receivers[0].borrowing);

        assert_eq!(
            sim.handle(&WatchWire::Send { ch: 'd' }, 1),
            vec![WatchEvent::SendBlocked]
        );
        let snap = sim.snapshot();
        assert_eq!(snap.pending_send, Some('d'));
        assert_eq!(value(&sim), Some('c'), "not committed while borrowed");

        assert_eq!(
            sim.handle(&WatchWire::LookInside { rx: r }, 2),
            vec![WatchEvent::Changed {
                version: 1,
                ch: 'd'
            }]
        );
        assert_eq!(value(&sim), Some('d'));
        assert_eq!(sim.snapshot().pending_send, None);
    }

    #[test]
    fn multiple_borrows_delay_send_until_all_released() {
        let mut sim = sim();
        create(&mut sim);
        let r1 = rx(&mut sim, 2);
        let r2 = rx(&mut sim, 3);
        sim.handle(&WatchWire::LookInside { rx: r1 }, 2);
        sim.handle(&WatchWire::LookInside { rx: r2 }, 3);
        sim.handle(&WatchWire::Send { ch: 'd' }, 1);
        assert_eq!(sim.snapshot().pending_send, Some('d'));

        // releasing only one keeps it pending
        assert!(sim.handle(&WatchWire::LookInside { rx: r1 }, 2).is_empty());
        assert_eq!(sim.snapshot().pending_send, Some('d'));

        assert_eq!(
            sim.handle(&WatchWire::LookInside { rx: r2 }, 3),
            vec![WatchEvent::Changed {
                version: 1,
                ch: 'd'
            }]
        );
        assert_eq!(sim.snapshot().pending_send, None);
    }

    #[test]
    fn dropping_last_borrower_flushes_pending_send() {
        let mut sim = sim();
        create(&mut sim);
        // a second, non-borrowing receiver survives the drop so the pending
        // send can resolve against remaining receivers
        let _survivor = rx(&mut sim, 3);
        let r = rx(&mut sim, 2);
        sim.handle(&WatchWire::LookInside { rx: r }, 2);
        sim.handle(&WatchWire::Send { ch: 'd' }, 1);
        assert_eq!(
            sim.handle(&WatchWire::DropReceiver { rx: r }, 2),
            vec![
                WatchEvent::ReceiverRemoved { id: r },
                WatchEvent::Changed {
                    version: 1,
                    ch: 'd'
                }
            ]
        );
        assert_eq!(value(&sim), Some('d'));
    }

    #[test]
    fn pending_send_refused_when_all_receivers_disappear() {
        let mut sim = sim();
        create(&mut sim);
        let r = rx(&mut sim, 2);
        sim.handle(&WatchWire::LookInside { rx: r }, 2);
        sim.handle(&WatchWire::Send { ch: 'd' }, 1);
        assert_eq!(
            sim.handle(&WatchWire::DropReceiver { rx: r }, 2),
            vec![
                WatchEvent::ReceiverRemoved { id: r },
                WatchEvent::SendRefused
            ]
        );
        assert_eq!(value(&sim), Some('c'));
    }

    #[test]
    fn send_while_pending_is_ignored() {
        let mut sim = sim();
        create(&mut sim);
        let r = rx(&mut sim, 2);
        sim.handle(&WatchWire::LookInside { rx: r }, 2);
        sim.handle(&WatchWire::Send { ch: 'd' }, 1);
        assert!(
            sim.handle(&WatchWire::Send { ch: 'e' }, 1).is_empty(),
            "second send dropped"
        );
        assert_eq!(sim.snapshot().pending_send, Some('d'));
    }

    #[test]
    fn clone_copies_seen_version_but_not_awaiting() {
        let mut sim = sim();
        create(&mut sim);
        let r = rx(&mut sim, 2);
        // player 2 awaits at the *current* version (not stale), so it blocks
        sim.handle(&WatchWire::AwaitChange { rx: r }, 2);
        assert!(sim.snapshot().receivers[0].awaiting);
        // presenter (1) clones it: fresh handle, no awaiting, same seen version
        let events = sim.handle(&WatchWire::CloneReceiver { rx: r }, 1);
        let id = match events[0] {
            WatchEvent::ReceiverAdded { id, owner } => {
                assert_eq!(owner, 1);
                id
            }
            ref other => panic!("expected ReceiverAdded, got {other:?}"),
        };
        let snap = sim.snapshot();
        let clone = snap.receivers.iter().find(|x| x.id == id).unwrap();
        assert_eq!(
            clone.version, snap.version,
            "clone starts at the source's seen version"
        );
        assert!(!clone.awaiting);
        assert!(!clone.borrowing);

        // a send wakes the source receiver only; the clone stays behind until
        // *it* calls changed() again
        sim.handle(&WatchWire::Send { ch: 'd' }, 1);
        let snap = sim.snapshot();
        let src = snap.receivers.iter().find(|x| x.id == r).unwrap();
        assert!(!src.awaiting, "source was waiting on changed() and woke");
        assert_eq!(src.version, snap.version);
        let clone = snap.receivers.iter().find(|x| x.id == id).unwrap();
        assert_eq!(clone.version, 0, "clone has not caught up");

        // cloning an unknown receiver does nothing
        assert!(sim
            .handle(&WatchWire::CloneReceiver { rx: 999 }, 1)
            .is_empty());
    }

    #[test]
    fn foreign_rx_operations_are_ignored() {
        let mut sim = sim();
        create(&mut sim);
        let r = rx(&mut sim, 2);
        // another player cannot await, borrow or drop conn 2's receiver
        assert!(sim.handle(&WatchWire::AwaitChange { rx: r }, 3).is_empty());
        assert!(sim.handle(&WatchWire::LookInside { rx: r }, 3).is_empty());
        assert!(sim.handle(&WatchWire::DropReceiver { rx: r }, 3).is_empty());
        assert_eq!(sim.snapshot().receivers.len(), 1);
    }

    #[test]
    fn owned_by_lists_only_that_connection() {
        let mut sim = sim();
        create(&mut sim);
        rx(&mut sim, 2);
        rx(&mut sim, 2);
        rx(&mut sim, 3);
        let owned = sim.owned_by(2);
        assert_eq!(owned.len(), 2);
        assert!(sim.owned_by(1).is_empty());
        assert_eq!(sim.owned_by(3).len(), 1);
    }
}

#[cfg(test)]
mod broadcast_tests {
    use super::*;
    use crate::protocol::{BroadcastError, BroadcastEvent, BroadcastWire, BROADCAST_CAPACITY};

    fn bsend(sim: &mut BroadcastSim, conn: u64, ch: char) -> Vec<BroadcastEvent> {
        sim.handle(&BroadcastWire::Send { conn, ch }, conn)
    }

    /// Send from the host seed handle on behalf of the connection that owns
    /// it (the presenter, once claimed).
    fn host_send(sim: &mut BroadcastSim, owner: u64, ch: char) -> Vec<BroadcastEvent> {
        sim.handle(
            &BroadcastWire::Send {
                conn: BROADCAST_HOST_CONN,
                ch,
            },
            owner,
        )
    }

    fn subscribe(sim: &mut BroadcastSim, conn: u64) -> u64 {
        match &sim.handle(&BroadcastWire::Subscribe, conn)[..] {
            [BroadcastEvent::ReceiverAdded { receiver, owner }] => {
                assert_eq!(*owner, conn);
                *receiver
            }
            other => panic!("expected ReceiverAdded, got {other:?}"),
        }
    }

    fn recv(sim: &mut BroadcastSim, receiver: u64, conn: u64) -> Vec<BroadcastEvent> {
        sim.handle(&BroadcastWire::Receive { receiver }, conn)
    }

    fn rx(sim: &BroadcastSim, receiver: u64) -> RxState {
        *sim.snapshot()
            .receivers
            .iter()
            .find(|r| r.receiver == receiver)
            .unwrap()
    }

    #[test]
    fn host_seed_exists_and_clone_is_owned_by_cloner() {
        let mut sim = BroadcastSim::new();
        assert!(sim.contains_sender(BROADCAST_HOST_CONN));

        // anyone may clone any sender; the clone belongs to the cloner
        let events = sim.handle(
            &BroadcastWire::CloneSender {
                source: BROADCAST_HOST_CONN,
            },
            7,
        );
        assert_eq!(
            events,
            vec![BroadcastEvent::SenderJoined { conn: 1, owner: 7 }]
        );
        assert!(sim.contains_sender(1));

        // a clone of a non-existent handle is ignored
        assert!(sim
            .handle(&BroadcastWire::CloneSender { source: 999 }, 7)
            .is_empty());
    }

    #[test]
    fn send_never_blocks_and_evicts_oldest() {
        let mut sim = BroadcastSim::new();
        // tokio's send fails when no receiver is left, so the channel needs
        // one before values can flow
        subscribe(&mut sim, 9);
        let mut events = Vec::new();
        for ch in ('a'..).take(BROADCAST_CAPACITY + 2) {
            events.extend(bsend(&mut sim, BROADCAST_HOST_CONN, ch));
        }
        // 'a' and 'b' were evicted, in order
        let evicted: Vec<char> = events
            .iter()
            .filter_map(|e| match e {
                BroadcastEvent::Evicted { ch } => Some(*ch),
                _ => None,
            })
            .collect();
        assert_eq!(evicted, vec!['a', 'b']);
        let snap = sim.snapshot();
        let chars: Vec<char> = snap.buffer.iter().map(|b| b.ch).collect();
        assert_eq!(chars, vec!['c', 'd', 'e', 'f', 'g']);
        assert_eq!(snap.tail, 7);
    }

    /// tokio's `subscribe` starts a receiver at the tail: it sees only what
    /// is sent afterwards, never the history still held in the ring.
    #[test]
    fn subscribe_mid_stream_starts_at_the_tail() {
        let mut sim = BroadcastSim::new();
        let early = subscribe(&mut sim, 9);
        for ch in 'a'..'d' {
            bsend(&mut sim, BROADCAST_HOST_CONN, ch);
        }
        let receiver = subscribe(&mut sim, 5);
        assert_eq!(rx(&sim, receiver).next, 4, "past every buffered value");

        // nothing to read yet: the recv arms and waits for the next send
        assert!(recv(&mut sim, receiver, 5).is_empty());
        assert!(rx(&sim, receiver).waiting);

        // the next send reaches it, and the buffered history never does
        let events = bsend(&mut sim, BROADCAST_HOST_CONN, 'z');
        assert!(events.contains(&BroadcastEvent::Received { receiver, ch: 'z' }));
        assert_eq!(rx(&sim, receiver).last.map(|b| b.ch), Some('z'));
        let _ = early;
    }

    #[test]
    fn recv_advances_index_and_reports_last_value() {
        let mut sim = BroadcastSim::new();
        let receiver = subscribe(&mut sim, 4);
        for ch in 'a'..'d' {
            bsend(&mut sim, BROADCAST_HOST_CONN, ch);
        }
        let events = recv(&mut sim, receiver, 4);
        assert_eq!(events, vec![BroadcastEvent::Received { receiver, ch: 'a' }]);
        let state = rx(&sim, receiver);
        assert_eq!(state.last, Some(BufferChar { ch: 'a', conn: 0 }));
        assert_eq!(state.next, 2);

        let events = recv(&mut sim, receiver, 4);
        assert_eq!(events, vec![BroadcastEvent::Received { receiver, ch: 'b' }]);
        assert_eq!(rx(&sim, receiver).next, 3);
    }

    /// Lag now arises the way it does in tokio: subscribe at the tail, then
    /// fall behind while the ring overwrites what you have not read.
    #[test]
    fn recv_yields_lagged_and_resumes_at_oldest() {
        let mut sim = BroadcastSim::new();
        let receiver = subscribe(&mut sim, 4);

        // capacity + 3 sends while the receiver reads nothing: the three
        // oldest values are evicted out from under it
        for ch in ('a'..).take(BROADCAST_CAPACITY + 3) {
            bsend(&mut sim, BROADCAST_HOST_CONN, ch);
        }

        let events = recv(&mut sim, receiver, 4);
        assert_eq!(events, vec![BroadcastEvent::Lagged { receiver, n: 3 }]);
        let state = rx(&sim, receiver);
        assert_eq!(state.lagged_total, 3);
        assert_eq!(state.error, Some(BroadcastError::Lagged(3)));

        // the next receive resumes at the oldest retained value, error cleared
        let events = recv(&mut sim, receiver, 4);
        assert_eq!(events, vec![BroadcastEvent::Received { receiver, ch: 'd' }]);
        assert!(rx(&sim, receiver).error.is_none());
    }

    #[test]
    fn blocked_recv_completes_on_next_send() {
        let mut sim = BroadcastSim::new();
        let receiver = subscribe(&mut sim, 2); // empty channel: next == tail == 0

        // nothing new to receive: the recv blocks
        assert!(recv(&mut sim, receiver, 2).is_empty());
        assert!(rx(&sim, receiver).waiting);

        // the next send completes the blocked recv
        let events = bsend(&mut sim, BROADCAST_HOST_CONN, 'x');
        assert!(events.contains(&BroadcastEvent::Received { receiver, ch: 'x' }));
        let state = rx(&sim, receiver);
        assert!(!state.waiting);
        assert_eq!(
            state.next, 2,
            "armed at tail+1=1, received seq 1, next is 2"
        );
        assert_eq!(state.last, Some(BufferChar { ch: 'x', conn: 0 }));
    }

    #[test]
    fn one_receiver_per_connection() {
        let mut sim = BroadcastSim::new();
        let first = subscribe(&mut sim, 2);
        assert!(sim.handle(&BroadcastWire::Subscribe, 2).is_empty());
        assert_eq!(sim.owned_receivers(2), vec![first]);
    }

    #[test]
    fn foreign_receive_and_unsubscribe_are_ignored() {
        let mut sim = BroadcastSim::new();
        let receiver = subscribe(&mut sim, 2);
        assert!(recv(&mut sim, receiver, 99).is_empty());
        assert!(sim
            .handle(&BroadcastWire::Unsubscribe { receiver }, 99)
            .is_empty());
        assert_eq!(sim.owned_receivers(2), vec![receiver]);

        // the owner drops it
        let events = sim.handle(&BroadcastWire::Unsubscribe { receiver }, 2);
        assert_eq!(events, vec![BroadcastEvent::ReceiverRemoved { receiver }]);
    }

    #[test]
    fn presenter_claim_transfers_host_sender() {
        let mut sim = BroadcastSim::new();
        assert_eq!(
            sim.owner_of_sender(BROADCAST_HOST_CONN),
            Some(BROADCAST_HOST_CONN)
        );

        sim.handle(&BroadcastWire::ClaimPresenter, 9);
        assert_eq!(sim.presenter(), Some(9));
        assert_eq!(sim.owner_of_sender(BROADCAST_HOST_CONN), Some(9));
        assert_eq!(sim.owned_senders(9), vec![BROADCAST_HOST_CONN]);
    }

    /// Regression: closing used to be checked before the buffer, so a
    /// receiver holding unread values got `Closed` and the values were lost.
    /// tokio drains what is retained first and only then reports closure.
    #[test]
    fn closed_channel_drains_buffered_values_first() {
        let mut sim = BroadcastSim::new();
        sim.handle(&BroadcastWire::ClaimPresenter, 1);
        let receiver = subscribe(&mut sim, 2);
        // the presenter owns the host handle once claimed, so it sends
        host_send(&mut sim, 1, 'a');
        host_send(&mut sim, 1, 'b');

        // the presenter leaves: every sender handle is gone
        sim.drop_connection(1);
        assert!(sim.snapshot().senders.is_empty());

        // the buffered values are still owed to the receiver
        let events = recv(&mut sim, receiver, 2);
        assert_eq!(events, vec![BroadcastEvent::Received { receiver, ch: 'a' }]);
        let events = recv(&mut sim, receiver, 2);
        assert_eq!(events, vec![BroadcastEvent::Received { receiver, ch: 'b' }]);

        // only once drained does the channel report itself closed
        assert!(recv(&mut sim, receiver, 2).is_empty());
        assert_eq!(rx(&sim, receiver).error, Some(BroadcastError::Closed));
    }

    #[test]
    fn all_senders_dropped_closes_the_channel() {
        let mut sim = BroadcastSim::new();
        // presenter claims the host sender, a player clones it, then both
        // connections leave: no senders remain
        sim.handle(&BroadcastWire::ClaimPresenter, 9);
        let _ = sim.handle(&BroadcastWire::CloneSender { source: 0 }, 7);
        sim.handle(&BroadcastWire::CloneSender { source: 0 }, 7);

        sim.drop_connection(9); // presenter: host sender drops
        assert!(!sim.contains_sender(BROADCAST_HOST_CONN));
        sim.drop_connection(7); // last owner: channel closed

        assert!(sim.snapshot().senders.is_empty());
        let receiver = subscribe(&mut sim, 42);
        let _ = recv(&mut sim, receiver, 42);
        assert_eq!(
            rx(&sim, receiver).error,
            Some(BroadcastError::Closed),
            "receiving on a closed channel yields the error"
        );
    }

    #[test]
    fn disconnect_drops_owned_handles_but_clones_of_others_survive() {
        let mut sim = BroadcastSim::new();
        // player 2 clones the host sender; the presenter (9) claims it
        let _ = sim.handle(&BroadcastWire::CloneSender { source: 0 }, 2);
        sim.handle(&BroadcastWire::ClaimPresenter, 9);
        assert_eq!(sim.owner_of_sender(0), Some(9));

        // player 2 leaves: its clone drops, the host sender stays
        sim.drop_connection(2);
        assert!(!sim.contains_sender(1));
        assert!(sim.contains_sender(BROADCAST_HOST_CONN));
        assert_eq!(sim.owner_of_sender(BROADCAST_HOST_CONN), Some(9));
    }
}

#[cfg(test)]
mod broadcast_decode_tests {
    use crate::protocol::BroadcastEvent;

    #[test]
    fn decodes_server_snapshot() {
        let json = r#"{"Snapshot":{"state":{"senders":[{"conn":0,"owner":0,"blocked":false}],"buffer":[],"tail":0,"receivers":[],"presenter":null}}}"#;
        let ev: BroadcastEvent = serde_json::from_str(json).expect("decode");
        match ev {
            BroadcastEvent::Snapshot { state } => assert_eq!(state.senders.len(), 1),
            other => panic!("wrong variant {other:?}"),
        }
    }
}
