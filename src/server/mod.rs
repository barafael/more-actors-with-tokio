use axum::extract::ws::{Message, WebSocket, WebSocketUpgrade};
use axum::extract::State;
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

/// Connection ids, unique across every game.
///
/// A connection's id doubles as its palette slot, so the same person must
/// get the same id — and colour — in all four games. Per-game counters made
/// that true only by coincidence, while join counts happened to match.
static NEXT_CONN: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(1);

pub fn next_conn() -> u64 {
    NEXT_CONN.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
}

/// How long a connection may stay silent before it is treated as gone.
/// Clients ping every 3s, so this is about eight missed pings.
pub(crate) const DEAD_PEER_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(25);

/// One game's control plane, as seen by a socket handler.
///
/// The actor behind it is replaced on every restart, so its handles are
/// published on a `watch` rather than kept behind a lock: the supervisor
/// sends, sockets borrow the latest. `None` means no actor is live right
/// now, which is what makes a connecting client retry.
pub struct GamePlane<H> {
    handles: tokio_watch::Receiver<Option<H>>,
    restart_tx: tokio_mpsc::UnboundedSender<()>,
}

impl<H: Clone> GamePlane<H> {
    /// The current actor's handles, if one is live.
    pub fn handles(&self) -> Option<H> {
        self.handles.borrow().clone()
    }

    /// Ask the supervisor to tear this actor down and spawn a fresh one.
    pub fn restart(&self) {
        self.restart_tx
            .send(())
            .inspect_err(|error| tracing::warn!(%error, "restart supervisor is gone"))
            .ok();
    }
}

/// Everything a request handler needs. Held by axum as router state; no
/// globals, and every field is a channel endpoint rather than shared
/// protected state.
#[derive(Clone)]
pub struct AppState(std::sync::Arc<Planes>);

pub struct Planes {
    /// Current slide, with `watch` semantics: late joiners get the value.
    pub slide_tx: tokio_watch::Sender<usize>,
    pub button: GamePlane<ButtonHandles>,
    pub mpsc: GamePlane<MpscHandles>,
    pub watch: GamePlane<WatchHandles>,
    pub broadcast: GamePlane<BroadcastHandles>,
}

impl std::ops::Deref for AppState {
    type Target = Planes;

    fn deref(&self) -> &Self::Target {
        &self.0
    }
}

impl AppState {
    /// Spawn every game actor and return the state that talks to them.
    pub fn spawn() -> Self {
        let (slide_tx, _slide_rx) = tokio_watch::channel(0usize);
        Self(std::sync::Arc::new(Planes {
            slide_tx,
            button: plane(button::backend),
            mpsc: plane(mpsc::backend),
            watch: plane(watch::backend),
            broadcast: plane(broadcast::backend),
        }))
    }

    pub fn plane_for(&self, game: Game) -> &dyn Restartable {
        match game {
            Game::Button => &self.button,
            Game::Mpsc => &self.mpsc,
            Game::Watch => &self.watch,
            Game::Broadcast => &self.broadcast,
        }
    }
}

/// Restarting is the one thing the app plane does uniformly across games.
pub trait Restartable {
    fn restart(&self);
}

impl<H: Clone> Restartable for GamePlane<H> {
    fn restart(&self) {
        GamePlane::restart(self);
    }
}

/// Spawn one game's supervisor and wire up its plane.
fn plane<H, F, Fut>(backend: F) -> GamePlane<H>
where
    H: Send + Sync + 'static,
    F: FnOnce(tokio_mpsc::UnboundedReceiver<()>, tokio_watch::Sender<Option<H>>) -> Fut,
    Fut: std::future::Future<Output = ()> + Send + 'static,
{
    let (restart_tx, restart_rx) = tokio_mpsc::unbounded_channel();
    let (handles_tx, handles_rx) = tokio_watch::channel(None);
    tokio::spawn(backend(restart_rx, handles_tx));
    GamePlane {
        handles: handles_rx,
        restart_tx,
    }
}

/// Supervises one game actor: spawn it, publish its handles, and respawn a
/// fresh one whenever a restart is requested or the actor returns.
///
/// `spawn` builds the actor's channels and returns the handles alongside the
/// running task. The new handles are published before the old ones are
/// cleared, so there is no window in which a connecting client finds no
/// actor at all — which is what a fixed sleep here used to guess at.
pub(crate) async fn supervise<H, S>(
    name: &'static str,
    mut restart: tokio_mpsc::UnboundedReceiver<()>,
    handles_tx: tokio_watch::Sender<Option<H>>,
    mut spawn: S,
) where
    S: FnMut(tokio_util::sync::CancellationToken) -> (H, tokio::task::JoinHandle<()>),
{
    loop {
        let token = tokio_util::sync::CancellationToken::new();
        let (handles, mut task) = spawn(token.clone());
        handles_tx.send_replace(Some(handles));

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
        handles_tx.send_replace(None);
    }
}

/// The websocket planes, as a router awaiting its state. Split out from
/// `serve_app` so tests can mount them on an ephemeral port without dioxus.
pub fn game_routes() -> axum::Router<AppState> {
    axum::Router::new()
        .route("/ws/app", axum::routing::get(app_socket))
        .route("/ws/game/button", axum::routing::get(button::button_socket))
        .route("/ws/game/mpsc", axum::routing::get(mpsc::mpsc_socket))
        .route("/ws/game/watch", axum::routing::get(watch::watch_socket))
        .route(
            "/ws/game/broadcast",
            axum::routing::get(broadcast::broadcast_socket),
        )
}

pub fn serve_app() -> ! {
    dioxus::server::serve(|| async {
        // `with_state` resolves the state away so the result merges into the
        // dioxus router, which is stateless.
        let sockets = game_routes().with_state(AppState::spawn());
        let router = dioxus::server::router(crate::App).merge(sockets);
        Ok::<_, anyhow::Error>(router)
    })
}

async fn app_socket(ws: WebSocketUpgrade, State(state): State<AppState>) -> Response {
    ws.on_upgrade(move |socket| handle_app_socket(socket, state))
}

async fn handle_app_socket(mut socket: WebSocket, state: AppState) {
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
                    Ok(AppUp::Restart { game }) => state.plane_for(game).restart(),
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
