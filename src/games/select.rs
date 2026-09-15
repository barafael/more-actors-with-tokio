//! The select minigames: CONCEPT.md §3 (one race) and §4 (the race in a
//! loop).
//!
//! Both slides draw the same picture — two futures side by side, the winner
//! underneath — so the picture is one component, [`Race`], and the two games
//! differ only in what happens after a branch wins. That is also the honest
//! shape of the code being taught: `loop { select! { .. } }` is the select
//! plus a loop, not a different construct.

use dioxus::prelude::*;

use crate::clock::{wall_offset_ms, Ticking};
use crate::games::ticker::{subscribe, use_clock, use_local_tick};
use crate::games::{use_game_connection, GameConnection};
use crate::protocol::{
    Color, Game, LoopSelectEvent, LoopSelectSnapshot, LoopSelectWire, SelectEvent, SelectSnapshot,
    SelectWinner, SelectWire, TimerSnapshot, Won,
};
use crate::sim_timer::{LoopSelectSim, SelectSim};
use crate::{AppCtx, GameMode};

// ---- the shared picture ----

/// Two futures racing, and whatever the select yielded.
///
/// `armed` is the select being awaited at all: before it, both branches are
/// drawn inert, because outside a `select!` neither future exists.
#[component]
fn Race(snapshot: SelectSnapshot, on_press: EventHandler<Color>, enabled: bool) -> Element {
    let branch_class = |won: bool, live: bool| {
        if won {
            "race-branch won"
        } else if live {
            "race-branch live"
        } else {
            "race-branch"
        }
    };

    let timer_won = matches!(snapshot.winner, Some(Won::Timer { .. }));
    let button_won = matches!(snapshot.winner, Some(Won::Button { .. }));

    rsx! {
        div { class: "race",
            div { class: branch_class(timer_won, snapshot.armed),
                div { class: "branch-head", "sleep_until(next 10s)" }
                div { class: "branch-body",
                    if snapshot.armed {
                        span { class: "countdown",
                            "{remaining_s(snapshot.timer)}s"
                        }
                    } else if let Some(waited) = snapshot.timer.waited_s {
                        span { class: "countdown done", "{waited:.0}s" }
                    } else {
                        span { class: "countdown idle", "—" }
                    }
                }
            }

            div { class: "race-vs", "select!" }

            div { class: branch_class(button_won, snapshot.armed),
                div { class: "branch-head", "button.pressed()" }
                div { class: "branch-body press-row",
                    for color in [Color::Red, Color::Green, Color::Blue] {
                        button {
                            class: "press-pad {color.css_name()}",
                            disabled: !snapshot.armed || !enabled,
                            title: "{color.display()}",
                            onclick: move |_| on_press.call(color),
                            ""
                        }
                    }
                }
            }
        }

        div { class: "race-result",
            match snapshot.winner {
                Some(Won::Timer { waited_s }) => rsx! {
                    span { class: "note-pill resolved",
                        "the timer won — yielded {waited_s:.0}s"
                    }
                },
                Some(Won::Button { color }) => rsx! {
                    span { class: "note-pill resolved {color.css_name()}",
                        "the button won — yielded {color.display()}"
                    }
                },
                None if snapshot.armed => rsx! {
                    span { class: "note-pill pending", "awaiting both — first one wins" }
                },
                None => rsx! {
                    span { class: "note-pill", "not in a select" }
                },
            }
        }
    }
}

/// Seconds left on the timer branch, rounded up so it never reads 0 while
/// the branch is still live.
fn remaining_s(timer: TimerSnapshot) -> u64 {
    (timer.remaining_ms.unwrap_or(0.0) / 1000.0).ceil().max(0.0) as u64
}

// ---- §3: one race ----

fn apply_select(mut state: Signal<SelectSnapshot>, event: SelectEvent) {
    if let SelectEvent::Snapshot { state: new } = event {
        state.set(new);
    }
}

/// Republish the local sim's state, for the paths that changed it.
///
/// Local mode has no actor to broadcast a snapshot, so every mutation has to
/// be followed by one of these or the change never reaches the screen.
fn publish_select(sim: Signal<SelectSim>, state: Signal<SelectSnapshot>) {
    apply_select(
        state,
        SelectEvent::Snapshot {
            state: sim.read().snapshot(),
        },
    );
}

fn dispatch_select(
    conn: GameConnection,
    mut sim: Signal<SelectSim>,
    state: Signal<SelectSnapshot>,
    wire: SelectWire,
) {
    if conn.is_local() {
        sim.with_mut(|sim| {
            sim.sync_now(crate::sim::now_ms());
            sim.handle(&wire);
        });
        publish_select(sim, state);
    } else {
        conn.send(&wire);
    }
}

/// Restart the select: a fresh actor remotely, a fresh sim locally.
///
/// Rebuilding rather than resetting in place is what makes the two modes
/// agree. A remote restart respawns the actor, so anything the old one
/// accumulated is gone; clearing in place would leave local state the
/// presenter can see and the room cannot.
fn restart_select(
    conn: GameConnection,
    ctx: AppCtx,
    mut sim: Signal<SelectSim>,
    mut state: Signal<SelectSnapshot>,
) {
    if conn.is_local() {
        sim.set(SelectSim::new(wall_offset_ms()));
        state.set(SelectSnapshot::default());
        conn.set_status("single-player");
    } else {
        ctx.send(crate::AppUp::Restart { game: Game::Select });
    }
}

#[component]
pub fn SelectGame() -> Element {
    let ctx: AppCtx = use_context();
    let mode = use_context::<GameMode>();
    let state = use_signal(SelectSnapshot::default);
    let sim = use_signal(|| SelectSim::new(wall_offset_ms()));

    let conn = use_game_connection::<SelectEvent>("/ws/game/select", move |event| {
        apply_select(state, event)
    });

    let clock = use_clock();
    // A tick that produced events is the frame the timer branch won, and
    // `apply_select` drops everything but snapshots — so republish here
    // rather than leaving it to the guarded effect, which by then sees an
    // unarmed sim and skips.
    use_local_tick(sim, clock, move |event| {
        apply_select(state, event);
        publish_select(sim, state);
    });
    // While the race is live the countdown restates itself each frame; once
    // it is decided there is nothing per-frame left to say.
    use_effect(move || {
        subscribe(clock);
        if mode == GameMode::Local && sim.peek().next_delay_ms().is_some() {
            publish_select(sim, state);
        }
    });

    let snapshot = state();
    let enabled = conn.connected() && ctx.may_play();

    rsx! {
        div { class: "diagram select-game",
            div { class: "game-header", "select!" }
            div { class: "status", "{conn.status}" }

            Race {
                snapshot,
                enabled,
                on_press: move |color| dispatch_select(
                    conn, sim, state, SelectWire::Press { color },
                ),
            }

            div { class: "create-row",
                button {
                    class: "btn await",
                    disabled: !enabled,
                    onclick: move |_| dispatch_select(conn, sim, state, SelectWire::Arm),
                    if snapshot.armed { "Restart the race" } else { "Enter the select" }
                }
                if snapshot.armed {
                    button {
                        class: "btn",
                        disabled: !enabled,
                        onclick: move |_| dispatch_select(conn, sim, state, SelectWire::Reset),
                        "Leave it"
                    }
                }
            }

            div { class: "game-footer",
                span { class: "dim", "the losing branch is dropped, not awaited" }
                if ctx.may_present() {
                    button {
                        class: "btn",
                        onclick: move |_| restart_select(conn, ctx, sim, state),
                        "restart"
                    }
                }
            }
        }
    }
}

// ---- §4: the race in a loop ----

fn apply_loop(mut state: Signal<LoopSelectSnapshot>, event: LoopSelectEvent) {
    if let LoopSelectEvent::Snapshot { state: new } = event {
        state.set(new);
    }
}

/// Republish the local sim's state, for the paths that changed it.
fn publish_loop(sim: Signal<LoopSelectSim>, state: Signal<LoopSelectSnapshot>) {
    apply_loop(
        state,
        LoopSelectEvent::Snapshot {
            state: sim.read().snapshot(),
        },
    );
}

fn dispatch_loop(
    conn: GameConnection,
    mut sim: Signal<LoopSelectSim>,
    state: Signal<LoopSelectSnapshot>,
    wire: LoopSelectWire,
) {
    if conn.is_local() {
        sim.with_mut(|sim| {
            sim.sync_now(crate::sim::now_ms());
            sim.handle(&wire);
        });
        publish_loop(sim, state);
    } else {
        conn.send(&wire);
    }
}

/// Restart the loop: a fresh actor remotely, a fresh sim locally.
///
/// `Stop` + `Clear` is not the same thing — it empties the tape but keeps
/// the round counter, so the badge would read "round 13" over an empty tape
/// while a remote restart reads "round 1".
fn restart_loop(
    conn: GameConnection,
    ctx: AppCtx,
    mut sim: Signal<LoopSelectSim>,
    mut state: Signal<LoopSelectSnapshot>,
) {
    if conn.is_local() {
        sim.set(LoopSelectSim::new(wall_offset_ms()));
        state.set(LoopSelectSnapshot::default());
        conn.set_status("single-player");
    } else {
        ctx.send(crate::AppUp::Restart {
            game: Game::LoopSelect,
        });
    }
}

#[component]
pub fn LoopSelectGame() -> Element {
    let ctx: AppCtx = use_context();
    let mode = use_context::<GameMode>();
    let state = use_signal(LoopSelectSnapshot::default);
    let sim = use_signal(|| LoopSelectSim::new(wall_offset_ms()));

    let conn = use_game_connection::<LoopSelectEvent>("/ws/game/loop-select", move |event| {
        apply_loop(state, event)
    });

    let clock = use_clock();
    // A tick that produced events is a round completing, which the guarded
    // effect below cannot publish: the loop re-arms within the same tick, so
    // by the time it runs the round is already recorded but the snapshot it
    // would send is the next race's.
    use_local_tick(sim, clock, move |event| {
        apply_loop(state, event);
        publish_loop(sim, state);
    });
    use_effect(move || {
        subscribe(clock);
        // `LoopSelectSnapshot::snapshot` clones the history tape, so an
        // unguarded republish is a heap allocation ten times a second for a
        // loop that is not running.
        if mode == GameMode::Local && sim.peek().next_delay_ms().is_some() {
            publish_loop(sim, state);
        }
    });

    let snapshot = state();
    let enabled = conn.connected() && ctx.may_play();

    rsx! {
        div { class: "diagram loop-select-game",
            div { class: "game-header",
                "loop / select"
                if snapshot.running {
                    span { class: "round-badge", "round {snapshot.rounds + 1}" }
                }
            }
            div { class: "status", "{conn.status}" }

            Race {
                snapshot: snapshot.select,
                enabled: enabled && snapshot.running,
                on_press: move |color| dispatch_loop(
                    conn, sim, state, LoopSelectWire::Press { color },
                ),
            }

            Tape { history: snapshot.history.clone() }

            div { class: "create-row",
                if snapshot.running {
                    button {
                        class: "btn",
                        disabled: !enabled,
                        onclick: move |_| dispatch_loop(conn, sim, state, LoopSelectWire::Stop),
                        "break"
                    }
                } else {
                    button {
                        class: "btn await",
                        disabled: !enabled,
                        onclick: move |_| dispatch_loop(conn, sim, state, LoopSelectWire::Start),
                        "Run the loop"
                    }
                }
                if !snapshot.history.is_empty() {
                    button {
                        class: "btn",
                        disabled: !enabled,
                        onclick: move |_| dispatch_loop(conn, sim, state, LoopSelectWire::Clear),
                        "clear"
                    }
                }
            }

            div { class: "game-footer",
                span { class: "dim", "one loop, one select, state only it can touch — that is an actor" }
                if ctx.may_present() {
                    button {
                        class: "btn",
                        onclick: move |_| restart_loop(conn, ctx, sim, state),
                        "restart"
                    }
                }
            }
        }
    }
}

/// The running tape of completed rounds, newest last.
#[component]
fn Tape(history: Vec<SelectWinner>) -> Element {
    if history.is_empty() {
        return rsx! {
            div { class: "tape empty", span { class: "dim", "no rounds yet" } }
        };
    }

    rsx! {
        div { class: "tape",
            for entry in history {
                div {
                    key: "{entry.round}",
                    class: "tape-cell {entry.won.css_name()}",
                    title: "round {entry.round}",
                    match entry.won {
                        Won::Timer { waited_s } => rsx! { "{waited_s:.0}s" },
                        Won::Button { color } => rsx! { "{color.display()}" },
                    }
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_live_branch_never_reads_zero_before_it_resolves() {
        let timer = TimerSnapshot {
            pending: true,
            wall_ms: 0.0,
            remaining_ms: Some(1.0),
            waited_s: None,
        };
        assert_eq!(remaining_s(timer), 1);
    }

    #[test]
    fn a_countdown_past_its_deadline_reads_zero_rather_than_negative() {
        let timer = TimerSnapshot {
            pending: true,
            wall_ms: 0.0,
            remaining_ms: Some(-20.0),
            waited_s: None,
        };
        assert_eq!(remaining_s(timer), 0);
    }

    #[test]
    fn an_idle_branch_reads_zero() {
        assert_eq!(remaining_s(TimerSnapshot::default()), 0);
    }

    #[test]
    fn a_winner_names_a_css_class_per_branch() {
        // the tape colours cells by branch, so these must not collide
        assert_eq!(Won::Timer { waited_s: 3.0 }.css_name(), "timer");
        assert_eq!(Won::Button { color: Color::Red }.css_name(), "red");
        assert_ne!(
            Won::Timer { waited_s: 3.0 }.css_name(),
            Won::Button {
                color: Color::Green
            }
            .css_name(),
        );
    }

    #[test]
    fn the_period_divides_the_dial_evenly() {
        // the Dial draws one tick per period; a remainder would leave a gap
        assert_eq!(60 % crate::protocol::TIMER_PERIOD_S, 0);
    }
}
