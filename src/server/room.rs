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

use axum::extract::ws::WebSocket;
use serde::de::DeserializeOwned;
use serde::Serialize;
use tokio::sync::{broadcast, mpsc, oneshot};
use tokio_util::sync::CancellationToken;

use crate::protocol::{Role, RoomEvent};
use crate::server::auth::Connecting;
use crate::server::{ask, close_restarting, relay, release, send_json, AppState};
use crate::sim::now_ms;

/// What a per-connection game must supply to be run by [`RoomService`].
pub trait RoomGame: Default + Send + 'static {
    type Wire: DeserializeOwned + Send + 'static;
    /// Compared before and after each message, at one instant, to decide
    /// whether there is anything to publish.
    type Snapshot: Clone + PartialEq + Serialize + Send + 'static;

    const NAME: &'static str;

    /// Tell the sim what time it is before anything happens to it.
    fn sync_now(&mut self, now: f64);

    /// A connection arrived. Its role decides whether it takes part.
    fn join(&mut self, conn: u64, role: Role);

    /// A connection left, by any route: drop whatever it held.
    fn leave(&mut self, conn: u64);

    fn handle(&mut self, conn: u64, role: Role, wire: Self::Wire);

    fn snapshot(&self) -> Self::Snapshot;
}

type Event<G> = RoomEvent<<G as RoomGame>::Snapshot>;

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
        reply: oneshot::Sender<Event<G>>,
    },
}

#[derive(Default)]
pub struct RoomService<G> {
    sim: G,
}

impl<G: RoomGame> RoomService<G> {
    fn snapshot_event(&self) -> Event<G> {
        RoomEvent::Snapshot {
            state: self.sim.snapshot(),
        }
    }

    pub async fn event_loop(
        mut self,
        mut rx: mpsc::Receiver<RoomMsg<G>>,
        events: broadcast::Sender<Event<G>>,
        token: CancellationToken,
    ) -> Self {
        loop {
            tokio::select! {
                msg = rx.recv() => {
                    let Some(msg) = msg else { break };
                    // One instant for the whole message, so the two snapshots
                    // differ only if the message changed something. A wire
                    // that changes nothing — a waiter locking again, a
                    // player trying to be the actor, a spectator joining —
                    // publishes nothing.
                    self.sim.sync_now(now_ms());
                    let before = self.sim.snapshot();
                    match msg {
                        RoomMsg::Join { conn, role } => self.sim.join(conn, role),
                        RoomMsg::Leave { conn } => self.sim.leave(conn),
                        RoomMsg::Wire { conn, role, wire } => self.sim.handle(conn, role, wire),
                        RoomMsg::Get { reply } => {
                            reply.send(self.snapshot_event()).ok();
                        }
                    }
                    if self.sim.snapshot() != before {
                        events
                            .send(self.snapshot_event())
                            .inspect_err(|_| tracing::trace!(game = G::NAME, "no subscribers"))
                            .ok();
                    }
                }
                _ = token.cancelled() => break,
            }
        }
        self
    }
}

pub struct RoomHandles<G: RoomGame> {
    cmd_tx: mpsc::Sender<RoomMsg<G>>,
    evt_tx: broadcast::Sender<Event<G>>,
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
            RoomService::<G>::default()
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

    // From here on the room knows this connection, so every path below
    // must end in the `Leave` after it.
    match ask(&cmd_tx, |reply| RoomMsg::Get { reply }).await {
        None => close_restarting(&mut socket).await,
        Some(snapshot) => {
            let hello: Event<G> = RoomEvent::Hello { conn };
            if send_json(&mut socket, &hello).await.is_ok()
                && send_json(&mut socket, &snapshot).await.is_ok()
            {
                relay(
                    &mut socket,
                    &identity,
                    conn,
                    G::NAME,
                    &cmd_tx,
                    &mut evt_rx,
                    |wire| RoomMsg::Wire { conn, role, wire },
                    |reply| RoomMsg::Get { reply },
                )
                .await;
            }
        }
    }

    cmd_tx.send(RoomMsg::Leave { conn }).await.ok();
    release(&app, &identity);
}
