use axum::extract::ws::{Message, WebSocket, WebSocketUpgrade};
use axum::response::Response;
use tokio::sync::{broadcast, mpsc, oneshot};
use tokio_util::sync::CancellationToken;

use crate::protocol::{ButtonEvent, ButtonState, ButtonWire, Color};
use crate::server::auth::Connecting;
use crate::server::{close_restarting, release, send_json, AppState};
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

pub async fn backend(
    restart: mpsc::UnboundedReceiver<()>,
    handles_tx: tokio::sync::watch::Sender<Option<ButtonHandles>>,
) {
    crate::server::supervise("button", restart, handles_tx, |token| {
        let (cmd_tx, cmd_rx) = mpsc::channel(64);
        let (evt_tx, _) = broadcast::channel(64);
        let events = evt_tx.clone();
        let task = tokio::spawn(async move {
            ButtonService::default()
                .event_loop(cmd_rx, events, token)
                .await;
        });
        (ButtonHandles { cmd_tx, evt_tx }, task)
    })
    .await
}

pub async fn button_socket(
    ws: WebSocketUpgrade,
    connecting: Connecting,
    axum::extract::State(state): axum::extract::State<AppState>,
) -> Response {
    ws.on_upgrade(move |socket| handle_button_socket(socket, state, connecting))
}

async fn handle_button_socket(mut socket: WebSocket, app: AppState, connecting: Connecting) {
    let Connecting { identity, conn: _ } = connecting;

    let Some(handles) = app.button.handles() else {
        release(&app, &identity);
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
        release(&app, &identity);
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
        release(&app, &identity);
        return;
    }

    loop {
        tokio::select! {
            msg = socket.recv() => match msg {
                Some(Ok(Message::Text(text))) => {
                    if let Ok(wire) = serde_json::from_str::<ButtonWire>(text.as_str()) {
                        // spectators watch the future resolve; they do not
                        // get to be the I/O that resolves it
                        if !identity.may_play() {
                            continue;
                        }
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

    release(&app, &identity);
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
