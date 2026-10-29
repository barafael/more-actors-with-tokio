//! The timer's argument, as the code shows it: `sleep(3s)`, with steppers
//! either side of the number. Drawn in the slide rather than handed to a
//! native control, so it reads as the call it sets.

use dioxus::prelude::*;

use super::model::MAX_WAIT_S;

/// `sleep(` − n s + `)`. `locked` greys the steppers while a sleep already
/// holds its argument.
#[component]
pub fn SleepCall(wait_s: u64, locked: bool, on_change: EventHandler<u64>) -> Element {
    rsx! {
        span { class: "sleep-call",
            "sleep("
            button {
                class: "sleep-step",
                disabled: locked || wait_s == 0,
                title: "shorter",
                onclick: move |_| on_change.call(wait_s.saturating_sub(1)),
                "−"
            }
            span { class: "sleep-arg", "{wait_s}s" }
            button {
                class: "sleep-step",
                disabled: locked || wait_s >= MAX_WAIT_S,
                title: "longer",
                onclick: move |_| on_change.call((wait_s + 1).min(MAX_WAIT_S)),
                "+"
            }
            ")"
        }
    }
}
