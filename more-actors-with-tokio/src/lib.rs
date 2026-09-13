pub mod games;
pub mod highlight;
pub mod protocol;
#[cfg(feature = "server")]
pub mod server;
pub mod sim;
pub mod slides;
pub mod ws_client;

use dioxus::prelude::*;

use crate::protocol::AppDown;
use crate::ws_client::{SocketHandle, WsEvent};

pub use crate::protocol::AppUp;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum GameMode {
    /// Talk mode: state lives in server actors, reached over websockets.
    Remote,
    /// Static export mode: the same sims run directly in the browser.
    Local,
}

impl GameMode {
    pub fn detect() -> Self {
        #[cfg(target_arch = "wasm32")]
        {
            let is_local = web_sys::window()
                .and_then(|w| w.document())
                .and_then(|d| d.query_selector("meta[name=\"game-mode\"]").ok().flatten())
                .and_then(|m| m.get_attribute("content"))
                .as_deref()
                == Some("local");
            if is_local {
                GameMode::Local
            } else {
                GameMode::Remote
            }
        }
        #[cfg(not(target_arch = "wasm32"))]
        {
            GameMode::Remote
        }
    }
}

/// In the static export, each page represents one slide (slide-N-name.html).
fn initial_slide() -> usize {
    #[cfg(target_arch = "wasm32")]
    {
        let path = web_sys::window()
            .map(|w| w.location().pathname().unwrap_or_default())
            .unwrap_or_default();
        path.split('/')
            .next_back()
            .and_then(|file| file.strip_prefix("slide-"))
            .and_then(|rest| rest.split('-').next())
            .and_then(|n| n.parse().ok())
            .unwrap_or(0)
    }
    #[cfg(not(target_arch = "wasm32"))]
    {
        0
    }
}

#[derive(Clone, Copy)]
pub struct AppCtx {
    pub app: SocketHandle,
}

impl AppCtx {
    pub fn send(&self, msg: AppUp) {
        self.app.send_json(&msg);
    }
}

#[component]
pub fn App() -> Element {
    let mode = use_hook(GameMode::detect);
    rsx! {
        Deck { mode, initial_slide: initial_slide() }
    }
}

/// The whole deck. `mode` and `initial_slide` are explicit so the static
/// export can render exactly the tree the client will boot.
#[component]
pub fn Deck(mode: GameMode, initial_slide: usize) -> Element {
    use_context_provider(|| mode);

    let socket = use_signal(|| None::<ws_client::RawSocket>);
    let mut app = SocketHandle::new(socket);
    use_context_provider(|| AppCtx { app });

    let mut slide = use_signal(move || initial_slide);

    use_effect(move || {
        if mode == GameMode::Local {
            return;
        }
        ws_client::spawn_ws_loop("/ws/app", move |event| match event {
            WsEvent::Ping => app.send_raw("ping"),
            WsEvent::Open { socket } => {
                app.set(socket);
            }
            WsEvent::Message(text) => {
                if let Ok(down) = serde_json::from_str::<AppDown>(&text) {
                    slide.set(down.slide);
                }
            }
            WsEvent::Closed { .. } => {
                app.clear();
            }
            WsEvent::Retry { .. } => {}
        });
    });

    rsx! {
        document::Link { rel: "icon", href: asset!("/assets/favicon.ico") }
        document::Link { rel: "stylesheet", href: asset!("/assets/main.css") }
        document::Link { rel: "stylesheet", href: "https://fonts.googleapis.com/css2?family=Bitter:ital@0;1&family=Fira+Mono&display=swap" }
        div { class: "topbar" }
        { match slide() {
            0 => rsx! { slides::Title {} },
            1 => rsx! { slides::ButtonGameSlide {} },
            2 => rsx! { slides::MpscGameSlide {} },
            3 => rsx! { slides::WatchGameSlide {} },
            4 => rsx! { slides::Recipe {} },
            _ => rsx! { slides::BroadcastGameSlide {} },
        } }
        slides::Chrome { slide: slide }
    }
}
