//! The mutex minigame: the hook. `Arc<Mutex<u64>>`, and the whole room
//! holds a clone.
//!
//! CONCEPT.md §1. Somebody will lock it. Everyone who tries next parks in
//! `lock().await` and can do nothing else — the only control a waiter has
//! is a clock counting how long it has been stuck. When the holder drops
//! the guard, the longest waiter gets it. Safety, yes; backpressure,
//! lifecycle, a say in any of it — no.
//!
//! In the talk every phone is one task. In the export the lone player runs
//! three, which is the fewest that makes a queue.

use dioxus::prelude::*;

use crate::games::ticker::{subscribe, use_clock};
use crate::games::{palette, use_game_connection, waited_s, GameConnection};
use crate::protocol::{Game, MutexEvent, MutexSnapshot, MutexTask, MutexWire};
use crate::sim::{now_ms, LOCAL_CONN};
use crate::sim_mutex::MutexSim;
use crate::{AppCtx, GameMode};

/// The export's tasks, all owned by the one local player.
const LOCAL_TASKS: [u64; 3] = [1, 2, 3];

fn local_sim() -> MutexSim {
    let mut sim = MutexSim::new();
    for task in LOCAL_TASKS {
        sim.add_task(task, LOCAL_CONN);
    }
    sim
}

#[derive(Clone, Copy)]
struct View {
    snap: Signal<MutexSnapshot>,
    /// When the current snapshot reached this client, for the count-ups.
    received_at: Signal<f64>,
}

impl View {
    fn set(mut self, snap: MutexSnapshot) {
        self.snap.set(snap);
        self.received_at.set(now_ms());
    }
}

fn dispatch(
    conn: GameConnection,
    mut sim: Signal<MutexSim>,
    view: View,
    task: u64,
    wire: MutexWire,
) {
    if conn.is_local() {
        sim.with_mut(|sim| {
            sim.sync_now(now_ms());
            sim.handle(task, wire);
        });
        view.set(sim.read().snapshot());
    } else {
        conn.send(&wire);
    }
}

fn restart(conn: GameConnection, ctx: AppCtx, mut sim: Signal<MutexSim>, view: View) {
    if conn.is_local() {
        sim.set(local_sim());
        view.set(sim.read().snapshot());
        conn.set_status("single-player");
    } else {
        ctx.send(crate::AppUp::Restart { game: Game::Mutex });
    }
}

/// What one task is doing, from the snapshot.
#[derive(Clone, Copy, PartialEq)]
enum Standing {
    Idle,
    /// Parked in `lock().await`: the place in the queue, and whole seconds
    /// waited so far. Seconds, not the snapshot's duration: the controls
    /// only re-render when this value changes.
    Waiting(usize, u64),
    Holding,
}

fn standing(snap: &MutexSnapshot, task: u64, at: f64) -> Standing {
    if snap.holder.is_some_and(|h| h.task == task) {
        return Standing::Holding;
    }
    snap.waiters
        .iter()
        .position(|w| w.task == task)
        .map(|place| Standing::Waiting(place + 1, waited_s(snap.waiters[place].for_ms, at)))
        .unwrap_or(Standing::Idle)
}

#[component]
pub fn MutexGame() -> Element {
    let ctx: AppCtx = use_context();
    let mode = use_context::<GameMode>();
    let sim = use_signal(local_sim);
    let view = View {
        snap: use_signal(move || match mode {
            GameMode::Local => sim.peek().snapshot(),
            GameMode::Remote => MutexSnapshot::default(),
        }),
        received_at: use_signal(now_ms),
    };
    let mut my_conn = use_signal(move || (mode == GameMode::Local).then_some(LOCAL_CONN));

    let conn = use_game_connection::<MutexEvent>("/ws/game/mutex", move |event| match event {
        MutexEvent::Hello { conn } => my_conn.set(Some(conn)),
        MutexEvent::Snapshot { state } => view.set(state),
    });

    // The count-ups need a frame clock, but only while someone is holding
    // or waiting; an unlocked, empty mutex has nothing to count.
    let clock = use_clock();
    let snap = view.snap.read().clone();
    if snap.holder.is_some() {
        subscribe(clock);
    }
    let at = (view.received_at)();

    let mine: Vec<u64> = match my_conn() {
        Some(me) => snap
            .holder
            .iter()
            .chain(&snap.waiters)
            .chain(&snap.idle)
            .filter(|t| t.owner == me)
            .map(|t| t.task)
            .collect::<std::collections::BTreeSet<_>>()
            .into_iter()
            .collect(),
        None => Vec::new(),
    };
    let local = conn.is_local();
    let me = my_conn();

    rsx! {
        div { class: "diagram mutex-game",
            div { class: "game-header", "Arc<Mutex<u64>>" }
            div { class: "status", "{conn.status}" }

            div { class: "mx-stage",
                div { class: "mx-queue",
                    span { class: "mx-caption", "parked in lock().await — first come, first served" }
                    div { class: "mx-chips",
                        if snap.waiters.is_empty() {
                            span { class: "mx-empty", "nobody waiting" }
                        }
                        for (place, waiter) in snap.waiters.iter().enumerate() {
                            TaskChip {
                                key: "{waiter.task}",
                                task: *waiter,
                                me,
                                local,
                                badge: format!("{}. {}s", place + 1, waited_s(waiter.for_ms, at)),
                                class: "waiting",
                            }
                        }
                    }
                }

                div { class: if snap.holder.is_some() { "mx-lock held" } else { "mx-lock" },
                    div { class: "mx-lock-head",
                        if snap.holder.is_some() { "locked" } else { "unlocked" }
                    }
                    div { class: "mx-value", "{snap.value}" }
                    div { class: "mx-holder",
                        match snap.holder {
                            Some(holder) => rsx! {
                                TaskChip {
                                    task: holder,
                                    me,
                                    local,
                                    badge: format!("guard · {}s", waited_s(holder.for_ms, at)),
                                    class: "holding",
                                }
                            },
                            None => rsx! { span { class: "mx-empty", "nobody holds the guard" } },
                        }
                    }
                }

                div { class: "mx-idle",
                    for task in snap.idle.iter() {
                        TaskChip { key: "{task.task}", task: *task, me, local, badge: String::new(), class: "idle" }
                    }
                }
            }

            if ctx.may_play() {
                div { class: "mx-controls",
                    for task in mine {
                        TaskControls {
                            key: "{task}",
                            task,
                            label: if local { format!("task {task}") } else { "you".to_string() },
                            standing: standing(&snap, task, at),
                            connected: conn.connected(),
                            onwire: move |wire| dispatch(conn, sim, view, task, wire),
                        }
                    }
                }
            }

            div { class: "game-footer",
                if !snap.waiters.is_empty() {
                    span { class: "note-pill blocked",
                        "{snap.waiters.len()} parked — nothing to do but wait"
                    }
                } else {
                    span {}
                }
                if ctx.may_present() {
                    button {
                        class: "btn",
                        onclick: move |_| restart(conn, ctx, sim, view),
                        "restart"
                    }
                }
            }
        }
    }
}

#[component]
fn TaskChip(
    task: MutexTask,
    me: Option<u64>,
    local: bool,
    badge: String,
    class: String,
) -> Element {
    let name = if !local && Some(task.owner) == me {
        "you".to_string()
    } else {
        format!("task {}", task.task)
    };
    let mine = if Some(task.owner) == me && !local {
        " mine"
    } else {
        ""
    };
    rsx! {
        span {
            class: "mx-task {class}{mine}",
            style: "--c: {palette::sender_hex(task.task)};",
            "{name}"
            if !badge.is_empty() {
                span { class: "blocked-clock", "{badge}" }
            }
        }
    }
}

#[component]
fn TaskControls(
    task: u64,
    label: String,
    standing: Standing,
    connected: bool,
    onwire: EventHandler<MutexWire>,
) -> Element {
    rsx! {
        div { class: "mx-row", style: "--c: {palette::sender_hex(task)};",
            span { class: "mx-row-name", "{label}" }
            match standing {
                Standing::Idle => rsx! {
                    button {
                        class: "btn await",
                        disabled: !connected,
                        onclick: move |_| onwire.call(MutexWire::Lock),
                        "lock().await"
                    }
                },
                Standing::Waiting(place, waited) => rsx! {
                    button { class: "btn", disabled: true,
                        "parked · #{place} in line · {waited}s"
                    }
                },
                Standing::Holding => rsx! {
                    button {
                        class: "btn",
                        disabled: !connected,
                        onclick: move |_| onwire.call(MutexWire::Increment),
                        "*guard += 1"
                    }
                    button {
                        class: "btn rcv",
                        disabled: !connected,
                        onclick: move |_| onwire.call(MutexWire::Unlock),
                        "drop(guard)"
                    }
                },
            }
        }
    }
}
