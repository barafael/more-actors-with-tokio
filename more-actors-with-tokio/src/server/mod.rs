use std::sync::OnceLock;

use axum::extract::ws::{Message, WebSocket, WebSocketUpgrade};
use axum::response::Response;
use tokio::sync::{mpsc as tokio_mpsc, watch as tokio_watch};

use crate::protocol::{AppDown, AppUp, Game, SLIDE_COUNT};
use crate::server::broadcast::BroadcastHandles;
use crate::server::button::ButtonHandles;
use crate::server::mpsc::MpscHandles;
use crate::server::watch::WatchHandles;

pub mod broadcast;
pub mod button;
pub mod mpsc;
pub mod watch;

/// How long a connection may stay silent before it is treated as gone.
/// Clients ping every 3s, so this is about eight missed pings.
pub(crate) const DEAD_PEER_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(25);

pub struct AppState {
    pub slide_tx: tokio_watch::Sender<usize>,
    pub button_restart_tx: tokio_mpsc::UnboundedSender<()>,
    pub mpsc_restart_tx: tokio_mpsc::UnboundedSender<()>,
    pub watch_restart_tx: tokio_mpsc::UnboundedSender<()>,
    pub broadcast_restart_tx: tokio_mpsc::UnboundedSender<()>,
}

static STATE: OnceLock<AppState> = OnceLock::new();
static BUTTON: std::sync::RwLock<Option<ButtonHandles>> = std::sync::RwLock::new(None);
static MPSC: std::sync::RwLock<Option<MpscHandles>> = std::sync::RwLock::new(None);
static WATCH: std::sync::RwLock<Option<WatchHandles>> = std::sync::RwLock::new(None);
static BROADCAST: std::sync::RwLock<Option<BroadcastHandles>> = std::sync::RwLock::new(None);

pub fn state() -> &'static AppState {
    STATE.get_or_init(|| {
        let (slide_tx, _slide_rx) = tokio_watch::channel(0usize);
        let (button_restart_tx, button_restart_rx) = tokio_mpsc::unbounded_channel();
        let (mpsc_restart_tx, mpsc_restart_rx) = tokio_mpsc::unbounded_channel();
        let (watch_restart_tx, watch_restart_rx) = tokio_mpsc::unbounded_channel();
        let (broadcast_restart_tx, broadcast_restart_rx) = tokio_mpsc::unbounded_channel();
        tokio::spawn(button::backend(button_restart_rx));
        tokio::spawn(mpsc::backend(mpsc_restart_rx));
        tokio::spawn(watch::backend(watch_restart_rx));
        tokio::spawn(broadcast::backend(broadcast_restart_rx));
        AppState {
            slide_tx,
            button_restart_tx,
            mpsc_restart_tx,
            watch_restart_tx,
            broadcast_restart_tx,
        }
    })
}

/// Supervises one game actor: spawn it, hand its handles out, and respawn a
/// fresh one whenever a restart is requested or the actor returns.
///
/// `spawn` builds the actor's channels and returns the handles alongside the
/// running task; `install` publishes them for socket handlers to pick up.
/// The new handles are installed before the old ones are cleared, so there
/// is no window in which a connecting client finds no actor at all — which
/// is what the previous fixed 50ms sleep was guessing at.
pub(crate) async fn supervise<H, F, S>(
    name: &'static str,
    mut restart: tokio_mpsc::UnboundedReceiver<()>,
    mut spawn: S,
    install: F,
) -> !
where
    F: Fn(Option<H>),
    S: FnMut(tokio_util::sync::CancellationToken) -> (H, tokio::task::JoinHandle<()>),
{
    loop {
        let token = tokio_util::sync::CancellationToken::new();
        let (handles, mut task) = spawn(token.clone());
        install(Some(handles));

        let outcome = tokio::select! {
            _ = restart.recv() => {
                token.cancel();
                task.await
            }
            outcome = &mut task => outcome,
        };

        if let Err(error) = outcome {
            // a panicking actor would otherwise respawn silently forever
            tracing::error!(%error, game = name, "actor task failed");
        }
        install(None);
    }
}

pub fn button_handles() -> Option<ButtonHandles> {
    BUTTON.read().ok()?.clone()
}

pub fn set_button_handles(handles: Option<ButtonHandles>) {
    *BUTTON.write().expect("button handles lock") = handles;
}

pub fn mpsc_handles() -> Option<MpscHandles> {
    MPSC.read().ok()?.clone()
}

pub fn set_mpsc_handles(handles: Option<MpscHandles>) {
    *MPSC.write().expect("mpsc handles lock") = handles;
}

pub fn watch_handles() -> Option<WatchHandles> {
    WATCH.read().ok()?.clone()
}

pub fn set_watch_handles(handles: Option<WatchHandles>) {
    *WATCH.write().expect("watch handles lock") = handles;
}

pub fn broadcast_handles() -> Option<BroadcastHandles> {
    BROADCAST.read().ok()?.clone()
}

pub fn set_broadcast_handles(handles: Option<BroadcastHandles>) {
    *BROADCAST.write().expect("broadcast handles lock") = handles;
}

pub fn serve_app() -> ! {
    dioxus::server::serve(|| async {
        let _state = state();
        let router = dioxus::server::router(crate::App)
            .route("/ws/app", axum::routing::get(app_socket))
            .route("/ws/game/button", axum::routing::get(button::button_socket))
            .route("/ws/game/mpsc", axum::routing::get(mpsc::mpsc_socket))
            .route("/ws/game/watch", axum::routing::get(watch::watch_socket))
            .route(
                "/ws/game/broadcast",
                axum::routing::get(broadcast::broadcast_socket),
            );
        Ok::<_, anyhow::Error>(router)
    })
}

async fn app_socket(ws: WebSocketUpgrade) -> Response {
    ws.on_upgrade(handle_app_socket)
}

async fn handle_app_socket(mut socket: WebSocket) {
    let state = state();
    let mut slide_rx = state.slide_tx.subscribe();

    let down = AppDown {
        slide: *slide_rx.borrow_and_update(),
    };
    if send_json(&mut socket, &down).await.is_err() {
        return;
    }

    loop {
        tokio::select! {
            changed = slide_rx.changed() => {
                if changed.is_err() {
                    break;
                }
                let down = AppDown {
                    slide: *slide_rx.borrow_and_update(),
                };
                if send_json(&mut socket, &down).await.is_err() {
                    break;
                }
            }
            msg = socket.recv() => match msg {
                Some(Ok(Message::Text(text))) => match serde_json::from_str::<AppUp>(text.as_str()) {
                    Ok(AppUp::AdvanceSlide) => {
                        state.slide_tx.send_modify(|slide| *slide = (*slide + 1) % SLIDE_COUNT);
                    }
                    Ok(AppUp::PreviousSlide) => {
                        state.slide_tx.send_modify(|slide| *slide = (*slide + SLIDE_COUNT - 1) % SLIDE_COUNT);
                    }
                    // exhaustive: a new game cannot be silently unroutable
                    Ok(AppUp::Restart { game }) => {
                        let restart = match game {
                            Game::Button => &state.button_restart_tx,
                            Game::Mpsc => &state.mpsc_restart_tx,
                            Game::Watch => &state.watch_restart_tx,
                            Game::Broadcast => &state.broadcast_restart_tx,
                        };
                        restart
                            .send(())
                            .inspect_err(|error| {
                                tracing::warn!(%error, ?game, "restart supervisor is gone");
                            })
                            .ok();
                    }
                    Err(error) => {
                        tracing::debug!(%error, "ignoring undecodable app message");
                    }
                },
                Some(Ok(_)) | None | Some(Err(_)) => break,
            },
        }
    }
}

pub(crate) async fn send_json(
    socket: &mut WebSocket,
    value: &impl serde::Serialize,
) -> Result<(), axum::Error> {
    let json = serde_json::to_string(value).expect("serialize message");
    socket.send(Message::text(json)).await
}
