//! The mutex and call-and-response games, as [`RoomGame`] impls.
//!
//! What differs between them is only who takes part and what a role may
//! do; the actor, relay and supervisor are all in [`super::room`].

use axum::extract::ws::{WebSocket, WebSocketUpgrade};
use axum::response::Response;

use crate::protocol::{CallEvent, CallWire, MutexEvent, MutexWire, Role};
use crate::server::auth::Connecting;
use crate::server::room::{handle_socket, RoomGame, RoomHandles};
use crate::server::AppState;
use crate::sim_call::CallSim;
use crate::sim_mutex::MutexSim;

pub type MutexHandles = RoomHandles<MutexSim>;
pub type CallHandles = RoomHandles<CallSim>;

impl RoomGame for MutexSim {
    type Wire = MutexWire;
    type Event = MutexEvent;

    const NAME: &'static str = "mutex";

    fn fresh() -> Self {
        MutexSim::new()
    }

    fn hello(conn: u64) -> MutexEvent {
        MutexEvent::Hello { conn }
    }

    fn sync_now(&mut self, now: f64) {
        MutexSim::sync_now(self, now);
    }

    /// Everyone who may play is a task, the presenter included: the hook
    /// works best when the person on stage gets stuck in the queue too.
    fn join(&mut self, conn: u64, role: Role) {
        if role.may_play() {
            self.add_task(conn, conn);
        }
    }

    fn leave(&mut self, conn: u64) {
        self.remove_task(conn);
    }

    fn handle(&mut self, conn: u64, _role: Role, wire: MutexWire) {
        MutexSim::handle(self, conn, wire);
    }

    fn snapshot_event(&self) -> MutexEvent {
        MutexEvent::Snapshot {
            state: self.snapshot(),
        }
    }
}

impl RoomGame for CallSim {
    type Wire = CallWire;
    type Event = CallEvent;

    const NAME: &'static str = "call";

    fn fresh() -> Self {
        CallSim::default()
    }

    fn hello(conn: u64) -> CallEvent {
        CallEvent::Hello { conn }
    }

    fn sync_now(&mut self, now: f64) {
        CallSim::sync_now(self, now);
    }

    /// Players are the requesters; the presenter is the actor's loop and
    /// does not queue requests to itself.
    fn join(&mut self, conn: u64, role: Role) {
        if role == Role::Player {
            self.add_task(conn, conn);
        }
    }

    fn leave(&mut self, conn: u64) {
        self.remove_task(conn);
    }

    fn handle(&mut self, conn: u64, role: Role, wire: CallWire) {
        CallSim::handle(self, conn, role.may_present(), wire);
    }

    fn snapshot_event(&self) -> CallEvent {
        CallEvent::Snapshot {
            state: self.snapshot(),
        }
    }
}

pub async fn mutex_socket(
    ws: WebSocketUpgrade,
    connecting: Connecting,
    axum::extract::State(state): axum::extract::State<AppState>,
) -> Response {
    ws.on_upgrade(move |socket: WebSocket| {
        handle_socket::<MutexSim>(socket, state, connecting, |app| app.mutex.handles())
    })
}

pub async fn call_socket(
    ws: WebSocketUpgrade,
    connecting: Connecting,
    axum::extract::State(state): axum::extract::State<AppState>,
) -> Response {
    ws.on_upgrade(move |socket: WebSocket| {
        handle_socket::<CallSim>(socket, state, connecting, |app| app.call.handles())
    })
}
