//! The FuturesUnordered minigame: many downloads in flight on one task.
//!
//! The warm-up for the pipeline. Push downloads into the set, answer them
//! as the network in any order, and call `next().await` to see which one
//! comes back. Switch to FuturesOrdered and a finished download waits
//! behind a slow one. Single-player, click-stepped.

mod model;

use dioxus::prelude::*;

use crate::games::board::{at_style, edge, use_stepper, Flight, FlightLayer};
use crate::games::palette;
use model::{Hop, Mode, Node, Set, URLS};

const TASK: (f64, f64) = (14.0, 48.0);
const SET: (f64, f64) = (50.0, 48.0);
const OUT: (f64, f64) = (86.0, 48.0);

fn position(node: Node) -> (f64, f64) {
    match node {
        Node::Task => TASK,
        Node::Set => SET,
        Node::Out => OUT,
    }
}

fn url_hex(url: usize) -> String {
    palette::sender_hex(url as u64 + 1)
}

fn flight(hop: &Hop) -> Flight {
    Flight {
        from: position(hop.from),
        to: position(hop.to),
        label: URLS[hop.url].to_string(),
        color: url_hex(hop.url),
        control: false,
        leg: 0,
    }
}

const START: &str =
    "Push a few downloads into the set, answer them in any order, then call next().await.";

#[component]
pub fn UnorderedGame() -> Element {
    let board = use_stepper(|| Set::new(Mode::Unordered), START, flight);
    let set = board.state.read();
    let mode = set.mode;

    rsx! {
        div { class: "diagram board-game unordered-game",
            div { class: "game-header", "{set.name()}" }
            div { class: "status", "single-player · you are the task, and the network" }

            div { class: "bd-board",
                svg { class: "arrow-layer bd-edges", view_box: "0 0 100 100", preserve_aspect_ratio: "none",
                    {edge(TASK, SET, false)}
                    {edge(SET, OUT, false)}
                }

                div {
                    class: if set.parked { "bd-actor fu-task parked" } else { "bd-actor fu-task" },
                    style: at_style(TASK),
                    div { class: "bd-head", "the task" }
                    div { class: "fu-pushes",
                        for (url, name) in URLS.iter().enumerate() {
                            button {
                                class: "bd-chip",
                                style: "--c: {url_hex(url)};",
                                disabled: set.pushed[url],
                                onclick: move |_| board.step(|s| s.click_push(url)),
                                "push({name})"
                            }
                        }
                    }
                    button { class: "btn await", onclick: move |_| board.step(|s| s.click_next()), "next().await" }
                    div { class: "bd-doing", if set.parked { "parked: Pending" } else { "running" } }
                }

                div { class: "bd-actor fu-set", style: at_style(SET),
                    div { class: "bd-head", "{set.name()} · {set.futs.len()} in the set" }
                    div { class: "fu-futs",
                        if set.futs.is_empty() {
                            span { class: "bd-hint", "empty: next() yields None" }
                        }
                        for (i, fut) in set.futs.iter().copied().enumerate() {
                            button {
                                key: "{fut.url}",
                                class: if fut.woke.is_some() { "bd-chip done" } else { "bd-chip pending" },
                                style: "--c: {url_hex(fut.url)};",
                                disabled: fut.woke.is_some(),
                                title: "click: the network answers",
                                onclick: move |_| board.step(|s| s.click_wake(fut.url)),
                                if mode == Mode::Ordered { span { class: "fu-pos", "{i + 1}." } }
                                "{URLS[fut.url]}"
                                span { class: "fu-state", if fut.woke.is_some() { " · ready" } else { " · pending" } }
                            }
                        }
                    }
                    span { class: "bd-hint", "click a pending future: the network answers" }
                }

                div { class: "bd-actor fu-out", style: at_style(OUT),
                    div { class: "bd-head", "yielded" }
                    div { class: "fu-yielded",
                        if set.yielded.is_empty() {
                            span { class: "bd-hint", "nothing yet" }
                        }
                        for (n, url) in set.yielded.iter().enumerate() {
                            span { class: "bd-record", style: "--c: {url_hex(*url)};", "{n + 1}. {URLS[*url]}" }
                        }
                    }
                }

                FlightLayer { flights: board.flights }
            }

            div { class: "bd-note",
                span { class: "note-pill", "{board.note}" }
            }

            div { class: "game-footer",
                div { class: "ut-variants",
                    button {
                        class: if mode == Mode::Unordered { "btn small on" } else { "btn small" },
                        onclick: move |_| board.reset(Set::new(Mode::Unordered)),
                        "FuturesUnordered"
                    }
                    button {
                        class: if mode == Mode::Ordered { "btn small on" } else { "btn small" },
                        onclick: move |_| board.reset(Set::new(Mode::Ordered)),
                        "FuturesOrdered"
                    }
                }
                button { class: "btn", onclick: move |_| board.reset(Set::new(mode)), "reset" }
            }
        }
    }
}
