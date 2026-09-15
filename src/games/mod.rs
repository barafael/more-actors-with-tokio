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
pub mod mpsc;
pub mod palette;
pub mod qr;
pub mod select;
pub mod ticker;
pub mod timer;
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
