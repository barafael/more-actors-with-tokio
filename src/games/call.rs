//! The call-and-response minigame: `mpsc` carrying `oneshot` callbacks.
//!
//! Every phone runs `get_unique_id(&tx).await` and then awaits its reply.
//! The presenter is the actor's event loop, by hand: receive a message,
//! then either answer through its callback or drop it. Nobody gets an id
//! the presenter has not personally handed out, and the room waits — with
//! a clock on every phone — for one loop's attention.
//!
//! In the export the lone player is both: three requesters and the actor.

use dioxus::prelude::*;

use crate::games::ticker::use_clock;
use crate::games::{
    palette, use_game_connection, use_timed, GameConnection, TaskChip, Timed, Viewer, Waited,
};
use crate::protocol::{
    CallEvent, CallOutcome, CallPhase, CallRequest, CallSnapshot, CallTask, CallWire, Game,
};
use crate::sim::{now_ms, LOCAL_CONN};
use crate::sim_call::CallSim;
use crate::{AppCtx, GameMode};

/// The export's requesters, all owned by the one local player.
const LOCAL_TASKS: [u64; 3] = [1, 2, 3];

fn local_sim() -> CallSim {
    let mut sim = CallSim::default();
    for task in LOCAL_TASKS {
        sim.add_task(task, LOCAL_CONN);
    }
    sim
}

/// `task` only matters locally: remotely the server knows who sent it.
fn dispatch(
    conn: GameConnection,
    mut sim: Signal<CallSim>,
    view: Timed<CallSnapshot>,
    task: u64,
    wire: CallWire,
) {
    if conn.is_local() {
        sim.with_mut(|sim| {
            sim.sync_now(now_ms());
            sim.handle(task, true, wire);
        });
        view.set(sim.read().snapshot());
    } else {
        conn.send(&wire);
    }
}

fn restart(conn: GameConnection, ctx: AppCtx, mut sim: Signal<CallSim>, view: Timed<CallSnapshot>) {
    if conn.is_local() {
        sim.set(local_sim());
        view.set(sim.read().snapshot());
        conn.set_status("single-player");
    } else {
        ctx.send(crate::AppUp::Restart { game: Game::Call });
    }
}

#[component]
pub fn CallGame() -> Element {
    let ctx: AppCtx = use_context();
    let mode = use_context::<GameMode>();
    let sim = use_signal(local_sim);
    let view = use_timed(move || match mode {
        GameMode::Local => sim.peek().snapshot(),
        GameMode::Remote => CallSnapshot::default(),
    });
    let mut my_conn = use_signal(move || (mode == GameMode::Local).then_some(LOCAL_CONN));

    let conn = use_game_connection::<CallEvent>("/ws/game/call", move |event| match event {
        CallEvent::Hello { conn } => my_conn.set(Some(conn)),
        CallEvent::Snapshot { state } => view.set(state),
    });

    // Read by the count-up badges alone, so only they redraw each frame.
    let clock = use_clock();
    let snap = view.snap.read().clone();
    let at = (view.received_at)();
    let viewer = Viewer {
        me: my_conn(),
        local: conn.is_local(),
    };
    let host = ctx.may_present();
    // A departed requester's message outlives it, so fall back to the task.
    let name_of = |task: u64| {
        let owner = snap
            .tasks
            .iter()
            .find(|t| t.task == task)
            .map_or(task, |t| t.owner);
        viewer.name(task, owner)
    };
    let mine: Vec<CallTask> = snap
        .tasks
        .iter()
        .filter(|t| viewer.owns(t.owner))
        .copied()
        .collect();
    let waiting_count = snap.tasks.iter().filter(|t| waiting(t.phase)).count();

    rsx! {
        div { class: "diagram call-game",
            div { class: "game-header", "mpsc + oneshot" }
            div { class: "status", "{conn.status}" }

            div { class: "cr-stage",
                div { class: "cr-col cr-requesters",
                    span { class: "mx-caption", "requesters" }
                    if snap.tasks.is_empty() {
                        span { class: "mx-empty", "scan the code to become a requester" }
                    }
                    for task in snap.tasks.iter() {
                        Requester { key: "{task.task}", task: *task, viewer, at, clock }
                    }
                }

                div { class: "cr-col cr-inbox",
                    span { class: "mx-caption", "the actor's inbox · mpsc::channel({snap.capacity})" }
                    div { class: "ut-slots cr-slots",
                        for slot in 0..snap.capacity {
                            match snap.queue.get(slot) {
                                Some(request) => rsx! {
                                    Message { request: *request, name: name_of(request.task) }
                                },
                                None => rsx! { span { class: "ut-slot cr-slot" } },
                            }
                        }
                    }
                    if !snap.parked.is_empty() {
                        span { class: "mx-caption", "parked in send().await" }
                        div { class: "mx-chips",
                            for request in snap.parked.iter() {
                                Message { request: *request, name: name_of(request.task) }
                            }
                        }
                    }
                    span { class: "flow-arrow", "→" }
                }

                div { class: "cr-col cr-actor",
                    div { class: "mx-lock cr-actor-box",
                        div { class: "mx-lock-head", if viewer.local { "the actor · you" } else { "the actor · the presenter" } }
                        div { class: "cr-state", "next_id: {snap.next_id}" }
                        div { class: "cr-hand",
                            match snap.in_hand {
                                Some(request) => rsx! {
                                    span { class: "mx-caption", "in hand" }
                                    Message { request, name: name_of(request.task) }
                                },
                                None => rsx! { span { class: "mx-empty", "nothing in hand" } },
                            }
                        }
                    }
                    if host {
                        div { class: "cr-host",
                            button {
                                class: "btn await",
                                disabled: !conn.connected() || snap.in_hand.is_some() || snap.queue.is_empty(),
                                onclick: move |_| dispatch(conn, sim, view, 0, CallWire::Recv),
                                "rx.recv()"
                            }
                            button {
                                class: "btn rcv",
                                disabled: !conn.connected() || snap.in_hand.is_none(),
                                onclick: move |_| dispatch(conn, sim, view, 0, CallWire::Reply),
                                "callback.send({snap.next_id})"
                            }
                            button {
                                class: "btn",
                                disabled: !conn.connected() || snap.in_hand.is_none(),
                                onclick: move |_| dispatch(conn, sim, view, 0, CallWire::DropCallback),
                                "drop(callback)"
                            }
                        }
                    }
                    if let Some(last) = snap.last {
                        Outcome { last, name: name_of(outcome_task(last)) }
                    }
                }
            }

            if ctx.may_play() && !mine.is_empty() {
                div { class: "mx-controls",
                    for task in mine {
                        RequestControls {
                            key: "{task.task}",
                            task,
                            label: viewer.name(task.task, task.owner),
                            connected: conn.connected(),
                            onwire: move |wire| dispatch(conn, sim, view, task.task, wire),
                        }
                    }
                }
            }

            div { class: "game-footer",
                if waiting_count > 0 {
                    span { class: "note-pill blocked", "{waiting_count} waiting on one event loop" }
                } else {
                    span {}
                }
                if host {
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

fn outcome_task(outcome: CallOutcome) -> u64 {
    match outcome {
        CallOutcome::Replied { task, .. }
        | CallOutcome::Unheard { task, .. }
        | CallOutcome::Dropped { task } => task,
    }
}

/// Whether a phase is a call still waiting on the actor.
fn waiting(phase: CallPhase) -> bool {
    matches!(phase, CallPhase::Sending | CallPhase::Awaiting)
}

#[component]
fn Requester(task: CallTask, viewer: Viewer, at: f64, clock: Signal<f64>) -> Element {
    let class = match task.phase {
        CallPhase::Idle | CallPhase::GaveUp => "idle",
        CallPhase::Sending | CallPhase::Awaiting => "waiting",
        CallPhase::Got(_) => "got",
        CallPhase::Closed => "failed",
    };
    rsx! {
        TaskChip { task: task.task, owner: task.owner, viewer, class,
            match task.phase {
                CallPhase::Idle => rsx! {},
                CallPhase::Sending => rsx! { Waited { for_ms: task.for_ms, at, clock, label: "send parked · " } },
                CallPhase::Awaiting => rsx! { Waited { for_ms: task.for_ms, at, clock, label: "awaiting reply · " } },
                CallPhase::Got(id) => rsx! { span { class: "blocked-clock", "Ok({id})" } },
                CallPhase::Closed => rsx! { span { class: "blocked-clock", "Err(RecvError)" } },
                CallPhase::GaveUp => rsx! { span { class: "blocked-clock", "gave up" } },
            }
        }
    }
}

#[component]
fn Message(request: CallRequest, name: String) -> Element {
    rsx! {
        span {
            class: if request.listening { "ut-slot filled cr-msg" } else { "ut-slot filled cr-msg deaf" },
            style: "--c: {palette::sender_hex(request.task)};",
            title: if request.listening { "a callback someone is waiting on" } else { "its receiver was dropped: nobody is listening" },
            "{name}"
        }
    }
}

#[component]
fn Outcome(last: CallOutcome, name: String) -> Element {
    let (class, text) = match last {
        CallOutcome::Replied { id, .. } => ("resolved", format!("sent {id} to {name} → Ok(())")),
        CallOutcome::Unheard { id, .. } => (
            "resolved red",
            format!("sent {id} to {name} → Err({id}): nobody was listening"),
        ),
        CallOutcome::Dropped { .. } => (
            "resolved red",
            format!("dropped {name}'s callback → their await yields Err(RecvError)"),
        ),
    };
    rsx! {
        span { class: "note-pill {class} cr-outcome", "{text}" }
    }
}

#[component]
fn RequestControls(
    task: CallTask,
    label: String,
    connected: bool,
    onwire: EventHandler<CallWire>,
) -> Element {
    let busy = waiting(task.phase);
    rsx! {
        div { class: "mx-row", style: "--c: {palette::sender_hex(task.task)};",
            span { class: "mx-row-name", "{label}" }
            if busy {
                button {
                    class: "btn",
                    disabled: !connected,
                    onclick: move |_| onwire.call(CallWire::GiveUp),
                    if task.phase == CallPhase::Sending { "drop the send" } else { "drop(rx)" }
                }
            } else {
                button {
                    class: "btn await",
                    disabled: !connected,
                    onclick: move |_| onwire.call(CallWire::Request),
                    "get_unique_id(&tx).await"
                }
            }
        }
    }
}
