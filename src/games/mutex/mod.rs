//! The mutex minigame: the hook. `Arc<Mutex<u64>>`, and three tasks each
//! holding a clone.
//!
//! CONCEPT.md §1. Somebody will lock it. Everyone who tries next parks in
//! `lock().await` and can do nothing else — the only control a waiter has
//! is a clock counting how long it has been stuck. When the holder drops
//! the guard, the longest waiter gets it. Safety, yes; backpressure,
//! lifecycle, a say in any of it — no.

mod model;

use dioxus::prelude::*;

use crate::clock::now_ms;
use crate::games::ticker::use_clock;
use crate::games::{palette, TaskChip, Waited};
use model::{MutexSim, Standing, TASKS};

#[component]
pub fn MutexGame() -> Element {
    let mut sim = use_signal(MutexSim::new);
    let mut act = move |act: fn(&mut MutexSim, u64), task: u64| {
        sim.with_mut(|s| {
            s.sync_now(now_ms());
            act(s, task);
        });
    };

    // Read by the count-up badges alone, so only they redraw each frame.
    let clock = use_clock();
    let mutex = sim.read();
    let value = mutex.value();
    let holder = mutex.holder();
    let waiters: Vec<(u64, f64)> = mutex.waiters().collect();
    let standings = TASKS.map(|task| (task, mutex.standing(task)));
    drop(mutex);

    rsx! {
        div { class: "diagram mutex-game",
            div { class: "game-header", "Arc<Mutex<u64>>" }

            div { class: "mx-stage",
                div { class: "mx-queue",
                    div { class: "mx-chips",
                        if waiters.is_empty() {
                            span { class: "mx-empty", "nobody waiting" }
                        }
                        for (place, (task, since)) in waiters.iter().copied().enumerate() {
                            TaskChip { key: "{task}", task, class: "waiting",
                                Waited { since, clock, label: format!("{}. ", place + 1) }
                            }
                        }
                    }
                }

                div { class: if holder.is_some() { "mx-lock held" } else { "mx-lock" },
                    div { class: "mx-lock-head",
                        if holder.is_some() { "locked" } else { "unlocked" }
                    }
                    div { class: "mx-value", "{value}" }
                    div { class: "mx-holder",
                        match holder {
                            Some((task, since)) => rsx! {
                                TaskChip { task, class: "holding",
                                    Waited { since, clock, label: "guard · " }
                                }
                            },
                            None => rsx! { span { class: "mx-empty", "nobody holds the guard" } },
                        }
                    }
                }

                div { class: "mx-idle",
                    for (task, _) in standings.iter().filter(|(_, s)| *s == Standing::Idle) {
                        TaskChip { key: "{task}", task: *task, class: "idle" }
                    }
                }
            }

            div { class: "mx-controls",
                for (task, standing) in standings {
                    div { key: "{task}", class: "mx-row", style: "--c: {palette::sender_hex(task)};",
                        span { class: "mx-row-name", "task {task}" }
                        match standing {
                            Standing::Idle => rsx! {
                                button {
                                    class: "btn await",
                                    onclick: move |_| act(MutexSim::lock, task),
                                    "lock().await"
                                }
                            },
                            Standing::Waiting { place, since } => rsx! {
                                button { class: "btn", disabled: true,
                                    Waited { since, clock, label: format!("parked · #{place} in line · "), class: "" }
                                }
                            },
                            Standing::Holding { .. } => rsx! {
                                button {
                                    class: "btn",
                                    onclick: move |_| act(MutexSim::increment, task),
                                    "*guard += 1"
                                }
                                button {
                                    class: "btn rcv",
                                    onclick: move |_| act(MutexSim::unlock, task),
                                    "drop(guard)"
                                }
                            },
                        }
                    }
                }
            }

            div { class: "game-footer",
                if !waiters.is_empty() {
                    span { class: "note-pill blocked",
                        "{waiters.len()} parked — nothing to do but wait"
                    }
                } else {
                    span {}
                }
                button {
                    class: "btn",
                    onclick: move |_| sim.set(MutexSim::new()),
                    "restart"
                }
            }
        }
    }
}
