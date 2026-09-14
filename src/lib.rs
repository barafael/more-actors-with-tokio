pub mod games;
pub mod highlight;
pub mod protocol;
#[cfg(feature = "server")]
pub mod server;
pub mod sim;
pub mod slides;
pub mod ws_client;

use dioxus::prelude::*;

use crate::protocol::{AppDown, Role, SLIDE_COUNT};
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
    /// What the server decided this connection may do. Read by the games to
    /// decide what to *show*; the server decides what to allow.
    pub identity: Signal<Identity>,
}

impl AppCtx {
    pub fn send(&self, msg: AppUp) {
        self.app.send_json(&msg);
    }

    pub fn role(&self) -> Role {
        self.identity.read().role
    }

    /// Whether to render controls that act on a game.
    ///
    /// A spectator sees the games play out but gets no buttons. This is a
    /// UI decision only: every one of these actions is also refused by the
    /// server, so hiding them is courtesy rather than enforcement.
    pub fn may_play(&self) -> bool {
        self.identity.read().role.may_play()
    }

    pub fn may_present(&self) -> bool {
        self.identity.read().role.may_present()
    }

    /// Step the deck by one slide in either direction.
    ///
    /// The server owns the slide index in a talk, so remote mode asks and
    /// waits to be told; the export has no server and moves its own signal.
    /// Buttons and keys share this so they cannot drift.
    pub fn step_slide(&self, mode: GameMode, mut slide: Signal<usize>, forward: bool) {
        match mode {
            GameMode::Remote => self.send(if forward {
                AppUp::AdvanceSlide
            } else {
                AppUp::PreviousSlide
            }),
            GameMode::Local => slide.with_mut(|index| {
                let step = if forward { 1 } else { SLIDE_COUNT - 1 };
                *index = (*index + step) % SLIDE_COUNT;
            }),
        }
    }
}

/// The client's view of who it is, as granted by the server.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Identity {
    pub role: Role,
    pub seat: Option<usize>,
    pub players_present: usize,
    pub players_capacity: usize,
    /// Set only for the presenter: the URL the QR code encodes.
    pub join_url: Option<String>,
}

impl Identity {
    /// In the static export there is no server to grant anything and no
    /// room to share: the games run single-player in one browser, so the
    /// lone visitor is both the player and the presenter of their own copy.
    fn local() -> Self {
        Self {
            role: Role::Presenter,
            ..Self::default()
        }
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
    let mut identity = use_signal(|| match mode {
        GameMode::Local => Identity::local(),
        GameMode::Remote => Identity::default(),
    });
    use_context_provider(|| AppCtx { app, identity });

    let mut slide = use_signal(move || initial_slide);
    let mut showing_join = use_signal(|| false);
    // The navigation bar is hidden until the mouse moves, marp-style.
    let mut awake = use_signal(|| false);

    use_effect(move || {
        if mode == GameMode::Local {
            return;
        }
        ws_client::spawn_ws_loop("/ws/app", move |event| match event {
            WsEvent::Ping => app.send_raw(crate::protocol::KEEPALIVE),
            WsEvent::Open { socket } => {
                app.set(socket);
            }
            WsEvent::Message(text) => {
                if let Ok(down) = serde_json::from_str::<AppDown>(&text) {
                    slide.set(down.slide);
                    // A ticket only arrives once, on the first payload of a
                    // connection that did not present one.
                    if let Some(ticket) = down.granted_ticket.as_deref() {
                        ws_client::remember_ticket(ticket);
                    }
                    identity.set(Identity {
                        role: down.role,
                        seat: down.seat,
                        players_present: down.players_present,
                        players_capacity: down.players_capacity,
                        join_url: down.join_url,
                    });
                }
            }
            WsEvent::Closed { .. } => {
                app.clear();
            }
            WsEvent::Retry { .. } => {}
        });
    });

    // Deck keys, bound on a focusable root rather than the window so the
    // export stays inert. `q` is the join code, as CONCEPT.md specifies;
    // the rest are the navigation a presenter expects from any deck.
    let join = identity.read().join_url.clone();
    let ctx = AppCtx { app, identity };
    let on_key = move |event: KeyboardEvent| {
        let character = |want: &str| event.key() == Key::Character(want.to_string());

        if character("q") {
            showing_join.toggle();
            return;
        }
        // Escape closes the overlay rather than moving the deck, so a
        // presenter cannot dismiss the QR and skip a slide in one press.
        if event.key() == Key::Escape {
            showing_join.set(false);
            return;
        }
        if !ctx.may_present() {
            return;
        }
        match event.key() {
            Key::ArrowRight | Key::PageDown => ctx.step_slide(mode, slide, true),
            Key::ArrowLeft | Key::PageUp => ctx.step_slide(mode, slide, false),
            // space is the remote-clicker key, and the one a presenter
            // reaches for without thinking
            _ if character(" ") => ctx.step_slide(mode, slide, true),
            _ => {}
        }
    };

    // Waking the bar lives here rather than on the bar's own container:
    // that container spans the bottom of the slide, where game controls
    // genuinely sit, so it must stay click-through and therefore cannot
    // hear the mouse itself. Each movement restarts the countdown and the
    // generation guard makes the last one win.
    let mut generation = use_signal(|| 0u64);
    let wake = move |_| {
        if !awake() {
            awake.set(true);
        }
        let mine = generation() + 1;
        generation.set(mine);
        spawn(async move {
            sleep_ms(2000).await;
            if generation() == mine {
                awake.set(false);
            }
        });
    };

    rsx! {
        document::Link { rel: "icon", href: asset!("/assets/favicon.ico") }
        document::Link { rel: "stylesheet", href: asset!("/assets/main.css") }
        document::Link { rel: "stylesheet", href: "https://fonts.googleapis.com/css2?family=Bitter:ital@0;1&family=Fira+Mono&display=swap" }
        div {
            class: "deck-root",
            // Focusable so `q` can be bound here rather than on the window,
            // which would also fire in the static export. Nothing else takes
            // focus on load, so the deck claims it and the key works without
            // the presenter having to click the page first.
            tabindex: 0,
            autofocus: true,
            onmounted: move |event| async move {
                event
                    .set_focus(true)
                    .await
                    .inspect_err(|error| {
                        tracing::warn!(%error, "deck did not take focus; `q` needs a click first")
                    })
                    .ok();
            },
            onkeydown: on_key,
            onmousemove: wake,
            div { class: "topbar",
                SeatBadge {}
            }
            { match slide() {
                0 => rsx! { slides::Title {} },
                1 => rsx! { slides::ButtonGameSlide {} },
                2 => rsx! { slides::MpscGameSlide {} },
                3 => rsx! { slides::WatchGameSlide {} },
                4 => rsx! { slides::Recipe {} },
                _ => rsx! { slides::BroadcastGameSlide {} },
            } }
            slides::Chrome { slide, awake }
            if showing_join() {
                games::qr::JoinOverlay {
                    url: join.clone(),
                    players_present: identity.read().players_present,
                    players_capacity: identity.read().players_capacity,
                    on_close: move |_| showing_join.set(false),
                }
            }
        }
    }
}

/// The viewer's own standing: which seat they hold, or that they hold none.
///
/// Small and always visible, because "am I a sender?" is the question the
/// whole talk turns on, and an attendee who quietly failed to get a handle
/// should be able to see that rather than wonder why nothing responds.
#[component]
fn SeatBadge() -> Element {
    let ctx: AppCtx = use_context();
    let mode = use_context::<GameMode>();
    let identity = ctx.identity.read();

    // The export is one browser playing alone: it holds every role, so
    // naming one would be a fiction. Say what is actually true instead.
    if mode == GameMode::Local {
        return rsx! {
            span { class: "seat-badge local", "single-player" }
        };
    }

    rsx! {
        span { class: "seat-badge {identity.role.label()}",
            match (identity.role, identity.seat) {
                (Role::Presenter, _) => rsx! { "presenter" },
                (Role::Player, Some(seat)) => rsx! { "sender #{seat}" },
                (Role::Player, None) => rsx! { "sender" },
                (Role::Spectator, _) => rsx! { "watching" },
            }
        }
    }
}

/// Sleep without pulling an async runtime into the client.
async fn sleep_ms(ms: u32) {
    #[cfg(target_arch = "wasm32")]
    gloo_timers::future::TimeoutFuture::new(ms).await;
    #[cfg(not(target_arch = "wasm32"))]
    let _ = ms;
}
