//! The select minigames: CONCEPT.md §3 (one race), the race spawned as a
//! task, and §4 (the race in a loop).
//!
//! All three slides draw the same picture — two futures side by side, the
//! winner underneath — so the picture is one component, [`Race`], and the
//! games differ only in what happens after a branch wins. That is also the
//! honest shape of the code being taught: `loop { select! { .. } }` is the
//! select plus a loop, not a different construct.

use dioxus::prelude::*;

use super::model::{Color, LoopSelectSim, Round, SelectSim, SpawnSim, TimerSim, Won};
use crate::clock::wall_offset_ms;
use crate::games::ticker::{act_now, use_clock, use_tick};

// ---- the shared picture ----

/// Two futures racing, and whatever the select yielded.
///
/// `armed` is the select being awaited at all: before it, both branches are
/// drawn inert, because outside a `select!` neither future exists.
#[component]
fn Race(
    armed: bool,
    timer: TimerSim,
    winner: Option<Won>,
    on_press: EventHandler<Color>,
) -> Element {
    let branch_class = |won: bool| {
        if won {
            "race-branch won"
        } else if armed {
            "race-branch live"
        } else {
            "race-branch"
        }
    };

    let timer_won = matches!(winner, Some(Won::Timer { .. }));
    let button_won = matches!(winner, Some(Won::Button { .. }));

    rsx! {
        div { class: "race",
            div { class: branch_class(timer_won),
                div { class: "branch-head", "sleep_until(next 10s)" }
                div { class: "branch-body",
                    if armed {
                        span { class: "countdown",
                            "{remaining_s(timer.remaining_ms())}s"
                        }
                    } else if let Some(waited) = timer.waited_s() {
                        span { class: "countdown done", "{waited:.0}s" }
                    } else {
                        span { class: "countdown idle", "—" }
                    }
                }
            }

            div { class: "race-vs", "select!" }

            div { class: branch_class(button_won),
                div { class: "branch-head", "button.pressed()" }
                // plain branch-body: `.press-row` is the button game's
                // absolute bottom-center tray — borrowing it here would rip
                // the pads out of the branch and drop them mid-diagram
                div { class: "branch-body",
                    for color in Color::ALL {
                        button {
                            class: "press-pad {color.css_name()}",
                            disabled: !armed,
                            title: "{color.display()}",
                            onclick: move |_| on_press.call(color),
                            "{color.display()}"
                        }
                    }
                }
            }
        }

        div { class: "race-result",
            match winner {
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
                None if armed => rsx! {
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
fn remaining_s(remaining_ms: Option<f64>) -> u64 {
    (remaining_ms.unwrap_or(0.0) / 1000.0).ceil().max(0.0) as u64
}

// ---- §3: one race ----

#[component]
pub fn SelectGame() -> Element {
    let mut sim = use_signal(|| SelectSim::new(wall_offset_ms()));
    let clock = use_clock();
    use_tick(sim, clock);

    let select = sim.read();
    let (armed, timer, winner) = (select.armed(), select.timer(), select.winner());
    drop(select);

    rsx! {
        div { class: "diagram select-game",
            div { class: "game-header", "select!" }

            Race {
                armed,
                timer,
                winner,
                on_press: move |color| {
                    act_now(sim, |s| s.press(color));
                },
            }

            div { class: "create-row",
                button {
                    class: "btn await",
                    onclick: move |_| act_now(sim, |s| s.arm()),
                    if armed { "Restart the race" } else { "Enter the select" }
                }
                if armed {
                    button {
                        class: "btn",
                        onclick: move |_| act_now(sim, |s| s.reset()),
                        "Leave it"
                    }
                }
            }

            div { class: "game-footer",
                span { class: "dim", "the losing branch is dropped, not awaited" }
                button {
                    class: "btn",
                    onclick: move |_| sim.set(SelectSim::new(wall_offset_ms())),
                    "restart"
                }
            }
        }
    }
}

// ---- the race, spawned ----

#[component]
pub fn SpawnGame() -> Element {
    let mut sim = use_signal(|| SpawnSim::new(wall_offset_ms()));
    let clock = use_clock();
    use_tick(sim, clock);

    let spawn = sim.read();
    let task = spawn.task();
    let select = spawn.select();
    let (armed, timer, winner) = (select.armed(), select.timer(), select.winner());
    drop(spawn);

    rsx! {
        div { class: "diagram spawn-game",
            div { class: "game-header", "tokio::spawn" }

            div { class: if task.is_some() { "task-frame live" } else { "task-frame" },
                div { class: "task-head",
                    match task {
                        Some(task) => rsx! { "task #{task}: async move {{ select! {{ .. }} }}" },
                        None => rsx! { "async move {{ select! {{ .. }} }}" },
                    }
                }
                Race {
                    armed,
                    timer,
                    winner,
                    on_press: move |color| {
                        act_now(sim, |s| s.press(color));
                    },
                }
            }

            div { class: "join-handle",
                code { "JoinHandle<Won>" }
                match (task, winner) {
                    (None, _) => rsx! {
                        span { class: "note-pill", "not spawned yet" }
                    },
                    (Some(_), Some(Won::Timer { waited_s })) => rsx! {
                        span { class: "note-pill resolved",
                            ".await → Ok(Timer({waited_s:.0}s))"
                        }
                    },
                    (Some(_), Some(Won::Button { color })) => rsx! {
                        span { class: "note-pill resolved {color.css_name()}",
                            ".await → Ok({color.display()})"
                        }
                    },
                    (Some(_), None) => rsx! {
                        span { class: "note-pill pending", "is_finished() == false" }
                    },
                }
            }

            div { class: "create-row",
                button {
                    class: "btn await",
                    disabled: armed,
                    onclick: move |_| {
                        act_now(sim, |s| s.spawn());
                    },
                    "tokio::spawn"
                }
            }

            div { class: "game-footer",
                span { class: "dim", "the task runs on its own; the handle is how you hear back" }
                button {
                    class: "btn",
                    onclick: move |_| sim.set(SpawnSim::new(wall_offset_ms())),
                    "restart"
                }
            }
        }
    }
}

// ---- §4: the race in a loop ----

#[component]
pub fn LoopSelectGame() -> Element {
    let mut sim = use_signal(|| LoopSelectSim::new(wall_offset_ms()));
    let clock = use_clock();
    use_tick(sim, clock);

    let looping = sim.read();
    let (running, rounds, history) = (
        looping.running(),
        looping.rounds(),
        looping.history().to_vec(),
    );
    let select = looping.select();
    let (armed, timer, winner) = (select.armed(), select.timer(), select.winner());
    drop(looping);

    rsx! {
        div { class: "diagram loop-select-game",
            div { class: "game-header",
                "loop / select"
                if running {
                    span { class: "round-badge", "round {rounds + 1}" }
                }
            }

            Race {
                armed: armed && running,
                timer,
                winner,
                on_press: move |color| {
                    act_now(sim, |s| s.press(color));
                },
            }

            Tape { history: history.clone() }

            div { class: "create-row",
                if running {
                    button {
                        class: "btn",
                        onclick: move |_| act_now(sim, |s| s.stop()),
                        "break"
                    }
                } else {
                    button {
                        class: "btn await",
                        onclick: move |_| {
                            act_now(sim, |s| s.start());
                        },
                        "Run the loop"
                    }
                }
                if !history.is_empty() {
                    button {
                        class: "btn",
                        onclick: move |_| sim.with_mut(|s| s.clear()),
                        "clear"
                    }
                }
            }

            div { class: "game-footer",
                span { class: "dim", "one loop, one select, state only it can touch — that is an actor" }
                // A fresh loop, not `break` + `clear`: that empties the tape
                // but keeps the round counter, so the badge would read
                // "round 13" over an empty tape.
                button {
                    class: "btn",
                    onclick: move |_| sim.set(LoopSelectSim::new(wall_offset_ms())),
                    "restart"
                }
            }
        }
    }
}

/// The running tape of completed rounds, newest last.
#[component]
fn Tape(history: Vec<Round>) -> Element {
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
        assert_eq!(remaining_s(Some(1.0)), 1);
        assert_eq!(remaining_s(Some(999.0)), 1);
        assert_eq!(remaining_s(Some(1_001.0)), 2);
    }

    #[test]
    fn a_countdown_past_its_deadline_reads_zero_rather_than_negative() {
        assert_eq!(remaining_s(Some(-20.0)), 0);
    }

    #[test]
    fn an_idle_branch_reads_zero() {
        assert_eq!(remaining_s(None), 0);
    }
}
