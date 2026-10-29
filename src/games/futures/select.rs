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
use super::wait::SleepCall;
use crate::games::ticker::{act_now, use_clock, use_tick};

// ---- the shared picture ----

/// Two futures racing, and whatever the select yielded.
///
/// `armed` is the select being awaited at all: before it, both branches are
/// drawn inert, because outside a `select!` neither future exists.
/// `wait_locked` greys the sleep's steppers while a race holds its sleep;
/// the loop leaves them free, since its next round picks up a new value.
/// `turn` is the loop's round count: each new round flashes the race grey
/// as the loop comes back around, alternating two animations so the class
/// change restarts it. Zero means no flash.
#[component]
fn Race(
    armed: bool,
    timer: TimerSim,
    winner: Option<Won>,
    on_press: EventHandler<Color>,
    on_wait: EventHandler<u64>,
    #[props(default)] wait_locked: bool,
    #[props(default)] turn: u64,
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

    let race_class = match turn {
        0 => "race",
        odd if odd % 2 == 1 => "race turn-a",
        _ => "race turn-b",
    };

    rsx! {
        div { class: race_class,
            div { class: branch_class(timer_won),
                div { class: "branch-head",
                    SleepCall { wait_s: timer.wait_s(), locked: wait_locked, on_change: on_wait }
                }
                div { class: "branch-body",
                    if armed {
                        span { class: "countdown",
                            "{remaining_s(timer.remaining_ms())}s"
                        }
                    } else if let Some(waited) = timer.waited_s() {
                        span { class: "countdown done", "{waited}s" }
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
                        "the timer won — yielded {waited_s}s"
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
    let mut sim = use_signal(SelectSim::new);
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
                on_wait: move |wait_s| {
                    sim.with_mut(|s| s.set_wait_s(wait_s));
                },
                wait_locked: armed,
            }

            // One button that turns into its own undo: entering the select
            // creates both branches, dropping it drops both.
            div { class: "create-row",
                button {
                    class: if armed { "btn" } else { "btn await" },
                    onclick: move |_| {
                        if armed {
                            act_now(sim, |s| s.reset());
                        } else {
                            act_now(sim, |s| s.arm());
                        }
                    },
                    if armed { "Drop the select" } else { "Enter the select" }
                }
            }

            div { class: "game-footer",
                span { class: "dim", "the losing branch is dropped, not awaited" }
                button {
                    class: "btn",
                    onclick: move |_| sim.set(SelectSim::new()),
                    "restart"
                }
            }
        }
    }
}

// ---- the loop, spawned ----

/// The spawn slide's whole program: valid Rust (it builds against tokio 1),
/// with each select arm's work deferred to an async function.
const SPAWNED_LOOP_CODE: &str = r#"use std::time::Duration;

use tokio::task::JoinHandle;
use tokio::time::sleep;

#[derive(Debug)]
enum Color { Red, Green, Blue }

/// The timer branch: sleep, then yield how long.
async fn sleep_for(secs: u64) -> u64 {
    sleep(Duration::from_secs(secs)).await;
    secs
}

/// The button branch: whichever button is pressed.
async fn pressed() -> Color {
    todo!("wait on real I/O")
}

async fn on_timer(secs: u64) { println!("timer: {secs}s"); }
async fn on_press(color: Color) { println!("button: {color:?}"); }

fn spawn_loop(secs: u64) -> JoinHandle<()> {
    tokio::spawn(async move {
        loop {
            tokio::select! {
                secs = sleep_for(secs) => on_timer(secs).await,
                color = pressed() => on_press(color).await,
            }
        }
    })
}

#[tokio::main]
async fn main() {
    // never resolves: the loop has no `break`
    spawn_loop(3).await.unwrap();
}"#;

#[component]
pub fn SpawnGame() -> Element {
    let mut sim = use_signal(SpawnSim::new);
    let clock = use_clock();
    use_tick(sim, clock);
    let mut show_code = use_signal(|| false);
    let code_lines = use_hook(|| crate::highlight::highlight_lines(SPAWNED_LOOP_CODE, "rust"));

    let spawn = sim.read();
    let spawned = spawn.spawned();
    let looping = spawn.looping();
    let (rounds, history) = (looping.rounds(), looping.history().to_vec());
    let select = looping.select();
    let (armed, timer, winner) = (select.armed(), select.timer(), select.winner());
    drop(spawn);

    rsx! {
        div { class: "diagram spawn-game",
            div { class: "game-header", "tokio::spawn" }

            div { class: if spawned { "task-frame live" } else { "task-frame" },
                div { class: "task-head",
                    if spawned {
                        "task #1: async move {{ loop {{ select! {{ .. }} }} }}"
                    } else {
                        "async move {{ loop {{ select! {{ .. }} }} }}"
                    }
                }
                Race {
                    armed,
                    timer,
                    winner,
                    on_press: move |color| {
                        act_now(sim, |s| s.press(color));
                    },
                    on_wait: move |wait_s| {
                        sim.with_mut(|s| s.set_wait_s(wait_s));
                    },
                    turn: rounds,
                }
                Tape { history }
            }

            div { class: "join-handle",
                code { "JoinHandle<()>" }
                if spawned {
                    span { class: "note-pill pending", "is_finished() == false, forever: the loop never returns" }
                } else {
                    span { class: "note-pill", "not spawned yet" }
                }
            }

            div { class: "create-row",
                button {
                    class: "btn await",
                    disabled: spawned,
                    onclick: move |_| {
                        act_now(sim, |s| s.spawn());
                    },
                    "tokio::spawn"
                }
                button {
                    class: "btn",
                    onclick: move |_| show_code.set(true),
                    "show the code"
                }
            }

            if show_code() {
                div { class: "spawn-code",
                    for (line , html) in code_lines.iter().enumerate() {
                        div { key: "{line}", class: "snippet-line", dangerous_inner_html: "{html}" }
                    }
                }
                button {
                    class: "btn spawn-code-close",
                    onclick: move |_| show_code.set(false),
                    "hide the code"
                }
            }

            div { class: "game-footer",
                span { class: "dim", "the task runs on its own; a loop with no break never hands anything back" }
                button {
                    class: "btn",
                    onclick: move |_| sim.set(SpawnSim::new()),
                    "restart"
                }
            }
        }
    }
}

// ---- §4: the race in a loop ----

#[component]
pub fn LoopSelectGame() -> Element {
    let mut sim = use_signal(LoopSelectSim::new);
    let clock = use_clock();
    use_tick(sim, clock);

    // The loop always runs: it starts as soon as the page is live. An
    // effect rather than the constructor, so the server render and the
    // hydrating client both draw it unstarted, and the sleep is measured
    // from the browser's clock.
    use_effect(move || {
        act_now(sim, |s| s.start());
    });

    let looping = sim.read();
    let (rounds, history) = (looping.rounds(), looping.history().to_vec());
    let select = looping.select();
    let (armed, timer, winner) = (select.armed(), select.timer(), select.winner());
    drop(looping);

    rsx! {
        div { class: "diagram loop-select-game",
            div { class: "game-header",
                "loop / select"
                if armed {
                    span { class: "round-badge", "round {rounds + 1}" }
                }
            }

            Race {
                armed,
                timer,
                winner,
                on_press: move |color| {
                    act_now(sim, |s| s.press(color));
                },
                on_wait: move |wait_s| {
                    sim.with_mut(|s| s.set_wait_s(wait_s));
                },
                turn: rounds,
            }

            Tape { history: history.clone() }

            div { class: "game-footer",
                span { class: "dim", "one loop, one select, state only it can touch — that is an actor" }
                button {
                    class: "btn",
                    onclick: move |_| {
                        sim.set(LoopSelectSim::new());
                        act_now(sim, |s| s.start());
                    },
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
                        Won::Timer { waited_s } => rsx! { "{waited_s}s" },
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
