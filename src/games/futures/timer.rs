//! The timer minigame: a future that resolves because time passed.
//!
//! CONCEPT.md §2, the first beat of the deck. Pick how long to sleep, from
//! nothing to ten seconds, and await it: nothing makes it resolve but time.
//! The dial is one turn of ten seconds, with the chosen sleep marked on its
//! rim, so the deadline is visible before it arrives.

use dioxus::prelude::*;

use super::model::{TimerSim, MAX_WAIT_S};
use super::wait::SleepCall;
use crate::games::ticker::{act_now, use_clock, use_tick};

/// Radius of the dial, in the svg's own units.
const DIAL_R: f64 = 46.0;

/// Radius of the ring that marks the sleep, just inside the rim.
const SPAN_R: f64 = 40.0;

#[component]
pub fn TimerGame() -> Element {
    let mut sim = use_signal(TimerSim::new);
    let clock = use_clock();
    use_tick(sim, clock);

    let timer = *sim.read();
    let pending = timer.pending();
    // a pending sleep shows its own length; otherwise the one about to be
    // asked for
    let span_s = timer.sleeping_s().unwrap_or(timer.wait_s());

    rsx! {
        div { class: "diagram timer-game",
            div { class: "game-header", "timer" }

            Dial {
                span_s,
                elapsed_ms: timer.elapsed_ms().unwrap_or(0.0),
                remaining_ms: timer.remaining_ms(),
                pending,
            }

            div { class: "timer-readout",
                match (pending, timer.waited_s()) {
                    (true, _) => rsx! {
                        span { class: "note-pill pending", "awaiting..." }
                    },
                    (false, Some(waited)) => rsx! {
                        span { class: "note-pill resolved", "yielded {waited}s" }
                    },
                    (false, None) => rsx! {
                        span { class: "note-pill", "idle — nothing is waiting" }
                    },
                }
            }

            div { class: "create-row",
                SleepCall {
                    wait_s: timer.wait_s(),
                    locked: pending,
                    on_change: move |wait_s| {
                        sim.with_mut(|s| s.set_wait_s(wait_s));
                    },
                }
                if !pending {
                    button {
                        class: "btn await",
                        onclick: move |_| {
                            act_now(sim, |s| s.activate());
                        },
                        ".await"
                    }
                } else {
                    button {
                        class: "btn",
                        onclick: move |_| {
                            act_now(sim, |s| s.cancel());
                        },
                        "Drop the future"
                    }
                }
            }

            div { class: "game-footer",
                span { class: "dim", "resolves once its seconds have passed, and yields them" }
                button {
                    class: "btn",
                    onclick: move |_| sim.set(TimerSim::new()),
                    "restart"
                }
            }
        }
    }
}

/// The ten-second dial: the sleep marked on the rim, filled in as it
/// passes, and a hand at how far it has got.
#[component]
fn Dial(span_s: u64, elapsed_ms: f64, remaining_ms: Option<f64>, pending: bool) -> Element {
    let turn_ms = MAX_WAIT_S as f64 * 1000.0;
    let angle = elapsed_ms / turn_ms * std::f64::consts::TAU - std::f64::consts::FRAC_PI_2;
    // the hand starts away from the pivot: the readout lives at the centre,
    // and a hand through it would draw over the number
    const HAND_INNER: f64 = 15.0;
    let (sin, cos) = angle.sin_cos();
    let (x1, y1) = (50.0 + HAND_INNER * cos, 50.0 + HAND_INNER * sin);
    let (x2, y2) = (50.0 + DIAL_R * 0.82 * cos, 50.0 + DIAL_R * 0.82 * sin);

    // Arcs as dashes on a circle turned to start at the top: the dash is
    // the share of one ten-second turn.
    let circumference = std::f64::consts::TAU * SPAN_R;
    let span = span_s as f64 / MAX_WAIT_S as f64 * circumference;
    let done = (elapsed_ms / turn_ms).min(1.0) * circumference;
    // counting down while pending, rounded up so it never reads 0 early;
    // otherwise the length of the sleep
    let label = match remaining_ms {
        Some(left) => (left / 1000.0).ceil() as u64,
        None => span_s,
    };

    rsx! {
        svg { class: "timer-dial", view_box: "0 0 100 100",
            circle {
                class: if pending { "dial-face pending" } else { "dial-face" },
                cx: "50", cy: "50", r: "{DIAL_R}",
            }
            circle {
                class: "dial-span",
                cx: "50", cy: "50", r: "{SPAN_R}",
                stroke_dasharray: "{span} {circumference}",
                transform: "rotate(-90 50 50)",
            }
            if pending {
                circle {
                    class: "dial-done",
                    cx: "50", cy: "50", r: "{SPAN_R}",
                    stroke_dasharray: "{done} {circumference}",
                    transform: "rotate(-90 50 50)",
                }
            }
            // one tick per second of the turn
            for (x1, y1, x2, y2) in dial_ticks() {
                line { class: "dial-tick", x1: "{x1}", y1: "{y1}", x2: "{x2}", y2: "{y2}" }
            }
            line {
                class: if pending { "dial-hand pending" } else { "dial-hand" },
                x1: "{x1}", y1: "{y1}", x2: "{x2}", y2: "{y2}",
            }
            text { class: "dial-label", x: "50", y: "56", text_anchor: "middle",
                "{label}s"
            }
        }
    }
}

/// The dial's tick marks, one per second, as `(x1, y1, x2, y2)` in svg
/// units. They depend only on constants, so they are computed once rather
/// than on every frame.
fn dial_ticks() -> &'static [(f64, f64, f64, f64)] {
    static TICKS: std::sync::OnceLock<Vec<(f64, f64, f64, f64)>> = std::sync::OnceLock::new();
    TICKS.get_or_init(|| {
        (0..MAX_WAIT_S)
            .map(|second| {
                let angle = second as f64 / MAX_WAIT_S as f64 * std::f64::consts::TAU
                    - std::f64::consts::FRAC_PI_2;
                let (sin, cos) = angle.sin_cos();
                (
                    50.0 + DIAL_R * 0.88 * cos,
                    50.0 + DIAL_R * 0.88 * sin,
                    50.0 + DIAL_R * cos,
                    50.0 + DIAL_R * sin,
                )
            })
            .collect()
    })
}
