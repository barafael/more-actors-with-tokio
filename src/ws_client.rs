use serde::Serialize;

use dioxus::prelude::{ReadableExt, Signal, WritableExt};

#[cfg(target_arch = "wasm32")]
#[derive(Clone)]
pub struct RawSocket(web_sys::WebSocket);

#[cfg(not(target_arch = "wasm32"))]
#[derive(Clone, Copy)]
pub struct RawSocket;

#[derive(Clone, Copy)]
pub struct SocketHandle(Signal<Option<RawSocket>>);

impl SocketHandle {
    pub fn new(signal: Signal<Option<RawSocket>>) -> Self {
        Self(signal)
    }

    /// True while a game websocket is open. Remote actions taken before this
    /// would otherwise be silently dropped.
    pub fn ready(&self) -> bool {
        self.0.read().is_some()
    }

    pub fn set(&mut self, socket: RawSocket) {
        if let Ok(mut slot) = self.0.try_write() {
            *slot = Some(socket);
        }
    }

    pub fn clear(&mut self) {
        if let Ok(mut slot) = self.0.try_write() {
            *slot = None;
        }
    }

    /// Close the underlying socket, if any. Safe to call after unmount.
    pub fn close(&mut self) {
        #[cfg(target_arch = "wasm32")]
        if let Ok(slot) = self.0.try_read() {
            if let Some(RawSocket(ws)) = slot.as_ref() {
                let _ = ws.close();
            }
        }
        self.clear();
    }

    /// Send a raw frame. Used for the keep-alive, whose arrival is the
    /// whole signal — see [`crate::protocol::KEEPALIVE`].
    pub fn send_raw(&self, text: &str) {
        #[cfg(target_arch = "wasm32")]
        if let Some(RawSocket(ws)) = self.0.read().as_ref() {
            let _ = ws.send_with_str(text);
        }
        #[cfg(not(target_arch = "wasm32"))]
        let _ = text;
    }

    pub fn send_json(&self, msg: &impl Serialize) {
        #[cfg(target_arch = "wasm32")]
        {
            if let Some(RawSocket(ws)) = self.0.read().as_ref() {
                let json = serde_json::to_string(msg).expect("serialize message");
                let _ = ws.send_with_str(&json);
            }
        }
        #[cfg(not(target_arch = "wasm32"))]
        let _ = msg;
    }
}

#[allow(dead_code)]
pub enum WsEvent {
    Open {
        socket: RawSocket,
    },
    Message(String),
    Closed {
        code: u16,
    },
    Retry {
        after_s: u64,
    },
    /// Keep-alive frame from the connection's heartbeat task.
    Ping,
}

#[cfg(target_arch = "wasm32")]
pub fn spawn_ws_loop(path: &'static str, mut on_event: impl FnMut(WsEvent) + 'static) {
    use dioxus::prelude::spawn;
    use futures_util::StreamExt;

    // Spawned through dioxus so the task is cancelled when the calling
    // component unmounts; a raw spawn_local loop would outlive its signals.
    spawn(async move {
        let mut delay_s = 1u64;
        loop {
            let (tx, mut rx) = futures_channel::mpsc::unbounded::<WsEvent>();
            // heartbeat: keep-alive frames so the server notices dead peers
            // (wifi loss, crashed tabs) quickly. The server recognises the
            // payload as `protocol::KEEPALIVE` and only updates its
            // last-seen stamp.
            let ping_tx = tx.clone();
            let opened = open_websocket(&ws_url(path), tx);
            let ping_task = spawn(async move {
                loop {
                    gloo_timers::future::TimeoutFuture::new(3000).await;
                    if ping_tx.unbounded_send(WsEvent::Ping).is_err() {
                        break;
                    }
                }
            });
            while let Some(event) = rx.next().await {
                match event {
                    WsEvent::Open { .. } => {
                        delay_s = 1;
                        on_event(event);
                    }
                    WsEvent::Closed { .. } => {
                        on_event(event);
                        break;
                    }
                    other => on_event(other),
                }
            }
            ping_task.cancel();
            drop(opened);
            on_event(WsEvent::Retry { after_s: delay_s });
            gloo_timers::future::TimeoutFuture::new((delay_s * 1000) as u32).await;
            delay_s = (delay_s * 2).min(4);
        }
    });
}

#[cfg(not(target_arch = "wasm32"))]
pub fn spawn_ws_loop(_path: &'static str, on_event: impl FnMut(WsEvent) + 'static) {
    let _ = on_event;
}

#[cfg(target_arch = "wasm32")]
fn ws_url(path: &str) -> String {
    let base = if cfg!(debug_assertions) {
        format!("ws://127.0.0.1:8080{path}")
    } else {
        let location = web_sys::window().expect("window").location();
        let scheme = if location.protocol().is_ok_and(|p| p == "https:") {
            "wss"
        } else {
            "ws"
        };
        let host = location.host().unwrap_or_default();
        format!("{scheme}://{host}{path}")
    };

    // Every socket presents the same credentials the page was opened with,
    // so the server can rule on them once per connection. Without this a
    // game socket would arrive anonymous and be seated as a spectator.
    match credentials_query() {
        Some(query) => format!("{base}?{query}"),
        None => base,
    }
}

/// The credential parameters from this page's own URL, ready to append.
///
/// Read from `location.search` every time rather than cached: the deck
/// rewrites the URL when the server grants a ticket, and sockets opened
/// after that must carry the new one.
#[cfg(target_arch = "wasm32")]
fn credentials_query() -> Option<String> {
    use crate::protocol::{PRESENTER_PARAM, TICKET_PARAM};

    let search = web_sys::window()?.location().search().ok()?;
    let mut kept: Vec<String> = Vec::new();
    for pair in search.trim_start_matches('?').split('&') {
        let Some((key, value)) = pair.split_once('=') else {
            continue;
        };
        if value.is_empty() {
            continue;
        }
        if key == TICKET_PARAM || key == PRESENTER_PARAM {
            kept.push(format!("{key}={value}"));
        }
    }
    (!kept.is_empty()).then(|| kept.join("&"))
}

#[cfg(target_arch = "wasm32")]
fn open_websocket(url: &str, tx: futures_channel::mpsc::UnboundedSender<WsEvent>) -> RawSocket {
    use wasm_bindgen::prelude::*;

    let ws = web_sys::WebSocket::new(url).expect("open websocket");
    ws.set_binary_type(web_sys::BinaryType::Arraybuffer);

    {
        let tx = tx.clone();
        let this = ws.clone();
        let onopen: Closure<dyn FnMut(JsValue)> = Closure::new(move |_: JsValue| {
            let _ = tx.unbounded_send(WsEvent::Open {
                socket: RawSocket(this.clone()),
            });
        });
        ws.set_onopen(Some(onopen.as_ref().unchecked_ref()));
        onopen.forget();
    }
    {
        let tx = tx.clone();
        let onmessage: Closure<dyn FnMut(JsValue)> = Closure::new(move |js: JsValue| {
            let event = js.unchecked_into::<web_sys::MessageEvent>();
            if let Some(text) = event.data().as_string() {
                let _ = tx.unbounded_send(WsEvent::Message(text));
            }
        });
        ws.set_onmessage(Some(onmessage.as_ref().unchecked_ref()));
        onmessage.forget();
    }
    {
        let onclose: Closure<dyn FnMut(JsValue)> = Closure::new(move |js: JsValue| {
            let event = js.unchecked_into::<web_sys::CloseEvent>();
            let _ = tx.unbounded_send(WsEvent::Closed { code: event.code() });
        });
        ws.set_onclose(Some(onclose.as_ref().unchecked_ref()));
        onclose.forget();
    }

    RawSocket(ws)
}

/// Record a ticket the server just granted, by writing it into this page's
/// URL without navigating.
///
/// The URL is the only place a ticket can live that survives a reload and
/// is visible to every socket the page opens. It also means an attendee who
/// bookmarks the page keeps their seat, which is the behaviour a phone
/// browser makes people expect.
pub fn remember_ticket(ticket: &str) {
    #[cfg(target_arch = "wasm32")]
    {
        use crate::protocol::TICKET_PARAM;

        let Some(window) = web_sys::window() else {
            return;
        };
        let location = window.location();
        let (Ok(pathname), Ok(search)) = (location.pathname(), location.search()) else {
            return;
        };
        if search.contains(&format!("{TICKET_PARAM}=")) {
            return;
        }
        let separator = if search.is_empty() { "?" } else { "&" };
        let url = format!("{pathname}{search}{separator}{TICKET_PARAM}={ticket}");
        if let Ok(history) = window.history() {
            history
                .replace_state_with_url(&wasm_bindgen::JsValue::NULL, "", Some(&url))
                .inspect_err(|_| tracing::warn!("could not record the ticket in the url"))
                .ok();
        }
    }
    #[cfg(not(target_arch = "wasm32"))]
    let _ = ticket;
}
