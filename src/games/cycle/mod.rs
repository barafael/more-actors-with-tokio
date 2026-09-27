//! The deadlock minigame: two actors in a cycle of bounded channels.
//!
//! Single-player, in the talk as in the export. The presenter feeds
//! messages in and the room watches them circulate — until one message too
//! many, when both actors park in `send().await` towards each other and the
//! only thing still moving is the clock on each of them. Then cut the cycle
//! and feed it the same messages again.

mod model;

use dioxus::prelude::*;

use crate::games::ticker::{use_frame_clock, use_local_tick};
use crate::games::{palette, Waited};
use crate::sim::now_ms;
use model::{Actor, CycleSim, Msg, A, B, CAPACITY};

#[component]
pub fn CycleGame() -> Element {
    let mut sim = use_signal(|| CycleSim::new(false));
    let (clock, _live) = use_frame_clock();
    use_local_tick(sim, clock, |()| {});

    let s = sim.read();
    // The count-ups are the only thing that moves once the cycle seizes.
    // They read the frame clock themselves; the sim is local, so its
    // instants share `now_ms`'s epoch and need no receipt time.
    let deadlock = s.deadlocked_since();
    let cut = s.cut;
    let inboxes: [Vec<Msg>; 2] = [
        s.inboxes[A].buffer().copied().collect(),
        s.inboxes[B].buffer().copied().collect(),
    ];
    let actors = s.actors;
    let stuck = [s.stuck(A), s.stuck(B)];
    let injecting = s.injecting;
    let sunk = s.sunk;
    let circulating = s.in_circulation();
    drop(s);

    let inject = move |_| {
        sim.with_mut(|sim| {
            use crate::clock::Ticking;
            sim.sync_now(now_ms());
            sim.inject();
        });
    };

    rsx! {
        div { class: "diagram cycle-game",
            div { class: "game-header", "cycle" }
            div { class: "status", "single-player · your own actors" }

            div { class: "cy-stage",
                div { class: "cy-row",
                    div { class: "cy-outside",
                        button {
                            class: "btn await",
                            disabled: injecting.is_some(),
                            onclick: inject,
                            "send into A"
                        }
                        if let Some((msg, _, since)) = injecting {
                            Waited { for_ms: 0.0, at: since, clock, label: format!("m{} parked · ", msg.id) }
                        }
                    }
                    Inbox { messages: inboxes[A].clone() }
                    ActorBox { name: "A", actor: actors[A], stuck: stuck[A], clock }
                    span { class: "flow-arrow", "→" }
                    Inbox { messages: inboxes[B].clone() }
                    ActorBox { name: "B", actor: actors[B], stuck: stuck[B], clock }
                    if cut {
                        span { class: "flow-arrow", "→" }
                        div { class: "mx-lock cy-sink",
                            div { class: "mx-lock-head", "sink" }
                            div { class: "cr-state", "{sunk} eaten" }
                        }
                    }
                }
                if !cut {
                    div { class: "cy-return", "B forwards every message back into A's inbox" }
                }
            }

            div { class: "cy-verdict",
                match deadlock {
                    Some(since) => rsx! {
                        span { class: "note-pill resolved red",
                            "deadlock · each parked sending to the other · "
                            Waited { for_ms: 0.0, at: since, clock, class: "" }
                            " and counting"
                        }
                    },
                    None if cut => rsx! {
                        span { class: "note-pill", "a DAG: whatever goes in drains out" }
                    },
                    None => rsx! {
                        span { class: "note-pill", "{circulating} circulating · the cycle holds {2 * CAPACITY + 2} before it seizes" }
                    },
                }
            }

            div { class: "game-footer",
                div { class: "ut-variants",
                    button {
                        class: if cut { "btn small" } else { "btn small on" },
                        onclick: move |_| sim.set(CycleSim::new(false)),
                        "cycle"
                    }
                    button {
                        class: if cut { "btn small on" } else { "btn small" },
                        onclick: move |_| sim.set(CycleSim::new(true)),
                        "cut the cycle"
                    }
                }
                button {
                    class: "btn",
                    onclick: move |_| sim.set(CycleSim::new(cut)),
                    "reset"
                }
            }
        }
    }
}

#[component]
fn Inbox(messages: Vec<Msg>) -> Element {
    rsx! {
        div { class: "ut-slots cy-inbox",
            for slot in 0..CAPACITY {
                match messages.get(slot) {
                    Some(msg) => rsx! { MsgChip { msg: *msg, class: "ut-slot filled" } },
                    None => rsx! { span { class: "ut-slot" } },
                }
            }
        }
    }
}

#[component]
fn MsgChip(msg: Msg, class: String) -> Element {
    rsx! {
        span { class: "{class}", style: "--c: {palette::sender_hex(msg.id)};",
            "m{msg.id}"
        }
    }
}

#[component]
fn ActorBox(name: String, actor: Actor, stuck: bool, clock: Signal<f64>) -> Element {
    let class = match actor {
        Actor::Receiving => "mx-lock cy-actor",
        Actor::Handling(_) => "mx-lock cy-actor held",
        Actor::Sending { .. } if stuck => "mx-lock cy-actor parked",
        Actor::Sending { .. } => "mx-lock cy-actor held",
    };
    rsx! {
        div { class,
            div { class: "mx-lock-head", "actor {name}" }
            match actor {
                Actor::Receiving => rsx! { div { class: "cr-state", "recv().await" } },
                Actor::Handling(msg) => rsx! {
                    div { class: "cr-state", "forwarding" }
                    MsgChip { msg, class: "ut-slot filled" }
                },
                Actor::Sending { msg, since, .. } => rsx! {
                    div { class: "cr-state", "send().await" }
                    MsgChip { msg, class: "ut-slot parked" }
                    Waited { for_ms: 0.0, at: since, clock, label: "parked · " }
                },
            }
        }
    }
}
