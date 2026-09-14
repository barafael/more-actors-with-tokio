use serde::{Deserialize, Serialize};

pub const SLIDE_COUNT: usize = 6;

pub const MPSC_CAPACITY: usize = 5;

/// Capacity of the broadcast channel's ring buffer.
pub const BROADCAST_CAPACITY: usize = 5;

/// Per-owner cap on live receivers for the watch game.
pub const WATCH_MAX_RX: usize = 6;

/// How long a value spends in flight, in milliseconds: the server actor
/// schedules landings with it and the client animates over the same span.
pub const MPSC_FLIGHT_MS: u64 = 700;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum Color {
    Red,
    Green,
    Blue,
}

impl Color {
    pub fn css_name(self) -> &'static str {
        match self {
            Color::Red => "red",
            Color::Green => "green",
            Color::Blue => "blue",
        }
    }

    pub fn display(self) -> &'static str {
        match self {
            Color::Red => "Red",
            Color::Green => "Green",
            Color::Blue => "Blue",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum ButtonState {
    Idle,
    Pending,
    Ready(Color),
}

impl ButtonState {
    pub fn css_name(self) -> &'static str {
        match self {
            ButtonState::Idle => "idle",
            ButtonState::Pending => "pending",
            ButtonState::Ready(_) => "ready",
        }
    }
}

/// The four minigames, each backed by one actor on the server.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum Game {
    Button,
    Mpsc,
    Watch,
    Broadcast,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum AppUp {
    AdvanceSlide,
    PreviousSlide,
    Restart { game: Game },
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
pub struct AppDown {
    pub slide: usize,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
pub enum ButtonWire {
    Activate,
    Press { color: Color },
}

#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub enum ButtonEvent {
    Snapshot { state: ButtonState },
    Activated,
    Resolved { color: Color },
}

/// One live `Sender<T>` handle: a connected client (`conn == owner`) or a
/// clone of one (`owner` = the handle it was cloned from).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct SenderInfo {
    pub conn: u64,
    pub owner: u64,
    pub blocked: bool,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
pub enum MpscWire {
    /// Send a value from the handle `conn`. Honored as-is by the local
    /// (loopback) driver; the server validates that `conn` is a handle the
    /// connection owns (itself or one of its clones) and rejects foreign
    /// ones.
    Send {
        conn: u64,
        ch: char,
    },
    Receive,
    /// Clone the requesting sender. `conn` is the fresh handle's id, allocated
    /// by the driver (server or local client); the request itself is
    /// authenticated by the connection the wire arrives on. Only the
    /// connection currently holding the presenter slot may clone.
    CloneSender {
        conn: u64,
    },
    /// Claim the presenter slot for this connection (last claim wins). The
    /// slot is held until the claiming connection's socket closes or the
    /// actor restarts.
    ClaimPresenter,
}

/// A char on its way through the channel, tagged with the sender it came from.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct BufferChar {
    pub ch: char,
    pub conn: u64,
}

/// The full authoritative state of the channel actor. Broadcast after every
/// state change; clients replace their local copy wholesale and derive
/// everything except in-flight animations from it.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct MpscSnapshot {
    pub senders: Vec<SenderInfo>,
    pub buffer: Vec<BufferChar>,
    /// Sends queued waiting for capacity, in FIFO wake order.
    pub blocked_sends: Vec<BufferChar>,
    pub in_flight: usize,
    pub waiting_receive: bool,
    pub last_received: Option<BufferChar>,
}

/// Server → client messages. `Snapshot` is the only authority; everything
/// else is a cosmetic trigger (animations) that never mutates channel state
/// on the client.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum MpscEvent {
    Hello {
        conn: u64,
    },
    Snapshot {
        state: MpscSnapshot,
    },
    SenderJoined {
        conn: u64,
        owner: u64,
    },
    SenderLeft {
        conn: u64,
    },
    /// A value left sender `conn` and is on its way (accepted or unblocked).
    InFlight {
        conn: u64,
        ch: char,
    },
    /// A value finished its flight (into the buffer or a waiting receiver).
    Consumed {
        conn: u64,
        ch: char,
    },
}

// ---- watch ----

/// One live `Receiver<T>` handle. `owner` is the connection that created (or
/// cloned) it and may operate it. `version` is the highest version the
/// receiver has observed so far (its `changed()` checkpoint).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct RxInfo {
    pub id: u64,
    pub owner: u64,
    pub version: u64,
    /// Blocked in `changed()`, waiting for the next *different* value.
    pub awaiting: bool,
    /// Holding a `borrow()` read guard; while any guard is held the
    /// presenter's send is blocked.
    pub borrowing: bool,
}

/// The full authoritative state of the watch actor. Unlike mpsc this is a
/// single shared cell, so sends move atomically: `pending_send` only exists
/// while a send is blocked by outstanding borrows.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct WatchSnapshot {
    /// Whether the presenter has created the channel (the create phase).
    pub created: bool,
    /// The contained value (the sender's rectangle shows it).
    pub value: Option<char>,
    /// Bumps on every accepted (non-dedup) send.
    pub version: u64,
    /// A send queued behind borrows, or None.
    pub pending_send: Option<char>,
    pub presenter: Option<u64>,
    pub receivers: Vec<RxInfo>,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
pub enum WatchWire {
    /// Presenter creates the channel with an initial value (tokio's
    /// `watch::channel(init)`).
    Create {
        init: Option<char>,
    },
    /// Presenter replaces the contained value.
    Send {
        ch: char,
    },
    /// Subscribe a brand-new receiver owned by the sender of this wire; it
    /// instantly starts at the current value.
    NewReceiver,
    /// Clone the receiver `rx` for the sender of this wire. Cloning preserves
    /// the source's seen-version but starts fresh (not awaiting/borrowing).
    /// Allocation and authorization happen driver/actor-side.
    CloneReceiver {
        rx: u64,
    },
    /// The owner toggles `changed()` on their receiver.
    AwaitChange {
        rx: u64,
    },
    /// The owner toggles a `borrow()` read guard on their receiver.
    LookInside {
        rx: u64,
    },
    /// The owner drops their receiver.
    DropReceiver {
        rx: u64,
    },
    ClaimPresenter,
}

/// Server → client messages. `Snapshot` is the only authority; everything
/// else is a cosmetic trigger that never mutates channel state on the client.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum WatchEvent {
    Hello {
        conn: u64,
    },
    Snapshot {
        state: WatchSnapshot,
    },
    Created,
    /// An accepted send landed: flights from the cell to every receiver.
    Changed {
        version: u64,
        ch: char,
    },
    /// A `changed()` that was already stale completed at once, without
    /// waiting for a send. Purely a cue: the state is in the snapshot.
    ChangedImmediately {
        rx: u64,
    },
    SameValue,
    SendRefused,
    SendBlocked,
    /// Cosmetics for a `borrow()`: a gray flight from the cell to the rx.
    BorrowFlight {
        rx: u64,
    },
    ReceiverAdded {
        id: u64,
        owner: u64,
    },
    ReceiverRemoved {
        id: u64,
    },
}

// ---- broadcast ----

/// What a receiver's last `recv()` yielded once it had fallen behind.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum BroadcastError {
    /// `RecvError::Lagged(n)`: the receiver skipped `n` evicted values; its
    /// index was reset to the oldest buffered value.
    Lagged(u64),
    /// `RecvError::Closed`: every sender handle has been dropped.
    Closed,
}

/// One `Receiver<T>` handle. `owner` is the connection that subscribed and
/// may operate it. `next` is the seq of the next value it will receive
/// (values are numbered from 1). `lagged_total` accumulates every missed
/// value; `error` holds the pending `Lagged`/`Closed` outcome.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct RxState {
    pub receiver: u64,
    pub conn: u64,
    pub owner: u64,
    pub next: u64,
    pub lagged_total: u64,
    pub last: Option<BufferChar>,
    pub error: Option<BroadcastError>,
    /// Blocked in `recv()`: armed, completes on the next send.
    pub waiting: bool,
}

/// The full authoritative state of the broadcast actor. `tail` is the number
/// of values ever accepted; the buffer holds the last `min(tail, capacity)`
/// of them, oldest first, so a receiver's lag is derivable as
/// `tail - buffer.len() - next`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct BroadcastSnapshot {
    pub senders: Vec<SenderInfo>,
    pub buffer: Vec<BufferChar>,
    pub tail: u64,
    pub receivers: Vec<RxState>,
    pub presenter: Option<u64>,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
pub enum BroadcastWire {
    /// Send from the handle `conn` (must be owned by the connection).
    Send { conn: u64, ch: char },
    /// Clone the sender `source` (any live handle); the fresh handle is owned
    /// by the sender of this wire. Allocation is driver/actor-side.
    CloneSender { source: u64 },
    /// Subscribe a receiver owned by the sender of this wire (one per
    /// connection; like tokio's `subscribe` it starts at the tail, so it
    /// only sees values sent after it subscribed).
    Subscribe,
    /// Drop the receiver `receiver` (must be owned by the connection).
    Unsubscribe { receiver: u64 },
    /// Receive on the receiver `receiver` (must be owned by the connection).
    Receive { receiver: u64 },
    /// Claim the presenter slot: ownership of host-owned senders transfers to
    /// this connection (last claim wins).
    ClaimPresenter,
}

/// Server → client messages. `Snapshot` is the only authority; everything
/// else is a cosmetic trigger (flights, flashes, badges).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum BroadcastEvent {
    Hello {
        conn: u64,
    },
    Snapshot {
        state: BroadcastSnapshot,
    },
    SenderJoined {
        conn: u64,
        owner: u64,
    },
    SenderLeft {
        conn: u64,
    },
    ReceiverAdded {
        receiver: u64,
        owner: u64,
    },
    ReceiverRemoved {
        receiver: u64,
    },
    /// A value left sender `conn` and is flying into the buffer tail.
    InFlight {
        conn: u64,
        ch: char,
    },
    /// `send` failed: a broadcast channel with no receivers left has nobody
    /// to deliver to, so tokio hands the value back in `SendError`.
    SendRefused,
    /// The oldest buffered value was evicted by a send.
    Evicted {
        ch: char,
    },
    /// A receiver consumed a value.
    Received {
        receiver: u64,
        ch: char,
    },
    /// A receiver woke up lagged: it missed `n` evicted values.
    Lagged {
        receiver: u64,
        n: u64,
    },
}
