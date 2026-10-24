//! The timer minigame: a future that resolves because time passed.
//!
//! CONCEPT.md §2, the first beat of the deck. Not interactive beyond
//! awaiting it — the point is that nothing makes this happen but time.
//! The dial exists so the wait is legible: you can see the deadline coming
//! and, more importantly, see that awaiting at 0:07 is a three-second wait
//! while awaiting at 0:11 is a nine-second one.

use dioxus::prelude::*;

use super::model::{TimerSim, TIMER_PERIOD_S};
use crate::clock::{wall_now_ms, wall_offset_ms};
use crate::games::ticker::{act_now, subscribe, use_frame_clock, use_tick};

/// Radius of the dial, in the svg's own units.
const DIAL_R: f64 = 46.0;

#[component]
pub fn TimerGame() -> Element {
    let mut sim = use_signal(|| TimerSim::new(wall_offset_ms()));
    let (clock, live) = use_frame_clock();
    use_tick(sim, clock);

    let timer = *sim.read();
    // The dial sweeps whether or not a future is pending, so this component
    // redraws each frame; the instant itself is the sim clock, which is not
    // what the dial shows, so it is deliberately only a subscription.
    subscribe(clock);
    // The hand comes off the wall clock so it sweeps continuously.
    //
    // Until the frame loop is running, though, the hand parks at the top of
    // the dial: an SSR first paint has to be reproducible (the export
    // asserts two renders match) and has to match what the client draws
    // when it hydrates. `live` is false in both of those renders and true
    // from the first browser frame onward.
    let wall_ms = if live() { wall_now_ms() } else { 0.0 };

    rsx! {
        div { class: "diagram timer-game",
            div { class: "game-header", "timer" }

            Dial { wall_ms, pending: timer.pending() }

            div { class: "timer-readout",
                match (timer.pending(), timer.waited_s()) {
                    (true, _) => rsx! {
                        span { class: "note-pill pending", "awaiting..." }
                    },
                    (false, Some(waited)) => rsx! {
                        span { class: "note-pill resolved", "yielded {waited:.0}s waited" }
                    },
                    (false, None) => rsx! {
                        span { class: "note-pill", "idle — nothing is waiting" }
                    },
                }
            }

            div { class: "create-row",
                if !timer.pending() {
                    button {
                        class: "btn await",
                        onclick: move |_| {
                            act_now(sim, |s| s.activate());
                        },
                        "Await the timer"
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
                span { class: "dim", "resolves whenever the seconds hit a multiple of {TIMER_PERIOD_S}" }
                button {
                    class: "btn",
                    onclick: move |_| sim.set(TimerSim::new(wall_offset_ms())),
                    "restart"
                }
            }
        }
    }
}

/// The seconds dial: a full minute around, with the period boundaries marked
/// so the next deadline is visible before it arrives.
#[component]
fn Dial(wall_ms: f64, pending: bool) -> Element {
    let seconds = wall_ms / 1000.0;
    let angle = seconds / 60.0 * std::f64::consts::TAU - std::f64::consts::FRAC_PI_2;
    // the hand starts away from the pivot: the seconds readout lives at the
    // center, and a hand through it would draw over the number
    const HAND_INNER: f64 = 15.0;
    let (sin, cos) = angle.sin_cos();
    let (x1, y1) = (50.0 + HAND_INNER * cos, 50.0 + HAND_INNER * sin);
    let (x2, y2) = (50.0 + DIAL_R * 0.82 * cos, 50.0 + DIAL_R * 0.82 * sin);

    rsx! {
        svg { class: "timer-dial", view_box: "0 0 100 100",
            circle {
                class: if pending { "dial-face pending" } else { "dial-face" },
                cx: "50", cy: "50", r: "{DIAL_R}",
            }
            // one tick per period boundary: these are the instants the
            // future can resolve at
            for (x1, y1, x2, y2) in dial_ticks() {
                line { class: "dial-tick", x1: "{x1}", y1: "{y1}", x2: "{x2}", y2: "{y2}" }
            }
            line {
                class: if pending { "dial-hand pending" } else { "dial-hand" },
                x1: "{x1}", y1: "{y1}", x2: "{x2}", y2: "{y2}",
            }
            text { class: "dial-label", x: "50", y: "56", text_anchor: "middle",
                "{seconds:.0}"
            }
        }
    }
}

/// The dial's tick marks, as `(x1, y1, x2, y2)` in svg units.
///
/// These depend only on `TIMER_PERIOD_S` and `DIAL_R`, so they are computed
/// once rather than re-derived — six trig calls apiece — on every frame.
fn dial_ticks() -> &'static [(f64, f64, f64, f64)] {
    static TICKS: std::sync::OnceLock<Vec<(f64, f64, f64, f64)>> = std::sync::OnceLock::new();
    TICKS.get_or_init(|| {
        (0..(60 / TIMER_PERIOD_S))
            .map(|step| {
                let angle = (step * TIMER_PERIOD_S) as f64 / 60.0 * std::f64::consts::TAU
                    - std::f64::consts::FRAC_PI_2;
                let (sin, cos) = angle.sin_cos();
                (
                    50.0 + DIAL_R * 0.82 * cos,
                    50.0 + DIAL_R * 0.82 * sin,
                    50.0 + DIAL_R * cos,
                    50.0 + DIAL_R * sin,
                )
            })
            .collect()
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_wall_clock_stays_inside_one_minute() {
        let wall = wall_now_ms();
        assert!((0.0..60_000.0).contains(&wall), "got {wall}");
    }

    #[test]
    fn the_offset_maps_the_monotonic_clock_onto_the_dial() {
        // a sim built with this offset reads the same minute as the wall
        let offset = wall_offset_ms();
        let mapped = (crate::clock::now_ms() + offset).rem_euclid(60_000.0);
        assert!(
            (mapped - wall_now_ms()).abs() < 1_000.0,
            "mapped {mapped} vs wall {}",
            wall_now_ms(),
        );
    }
}
