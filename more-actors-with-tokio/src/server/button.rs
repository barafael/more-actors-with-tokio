use axum::extract::ws::{CloseFrame, Message, WebSocket, WebSocketUpgrade};
use axum::response::Response;
use tokio::sync::{broadcast, mpsc, oneshot};
use tokio_util::sync::CancellationToken;

use crate::protocol::{ButtonEvent, ButtonState, ButtonWire, Color};
use crate::server::{send_json, set_button_handles};
use crate::sim::ButtonSim;

pub enum ButtonMsg {
    Press { color: Color },
    Activate,
    Get { reply: oneshot::Sender<ButtonState> },
}

#[derive(Default)]
pub struct ButtonService {
    sim: ButtonSim,
}

impl ButtonService {
    pub async fn event_loop(
        mut self,
        mut rx: mpsc::Receiver<ButtonMsg>,
        events: broadcast::Sender<ButtonEvent>,
        token: CancellationToken,
    ) -> Self {
        loop {
            tokio::select! {
                msg = rx.recv() => match msg {
                    Some(msg) => {
                        let wire = match msg {
                            ButtonMsg::Press { color } => ButtonWire::Press { color },
                            ButtonMsg::Activate => ButtonWire::Activate,
                            ButtonMsg::Get { reply } => {
                                let _ = reply.send(self.sim.state);
                                continue;
                            }
                        };
                        for event in self.sim.handle(&wire) {
                            let _ = events.send(event);
                        }
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
pub struct ButtonHandles {
    cmd_tx: mpsc::Sender<ButtonMsg>,
    evt_tx: broadcast::Sender<ButtonEvent>,
}

impl ButtonHandles {
    pub fn subscribe(&self) -> broadcast::Receiver<ButtonEvent> {
        self.evt_tx.subscribe()
    }

    pub fn cmd_tx(&self) -> mpsc::Sender<ButtonMsg> {
        self.cmd_tx.clone()
    }
}

pub async fn backend(mut restart: mpsc::UnboundedReceiver<()>) {
    loop {
        let token = CancellationToken::new();
        let (cmd_tx, cmd_rx) = mpsc::channel(64);
        let (evt_tx, _) = broadcast::channel(64);
        let keepalive = cmd_tx.clone();
        let mut task = tokio::spawn(ButtonService::default().event_loop(
            cmd_rx,
            evt_tx.clone(),
            token.clone(),
        ));
        set_button_handles(Some(ButtonHandles { cmd_tx, evt_tx }));

        let outcome = tokio::select! {
            _ = restart.recv() => {
                token.cancel();
                task.await
            }
            outcome = &mut task => outcome,
        };

        let _final_state = outcome.expect("button actor panicked");
        set_button_handles(None);
        drop(keepalive);
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
    }
}

pub async fn button_socket(ws: WebSocketUpgrade) -> Response {
    ws.on_upgrade(handle_button_socket)
}

async fn handle_button_socket(mut socket: WebSocket) {
    let Some(handles) = crate::server::button_handles() else {
        close_restarting(&mut socket).await;
        return;
    };

    let (reply_tx, reply_rx) = oneshot::channel();
    if handles
        .cmd_tx()
        .send(ButtonMsg::Get { reply: reply_tx })
        .await
        .is_err()
    {
        close_restarting(&mut socket).await;
        return;
    }
    let state = reply_rx.await.unwrap_or(ButtonState::Idle);

    // Subscribe first, then drop our sender clone so the channel can close
    // when the backend restarts the actor. Holding the sender here would
    // keep the channel alive forever and sockets would never observe Closed.
    let cmd_tx = handles.cmd_tx();
    let mut evt_rx = handles.subscribe();
    drop(handles);

    if send_json(&mut socket, &ButtonEvent::Snapshot { state })
        .await
        .is_err()
    {
        return;
    }

    loop {
        tokio::select! {
            msg = socket.recv() => match msg {
                Some(Ok(Message::Text(text))) => {
                    if let Ok(wire) = serde_json::from_str::<ButtonWire>(text.as_str()) {
                        let msg = match wire {
                            ButtonWire::Activate => ButtonMsg::Activate,
                            ButtonWire::Press { color } => ButtonMsg::Press { color },
                        };
                        if cmd_tx.send(msg).await.is_err() {
                            close_restarting(&mut socket).await;
                            break;
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
                    break;
                }
                Err(broadcast::error::RecvError::Lagged(_)) => {
                    resync(&mut socket, &cmd_tx).await;
                }
            },
        }
    }
}

async fn resync(socket: &mut WebSocket, cmd_tx: &mpsc::Sender<ButtonMsg>) {
    let (reply_tx, reply_rx) = oneshot::channel();
    if cmd_tx
        .send(ButtonMsg::Get { reply: reply_tx })
        .await
        .is_ok()
    {
        if let Ok(state) = reply_rx.await {
            let _ = send_json(socket, &ButtonEvent::Snapshot { state }).await;
        }
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
