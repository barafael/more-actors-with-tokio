//! The mpsc actor: one bounded channel, one presenter slot, one broadcast
//! plane. After every state change the actor publishes the full snapshot;
//! discrete events only carry animation triggers.

use std::time::Instant;

use axum::extract::ws::{CloseFrame, Message, WebSocket, WebSocketUpgrade};
use axum::response::Response;
use tokio::sync::{broadcast, mpsc, oneshot};
use tokio_util::sync::CancellationToken;

use crate::protocol::{MpscEvent, MpscSnapshot, MpscWire, MPSC_FLIGHT_MS};
use crate::server::{next_conn, send_json, AppState, DEAD_PEER_TIMEOUT};
use crate::sim::{now_ms, MpscSim};

pub enum MpscMsg {
    Send {
        conn: u64,
        ch: char,
    },
    Receive,
    AddSender {
        conn: u64,
        owner: u64,
    },
    RemoveSender {
        conn: u64,
    },
    /// Clone the requester's handle; replies with the new conn id, or None
    /// when the requester does not hold the presenter slot.
    Clone {
        requester: u64,
        reply: oneshot::Sender<Option<u64>>,
    },
    /// Claim the presenter slot (last claim wins).
    Claim {
        conn: u64,
    },
    Get {
        reply: oneshot::Sender<MpscSnapshot>,
    },
}

pub struct MpscService {
    sim: MpscSim,
    presenter: Option<u64>,
}

impl Default for MpscService {
    fn default() -> Self {
        Self {
            sim: MpscSim::new(MPSC_FLIGHT_MS as f64),
            presenter: None,
        }
    }
}

impl MpscService {
    fn publish(&self, events: &broadcast::Sender<MpscEvent>, cosmetic: Vec<MpscEvent>) {
        for event in cosmetic {
            let _ = events.send(event);
        }
        let _ = events.send(MpscEvent::Snapshot {
            state: self.sim.snapshot(),
        });
    }

    pub async fn event_loop(
        mut self,
        mut rx: mpsc::Receiver<MpscMsg>,
        events: broadcast::Sender<MpscEvent>,
        token: CancellationToken,
    ) -> Self {
        loop {
            self.sim.sync_now(now_ms());
            let delay = self.sim.next_delay_ms();
            tokio::select! {
                msg = rx.recv() => match msg {
                    Some(MpscMsg::Send { conn, ch }) => {
                        let cosmetic = self.sim.handle(&MpscWire::Send { conn, ch }, conn);
                        self.publish(&events, cosmetic);
                    }
                    Some(MpscMsg::Receive) => {
                        self.sim.handle(&MpscWire::Receive, 0);
                        self.publish(&events, Vec::new());
                    }
                    Some(MpscMsg::AddSender { conn, owner }) => {
                        let cosmetic = self.sim.add_sender(conn, owner);
                        self.publish(&events, cosmetic);
                    }
                    Some(MpscMsg::RemoveSender { conn }) => {
                        let cosmetic = self.sim.remove_sender(conn);
                        if self.presenter == Some(conn) {
                            self.presenter = None;
                        }
                        self.publish(&events, cosmetic);
                    }
                    Some(MpscMsg::Clone { requester, reply }) => {
                        let new = if self.presenter == Some(requester) {
                            let new = next_conn();
                            let cosmetic = self.sim.add_sender(new, requester);
                            self.publish(&events, cosmetic);
                            Some(new)
                        } else {
                            None
                        };
                        let _ = reply.send(new);
                    }
                    Some(MpscMsg::Claim { conn }) => {
                        self.presenter = Some(conn);
                    }
                    Some(MpscMsg::Get { reply }) => {
                        let _ = reply.send(self.sim.snapshot());
                    }
                    None => break,
                },
                _ = async {
                    match delay {
                        Some(ms) => tokio::time::sleep(std::time::Duration::from_secs_f64(ms / 1000.0)).await,
                        None => std::future::pending().await,
                    }
                } => {
                    let cosmetic = {
                        self.sim.sync_now(now_ms());
                        self.sim.poll_due()
                    };
                    self.publish(&events, cosmetic);
                }
                _ = token.cancelled() => break,
            }
        }
        self
    }
}

#[derive(Clone)]
pub struct MpscHandles {
    cmd_tx: mpsc::Sender<MpscMsg>,
    evt_tx: broadcast::Sender<MpscEvent>,
}

impl MpscHandles {
    pub fn subscribe(&self) -> broadcast::Receiver<MpscEvent> {
        self.evt_tx.subscribe()
    }

    pub fn cmd_tx(&self) -> mpsc::Sender<MpscMsg> {
        self.cmd_tx.clone()
    }
}

pub async fn backend(
    restart: mpsc::UnboundedReceiver<()>,
    handles_tx: tokio::sync::watch::Sender<Option<MpscHandles>>,
) {
    crate::server::supervise("mpsc", restart, handles_tx, |token| {
        let (cmd_tx, cmd_rx) = mpsc::channel(64);
        let (evt_tx, _) = broadcast::channel(256);
        let events = evt_tx.clone();
        let task = tokio::spawn(async move {
            MpscService::default()
                .event_loop(cmd_rx, events, token)
                .await;
        });
        (MpscHandles { cmd_tx, evt_tx }, task)
    })
    .await
}

pub async fn mpsc_socket(
    ws: WebSocketUpgrade,
    axum::extract::State(state): axum::extract::State<AppState>,
) -> Response {
    ws.on_upgrade(move |socket| handle_mpsc_socket(socket, state))
}

async fn handle_mpsc_socket(mut socket: WebSocket, state: AppState) {
    let conn = next_conn();

    let Some(handles) = state.mpsc.handles() else {
        close_restarting(&mut socket).await;
        return;
    };
    let cmd_tx = handles.cmd_tx();

    // register this connection as a sender before snapshotting, so the
    // snapshot it receives already contains itself
    if cmd_tx
        .send(MpscMsg::AddSender { conn, owner: conn })
        .await
        .is_err()
    {
        close_restarting(&mut socket).await;
        return;
    }

    let (reply_tx, reply_rx) = oneshot::channel();
    if cmd_tx.send(MpscMsg::Get { reply: reply_tx }).await.is_err() {
        close_restarting(&mut socket).await;
        return;
    }
    let state = reply_rx
        .await
        .unwrap_or_else(|_| MpscSim::new(0.0).snapshot());

    // Subscribe first, then drop our sender clone so the channel can close
    // when the backend restarts the actor.
    let mut evt_rx = handles.subscribe();
    drop(handles);

    // every handle this connection owns: itself plus its clones. From here on,
    // every exit path must run the cleanup below — dropping the connection
    // drops all owned handles, like dropping mpsc senders.
    let mut owned = vec![conn];

    'conn: {
        if send_json(&mut socket, &MpscEvent::Hello { conn })
            .await
            .is_err()
        {
            break 'conn;
        }
        if send_json(&mut socket, &MpscEvent::Snapshot { state })
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
                        match serde_json::from_str::<MpscWire>(text.as_str()) {
                            Ok(MpscWire::CloneSender { .. }) => {
                                let (reply_tx, reply_rx) = oneshot::channel();
                                if cmd_tx
                                    .send(MpscMsg::Clone { requester: conn, reply: reply_tx })
                                    .await
                                    .is_err()
                                {
                                    close_restarting(&mut socket).await;
                                    break 'conn;
                                }
                                if let Ok(Some(new)) = reply_rx.await {
                                    owned.push(new);
                                }
                            }
                            Ok(MpscWire::ClaimPresenter) => {
                                if cmd_tx.send(MpscMsg::Claim { conn }).await.is_err() {
                                    close_restarting(&mut socket).await;
                                    break 'conn;
                                }
                            }
                            Ok(wire) => {
                                // a connection may send from any handle it
                                // owns (clones included); sends claiming a
                                // foreign handle are ignored
                                let msg = match wire {
                                    MpscWire::Send { conn: sender, ch } => {
                                        owned
                                            .contains(&sender)
                                            .then_some(MpscMsg::Send { conn: sender, ch })
                                    }
                                    MpscWire::Receive => Some(MpscMsg::Receive),
                                    MpscWire::CloneSender { .. }
                                    | MpscWire::ClaimPresenter => unreachable!(),
                                };
                                if let Some(msg) = msg {
                                    if cmd_tx.send(msg).await.is_err() {
                                        close_restarting(&mut socket).await;
                                        break 'conn;
                                    }
                                }
                            }
                            Err(_) => {}
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
                        if cmd_tx.send(MpscMsg::Get { reply: reply_tx }).await.is_ok() {
                            if let Ok(state) = reply_rx.await {
                                let _ = send_json(&mut socket, &MpscEvent::Snapshot { state }).await;
                            }
                        }
                    }
                },
            }
        }
    }

    // dropping the connection drops every handle it owns
    for conn in owned {
        let _ = cmd_tx.send(MpscMsg::RemoveSender { conn }).await;
    }
}

async fn close_restarting(socket: &mut WebSocket) {
    let _ = socket
        .send(Message::Close(Some(CloseFrame {
            code: 4001,
            reason: "restarting".into(),
        })))
        .await;
}
