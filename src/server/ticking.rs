//! One actor body for every clock-driven game.
//!
//! The timer, select and loop-select actors differ only in which sim they
//! hold and which wire they speak. Everything else — the `select!` over an
//! inbox, a deadline and a cancellation token, publishing a snapshot after
//! every change, replying to a late joiner — is identical, so it is written
//! once here and the three games supply a [`TickingGame`].
//!
//! This is the actor recipe from CONCEPT.md §6 applied to itself: the actor
//! is plain data, the event loop is a consuming method returning `Self`, and
//! there is no handle type beyond the channel endpoints. That it could be
//! made generic over three games without a trait object or a lock is the
//! argument the slide makes.

use axum::extract::ws::{Message, WebSocket};
use serde::de::DeserializeOwned;
use serde::Serialize;
use tokio::sync::{broadcast, mpsc, oneshot};
use tokio_util::sync::CancellationToken;

use crate::clock::Ticking;
use crate::server::auth::Connecting;
use crate::server::{close_restarting, release, send_json, AppState, DEAD_PEER_TIMEOUT};
use crate::sim::now_ms;

/// What a clock-driven game must supply to be run by [`TickingService`].
pub trait TickingGame: Ticking<Event = <Self as TickingGame>::Ev> + Send + 'static {
    /// This game's event type.
    ///
    /// A redundant-looking projection of [`Ticking::Event`], and the reason
    /// is worth stating: Rust does not elaborate a trait's `where` clauses
    /// into implied bounds at use sites, so spelling the bound here — where
    /// it can be written on an associated type — is what stops every impl,
    /// struct and function downstream from having to repeat
    /// `where Self::Event: Clone + Send`.
    type Ev: Clone + Send + 'static;

    /// The command a client sends.
    type Wire: DeserializeOwned + Send + 'static;

    /// The name used in logs and in the restart supervisor.
    const NAME: &'static str;

    /// Build a fresh sim. `epoch_offset_ms` aligns the sim clock to the wall
    /// clock so every peer draws the same seconds dial.
    fn fresh(epoch_offset_ms: f64) -> Self;

    /// Apply a client command.
    fn handle(&mut self, wire: &Self::Wire) -> Vec<Self::Event>;

    /// The event carrying the whole state, sent to a joiner and after every
    /// change.
    fn snapshot_event(&self) -> Self::Event;
}

/// Commands the socket handlers send to a ticking actor.
///
/// Keyed on the game rather than on `(Wire, Event)` separately: the game
/// already determines both, and two free parameters would permit a
/// `TickMsg<TimerWire, SelectEvent>` that no actor could ever receive.
pub enum TickMsg<G: TickingGame> {
    Wire(G::Wire),
    /// A late joiner asking for the current state.
    Get {
        reply: oneshot::Sender<G::Event>,
    },
}

/// The actor: one sim, one inbox, one event plane.
pub struct TickingService<G> {
    sim: G,
}

impl<G: TickingGame> TickingService<G> {
    pub fn new(epoch_offset_ms: f64) -> Self {
        Self {
            sim: G::fresh(epoch_offset_ms),
        }
    }

    /// Publish cosmetic events, then the snapshot that is the actual
    /// authority. Same discipline as the channel actors: a client may drop
    /// any cosmetic event and still be correct.
    fn publish(&self, events: &broadcast::Sender<G::Event>, cosmetic: Vec<G::Event>) {
        for event in cosmetic {
            events
                .send(event)
                .inspect_err(|_| tracing::trace!(game = G::NAME, "no subscribers for event"))
                .ok();
        }
        events.send(self.sim.snapshot_event()).ok();
    }

    /// The event loop, as a consuming method returning `Self`.
    ///
    /// The three branches are the whole story of the deck up to this point:
    /// an inbox (`mpsc`), a timer, and a cancellation token — raced by
    /// `select!`, in a loop, over state nothing else can touch.
    pub async fn event_loop(
        mut self,
        mut rx: mpsc::Receiver<TickMsg<G>>,
        events: broadcast::Sender<G::Event>,
        token: CancellationToken,
    ) -> Self {
        loop {
            self.sim.sync_now(now_ms());
            let delay = self.sim.next_delay_ms();

            tokio::select! {
                msg = rx.recv() => match msg {
                    Some(TickMsg::Wire(wire)) => {
                        // Sync again: the clock moved while we were parked,
                        // and a wire that arrives after a deadline must not
                        // be applied to a stale sim.
                        self.sim.sync_now(now_ms());
                        let due = self.sim.poll_due();
                        let handled = self.sim.handle(&wire);
                        self.publish(&events, due.into_iter().chain(handled).collect());
                    }
                    Some(TickMsg::Get { reply }) => {
                        reply.send(self.sim.snapshot_event()).ok();
                    }
                    None => break,
                },
                // A sim with nothing due parks here forever and is woken by
                // the inbox instead — which is what `Option<Sleep>` in a
                // select branch is for.
                () = sleep_for(delay) => {
                    self.sim.sync_now(now_ms());
                    let due = self.sim.poll_due();
                    if !due.is_empty() {
                        self.publish(&events, due);
                    }
                }
                _ = token.cancelled() => break,
            }
        }
        self
    }
}

/// Wait `delay` milliseconds, or forever if there is nothing due.
async fn sleep_for(delay: Option<f64>) {
    match delay {
        Some(ms) => tokio::time::sleep(std::time::Duration::from_secs_f64(ms / 1000.0)).await,
        None => std::future::pending().await,
    }
}

/// The handles a socket needs to talk to one ticking actor.
pub struct TickingHandles<G: TickingGame> {
    cmd_tx: mpsc::Sender<TickMsg<G>>,
    evt_tx: broadcast::Sender<G::Event>,
}

// Derived `Clone` would demand `G: Clone`, which no sim is: only the two
// channel endpoints are cloned, and both are always cloneable.
impl<G: TickingGame> Clone for TickingHandles<G> {
    fn clone(&self) -> Self {
        Self {
            cmd_tx: self.cmd_tx.clone(),
            evt_tx: self.evt_tx.clone(),
        }
    }
}

impl<G: TickingGame> TickingHandles<G> {
    pub fn cmd_tx(&self) -> mpsc::Sender<TickMsg<G>> {
        self.cmd_tx.clone()
    }

    pub fn subscribe(&self) -> broadcast::Receiver<G::Event> {
        self.evt_tx.subscribe()
    }
}

/// Spawn one ticking game's supervisor.
pub async fn backend<G: TickingGame>(
    restart: mpsc::UnboundedReceiver<()>,
    handles_tx: tokio::sync::watch::Sender<Option<TickingHandles<G>>>,
) {
    crate::server::supervise(G::NAME, restart, handles_tx, |token| {
        let (cmd_tx, cmd_rx) = mpsc::channel(64);
        let (evt_tx, _) = broadcast::channel(64);
        let events = evt_tx.clone();
        let task = tokio::spawn(async move {
            TickingService::<G>::new(crate::clock::wall_offset_ms())
                .event_loop(cmd_rx, events, token)
                .await;
        });
        (TickingHandles { cmd_tx, evt_tx }, task)
    })
    .await
}

/// Drive one client socket for a ticking game.
///
/// `plane` picks this game's plane out of the app state; everything else is
/// the same handshake and relay loop the channel games use.
pub async fn handle_socket<G: TickingGame>(
    mut socket: WebSocket,
    app: AppState,
    connecting: Connecting,
    plane: impl Fn(&AppState) -> Option<TickingHandles<G>>,
) where
    G::Event: Serialize,
{
    let Connecting { identity, conn } = connecting;
    tracing::debug!(conn, game = G::NAME, "game socket");

    let Some(handles) = plane(&app) else {
        release(&app, &identity);
        close_restarting(&mut socket).await;
        return;
    };

    let cmd_tx = handles.cmd_tx();
    // Subscribe before dropping our handle clone so no event can slip
    // between the snapshot and the first relayed event.
    let mut evt_rx = handles.subscribe();
    drop(handles);

    let (reply_tx, reply_rx) = oneshot::channel();
    if cmd_tx.send(TickMsg::Get { reply: reply_tx }).await.is_err() {
        release(&app, &identity);
        close_restarting(&mut socket).await;
        return;
    }
    let Ok(snapshot) = reply_rx.await else {
        release(&app, &identity);
        close_restarting(&mut socket).await;
        return;
    };
    if send_json(&mut socket, &snapshot).await.is_err() {
        release(&app, &identity);
        return;
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
                    // Spectators watch the futures resolve; they are not the
                    // I/O that resolves them. The client hides the controls
                    // too, but hiding a button is not a check.
                    if !identity.may_play() {
                        continue;
                    }
                    if cmd_tx.send(TickMsg::Wire(wire)).await.is_err() {
                        close_restarting(&mut socket).await;
                        break;
                    }
                }
                Ok(Some(Ok(_))) | Ok(None) | Ok(Some(Err(_))) => break,
                // a phone that went into a tunnel, or a lid that closed
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
                    break;
                }
                // A lagged client missed cosmetics; the next snapshot makes
                // it whole, so ask for one rather than dropping the socket.
                Err(broadcast::error::RecvError::Lagged(missed)) => {
                    tracing::debug!(conn, game = G::NAME, missed, "client lagged; resyncing");
                    let (reply_tx, reply_rx) = oneshot::channel();
                    if cmd_tx.send(TickMsg::Get { reply: reply_tx }).await.is_err() {
                        break;
                    }
                    let Ok(snapshot) = reply_rx.await else { break };
                    if send_json(&mut socket, &snapshot).await.is_err() {
                        break;
                    }
                }
            },
        }
    }

    release(&app, &identity);
}
