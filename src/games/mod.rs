//! Shared minigame plumbing: one connection type per game component that
//! owns the game websocket (remote mode), the status line and the ready gate
//! that keeps clicks before `connected` from being silently dropped.

use dioxus::prelude::*;
use serde::de::DeserializeOwned;
use serde::Serialize;

use crate::ws_client::{SocketHandle, WsEvent};
use crate::GameMode;

pub mod broadcast;
pub mod button;
pub mod call;
pub mod cycle;
pub mod mpsc;
pub mod mutex;
pub mod oneshot;
pub mod palette;
pub mod qr;
pub mod select;
pub mod speedd;
pub mod ticker;
pub mod timer;
pub mod unit_test;
pub mod watch;

#[derive(Clone, Copy)]
pub struct GameConnection {
    pub handle: SocketHandle,
    pub status: Signal<String>,
    mode: GameMode,
}

impl GameConnection {
    /// True while game actions may hit the wire (always true in local mode).
    pub fn connected(&self) -> bool {
        self.mode == GameMode::Local || self.handle.ready()
    }

    pub fn send<W: Serialize>(&self, wire: &W) {
        self.handle.send_json(wire);
    }

    pub fn is_local(&self) -> bool {
        self.mode == GameMode::Local
    }

    pub fn set_status(mut self, status: impl Into<String>) {
        self.status.set(status.into());
    }
}

/// Mount the game websocket (remote mode only) and forward decoded server
/// events to `on_event`. Call once, inside a game component body.
pub fn use_game_connection<E: DeserializeOwned>(
    path: &'static str,
    on_event: impl FnMut(E) + 'static,
) -> GameConnection {
    // shared with the spawned ws task; wasm is single-threaded
    let on_event = std::rc::Rc::new(std::cell::RefCell::new(on_event));
    let mode = use_context::<GameMode>();
    let socket = use_signal(|| None::<crate::ws_client::RawSocket>);
    let handle = SocketHandle::new(socket);
    let mut status = use_signal(|| match mode {
        GameMode::Local => "single-player".to_string(),
        GameMode::Remote => "connecting…".to_string(),
    });

    let mut handle = handle;
    use_drop(move || handle.close());

    use_effect(move || {
        if mode == GameMode::Local {
            return;
        }
        let on_event = on_event.clone();
        crate::ws_client::spawn_ws_loop(path, move |event| match event {
            WsEvent::Ping => handle.send_raw(crate::protocol::KEEPALIVE),
            WsEvent::Open { socket } => {
                handle.set(socket);
                status.set("connected".to_string());
            }
            WsEvent::Message(text) => {
                if let Ok(event) = serde_json::from_str::<E>(&text) {
                    (on_event.borrow_mut())(event);
                }
            }
            WsEvent::Closed { code } => {
                handle.clear();
                if code == crate::protocol::RESTARTING_CLOSE_CODE {
                    status.set("restarting…".to_string());
                } else {
                    status.set(format!("disconnected ({code})"));
                }
            }
            WsEvent::Retry { after_s } => {
                status.set(format!("retrying in {after_s}s…"));
            }
        });
    });

    GameConnection {
        handle,
        status,
        mode,
    }
}

/// A snapshot, and the moment it reached this client.
///
/// Waits travel as durations as of the snapshot, never as instants: the
/// server's monotonic clock and this browser's share no epoch. The receipt
/// time is what lets a [`Waited`] badge keep counting between snapshots.
pub struct Timed<S: 'static> {
    pub snap: Signal<S>,
    pub received_at: Signal<f64>,
}

impl<S> Clone for Timed<S> {
    fn clone(&self) -> Self {
        *self
    }
}

impl<S> Copy for Timed<S> {}

impl<S> Timed<S> {
    pub fn set(mut self, snap: S) {
        self.snap.set(snap);
        self.received_at.set(crate::sim::now_ms());
    }
}

pub fn use_timed<S>(init: impl FnOnce() -> S) -> Timed<S> {
    Timed {
        snap: use_signal(init),
        received_at: use_signal(crate::sim::now_ms),
    }
}

/// Who is looking at a room game: which connection this client is, and
/// whether it is the export's lone player — who owns every task, so no one
/// task of theirs is "you".
#[derive(Clone, Copy, PartialEq)]
pub struct Viewer {
    pub me: Option<u64>,
    pub local: bool,
}

impl Viewer {
    /// Whether this client may drive a task with this owner.
    pub fn owns(self, owner: u64) -> bool {
        Some(owner) == self.me
    }

    /// Whether to single this task out as "you".
    pub fn is_you(self, owner: u64) -> bool {
        !self.local && self.owns(owner)
    }

    pub fn name(self, task: u64, owner: u64) -> String {
        if self.is_you(owner) {
            "you".to_string()
        } else {
            format!("task {task}")
        }
    }
}

/// One task in its owner's colour, with whatever badge the game gives it.
#[component]
pub fn TaskChip(
    task: u64,
    owner: u64,
    viewer: Viewer,
    class: String,
    children: Element,
) -> Element {
    let you = if viewer.is_you(owner) { " mine" } else { "" };
    rsx! {
        span {
            class: "mx-task {class}{you}",
            style: "--c: {palette::sender_hex(task)};",
            {viewer.name(task, owner)}
            {children}
        }
    }
}

/// A wait that counts itself up: `for_ms` as of the snapshot that arrived
/// at `at`, plus however long ago that was, in whole seconds.
///
/// It reads the frame clock itself, so each frame only this badge redraws;
/// the game around it re-renders when a snapshot arrives and not before.
/// Unbounded by design — nothing times out a parked `lock()` or `send()`.
#[component]
pub fn Waited(
    for_ms: f64,
    at: f64,
    clock: Signal<f64>,
    #[props(default)] label: String,
    #[props(default = "blocked-clock".to_string())] class: String,
) -> Element {
    let seconds = ((for_ms + clock() - at) / 1000.0).floor().max(0.0) as u64;
    rsx! {
        span { class, "{label}{seconds}s" }
    }
}
