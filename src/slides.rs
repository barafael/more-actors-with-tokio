use dioxus::prelude::*;

use crate::games::broadcast::BroadcastGame;
use crate::games::button::ButtonGame;
use crate::games::mpsc::MpscGame;
use crate::games::watch::WatchGame;
use crate::highlight::highlight;
use crate::{protocol::SLIDE_COUNT, AppCtx, GameMode};

#[component]
pub fn Title() -> Element {
    rsx! {
        div { class: "slide lead",
            h1 { "Actors with Tokio" }
            h2 { "Live minigames" }
            p { class: "dim", "You are the senders." }
        }
    }
}

#[component]
pub fn ButtonGameSlide() -> Element {
    rsx! {
        div { class: "slide",
            h2 { "The button future" }
            p { class: "dim", "This is not an actor — just a future. Activate it, then do real I/O: the future resolves with whichever button is pressed." }
            ButtonGame {}
        }
    }
}

const RECIPE_CODE: &str = r#"struct ButtonService {
    pressed: Option<Color>,
}

impl ButtonService {
    pub async fn event_loop(
        mut self,
        mut rx: mpsc::Receiver<ButtonMsg>,
        token: CancellationToken,
    ) -> Self {
        loop {
            tokio::select! {
                msg = rx.recv() => match msg { /* ... */ },
                _ = token.cancelled() => break,
            }
        }
        self
    }
}"#;

#[component]
pub fn MpscGameSlide() -> Element {
    rsx! {
        div { class: "slide",
            h2 { "The mpsc channel" }
            p { class: "dim", "One bounded buffer, many senders. You are a sender: send on a full channel and you block until a receive frees a slot." }
            MpscGame {}
        }
    }
}

#[component]
pub fn WatchGameSlide() -> Element {
    rsx! {
        div { class: "slide",
            h2 { "The watch channel" }
            p { class: "dim", "One value, many receivers. borrow() reveals it; changed() waits for the next different one." }
            WatchGame {}
        }
    }
}

#[component]
pub fn BroadcastGameSlide() -> Element {
    rsx! {
        div { class: "slide",
            h2 { "The broadcast channel" }
            p { class: "dim", "Sending never blocks: a full buffer evicts its oldest value. A new receiver starts at the tail — it never sees history. Fall behind and your next receive yields Lagged(n)." }
            BroadcastGame {}
        }
    }
}

#[component]
pub fn Recipe() -> Element {
    rsx! {
        div { class: "slide",
            h2 { "The actor recipe" }
            pre { class: "code", dangerous_inner_html: highlight(RECIPE_CODE, "rust") }
        }
    }
}

/// The presenter's navigation overlay, in the marp idiom: it fades in when
/// the mouse moves and fades back out when it rests.
///
/// A talk is mostly a still image, and a control bar parked over every
/// slide is one more thing on the projector. Keyboard navigation is the
/// primary interface — this is for when the presenter is holding a mouse.
#[component]
pub fn Chrome(slide: Signal<usize>, awake: Signal<bool>) -> Element {
    let ctx: AppCtx = use_context();
    let mode = use_context::<GameMode>();
    let current = slide() + 1;

    // Only the presenter drives the deck. The server refuses these commands
    // from anyone else, so this hides a control that would not have worked
    // rather than enforcing anything. No hooks run above this point, so the
    // early return cannot desynchronise them.
    if !ctx.may_present() {
        return rsx! {};
    }

    rsx! {
        div { class: "chrome-zone",
            div {
                class: if awake() { "chrome awake" } else { "chrome" },
                button {
                    class: "nav",
                    aria_label: "previous slide",
                    onclick: move |_| ctx.step_slide(mode, slide, false),
                    "←"
                }
                span { class: "counter", "{current} / {SLIDE_COUNT}" }
                button {
                    class: "nav",
                    aria_label: "next slide",
                    onclick: move |_| ctx.step_slide(mode, slide, true),
                    "→"
                }
            }
        }
    }
}
