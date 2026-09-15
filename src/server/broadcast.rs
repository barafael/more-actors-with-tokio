//! The broadcast actor: one ring buffer, cloneable senders, per-connection
//! receivers and one broadcast plane. After every state change the actor
//! publishes the full snapshot; discrete events only carry animation
//! triggers. Sending is free — a full buffer evicts its oldest value.

use std::time::Instant;

use axum::extract::ws::{Message, WebSocket, WebSocketUpgrade};
use axum::response::Response;
use tokio::sync::{broadcast, mpsc, oneshot};
use tokio_util::sync::CancellationToken;

use crate::protocol::{BroadcastEvent, BroadcastSnapshot, BroadcastWire};
use crate::server::auth::Connecting;
use crate::server::{close_restarting, release, send_json, AppState, DEAD_PEER_TIMEOUT};
use crate::sim::BroadcastSim;

pub enum BroadcastMsg {
    /// Send from the handle `handle`, which must be owned by `conn`.
    Send { conn: u64, handle: u64, ch: char },
    /// Clone the sender `source` for `requester`.
    Clone { requester: u64, source: u64 },
    /// Subscribe a receiver owned by `conn` (one per connection).
    Subscribe { conn: u64 },
    /// Receive on a receiver `conn` owns.
    Receive { conn: u64, receiver: u64 },
    /// Drop a receiver `conn` owns.
    Unsubscribe { conn: u64, receiver: u64 },
    /// Claim the presenter slot; host-owned senders transfer to the claimer.
    Claim { conn: u64 },
    /// The connection left: drop its handles and receiver.
    DropConnection { conn: u64 },
    Get {
        reply: oneshot::Sender<BroadcastSnapshot>,
    },
}

#[derive(Default)]
pub struct BroadcastService {
    sim: BroadcastSim,
}

impl BroadcastService {
    fn publish(&self, events: &broadcast::Sender<BroadcastEvent>, cosmetic: Vec<BroadcastEvent>) {
        for event in cosmetic {
            let _ = events.send(event);
        }
        let _ = events.send(BroadcastEvent::Snapshot {
            state: self.sim.snapshot(),
        });
    }

    pub async fn event_loop(
        mut self,
        mut rx: mpsc::Receiver<BroadcastMsg>,
        events: broadcast::Sender<BroadcastEvent>,
        token: CancellationToken,
    ) -> Self {
        loop {
            tokio::select! {
                msg = rx.recv() => match msg {
                    Some(BroadcastMsg::Send { conn, handle, ch }) => {
                        let cosmetic = self.sim.handle(&BroadcastWire::Send { conn: handle, ch }, conn);
                        self.publish(&events, cosmetic);
                    }
                    Some(BroadcastMsg::Clone { requester, source }) => {
                        let cosmetic = self
                            .sim
                            .handle(&BroadcastWire::CloneSender { source }, requester);
                        self.publish(&events, cosmetic);
                    }
                    Some(BroadcastMsg::Subscribe { conn }) => {
                        let cosmetic = self.sim.handle(&BroadcastWire::Subscribe, conn);
                        self.publish(&events, cosmetic);
                    }
                    Some(BroadcastMsg::Receive { conn, receiver }) => {
                        let cosmetic = self
                            .sim
                            .handle(&BroadcastWire::Receive { receiver }, conn);
                        self.publish(&events, cosmetic);
                    }
                    Some(BroadcastMsg::Unsubscribe { conn, receiver }) => {
                        let cosmetic = self
                            .sim
                            .handle(&BroadcastWire::Unsubscribe { receiver }, conn);
                        self.publish(&events, cosmetic);
                    }
                    Some(BroadcastMsg::Claim { conn }) => {
                        self.sim.handle(&BroadcastWire::ClaimPresenter, conn);
                        self.publish(&events, Vec::new());
                    }
                    Some(BroadcastMsg::DropConnection { conn }) => {
                        let cosmetic = self.sim.drop_connection(conn);
                        self.publish(&events, cosmetic);
                    }
                    Some(BroadcastMsg::Get { reply }) => {
                        let _ = reply.send(self.sim.snapshot());
                    }
                    None => break,
                },
                _ = token.cancelled() => break,
            }
        }
        self
    }
}

#[derive(Clone)]
pub struct BroadcastHandles {
    cmd_tx: mpsc::Sender<BroadcastMsg>,
    evt_tx: broadcast::Sender<BroadcastEvent>,
}

impl BroadcastHandles {
    pub fn subscribe(&self) -> broadcast::Receiver<BroadcastEvent> {
        self.evt_tx.subscribe()
    }

    pub fn cmd_tx(&self) -> mpsc::Sender<BroadcastMsg> {
        self.cmd_tx.clone()
    }
}

pub async fn backend(
    restart: mpsc::UnboundedReceiver<()>,
    handles_tx: tokio::sync::watch::Sender<Option<BroadcastHandles>>,
) {
    crate::server::supervise("broadcast", restart, handles_tx, |token| {
        let (cmd_tx, cmd_rx) = mpsc::channel(64);
        let (evt_tx, _) = broadcast::channel(256);
        let events = evt_tx.clone();
        let task = tokio::spawn(async move {
            BroadcastService::default()
                .event_loop(cmd_rx, events, token)
                .await;
        });
        (BroadcastHandles { cmd_tx, evt_tx }, task)
    })
    .await
}

pub async fn broadcast_socket(
    ws: WebSocketUpgrade,
    connecting: Connecting,
    axum::extract::State(state): axum::extract::State<AppState>,
) -> Response {
    ws.on_upgrade(move |socket| handle_broadcast_socket(socket, state, connecting))
}

async fn handle_broadcast_socket(mut socket: WebSocket, app: AppState, connecting: Connecting) {
    let Connecting { identity, conn } = connecting;

    let Some(handles) = app.broadcast.handles() else {
        close_restarting(&mut socket).await;
        return;
    };
    let cmd_tx = handles.cmd_tx();

    // snapshot before subscribing so the handshake already carries the state
    let (reply_tx, reply_rx) = oneshot::channel();
    if cmd_tx
        .send(BroadcastMsg::Get { reply: reply_tx })
        .await
        .is_err()
    {
        close_restarting(&mut socket).await;
        return;
    }
    let state = reply_rx
        .await
        .unwrap_or_else(|_| BroadcastSim::new().snapshot());

    // subscribe first, then drop our sender clone so the channel can close
    // when the backend restarts the actor
    let mut evt_rx = handles.subscribe();
    drop(handles);

    'conn: {
        if send_json(&mut socket, &BroadcastEvent::Hello { conn })
            .await
            .is_err()
        {
            break 'conn;
        }
        if send_json(&mut socket, &BroadcastEvent::Snapshot { state })
            .await
            .is_err()
        {
            break 'conn;
        }

        let mut last_seen = Instant::now();
        loop {
            tokio::select! {
                _ = tokio::time::sleep(std::time::Duration::from_secs(5)) => {
                    // Dead-peer detection. Clients ping every 3s, so 25s is
                    // about eight missed pings: long enough that a phone
                    // backgrounded mid-talk keeps its handle, short enough
                    // that a crashed tab frees one.
                    if last_seen.elapsed() > DEAD_PEER_TIMEOUT {
                        break;
                    }
                }
                msg = socket.recv() => match msg {
                    Some(Ok(Message::Text(text))) => {
                        last_seen = Instant::now();
                        // every wire here is connection-scoped: the actor
                        // rejects foreign handles/receivers
                        let msg = match serde_json::from_str::<BroadcastWire>(text.as_str()) {
                            // Spectators are present but handle-less: they
                            // watch the fan-out without being part of it.
                            Ok(_) if !identity.may_play() => None,
                            // The presenter slot is granted by the server,
                            // not asked for by the client.
                            Ok(BroadcastWire::ClaimPresenter) if !identity.may_present() => None,
                            Ok(BroadcastWire::Send { conn: sender, ch }) => {
                                // ownership is enforced by the sim (owner ==
                                // requester); unknown handles are ignored
                                Some(BroadcastMsg::Send { conn, handle: sender, ch })
                            }
                            Ok(BroadcastWire::CloneSender { source }) => {
                                Some(BroadcastMsg::Clone { requester: conn, source })
                            }
                            Ok(BroadcastWire::Subscribe) => {
                                Some(BroadcastMsg::Subscribe { conn })
                            }
                            Ok(BroadcastWire::Receive { receiver }) => {
                                Some(BroadcastMsg::Receive { conn, receiver })
                            }
                            Ok(BroadcastWire::Unsubscribe { receiver }) => {
                                Some(BroadcastMsg::Unsubscribe { conn, receiver })
                            }
                            Ok(BroadcastWire::ClaimPresenter) => {
                                Some(BroadcastMsg::Claim { conn })
                            }
                            Err(_) => None,
                        };
                        if let Some(msg) = msg {
                            if cmd_tx.send(msg).await.is_err() {
                                close_restarting(&mut socket).await;
                                break 'conn;
                            }
                        }
                    }
                    Some(Ok(_)) | None | Some(Err(_)) => break,
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
                    Err(broadcast::error::RecvError::Lagged(_)) => {
                        let (reply_tx, reply_rx) = oneshot::channel();
                        if cmd_tx.send(BroadcastMsg::Get { reply: reply_tx }).await.is_ok() {
                            if let Ok(state) = reply_rx.await {
                                let _ = send_json(&mut socket, &BroadcastEvent::Snapshot { state }).await;
                            }
                        }
                    }
                },
            }
        }
    }

    // dropping the connection drops its senders and its receiver
    let _ = cmd_tx.send(BroadcastMsg::DropConnection { conn }).await;
    release(&app, &identity);
}
