//! The watch actor: one fixed sender (the presenter), a create phase for the
//! initial value, cloneable receivers and one broadcast plane. After every
//! state change the actor publishes the full snapshot; discrete events only
//! carry animation triggers. Sending is free — there is no bounded buffer —
//! but a `borrow()` read guard blocks `send`, so the sim queues the send as
//! `pending_send` until the last guard drops.

use std::sync::atomic::{AtomicU64, Ordering};

use axum::extract::ws::{CloseFrame, Message, WebSocket, WebSocketUpgrade};
use axum::response::Response;
use tokio::sync::{broadcast, mpsc, oneshot};
use tokio_util::sync::CancellationToken;

use crate::protocol::{WatchEvent, WatchSnapshot, WatchWire};
use crate::server::{send_json, set_watch_handles};
use crate::sim::WatchSim;

static NEXT_CONN: AtomicU64 = AtomicU64::new(1);

pub fn next_conn() -> u64 {
    NEXT_CONN.fetch_add(1, Ordering::Relaxed)
}

pub enum WatchMsg {
    /// Presenter-only (authorized against the sim's presenter slot).
    Send {
        conn: u64,
        ch: char,
    },
    Create {
        conn: u64,
        init: Option<char>,
    },
    NewReceiver {
        conn: u64,
    },
    /// Clone `rx` for `requester`; only if the requester is the owner or holds
    /// the presenter slot. Replies with the fresh receiver id.
    CloneReceiver {
        requester: u64,
        rx: u64,
        reply: oneshot::Sender<Option<u64>>,
    },
    /// Owner-operated receiver commands; the sim rejects foreign owners.
    AwaitChange {
        conn: u64,
        rx: u64,
    },
    LookInside {
        conn: u64,
        rx: u64,
    },
    DropReceiver {
        conn: u64,
        rx: u64,
    },
    Claim {
        conn: u64,
    },
    /// A connection left without releasing the presenter slot; the actor
    /// clears it (last claim wins, slot dies with its holder).
    ReleasePresenter {
        conn: u64,
    },
    /// All receiver ids owned by `conn`, for disconnect cleanup.
    Owned {
        conn: u64,
        reply: oneshot::Sender<Vec<u64>>,
    },
    Get {
        reply: oneshot::Sender<WatchSnapshot>,
    },
}

#[derive(Default)]
pub struct WatchService {
    sim: WatchSim,
}

impl WatchService {
    fn publish(&self, events: &broadcast::Sender<WatchEvent>, cosmetic: Vec<WatchEvent>) {
        for event in cosmetic {
            let _ = events.send(event);
        }
        let _ = events.send(WatchEvent::Snapshot {
            state: self.sim.snapshot(),
        });
    }

    pub async fn event_loop(
        mut self,
        mut rx: mpsc::Receiver<WatchMsg>,
        events: broadcast::Sender<WatchEvent>,
        token: CancellationToken,
    ) -> Self {
        loop {
            tokio::select! {
                msg = rx.recv() => match msg {
                    Some(WatchMsg::Send { conn, ch }) => {
                        let cosmetic = self.sim.handle(&WatchWire::Send { ch }, conn);
                        self.publish(&events, cosmetic);
                    }
                    Some(WatchMsg::Create { conn, init }) => {
                        let cosmetic = self.sim.handle(&WatchWire::Create { init }, conn);
                        self.publish(&events, cosmetic);
                    }
                    Some(WatchMsg::NewReceiver { conn }) => {
                        let cosmetic = self.sim.handle(&WatchWire::NewReceiver, conn);
                        self.publish(&events, cosmetic);
                    }
                    Some(WatchMsg::CloneReceiver { requester, rx, reply }) => {
                        let owns = self.sim.owner_of(rx) == Some(requester);
                        let presents = self.sim.presenter() == Some(requester);
                        if owns || presents {
                            let cosmetic = self.sim.handle(&WatchWire::CloneReceiver { rx }, requester);
                            let new_id = cosmetic.iter().find_map(|event| match event {
                                WatchEvent::ReceiverAdded { id, .. } => Some(*id),
                                _ => None,
                            });
                            self.publish(&events, cosmetic);
                            let _ = reply.send(new_id);
                        } else {
                            let _ = reply.send(None);
                        }
                    }
                    Some(WatchMsg::AwaitChange { conn, rx }) => {
                        let cosmetic = self.sim.handle(&WatchWire::AwaitChange { rx }, conn);
                        self.publish(&events, cosmetic);
                    }
                    Some(WatchMsg::LookInside { conn, rx }) => {
                        let cosmetic = self.sim.handle(&WatchWire::LookInside { rx }, conn);
                        self.publish(&events, cosmetic);
                    }
                    Some(WatchMsg::DropReceiver { conn, rx }) => {
                        let cosmetic = self.sim.handle(&WatchWire::DropReceiver { rx }, conn);
                        self.publish(&events, cosmetic);
                    }
                    Some(WatchMsg::Claim { conn }) => {
                        self.sim.set_presenter(Some(conn));
                        self.publish(&events, Vec::new());
                    }
                    Some(WatchMsg::ReleasePresenter { conn }) => {
                        if self.sim.presenter() == Some(conn) {
                            self.sim.set_presenter(None);
                            self.publish(&events, Vec::new());
                        }
                    }
                    Some(WatchMsg::Owned { conn, reply }) => {
                        let _ = reply.send(self.sim.owned_by(conn));
                    }
                    Some(WatchMsg::Get { reply }) => {
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
pub struct WatchHandles {
    cmd_tx: mpsc::Sender<WatchMsg>,
    evt_tx: broadcast::Sender<WatchEvent>,
}

impl WatchHandles {
    pub fn subscribe(&self) -> broadcast::Receiver<WatchEvent> {
        self.evt_tx.subscribe()
    }

    pub fn cmd_tx(&self) -> mpsc::Sender<WatchMsg> {
        self.cmd_tx.clone()
    }
}

pub async fn backend(restart: mpsc::UnboundedReceiver<()>) {
    crate::server::supervise(
        "watch",
        restart,
        |token| {
            let (cmd_tx, cmd_rx) = mpsc::channel(64);
            let (evt_tx, _) = broadcast::channel(256);
            let events = evt_tx.clone();
            let task = tokio::spawn(async move {
                WatchService::default()
                    .event_loop(cmd_rx, events, token)
                    .await;
            });
            (WatchHandles { cmd_tx, evt_tx }, task)
        },
        set_watch_handles,
    )
    .await
}

pub async fn watch_socket(ws: WebSocketUpgrade) -> Response {
    ws.on_upgrade(handle_watch_socket)
}

async fn handle_watch_socket(mut socket: WebSocket) {
    let conn = next_conn();

    let Some(handles) = crate::server::watch_handles() else {
        close_restarting(&mut socket).await;
        return;
    };
    let cmd_tx = handles.cmd_tx();

    // snapshot before subscribing so the handshake already carries the state
    let (reply_tx, reply_rx) = oneshot::channel();
    if cmd_tx
        .send(WatchMsg::Get { reply: reply_tx })
        .await
        .is_err()
    {
        close_restarting(&mut socket).await;
        return;
    }
    let state = reply_rx
        .await
        .unwrap_or_else(|_| WatchSim::new().snapshot());

    // subscribe first, then drop our sender clone so the channel can close
    // when the backend restarts the actor
    let mut evt_rx = handles.subscribe();
    drop(handles);

    'conn: {
        if send_json(&mut socket, &WatchEvent::Hello { conn })
            .await
            .is_err()
        {
            break 'conn;
        }
        if send_json(&mut socket, &WatchEvent::Snapshot { state })
            .await
            .is_err()
        {
            break 'conn;
        }

        loop {
            tokio::select! {
                msg = socket.recv() => match msg {
                    Some(Ok(Message::Text(text))) => {
                        match serde_json::from_str::<WatchWire>(text.as_str()) {
                            Ok(WatchWire::ClaimPresenter) => {
                                if cmd_tx.send(WatchMsg::Claim { conn }).await.is_err() {
                                    close_restarting(&mut socket).await;
                                    break 'conn;
                                }
                            }
                            Ok(WatchWire::CloneReceiver { rx }) => {
                                let (reply_tx, reply_rx) = oneshot::channel();
                                if cmd_tx
                                    .send(WatchMsg::CloneReceiver { requester: conn, rx, reply: reply_tx })
                                    .await
                                    .is_err()
                                {
                                    close_restarting(&mut socket).await;
                                    break 'conn;
                                }
                                // the fresh id surfaces in the broadcast; the
                                // actor tracks ownership for disconnect cleanup
                                let _ = reply_rx.await;
                            }
                            Ok(wire) => {
                                // ownership of receiver commands is enforced by
                                // the sim (owner == conn); forwards as-is
                                let msg = match wire {
                                    WatchWire::Create { init } => Some(WatchMsg::Create { conn, init }),
                                    WatchWire::Send { ch } => Some(WatchMsg::Send { conn, ch }),
                                    WatchWire::NewReceiver => Some(WatchMsg::NewReceiver { conn }),
                                    WatchWire::AwaitChange { rx } => Some(WatchMsg::AwaitChange { conn, rx }),
                                    WatchWire::LookInside { rx } => Some(WatchMsg::LookInside { conn, rx }),
                                    WatchWire::DropReceiver { rx } => Some(WatchMsg::DropReceiver { conn, rx }),
                                    WatchWire::CloneReceiver { .. }
                                    | WatchWire::ClaimPresenter => unreachable!(),
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
                        if cmd_tx.send(WatchMsg::Get { reply: reply_tx }).await.is_ok() {
                            if let Ok(state) = reply_rx.await {
                                let _ = send_json(&mut socket, &WatchEvent::Snapshot { state }).await;
                            }
                        }
                    }
                },
            }
        }
    }

    // dropping the connection drops every receiver it owns, like dropping an
    // rx handle; a send it had blocked flushes (or refuses) as a side effect
    let (reply_tx, reply_rx) = oneshot::channel();
    if cmd_tx
        .send(WatchMsg::Owned {
            conn,
            reply: reply_tx,
        })
        .await
        .is_ok()
    {
        if let Ok(ids) = reply_rx.await {
            for rx in ids {
                let _ = cmd_tx.send(WatchMsg::DropReceiver { conn, rx }).await;
            }
        }
    }
    let _ = cmd_tx.send(WatchMsg::ReleasePresenter { conn }).await;
}

async fn close_restarting(socket: &mut WebSocket) {
    let _ = socket
        .send(Message::Close(Some(CloseFrame {
            code: 4001,
            reason: "restarting".into(),
        })))
        .await;
}
