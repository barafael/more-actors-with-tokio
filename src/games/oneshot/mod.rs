//! The oneshot minigame: one value, one sender, one receiver, and nothing
//! you can do twice.
//!
//! Single-player, in the talk as in the export: it is an exercise in
//! ownership, and ownership is personal. Each click is a call that takes a
//! handle by value; the handle disappears from your hands and the call's
//! result is written down, so what `send` and `.await` return — including
//! both errors — is read off the screen rather than recited.

mod model;

use dioxus::prelude::*;

use model::{OneshotModel, Rx, Slot, Tx};

#[component]
pub fn OneshotGame() -> Element {
    let mut model = use_signal(OneshotModel::new);
    let mut draft = use_signal(|| 'a');

    let m = model.read();
    let (tx, rx, slot, exists, finished) = (m.tx, m.rx, m.slot(), m.exists(), m.finished());
    let log = m.log.clone();
    drop(m);

    let slot_text = match slot {
        Slot::Empty => "empty".to_string(),
        Slot::Holds(value) => format!("'{value}'"),
        Slot::Taken => "taken".to_string(),
        Slot::Closed => "closed".to_string(),
    };

    rsx! {
        div { class: "diagram oneshot-game",
            div { class: "game-header", "oneshot" }
            div { class: "status", "single-player · your own channel" }

            div { class: "os-stage",
                div { class: "os-half",
                    Handle {
                        name: "tx",
                        kind: "Sender<char>",
                        state: match tx {
                            Tx::None => "—",
                            Tx::Held => "yours",
                            Tx::Spent => "moved into send",
                            Tx::Dropped => "dropped",
                        },
                        live: tx == Tx::Held,
                    }
                    if tx == Tx::Held {
                        div { class: "os-actions",
                            input {
                                class: "field",
                                maxlength: 1,
                                value: "{draft}",
                                oninput: move |e| {
                                    if let Some(ch) = e.value().chars().next() {
                                        draft.set(ch);
                                    }
                                },
                            }
                            button {
                                class: "btn send",
                                onclick: move |_| model.with_mut(|m| m.send(draft())),
                                "send"
                            }
                            button {
                                class: "btn",
                                onclick: move |_| model.with_mut(|m| m.drop_tx()),
                                "drop"
                            }
                        }
                    }
                }

                div { class: if matches!(slot, Slot::Holds(_)) { "os-slot full" } else { "os-slot" },
                    span { class: "mx-caption", "the channel" }
                    span { class: "os-slot-value", "{slot_text}" }
                }

                div { class: "os-half",
                    Handle {
                        name: "rx",
                        kind: "Receiver<char>",
                        state: match rx {
                            Rx::None => "—".to_string(),
                            Rx::Held => "yours".to_string(),
                            Rx::Awaiting => "inside rx.await".to_string(),
                            Rx::Done(Ok(value)) => format!("yielded '{value}'"),
                            Rx::Done(Err(())) => "yielded RecvError".to_string(),
                            Rx::Dropped => "dropped".to_string(),
                        },
                        live: matches!(rx, Rx::Held | Rx::Awaiting),
                    }
                    match rx {
                        Rx::Held => rsx! {
                            div { class: "os-actions",
                                button {
                                    class: "btn await",
                                    onclick: move |_| model.with_mut(|m| m.await_rx()),
                                    ".await"
                                }
                                button {
                                    class: "btn",
                                    onclick: move |_| model.with_mut(|m| m.drop_rx()),
                                    "drop"
                                }
                            }
                        },
                        Rx::Awaiting => rsx! {
                            div { class: "os-actions",
                                span { class: "note-pill pending", "pending…" }
                                button {
                                    class: "btn",
                                    onclick: move |_| model.with_mut(|m| m.drop_future()),
                                    "drop the future"
                                }
                            }
                        },
                        _ => rsx! {},
                    }
                }
            }

            div { class: "os-log",
                for (index, line) in log.iter().enumerate() {
                    div { key: "{index}", class: if line.ok { "os-line" } else { "os-line err" },
                        span { class: "os-call", "{line.call}" }
                        span { class: "os-arrow", "→" }
                        span { class: "os-result", "{line.result}" }
                    }
                }
            }

            div { class: "create-row",
                if !exists || finished {
                    button {
                        class: "btn await",
                        onclick: move |_| model.with_mut(|m| m.create()),
                        if exists { "another channel" } else { "create the channel" }
                    }
                }
            }

            div { class: "game-footer",
                span { class: "dim", "every call takes its handle by value" }
            }
        }
    }
}

#[component]
fn Handle(name: String, kind: String, state: String, live: bool) -> Element {
    rsx! {
        div { class: if live { "os-handle live" } else { "os-handle" },
            span { class: "os-handle-name", "{name}" }
            span { class: "os-handle-kind", "{kind}" }
            span { class: "os-handle-state", "{state}" }
        }
    }
}
