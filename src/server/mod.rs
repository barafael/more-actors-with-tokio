use axum::extract::ws::{Message, WebSocket, WebSocketUpgrade};
use axum::extract::State;
use axum::response::Response;
use tokio::sync::{mpsc as tokio_mpsc, watch as tokio_watch};

use crate::protocol::{AppDown, AppUp, Game, KEEPALIVE, SLIDE_COUNT};
use crate::server::auth::{Connecting, Joining};
use crate::server::broadcast::BroadcastHandles;
use crate::server::button::ButtonHandles;
use crate::server::futures_games::{LoopSelectHandles, SelectHandles, TimerHandles};
use crate::server::mpsc::MpscHandles;
use crate::server::tickets::Tickets;
use crate::server::watch::WatchHandles;

pub mod auth;
pub mod broadcast;
pub mod button;
pub mod futures_games;
pub mod mpsc;
pub mod tickets;
pub mod ticking;
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
    /// The player-ticket pool: who in the room holds a handle.
    pub tickets: Tickets,
    /// The three clock-driven games (CONCEPT.md §2–§4), which resolve on
    /// their own as well as on a wire.
    pub timer: GamePlane<TimerHandles>,
    pub button: GamePlane<ButtonHandles>,
    pub select: GamePlane<SelectHandles>,
    pub loop_select: GamePlane<LoopSelectHandles>,
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
            tickets: Tickets::spawn(),
            timer: plane(ticking::backend),
            button: plane(button::backend),
            select: plane(ticking::backend),
            loop_select: plane(ticking::backend),
            mpsc: plane(mpsc::backend),
            watch: plane(watch::backend),
            broadcast: plane(broadcast::backend),
        }))
    }

    pub fn plane_for(&self, game: Game) -> &dyn Restartable {
        match game {
            Game::Timer => &self.timer,
            Game::Button => &self.button,
            Game::Select => &self.select,
            Game::LoopSelect => &self.loop_select,
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
        .route(
            "/ws/game/timer",
            axum::routing::get(futures_games::timer_socket),
        )
        .route("/ws/game/button", axum::routing::get(button::button_socket))
        .route(
            "/ws/game/select",
            axum::routing::get(futures_games::select_socket),
        )
        .route(
            "/ws/game/loop-select",
            axum::routing::get(futures_games::loop_select_socket),
        )
        .route("/ws/game/mpsc", axum::routing::get(mpsc::mpsc_socket))
        .route("/ws/game/watch", axum::routing::get(watch::watch_socket))
        .route(
            "/ws/game/broadcast",
            axum::routing::get(broadcast::broadcast_socket),
        )
}

pub fn serve_app() -> ! {
    announce_presenter_key();
    dioxus::server::serve(|| async {
        // `with_state` resolves the state away so the result merges into the
        // dioxus router, which is stateless.
        let sockets = game_routes().with_state(AppState::spawn());
        let router = dioxus::server::router(crate::App).merge(sockets);
        Ok::<_, anyhow::Error>(router)
    })
}

async fn app_socket(
    ws: WebSocketUpgrade,
    Joining(connecting): Joining,
    State(state): State<AppState>,
) -> Response {
    ws.on_upgrade(move |socket| handle_app_socket(socket, state, connecting))
}

/// Build the app-plane payload for one connection.
///
/// `granted_ticket` is only worth sending once — on the first push, when the
/// client may not have one yet. The join URL is presenter-only: putting it
/// in every payload would hand the audience the address to share onward.
async fn app_down(
    state: &AppState,
    slide: usize,
    identity: &auth::Identity,
    include_ticket: bool,
) -> AppDown {
    let headcount = state.tickets.headcount().await;
    AppDown {
        slide,
        role: identity.role,
        seat: identity.seat,
        granted_ticket: if include_ticket {
            identity.ticket.clone()
        } else {
            None
        },
        players_present: headcount.present,
        players_capacity: headcount.capacity,
        join_url: identity
            .may_present()
            .then(|| join_url(identity.loopback_host.as_deref()))
            .flatten(),
    }
}

async fn handle_app_socket(mut socket: WebSocket, state: AppState, connecting: Connecting) {
    let Connecting { identity, conn } = connecting;
    tracing::info!(conn, role = identity.role.label(), seat = ?identity.seat, "app socket");

    let mut slide_rx = state.slide_tx.subscribe();
    let slide = *slide_rx.borrow_and_update();
    let down = app_down(&state, slide, &identity, true).await;
    if send_json(&mut socket, &down).await.is_err() {
        release(&state, &identity);
        return;
    }

    loop {
        tokio::select! {
            changed = slide_rx.changed() => {
                if changed.is_err() {
                    break;
                }
                let slide = *slide_rx.borrow_and_update();
                let down = app_down(&state, slide, &identity, false).await;
                if send_json(&mut socket, &down).await.is_err() {
                    break;
                }
            }
            msg = socket.recv() => match msg {
                // the heartbeat proves liveness by arriving; nothing to decode
                Some(Ok(Message::Text(text))) if text.as_str() == KEEPALIVE => {}
                Some(Ok(Message::Text(text))) => match serde_json::from_str::<AppUp>(text.as_str()) {
                    // Every branch here drives the room, so every branch is
                    // presenter-only. The client hides these controls, but
                    // hiding a button is not a check.
                    Ok(command) if !identity.may_present() => {
                        tracing::warn!(
                            conn,
                            role = identity.role.label(),
                            ?command,
                            "refusing a presenter command",
                        );
                    }
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

    release(&state, &identity);
}

/// Hand this socket's ticket back to the pool. The seat itself survives:
/// only the hold on it ends.
fn release(state: &AppState, identity: &auth::Identity) {
    if let Some(ticket) = identity.ticket.clone() {
        state.tickets.release(ticket);
    }
}

/// The address the audience scans, if it can be determined.
///
/// Read from the environment because the server cannot see the URL the room
/// will use: behind fly's proxy the bound address is a private one, and the
/// public hostname is deployment knowledge.
fn join_url(loopback_host: Option<&str>) -> Option<String> {
    let configured = std::env::var(JOIN_URL_ENV)
        .ok()
        .filter(|url| !url.is_empty())
        .map(|url| url.trim_end_matches('/').to_string());
    if configured.is_some() {
        return configured;
    }

    // On a laptop the presenter is already browsing an address that
    // reaches the server, so show that rather than nothing. A phone will
    // not resolve `localhost`, but a local run has no audience anyway, and
    // this makes `q` demonstrable without any configuration.
    loopback_host
        .filter(|host| !host.is_empty())
        .map(|host| format!("http://{host}"))
}

/// Environment variable holding the public base URL of the deck, e.g.
/// `https://more-actors-with-tokio.fly.dev`.
pub const JOIN_URL_ENV: &str = "JOIN_URL";

pub(crate) async fn send_json(
    socket: &mut WebSocket,
    value: &impl serde::Serialize,
) -> Result<(), axum::Error> {
    let json = serde_json::to_string(value).expect("serialize message");
    socket.send(Message::text(json)).await
}

/// Make sure a presenter key exists, and print the URL that uses it.
///
/// Without this the safe default (no key configured, nobody may present)
/// would lock the presenter out of their own talk with no way in.
/// Generating one keeps the default safe *and* usable: the room cannot
/// present, the person reading the logs can.
///
/// The whole URL is logged, not just the key, because that is the thing
/// you actually need — copy it out of the terminal and open it. It is only
/// a secret from the audience, and the audience is not reading the server
/// log.
pub fn announce_presenter_key() {
    let configured = auth::presenter_key().is_some();

    if !configured {
        let generated = format!("{:016x}", fastrand_u64());
        std::env::set_var(auth::PRESENTER_KEY_ENV, &generated);
    }

    let Some(key) = auth::presenter_key() else {
        tracing::error!("could not install a generated presenter key");
        return;
    };

    let url = presenter_url(&key);
    if configured {
        tracing::info!(%url, "present here (key from {})", auth::PRESENTER_KEY_ENV);
    } else {
        tracing::warn!(
            %url,
            "no {} set, so one was generated for this run; it changes on every restart",
            auth::PRESENTER_KEY_ENV,
        );
    }
}

/// The URL a presenter opens, built from `JOIN_URL` when it is set.
///
/// Falls back to a bare path: the server cannot know its own public
/// address, and a path is still enough to paste after a hostname.
fn presenter_url(key: &str) -> String {
    let param = crate::protocol::PRESENTER_PARAM;
    match join_url(None) {
        Some(base) => format!("{base}/?{param}={key}"),
        None => format!("/?{param}={key}"),
    }
}

/// One random u64, without taking on a dependency for it.
fn fastrand_u64() -> u64 {
    use std::hash::{BuildHasher, Hasher};
    let mut hasher = std::collections::hash_map::RandomState::new().build_hasher();
    hasher.write_u128(
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|elapsed| elapsed.as_nanos())
            .unwrap_or_default(),
    );
    hasher.finish()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn announcing_a_generated_key_does_not_panic() {
        // the startup path: no key configured, so one is generated and the
        // url is logged. Runs in-process, so it also proves the generated
        // key installs into the cache `presenter_key` reads.
        announce_presenter_key();
        assert!(
            auth::presenter_key().is_some(),
            "a key must exist after announcing",
        );
    }

    #[test]
    fn the_presenter_url_carries_the_key_and_is_openable() {
        // JOIN_URL is read once per process, so this covers whichever
        // branch this run takes: with a base it is absolute, without one a
        // path — either way it ends in the key and can be pasted.
        let url = presenter_url("secret123");
        assert!(url.ends_with("k=secret123"), "got {url}");
        assert!(url.contains("/?"), "got {url}");
        assert!(!url.contains("PRESENTER"), "no env var name leaked: {url}");
    }
}
