// Dioxus rsx string literals trip this lint on every span.
#![allow(clippy::useless_format)]

pub mod clock;
pub mod games;
pub mod highlight;
pub mod slides;

use dioxus::prelude::*;

use crate::slides::SLIDE_COUNT;

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

/// Step the deck by one slide in either direction. Buttons and keys share
/// this so they cannot drift.
pub fn step_slide(mut slide: Signal<usize>, forward: bool) {
    slide.with_mut(|index| {
        let step = if forward { 1 } else { SLIDE_COUNT - 1 };
        *index = (*index + step) % SLIDE_COUNT;
    });
}

#[component]
pub fn App() -> Element {
    rsx! {
        Deck { initial_slide: initial_slide() }
    }
}

/// The whole deck. `initial_slide` is explicit so the static export can
/// render exactly the tree the client will boot.
#[component]
pub fn Deck(initial_slide: usize) -> Element {
    let slide = use_signal(move || initial_slide);
    // The navigation bar is hidden until the mouse moves, marp-style.
    let mut awake = use_signal(|| false);

    // Deck keys, bound on a focusable root rather than the window so the
    // export stays inert. These are simply the navigation you expect from
    // any deck. The games' text fields stop their own keydowns, so typing a
    // space or moving the cursor in one never turns the slide.
    let on_key = move |event: KeyboardEvent| {
        // Alt+Arrow is the browser's back/forward, Ctrl/Cmd chords belong to
        // the browser too: same rule as the export's fallback handler.
        if event
            .modifiers()
            .intersects(Modifiers::ALT | Modifiers::CONTROL | Modifiers::META)
        {
            return;
        }
        let character = |want: &str| event.key() == Key::Character(want.to_string());

        match event.key() {
            Key::ArrowRight | Key::PageDown => step_slide(slide, true),
            Key::ArrowLeft | Key::PageUp => step_slide(slide, false),

            // space is the clicker key, and the one you
            // reach for without thinking
            _ if character(" ") => step_slide(slide, true),
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
            games::ticker::sleep_ms(2000).await;
            if generation() == mine {
                awake.set(false);
            }
        });
    };

    rsx! {
        document::Link { rel: "icon", href: asset!("/assets/favicon.ico") }
        document::Link { rel: "stylesheet", href: asset!("/assets/main.css") }
        document::Link { rel: "stylesheet", href: "https://fonts.googleapis.com/css2?family=Atkinson+Hyperlegible&family=Bitter:ital@0;1&family=Fira+Mono&family=Space+Mono:wght@400;700&display=swap" }
        div {
            class: "deck-root",
            // Focusable so the navigation keys can be bound here rather than
            // on the window, which would also fire in the static export.
            // Nothing else takes focus on load, so the deck claims it and the
            // keys work without having to click the page first.
            tabindex: 0,
            autofocus: true,
            onmounted: move |event| async move {
                event
                    .set_focus(true)
                    .await
                    .inspect_err(|error| {
                        tracing::warn!(%error, "deck did not take focus; keys need a click first")
                    })
                    .ok();
            },
            onkeydown: on_key,
            onmousemove: wake,
            div { class: "topbar" }
            { slides::slide(slide()) }
            slides::Chrome { slide, awake }
        }
    }
}
