//! The three clock-driven games, as [`TickingGame`] impls.
//!
//! Each is a handful of lines because the actor body, the supervisor and the
//! socket relay are all in [`super::ticking`]. What is left here is the part
//! that genuinely differs between the games: which sim, which wire, which
//! snapshot.

use axum::extract::ws::{WebSocket, WebSocketUpgrade};
use axum::response::Response;

use crate::protocol::{
    LoopSelectEvent, LoopSelectWire, SelectEvent, SelectWire, TimerEvent, TimerWire,
};
use crate::server::auth::Connecting;
use crate::server::ticking::{handle_socket, TickingGame, TickingHandles};
use crate::server::AppState;
use crate::sim_timer::{LoopSelectSim, SelectSim, TimerSim};

pub type TimerHandles = TickingHandles<TimerSim>;
pub type SelectHandles = TickingHandles<SelectSim>;
pub type LoopSelectHandles = TickingHandles<LoopSelectSim>;

impl TickingGame for TimerSim {
    type Ev = TimerEvent;
    type Wire = TimerWire;

    const NAME: &'static str = "timer";

    fn fresh(epoch_offset_ms: f64) -> Self {
        TimerSim::new(epoch_offset_ms)
    }

    fn handle(&mut self, wire: &TimerWire) -> Vec<TimerEvent> {
        TimerSim::handle(self, wire)
    }

    fn snapshot_event(&self) -> TimerEvent {
        TimerEvent::Snapshot {
            state: self.snapshot(),
        }
    }
}

impl TickingGame for SelectSim {
    type Ev = SelectEvent;
    type Wire = SelectWire;

    const NAME: &'static str = "select";

    fn fresh(epoch_offset_ms: f64) -> Self {
        SelectSim::new(epoch_offset_ms)
    }

    fn handle(&mut self, wire: &SelectWire) -> Vec<SelectEvent> {
        SelectSim::handle(self, wire)
    }

    fn snapshot_event(&self) -> SelectEvent {
        SelectEvent::Snapshot {
            state: self.snapshot(),
        }
    }
}

impl TickingGame for LoopSelectSim {
    type Ev = LoopSelectEvent;
    type Wire = LoopSelectWire;

    const NAME: &'static str = "loop-select";

    fn fresh(epoch_offset_ms: f64) -> Self {
        LoopSelectSim::new(epoch_offset_ms)
    }

    fn handle(&mut self, wire: &LoopSelectWire) -> Vec<LoopSelectEvent> {
        LoopSelectSim::handle(self, wire)
    }

    fn snapshot_event(&self) -> LoopSelectEvent {
        LoopSelectEvent::Snapshot {
            state: self.snapshot(),
        }
    }
}

// The `Ticking` impls live with the sims; these re-export the actor plumbing
// under one name per game so `server::mod` can route to them uniformly.

pub async fn timer_socket(
    ws: WebSocketUpgrade,
    connecting: Connecting,
    axum::extract::State(state): axum::extract::State<AppState>,
) -> Response {
    ws.on_upgrade(move |socket: WebSocket| {
        handle_socket::<TimerSim>(socket, state, connecting, |app| app.timer.handles())
    })
}

pub async fn select_socket(
    ws: WebSocketUpgrade,
    connecting: Connecting,
    axum::extract::State(state): axum::extract::State<AppState>,
) -> Response {
    ws.on_upgrade(move |socket: WebSocket| {
        handle_socket::<SelectSim>(socket, state, connecting, |app| app.select.handles())
    })
}

pub async fn loop_select_socket(
    ws: WebSocketUpgrade,
    connecting: Connecting,
    axum::extract::State(state): axum::extract::State<AppState>,
) -> Response {
    ws.on_upgrade(move |socket: WebSocket| {
        handle_socket::<LoopSelectSim>(socket, state, connecting, |app| app.loop_select.handles())
    })
}
