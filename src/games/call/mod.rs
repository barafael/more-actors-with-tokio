//! The call-and-response minigame: `mpsc` carrying `oneshot` callbacks.
//!
//! Three requesters each run `get_unique_id(&tx).await` and then await their
//! reply. The player is also the actor's event loop, by hand: receive a
//! message, then either answer through its callback or drop it. Nobody gets
//! an id the actor has not personally handed out, and the requesters wait —
//! with a clock on each — for one loop's attention.

mod model;

use dioxus::prelude::*;

use crate::clock::now_ms;
use crate::games::ticker::use_clock;
use crate::games::{palette, TaskChip, Waited};
use model::{CallOutcome, CallPhase, CallSim, Request};

#[component]
pub fn CallGame() -> Element {
    let mut sim = use_signal(CallSim::default);
    let mut act = move |act: &dyn Fn(&mut CallSim)| {
        sim.with_mut(|s| {
            s.sync_now(now_ms());
            act(s);
        });
    };

    // Read by the count-up badges alone, so only they redraw each frame.
    let clock = use_clock();
    let call = sim.read();
    let (capacity, next_id, queue, parked, in_hand, last) = (
        call.capacity(),
        call.next_id(),
        call.queue(),
        call.parked(),
        call.in_hand(),
        call.last(),
    );
    let requesters: Vec<(u64, CallPhase, Option<f64>)> = call.requesters().collect();
    drop(call);
    let waiting_count = requesters.iter().filter(|(_, p, _)| p.waiting()).count();

    rsx! {
        div { class: "diagram call-game",
            div { class: "game-header", "mpsc + oneshot" }

            div { class: "cr-stage",
                div { class: "cr-col cr-requesters",
                    span { class: "mx-caption", "requesters" }
                    for (task, phase, since) in requesters.iter().copied() {
                        Requester { key: "{task}", task, phase, since: since.unwrap_or_default(), clock }
                    }
                }

                div { class: "cr-col cr-inbox",
                    span { class: "mx-caption", "the actor's inbox · mpsc::channel({capacity})" }
                    div { class: "ut-slots cr-slots",
                        for slot in 0..capacity {
                            match queue.get(slot) {
                                Some(request) => rsx! { Message { request: *request } },
                                None => rsx! { span { class: "ut-slot cr-slot" } },
                            }
                        }
                    }
                    if !parked.is_empty() {
                        span { class: "mx-caption", "parked in send().await" }
                        div { class: "mx-chips",
                            for request in parked.iter() {
                                Message { request: *request }
                            }
                        }
                    }
                    span { class: "flow-arrow", "→" }
                }

                div { class: "cr-col cr-actor",
                    div { class: "mx-lock cr-actor-box",
                        div { class: "mx-lock-head", "the actor · you" }
                        div { class: "cr-state", "next_id: {next_id}" }
                        div { class: "cr-hand",
                            match in_hand {
                                Some(request) => rsx! {
                                    span { class: "mx-caption", "in hand" }
                                    Message { request }
                                },
                                None => rsx! { span { class: "mx-empty", "nothing in hand" } },
                            }
                        }
                    }
                    div { class: "cr-host",
                        button {
                            class: "btn await",
                            disabled: in_hand.is_some() || queue.is_empty(),
                            onclick: move |_| act(&CallSim::recv),
                            "rx.recv()"
                        }
                        button {
                            class: "btn rcv",
                            disabled: in_hand.is_none(),
                            onclick: move |_| act(&CallSim::reply),
                            "callback.send({next_id})"
                        }
                        button {
                            class: "btn",
                            disabled: in_hand.is_none(),
                            onclick: move |_| act(&CallSim::drop_callback),
                            "drop(callback)"
                        }
                    }
                    if let Some(last) = last {
                        Outcome { last }
                    }
                }
            }

            div { class: "mx-controls",
                for (task, phase, _) in requesters.iter().copied() {
                    div { key: "{task}", class: "mx-row", style: "--c: {palette::sender_hex(task)};",
                        span { class: "mx-row-name", "task {task}" }
                        if phase.waiting() {
                            button {
                                class: "btn",
                                onclick: move |_| act(&|s| s.give_up(task)),
                                if phase == CallPhase::Sending { "drop the send" } else { "drop(rx)" }
                            }
                        } else {
                            button {
                                class: "btn await",
                                onclick: move |_| act(&|s| s.request(task)),
                                "get_unique_id(&tx).await"
                            }
                        }
                    }
                }
            }

            div { class: "game-footer",
                if waiting_count > 0 {
                    span { class: "note-pill blocked", "{waiting_count} waiting on one event loop" }
                } else {
                    span {}
                }
                button {
                    class: "btn",
                    onclick: move |_| sim.set(CallSim::default()),
                    "restart"
                }
            }
        }
    }
}

#[component]
fn Requester(task: u64, phase: CallPhase, since: f64, clock: Signal<f64>) -> Element {
    let class = match phase {
        CallPhase::Idle | CallPhase::GaveUp => "idle",
        CallPhase::Sending | CallPhase::Awaiting => "waiting",
        CallPhase::Got(_) => "got",
        CallPhase::Closed => "failed",
    };
    rsx! {
        TaskChip { task, class,
            match phase {
                CallPhase::Idle => rsx! {},
                CallPhase::Sending => rsx! { Waited { since, clock, label: "send parked · " } },
                CallPhase::Awaiting => rsx! { Waited { since, clock, label: "awaiting reply · " } },
                CallPhase::Got(id) => rsx! { span { class: "blocked-clock", "Ok({id})" } },
                CallPhase::Closed => rsx! { span { class: "blocked-clock", "Err(RecvError)" } },
                CallPhase::GaveUp => rsx! { span { class: "blocked-clock", "gave up" } },
            }
        }
    }
}

#[component]
fn Message(request: Request) -> Element {
    rsx! {
        span {
            class: if request.listening { "ut-slot filled cr-msg" } else { "ut-slot filled cr-msg deaf" },
            style: "--c: {palette::sender_hex(request.task)};",
            title: if request.listening { "a callback someone is waiting on" } else { "its receiver was dropped: nobody is listening" },
            "task {request.task}"
        }
    }
}

#[component]
fn Outcome(last: CallOutcome) -> Element {
    let name = format!("task {}", last.task());
    let (class, text) = match last {
        CallOutcome::Replied { id, .. } => ("resolved", format!("sent {id} to {name} → Ok(())")),
        CallOutcome::Unheard { id, .. } => (
            "resolved red",
            format!("sent {id} to {name} → Err({id}): nobody was listening"),
        ),
        CallOutcome::Dropped { .. } => (
            "resolved red",
            format!("dropped {name}'s callback → their await yields Err(RecvError)"),
        ),
    };
    rsx! {
        span { class: "note-pill {class} cr-outcome", "{text}" }
    }
}
