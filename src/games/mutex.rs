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

use crate::games::ticker::use_clock;
use crate::games::{
    palette, use_game_connection, use_timed, GameConnection, TaskChip, Timed, Viewer, Waited,
};
use crate::protocol::{Game, MutexEvent, MutexSnapshot, MutexWire};
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

fn dispatch(
    conn: GameConnection,
    mut sim: Signal<MutexSim>,
    view: Timed<MutexSnapshot>,
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

fn restart(
    conn: GameConnection,
    ctx: AppCtx,
    mut sim: Signal<MutexSim>,
    view: Timed<MutexSnapshot>,
) {
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
    /// Parked in `lock().await`: the place in the queue, and the wait as
    /// of the snapshot.
    Waiting(usize, f64),
    Holding,
}

fn standing(snap: &MutexSnapshot, task: u64) -> Standing {
    if snap.holder.is_some_and(|h| h.task == task) {
        return Standing::Holding;
    }
    snap.waiters
        .iter()
        .position(|w| w.task == task)
        .map(|place| Standing::Waiting(place + 1, snap.waiters[place].for_ms))
        .unwrap_or(Standing::Idle)
}

#[component]
pub fn MutexGame() -> Element {
    let ctx: AppCtx = use_context();
    let mode = use_context::<GameMode>();
    let sim = use_signal(local_sim);
    let view = use_timed(move || match mode {
        GameMode::Local => sim.peek().snapshot(),
        GameMode::Remote => MutexSnapshot::default(),
    });
    let mut my_conn = use_signal(move || (mode == GameMode::Local).then_some(LOCAL_CONN));

    let conn = use_game_connection::<MutexEvent>("/ws/game/mutex", move |event| match event {
        MutexEvent::Hello { conn } => my_conn.set(Some(conn)),
        MutexEvent::Snapshot { state } => view.set(state),
    });

    // Read by the count-up badges alone, so only they redraw each frame.
    let clock = use_clock();
    let snap = view.snap.read().clone();
    let at = (view.received_at)();
    let viewer = Viewer {
        me: my_conn(),
        local: conn.is_local(),
    };

    // holder, waiters and idle never share a task, so sorting is all it
    // takes to keep each task's controls in a stable place
    let mut mine: Vec<u64> = snap
        .holder
        .iter()
        .chain(&snap.waiters)
        .chain(&snap.idle)
        .filter(|t| viewer.owns(t.owner))
        .map(|t| t.task)
        .collect();
    mine.sort_unstable();

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
                            TaskChip { key: "{waiter.task}", task: waiter.task, owner: waiter.owner, viewer, class: "waiting",
                                Waited { for_ms: waiter.for_ms, at, clock, label: format!("{}. ", place + 1) }
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
                                TaskChip { task: holder.task, owner: holder.owner, viewer, class: "holding",
                                    Waited { for_ms: holder.for_ms, at, clock, label: "guard · " }
                                }
                            },
                            None => rsx! { span { class: "mx-empty", "nobody holds the guard" } },
                        }
                    }
                }

                div { class: "mx-idle",
                    for task in snap.idle.iter() {
                        TaskChip { key: "{task.task}", task: task.task, owner: task.owner, viewer, class: "idle" }
                    }
                }
            }

            if ctx.may_play() {
                div { class: "mx-controls",
                    for task in mine {
                        TaskControls {
                            key: "{task}",
                            task,
                            label: if viewer.local { format!("task {task}") } else { "you".to_string() },
                            standing: standing(&snap, task),
                            at,
                            clock,
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
fn TaskControls(
    task: u64,
    label: String,
    standing: Standing,
    at: f64,
    clock: Signal<f64>,
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
                Standing::Waiting(place, for_ms) => rsx! {
                    button { class: "btn", disabled: true,
                        Waited { for_ms, at, clock, label: format!("parked · #{place} in line · "), class: "" }
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
