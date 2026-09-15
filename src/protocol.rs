use serde::{Deserialize, Serialize};

pub const SLIDE_COUNT: usize = 9;

pub const MPSC_CAPACITY: usize = 5;

/// Capacity of the broadcast channel's ring buffer.
pub const BROADCAST_CAPACITY: usize = 5;

/// Per-owner cap on live receivers for the watch game.
pub const WATCH_MAX_RX: usize = 6;

/// How long a value spends in flight, in milliseconds: the server actor
/// schedules landings with it and the client animates over the same span.
pub const MPSC_FLIGHT_MS: u64 = 700;

/// The client's keep-alive frame.
///
/// Deliberately not JSON: its only job is to prove the peer is alive, and
/// the arrival of the frame does that. Servers recognise it by value so a
/// heartbeat is never mistaken for a malformed command — logging one of
/// these every three seconds per phone would bury anything real.
pub const KEEPALIVE: &str = "ping";

/// The websocket close code a game socket sends when its actor is being
/// restarted.
///
/// In the private range (4000-4999) so it cannot collide with a protocol
/// code. The client recognises it as "your actor is coming back" and retries
/// rather than reporting a disconnection.
pub const RESTARTING_CLOSE_CODE: u16 = 4001;

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

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum ButtonState {
    #[default]
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

/// The minigames, each backed by one actor on the server.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum Game {
    Timer,
    Button,
    Select,
    LoopSelect,
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

/// The app plane's state, pushed on connect and on every change.
///
/// Carries the connection's decided identity: the client never assumes a
/// role, it renders whatever the server granted.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AppDown {
    pub slide: usize,
    pub role: Role,
    /// 1-based seat number for a ticket holder.
    pub seat: Option<usize>,
    /// The ticket the server issued, when this connection arrived without
    /// one. The client puts it in its URL so a reload keeps the seat.
    pub granted_ticket: Option<String>,
    /// Seats with a live socket right now, and the size of the pool.
    pub players_present: usize,
    pub players_capacity: usize,
    /// The URL the QR code encodes; only sent to the presenter.
    pub join_url: Option<String>,
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

// ---- timer, select and loop-select ----

/// The timer future's period, in seconds: it resolves when the wall clock's
/// seconds next reach a multiple of this.
pub const TIMER_PERIOD_S: u64 = 10;

/// How many completed rounds the loop-select tape shows.
pub const LOOP_SELECT_HISTORY: usize = 8;

#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub enum TimerWire {
    /// Await the timer: it resolves at the next period boundary.
    Activate,
    /// Drop the pending future without awaiting it.
    Cancel,
}

#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub enum TimerEvent {
    Snapshot {
        state: TimerSnapshot,
    },
    /// The future was created and will resolve in `waiting_ms`.
    Activated {
        waiting_ms: f64,
    },
    /// It resolved, yielding how long it waited.
    Resolved {
        waited_s: f64,
    },
    Cancelled,
}

/// The timer's full state. `wall_ms` drives the seconds dial, so the server
/// and every phone draw the same hand in the same place.
#[derive(Debug, Clone, Copy, Default, PartialEq, Serialize, Deserialize)]
pub struct TimerSnapshot {
    pub pending: bool,
    /// Milliseconds into the current minute: where the dial's hand points.
    pub wall_ms: f64,
    /// Time left on the pending future, or `None` when idle.
    pub remaining_ms: Option<f64>,
    /// What the last completed await yielded.
    pub waited_s: Option<f64>,
}

/// Which branch of the `select!` completed first.
///
/// Carries the branch's value, not just its name: the point of the slide is
/// that a select yields whatever the winning future yielded, and the two
/// branches yield different types.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub enum Won {
    Timer { waited_s: f64 },
    Button { color: Color },
}

impl Won {
    pub fn css_name(self) -> &'static str {
        match self {
            Won::Timer { .. } => "timer",
            Won::Button { color } => color.css_name(),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub enum SelectWire {
    /// Enter the select, which creates both futures.
    Arm,
    Press {
        color: Color,
    },
    /// Leave the select, dropping both branches.
    Reset,
}

#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub enum SelectEvent {
    Snapshot { state: SelectSnapshot },
    Armed,
    Won { won: Won },
    Reset,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Serialize, Deserialize)]
pub struct SelectSnapshot {
    /// Inside the select, with both branches live.
    pub armed: bool,
    pub timer: TimerSnapshot,
    pub button: ButtonState,
    pub winner: Option<Won>,
}

/// One completed trip around the loop.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct SelectWinner {
    /// 1-based, and never reused: the client keys its tape on it.
    pub round: u64,
    pub won: Won,
}

#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub enum LoopSelectWire {
    Start,
    Stop,
    Press {
        color: Color,
    },
    /// Empty the tape without breaking the loop.
    Clear,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum LoopSelectEvent {
    Snapshot {
        state: LoopSelectSnapshot,
    },
    Started,
    Stopped,
    /// A round finished; the loop is about to go around again.
    Completed {
        winner: SelectWinner,
    },
    Cleared,
    /// What the inner select did. Nested rather than flattened so the two
    /// games can share the select's rendering.
    Select(SelectEvent),
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct LoopSelectSnapshot {
    pub running: bool,
    pub select: SelectSnapshot,
    /// The last `LOOP_SELECT_HISTORY` rounds, oldest first.
    pub history: Vec<SelectWinner>,
    /// Rounds completed since the actor started (or since the tape was
    /// cleared), which is also the newest round's number.
    pub rounds: u64,
}

// ---- tickets and the presenter slot ----

/// How many audience members may hold a player ticket at once.
///
/// From CONCEPT.md: the room gets a fixed pool of handles, so "the channel
/// is full" is a fact about the room rather than a number on a slide. Past
/// this, joiners are seated as spectators.
pub const PLAYER_TICKETS: usize = 24;

/// The query parameter carrying a ticket across the app and game sockets.
/// The QR code encodes a URL with this set.
pub const TICKET_PARAM: &str = "t";

/// The query parameter carrying the presenter secret.
pub const PRESENTER_PARAM: &str = "k";

/// What a connection is allowed to do, decided by the server at connect
/// time and echoed to the client so the UI can match it.
///
/// The client never picks its own role: it presents whatever credentials it
/// has in its URL and the server rules on them. Screen size decided this
/// once; a media query is not an authorization check.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
pub enum Role {
    /// No ticket: may watch, may not act. The default for anyone who opens
    /// the URL without scanning.
    #[default]
    Spectator,
    /// Holds one of the `PLAYER_TICKETS` handles: may play every game.
    Player,
    /// Presented the presenter secret: may play, drive slides, claim the
    /// presenter slot in each game, and restart actors.
    Presenter,
}

impl Role {
    /// Whether this role may act in the minigames at all.
    pub fn may_play(self) -> bool {
        matches!(self, Role::Player | Role::Presenter)
    }

    /// Whether this role may drive the deck and the game control slots.
    pub fn may_present(self) -> bool {
        matches!(self, Role::Presenter)
    }

    pub fn label(self) -> &'static str {
        match self {
            Role::Spectator => "spectator",
            Role::Player => "player",
            Role::Presenter => "presenter",
        }
    }
}

/// The credentials a client presents when opening any socket, read from its
/// own URL. Absent fields simply mean "not claiming that".
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Credentials {
    pub ticket: Option<String>,
    pub presenter_key: Option<String>,
}
