//! The button-future minigame: one future, three buttons, real I/O.

use dioxus::prelude::*;

use super::model::{ButtonSim, ButtonState, Color};

#[component]
pub fn ButtonGame() -> Element {
    let mut sim = use_signal(ButtonSim::new);
    let state = sim.read().state();

    let (dot_class, dot_label) = match state {
        ButtonState::Idle => ("future-pad", "idle"),
        ButtonState::Pending => ("future-pad pending", "pending"),
        ButtonState::Ready(_) => ("future-pad resolved", "resolved"),
    };

    rsx! {
        div { class: "diagram button-game",
            div { class: "game-header", "button" }

            // the future, as an actor dot
            div { class: dot_class, {dot_label} }

            // resolved: arrow to the yielded value, right of the consumed future
            if let ButtonState::Ready(color) = state {
                div { class: "future-arrow" }
                div { class: "result-pad {color.css_name()}", "{color.css_name()}" }
            }

            div { class: "create-row",
                if state != ButtonState::Pending {
                    button {
                        class: "btn await",
                        onclick: move |_| {
                            sim.with_mut(|s| s.activate());
                        },
                        "Await the future"
                    }
                }
            }

            div { class: "press-row",
                for color in Color::ALL {
                    button {
                        class: "press-pad {color.css_name()}",
                        disabled: state != ButtonState::Pending,
                        title: "{color.display()}",
                        onclick: move |_| {
                            sim.with_mut(|s| s.press(color));
                        },
                        "{color.display()}"
                    }
                }
            }

            div { class: "game-footer",
                if matches!(state, ButtonState::Ready(_)) {
                    span { class: "note-pill blocked", "the resolved future is consumed — start a new one" }
                } else {
                    span {}
                }
                button {
                    class: "btn",
                    onclick: move |_| sim.set(ButtonSim::new()),
                    "restart"
                }
            }
        }
    }
}
