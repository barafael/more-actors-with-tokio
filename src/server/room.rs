//! One actor body for the games where *who* acts matters.
//!
//! The ticking games only need a wire; the mutex and call-and-response
//! games need to know which connection sent it, what that connection may
//! do, and — above all — when it leaves. A phone that locks the mutex and
//! then closes its browser must drop its guard, exactly as a task that
//! ends drops what it holds; a requester that leaves must drop its
//! receiver. So every connection joins the room on connect and leaves it on
//! every exit path of its socket, and the sim decides what leaving means.
//!
//! Same recipe as [`super::ticking`]: the actor is plain data, the loop is a
//! consuming method returning `Self`, and the snapshot is the only
//! authority a client ever sees.

use axum::extract::ws::{Message, WebSocket};
use serde::de::DeserializeOwned;
use serde::Serialize;
use tokio::sync::{broadcast, mpsc, oneshot};
use tokio_util::sync::CancellationToken;

use crate::protocol::Role;
use crate::server::auth::Connecting;
use crate::server::{close_restarting, release, send_json, AppState, DEAD_PEER_TIMEOUT};
use crate::sim::now_ms;

/// What a per-connection game must supply to be run by [`RoomService`].
pub trait RoomGame: Send + 'static {
    type Wire: DeserializeOwned + Send + 'static;
    type Event: Clone + Serialize + Send + 'static;

    const NAME: &'static str;

    fn fresh() -> Self;

    /// The first event a socket receives: which connection it is.
    fn hello(conn: u64) -> Self::Event;

    /// Tell the sim what time it is before anything happens to it.
    fn sync_now(&mut self, now: f64);

    /// A connection arrived. Its role decides whether it takes part.
    fn join(&mut self, conn: u64, role: Role);

    /// A connection left, by any route: drop whatever it held.
    fn leave(&mut self, conn: u64);

    fn handle(&mut self, conn: u64, role: Role, wire: Self::Wire);

    fn snapshot_event(&self) -> Self::Event;
}

pub enum RoomMsg<G: RoomGame> {
    Join {
        conn: u64,
        role: Role,
    },
    Leave {
        conn: u64,
    },
    Wire {
        conn: u64,
        role: Role,
        wire: G::Wire,
    },
    Get {
        reply: oneshot::Sender<G::Event>,
    },
}

pub struct RoomService<G> {
    sim: G,
}

impl<G: RoomGame> RoomService<G> {
    pub fn new() -> Self {
        Self { sim: G::fresh() }
    }

    fn publish(&self, events: &broadcast::Sender<G::Event>) {
        events
            .send(self.sim.snapshot_event())
            .inspect_err(|_| tracing::trace!(game = G::NAME, "no subscribers for snapshot"))
            .ok();
    }

    pub async fn event_loop(
        mut self,
        mut rx: mpsc::Receiver<RoomMsg<G>>,
        events: broadcast::Sender<G::Event>,
        token: CancellationToken,
    ) -> Self {
        loop {
            tokio::select! {
                msg = rx.recv() => {
                    let Some(msg) = msg else { break };
                    self.sim.sync_now(now_ms());
                    match msg {
                        RoomMsg::Join { conn, role } => self.sim.join(conn, role),
                        RoomMsg::Leave { conn } => self.sim.leave(conn),
                        RoomMsg::Wire { conn, role, wire } => self.sim.handle(conn, role, wire),
                        RoomMsg::Get { reply } => {
                            reply.send(self.sim.snapshot_event()).ok();
                            continue;
                        }
                    }
                    self.publish(&events);
                }
                _ = token.cancelled() => break,
            }
        }
        self
    }
}

impl<G: RoomGame> Default for RoomService<G> {
    fn default() -> Self {
        Self::new()
    }
}

pub struct RoomHandles<G: RoomGame> {
    cmd_tx: mpsc::Sender<RoomMsg<G>>,
    evt_tx: broadcast::Sender<G::Event>,
}

// Only the endpoints are cloned; a derive would demand `G: Clone`.
impl<G: RoomGame> Clone for RoomHandles<G> {
    fn clone(&self) -> Self {
        Self {
            cmd_tx: self.cmd_tx.clone(),
            evt_tx: self.evt_tx.clone(),
        }
    }
}

pub async fn backend<G: RoomGame>(
    restart: mpsc::UnboundedReceiver<()>,
    handles_tx: tokio::sync::watch::Sender<Option<RoomHandles<G>>>,
) {
    crate::server::supervise(G::NAME, restart, handles_tx, |token| {
        let (cmd_tx, cmd_rx) = mpsc::channel(64);
        let (evt_tx, _) = broadcast::channel(64);
        let events = evt_tx.clone();
        let task = tokio::spawn(async move {
            RoomService::<G>::new()
                .event_loop(cmd_rx, events, token)
                .await;
        });
        (RoomHandles { cmd_tx, evt_tx }, task)
    })
    .await
}

/// Drive one client socket for a room game.
pub async fn handle_socket<G: RoomGame>(
    mut socket: WebSocket,
    app: AppState,
    connecting: Connecting,
    plane: impl Fn(&AppState) -> Option<RoomHandles<G>>,
) {
    let Connecting { identity, conn } = connecting;
    let role = identity.role;
    tracing::debug!(conn, game = G::NAME, "game socket");

    let Some(handles) = plane(&app) else {
        release(&app, &identity);
        close_restarting(&mut socket).await;
        return;
    };
    let cmd_tx = handles.cmd_tx.clone();
    let mut evt_rx = handles.evt_tx.subscribe();
    // Holding the handles would keep the actor's inbox open across a
    // restart, and this socket would never learn its actor had gone.
    drop(handles);

    if cmd_tx.send(RoomMsg::Join { conn, role }).await.is_err() {
        release(&app, &identity);
        close_restarting(&mut socket).await;
        return;
    }

    // From here on the room knows this connection, so every exit must
    // leave it: breaking out of this block is the only way out.
    'conn: {
        let (reply_tx, reply_rx) = oneshot::channel();
        if cmd_tx.send(RoomMsg::Get { reply: reply_tx }).await.is_err() {
            close_restarting(&mut socket).await;
            break 'conn;
        }
        let Ok(snapshot) = reply_rx.await else {
            close_restarting(&mut socket).await;
            break 'conn;
        };
        if send_json(&mut socket, &G::hello(conn)).await.is_err()
            || send_json(&mut socket, &snapshot).await.is_err()
        {
            break 'conn;
        }

        loop {
            tokio::select! {
                msg = tokio::time::timeout(DEAD_PEER_TIMEOUT, socket.recv()) => match msg {
                    Ok(Some(Ok(Message::Text(text)))) => {
                        if text.as_str() == crate::protocol::KEEPALIVE {
                            continue;
                        }
                        let Ok(wire) = serde_json::from_str::<G::Wire>(text.as_str()) else {
                            tracing::debug!(game = G::NAME, "ignoring undecodable wire");
                            continue;
                        };
                        // Spectators watch; the sim additionally checks the
                        // role for anything only the presenter may do.
                        if !identity.may_play() {
                            continue;
                        }
                        if cmd_tx.send(RoomMsg::Wire { conn, role, wire }).await.is_err() {
                            close_restarting(&mut socket).await;
                            break 'conn;
                        }
                    }
                    Ok(Some(Ok(_))) | Ok(None) | Ok(Some(Err(_))) => break,
                    Err(_) => {
                        tracing::debug!(conn, game = G::NAME, "peer went silent");
                        break;
                    }
                },
                event = evt_rx.recv() => match event {
                    Ok(event) => {
                        if send_json(&mut socket, &event).await.is_err() {
                            break;
                        }
                    }
                    Err(broadcast::error::RecvError::Closed) => {
                        close_restarting(&mut socket).await;
                        break 'conn;
                    }
                    // Every event is a snapshot, so the next one heals a lag.
                    Err(broadcast::error::RecvError::Lagged(missed)) => {
                        tracing::debug!(conn, game = G::NAME, missed, "client lagged");
                    }
                },
            }
        }
    }

    cmd_tx.send(RoomMsg::Leave { conn }).await.ok();
    release(&app, &identity);
}
