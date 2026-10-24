//! The minigames, one module each, and the pieces several of them share:
//! task chips and wait badges.

use dioxus::prelude::*;

pub mod board;
pub mod broadcast;
pub mod call;
pub mod chat;
pub mod cycle;
pub mod futures;
pub mod mpsc;
pub mod mutex;
pub mod oneshot;
pub mod palette;
pub mod pipeline;
pub mod speedd;
pub mod step;
pub mod ticker;
pub mod unit_test;
pub mod unordered;
pub mod watch;
pub mod watchdog;

/// One task in its own colour, with whatever badge the game gives it.
#[component]
pub fn TaskChip(task: u64, class: String, children: Element) -> Element {
    rsx! {
        span {
            class: "mx-task {class}",
            style: "--c: {palette::sender_hex(task)};",
            "task {task}"
            {children}
        }
    }
}

/// A wait that counts itself up: whole seconds since `since`, on the frame
/// clock.
///
/// It reads the frame clock itself, so each frame only this badge redraws;
/// the game around it re-renders when its sim changes and not before.
/// Unbounded by design — nothing times out a parked `lock()` or `send()`.
#[component]
pub fn Waited(
    since: f64,
    clock: Signal<f64>,
    #[props(default)] label: String,
    #[props(default = "blocked-clock".to_string())] class: String,
) -> Element {
    let seconds = ((clock() - since) / 1000.0).floor().max(0.0) as u64;
    rsx! {
        span { class, "{label}{seconds}s" }
    }
}
