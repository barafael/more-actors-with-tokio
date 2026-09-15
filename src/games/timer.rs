//! The timer minigame: a future that resolves because time passed.
//!
//! CONCEPT.md §2, the first beat of the deck. Not interactive beyond
//! awaiting it — the point is that nothing in the room makes this happen.
//! The dial exists so the wait is legible: the audience can see the deadline
//! coming and, more importantly, see that awaiting at 0:07 is a three-second
//! wait while awaiting at 0:11 is a nine-second one.

use dioxus::prelude::*;

use crate::clock::{wall_now_ms, wall_offset_ms, Ticking};
use crate::games::ticker::{subscribe, use_clock, use_local_tick};
use crate::games::{use_game_connection, GameConnection};
use crate::protocol::{Game, TimerEvent, TimerSnapshot, TimerWire, TIMER_PERIOD_S};
use crate::sim_timer::TimerSim;
use crate::{AppCtx, GameMode};

/// Radius of the dial, in the svg's own units.
const DIAL_R: f64 = 46.0;

fn apply(mut state: Signal<TimerSnapshot>, event: TimerEvent) {
    // Only the snapshot is authoritative; the rest are cues the UI may drop.
    if let TimerEvent::Snapshot { state: new } = event {
        state.set(new);
    }
}

fn dispatch(
    conn: GameConnection,
    mut sim: Signal<TimerSim>,
    state: Signal<TimerSnapshot>,
    wire: TimerWire,
) {
    if conn.is_local() {
        sim.with_mut(|sim| {
            sim.sync_now(crate::sim::now_ms());
            sim.handle(&wire);
        });
        apply(
            state,
            TimerEvent::Snapshot {
                state: sim.read().snapshot(),
            },
        );
    } else {
        conn.send(&wire);
    }
}

#[component]
pub fn TimerGame() -> Element {
    let ctx: AppCtx = use_context();
    let mode = use_context::<GameMode>();
    let state = use_signal(TimerSnapshot::default);
    let sim = use_signal(|| TimerSim::new(wall_offset_ms()));

    let conn =
        use_game_connection::<TimerEvent>("/ws/game/timer", move |event| apply(state, event));

    // The dial must sweep between snapshots, so the clock runs in both
    // modes; only in local mode does it also resolve the future.
    let clock = use_clock();
    use_local_tick(sim, clock, move |event| apply(state, event));

    // In local mode the snapshot is republished every frame so the dial and
    // the countdown follow the clock rather than the last wire.
    use_effect(move || {
        subscribe(clock);
        // Only while the future is actually counting down: republishing an
        // idle snapshot every frame allocates and re-renders for a screen
        // that is not moving.
        if mode == GameMode::Local && sim.peek().next_delay_ms().is_some() {
            apply(
                state,
                TimerEvent::Snapshot {
                    state: sim.read().snapshot(),
                },
            );
        }
    });

    let snapshot = state();
    // Subscribing to `clock` is what re-runs this component each frame; the
    // returned instant is the sim clock, which is not what the dial shows,
    // so it is deliberately only a subscription.
    subscribe(clock);
    // In local mode the sim's own snapshot already carries the dial. In
    // remote mode snapshots only arrive when something happens, so the hand
    // is read off the wall clock and sweeps between them.
    let wall_ms = match mode {
        GameMode::Local => snapshot.wall_ms,
        GameMode::Remote => wall_now_ms(),
    };

    rsx! {
        div { class: "diagram timer-game",
            div { class: "game-header", "timer-future" }
            div { class: "status", "{conn.status}" }

            Dial { wall_ms, pending: snapshot.pending }

            div { class: "timer-readout",
                match (snapshot.pending, snapshot.waited_s) {
                    (true, _) => rsx! {
                        span { class: "note-pill pending",
                            "awaiting… resolves in {seconds(snapshot.remaining_ms.unwrap_or(0.0))}s"
                        }
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
                if !snapshot.pending {
                    button {
                        class: "btn await",
                        disabled: !conn.connected() || !ctx.may_play(),
                        onclick: move |_| dispatch(conn, sim, state, TimerWire::Activate),
                        "Await the timer"
                    }
                } else {
                    button {
                        class: "btn",
                        disabled: !conn.connected() || !ctx.may_play(),
                        onclick: move |_| dispatch(conn, sim, state, TimerWire::Cancel),
                        "Drop the future"
                    }
                }
            }

            div { class: "game-footer",
                span { class: "dim", "resolves whenever the seconds hit a multiple of {TIMER_PERIOD_S}" }
                if ctx.may_present() {
                    button {
                        class: "btn",
                        onclick: move |_| restart(conn, ctx, sim, state),
                        "restart"
                    }
                }
            }
        }
    }
}

fn restart(
    conn: GameConnection,
    ctx: AppCtx,
    mut sim: Signal<TimerSim>,
    mut state: Signal<TimerSnapshot>,
) {
    if conn.is_local() {
        sim.set(TimerSim::new(wall_offset_ms()));
        state.set(TimerSnapshot::default());
        conn.set_status("single-player");
    } else {
        ctx.send(crate::AppUp::Restart { game: Game::Timer });
    }
}

/// The seconds dial: a full minute around, with the period boundaries marked
/// so the next deadline is visible before it arrives.
#[component]
fn Dial(wall_ms: f64, pending: bool) -> Element {
    let seconds = wall_ms / 1000.0;
    let angle = seconds / 60.0 * std::f64::consts::TAU - std::f64::consts::FRAC_PI_2;
    let (hand_x, hand_y) = (
        50.0 + DIAL_R * 0.82 * angle.cos(),
        50.0 + DIAL_R * 0.82 * angle.sin(),
    );

    rsx! {
        svg { class: "timer-dial", view_box: "0 0 100 100", width: "220", height: "220",
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
                x1: "50", y1: "50", x2: "{hand_x}", y2: "{hand_y}",
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

/// Whole seconds, rounded up, so a countdown never shows 0 while waiting.
fn seconds(ms: f64) -> u64 {
    (ms / 1000.0).ceil().max(0.0) as u64
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_countdown_rounds_up_so_it_never_shows_zero_while_waiting() {
        assert_eq!(seconds(1.0), 1);
        assert_eq!(seconds(999.0), 1);
        assert_eq!(seconds(1_000.0), 1);
        assert_eq!(seconds(1_001.0), 2);
    }

    #[test]
    fn a_countdown_never_goes_negative() {
        // a snapshot can arrive a hair after its own deadline
        assert_eq!(seconds(-5.0), 0);
    }

    #[test]
    fn the_wall_clock_stays_inside_one_minute() {
        let wall = wall_now_ms();
        assert!((0.0..60_000.0).contains(&wall), "got {wall}");
    }

    #[test]
    fn the_offset_maps_the_monotonic_clock_onto_the_dial() {
        // a sim built with this offset reads the same minute as the wall
        let offset = wall_offset_ms();
        let mapped = (crate::sim::now_ms() + offset).rem_euclid(60_000.0);
        assert!(
            (mapped - wall_now_ms()).abs() < 1_000.0,
            "mapped {mapped} vs wall {}",
            wall_now_ms(),
        );
    }
}
