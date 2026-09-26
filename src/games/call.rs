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

use crate::games::ticker::{subscribe, use_clock};
use crate::games::{palette, use_game_connection, waited_s, GameConnection};
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

#[derive(Clone, Copy)]
struct View {
    snap: Signal<CallSnapshot>,
    received_at: Signal<f64>,
}

impl View {
    fn set(mut self, snap: CallSnapshot) {
        self.snap.set(snap);
        self.received_at.set(now_ms());
    }
}

/// `task` only matters locally: remotely the server knows who sent it.
fn dispatch(conn: GameConnection, mut sim: Signal<CallSim>, view: View, task: u64, wire: CallWire) {
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

fn restart(conn: GameConnection, ctx: AppCtx, mut sim: Signal<CallSim>, view: View) {
    if conn.is_local() {
        sim.set(local_sim());
        view.set(sim.read().snapshot());
        conn.set_status("single-player");
    } else {
        ctx.send(crate::AppUp::Restart { game: Game::Call });
    }
}

fn task_name(task: u64, owner: u64, me: Option<u64>, local: bool) -> String {
    if !local && Some(owner) == me {
        "you".to_string()
    } else {
        format!("task {task}")
    }
}

#[component]
pub fn CallGame() -> Element {
    let ctx: AppCtx = use_context();
    let mode = use_context::<GameMode>();
    let sim = use_signal(local_sim);
    let view = View {
        snap: use_signal(move || match mode {
            GameMode::Local => sim.peek().snapshot(),
            GameMode::Remote => CallSnapshot::default(),
        }),
        received_at: use_signal(now_ms),
    };
    let mut my_conn = use_signal(move || (mode == GameMode::Local).then_some(LOCAL_CONN));

    let conn = use_game_connection::<CallEvent>("/ws/game/call", move |event| match event {
        CallEvent::Hello { conn } => my_conn.set(Some(conn)),
        CallEvent::Snapshot { state } => view.set(state),
    });

    let clock = use_clock();
    let snap = view.snap.read().clone();
    let waiting = |t: &CallTask| matches!(t.phase, CallPhase::Sending | CallPhase::Awaiting);
    if snap.tasks.iter().any(waiting) {
        subscribe(clock);
    }
    let at = (view.received_at)();
    let local = conn.is_local();
    let me = my_conn();
    let host = ctx.may_present();
    let owner_of = |task: u64| {
        snap.tasks
            .iter()
            .find(|t| t.task == task)
            .map(|t| t.owner)
            .unwrap_or(task)
    };
    let mine: Vec<CallTask> = snap
        .tasks
        .iter()
        .filter(|t| Some(t.owner) == me)
        .copied()
        .collect();
    let waiting_count = snap.tasks.iter().filter(|t| waiting(t)).count();

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
                        Requester {
                            key: "{task.task}",
                            task: *task,
                            name: task_name(task.task, task.owner, me, local),
                            mine: !local && Some(task.owner) == me,
                            waited: waited_s(task.for_ms, at),
                        }
                    }
                }

                div { class: "cr-col cr-inbox",
                    span { class: "mx-caption", "the actor's inbox · mpsc::channel({snap.capacity})" }
                    div { class: "ut-slots cr-slots",
                        for slot in 0..snap.capacity {
                            match snap.queue.get(slot) {
                                Some(request) => rsx! {
                                    Message { request: *request, name: task_name(request.task, owner_of(request.task), me, local) }
                                },
                                None => rsx! { span { class: "ut-slot cr-slot" } },
                            }
                        }
                    }
                    if !snap.parked.is_empty() {
                        span { class: "mx-caption", "parked in send().await" }
                        div { class: "mx-chips",
                            for request in snap.parked.iter() {
                                Message { request: *request, name: task_name(request.task, owner_of(request.task), me, local) }
                            }
                        }
                    }
                    span { class: "cr-arrow", "→" }
                }

                div { class: "cr-col cr-actor",
                    div { class: "mx-lock cr-actor-box",
                        div { class: "mx-lock-head", if local { "the actor · you" } else { "the actor · the presenter" } }
                        div { class: "cr-state", "next_id: {snap.next_id}" }
                        div { class: "cr-hand",
                            match snap.in_hand {
                                Some(request) => rsx! {
                                    span { class: "mx-caption", "in hand" }
                                    Message { request, name: task_name(request.task, owner_of(request.task), me, local) }
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
                        Outcome { last, name: task_name(outcome_task(last), owner_of(outcome_task(last)), me, local) }
                    }
                }
            }

            if ctx.may_play() && !mine.is_empty() {
                div { class: "mx-controls",
                    for task in mine {
                        RequestControls {
                            key: "{task.task}",
                            task,
                            label: task_name(task.task, task.owner, me, local),
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

#[component]
/// `waited` is worked out by the caller, which the clock re-renders: this
/// component only re-renders when its props change.
fn Requester(task: CallTask, name: String, mine: bool, waited: u64) -> Element {
    let (class, text) = match task.phase {
        CallPhase::Idle => ("idle", String::new()),
        CallPhase::Sending => ("waiting", format!("send parked · {waited}s")),
        CallPhase::Awaiting => ("waiting", format!("awaiting reply · {waited}s")),
        CallPhase::Got(id) => ("got", format!("Ok({id})")),
        CallPhase::Closed => ("failed", "Err(RecvError)".to_string()),
        CallPhase::GaveUp => ("idle", "gave up".to_string()),
    };
    rsx! {
        span {
            class: if mine { "mx-task {class} mine" } else { "mx-task {class}" },
            style: "--c: {palette::sender_hex(task.task)};",
            "{name}"
            if !text.is_empty() {
                span { class: "blocked-clock", "{text}" }
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
    let busy = matches!(task.phase, CallPhase::Sending | CallPhase::Awaiting);
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
