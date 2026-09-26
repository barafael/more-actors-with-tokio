//! The unit-test stepper: the `UniqueIdService` test from "More Actors with
//! Tokio", run one line at a time.
//!
//! Not a shared game. Every viewer steps their own copy, in the talk as in
//! the export, because a debugger with forty hands on it is no debugger —
//! and because the thing on show needs no server: the test is deterministic,
//! so its whole run is recorded up front in [`trace`] and stepping is an
//! index into that record.

mod trace;

use std::rc::Rc;

use dioxus::prelude::*;

use crate::highlight::highlight_lines;
use trace::{
    step_over, trace, Binding, Callee, LoopState, Outcome, Resp, Variant, World, REQUESTS,
};

#[component]
pub fn UnitTestGame() -> Element {
    let mut variant = use_signal(Variant::default);
    let mut at = use_signal(|| 0usize);
    let frames = use_memo(move || trace(variant()));
    let test_lines = use_memo(move || highlight_lines(&variant().test_code(), "rust"));
    // The callees never change, so they are highlighted once per mount
    // rather than on every step.
    let callee_lines = use_hook(|| {
        Rc::new([
            highlight_lines(trace::GET_UNIQUE_ID, "rust"),
            highlight_lines(trace::EVENT_LOOP, "rust"),
        ])
    });

    let frames = frames.read();
    let index = at().min(frames.len() - 1);
    let frame = &frames[index];
    let last = frames.len() - 1;
    let over = step_over(&frames, index);

    // Lines of the test that have finished, for the gutter. A line with a
    // call still in progress has not finished.
    let finished: Vec<usize> = frames[..=index]
        .iter()
        .filter(|f| f.call.is_none())
        .filter_map(|f| f.test_line)
        .collect();

    // The callee pane shows the call in progress; between calls, the next
    // one (or, once they are all done, the last), un-highlighted.
    let shown = frame.call.map(|call| call.callee).or_else(|| {
        let ahead = frames[index..].iter().find_map(|f| f.call);
        let behind = frames[..index].iter().rev().find_map(|f| f.call);
        ahead.or(behind).map(|call| call.callee)
    });
    let stopped = frame.world.outcome == Outcome::Hangs;

    let test_rows = test_lines
        .read()
        .iter()
        .enumerate()
        .map(|(line, html)| {
            let kind = if frame.test_line == Some(line) {
                if frame.call.is_some() {
                    "calling"
                } else {
                    "now"
                }
            } else if finished.contains(&line) {
                "ran"
            } else {
                ""
            };
            code_row(line, html, kind)
        })
        .collect::<Vec<_>>();

    let callee_rows = shown.map(|callee| {
        let lines = match callee {
            Callee::GetUniqueId(_) => &callee_lines[0],
            Callee::EventLoop => &callee_lines[1],
        };
        let active = frame.call.filter(|call| call.callee == callee);
        lines
            .iter()
            .enumerate()
            .map(|(line, html)| {
                let kind = match active {
                    Some(call) if call.line == line && stopped => "parked",
                    Some(call) if call.line == line => "now",
                    _ => "",
                };
                code_row(line, html, kind)
            })
            .collect::<Vec<_>>()
    });

    rsx! {
        div { class: "diagram unit-test-game",
            div { class: "game-header", "unit-test" }
            div { class: "status", "single-player · your own copy" }

            div { class: "ut-panes",
                div { class: "ut-pane",
                    div { class: "ut-pane-head", "tests.rs" }
                    div { class: "ut-code", {test_rows.into_iter()} }
                }
                div { class: if frame.call.is_some() { "ut-pane live" } else { "ut-pane" },
                    div { class: "ut-pane-head", {callee_title(shown, frame.call.is_some())} }
                    div { class: "ut-code",
                        if let Some(rows) = callee_rows {
                            {rows.into_iter()}
                        }
                    }
                    Locals { world: frame.world.clone() }
                }
                div { class: "ut-pane",
                    div { class: "ut-pane-head", "the world after this line" }
                    WorldView { world: frame.world.clone() }
                }
            }

            p { class: "ut-note", {note(&frame.note)} }

            div { class: "ut-controls",
                div { class: "ut-steps",
                    button {
                        class: "btn small",
                        disabled: index == 0,
                        onclick: move |_| at.set(0),
                        "reset"
                    }
                    button {
                        class: "btn small",
                        disabled: index == 0,
                        onclick: move |_| at.set(index.saturating_sub(1)),
                        "◀ back"
                    }
                    button {
                        class: "btn small await",
                        disabled: index == last,
                        onclick: move |_| at.set((index + 1).min(last)),
                        "step into"
                    }
                    button {
                        class: "btn small await",
                        disabled: index == last,
                        onclick: move |_| at.set(over),
                        "step over ▶"
                    }
                    span { class: "ut-count", "{index + 1} / {frames.len()}" }
                }
                div { class: "ut-variants",
                    for choice in Variant::ALL {
                        button {
                            class: if variant() == choice { "btn small on" } else { "btn small" },
                            onclick: move |_| {
                                variant.set(choice);
                                at.set(0);
                            },
                            "{choice.label()}"
                        }
                    }
                }
            }
        }
    }
}

fn code_row(line: usize, html: &str, kind: &str) -> Element {
    rsx! {
        div { class: "ut-line {kind}",
            span { class: "ut-gutter", "{line + 1}" }
            span { class: "ut-src", dangerous_inner_html: "{html}" }
        }
    }
}

fn callee_title(shown: Option<Callee>, live: bool) -> String {
    let name = match shown {
        Some(Callee::GetUniqueId(index)) => format!("get_unique_id(&tx)  ·  resp{}", index + 1),
        Some(Callee::EventLoop) => "actor.event_loop(rx)".to_string(),
        None => return "no call".to_string(),
    };
    if live {
        name
    } else {
        format!("{name}  ·  not running")
    }
}

/// Render a note, turning `backticked` spans into code.
fn note(text: &str) -> Element {
    rsx! {
        for (i, part) in text.split('`').enumerate() {
            if i % 2 == 1 {
                code { class: "ut-tick", "{part}" }
            } else {
                "{part}"
            }
        }
    }
}

/// The oneshot pairs are coloured by request, so `cb2` in the buffer and
/// `resp2` in the test's scope read as two ends of one thing.
fn pair_class(index: usize) -> &'static str {
    ["ut-pair-0", "ut-pair-1", "ut-pair-2"][index % 3]
}

/// What the running call holds in its own scope.
#[component]
fn Locals(world: World) -> Element {
    let chips: Vec<(usize, String)> = if let Some(hand) = world.in_hand {
        let n = hand.index + 1;
        if hand.packed {
            vec![
                (hand.index, format!("message: GetUniqueId {{ cb{n} }}")),
                (hand.index, "callback_receiver".into()),
            ]
        } else {
            vec![
                (hand.index, format!("callback: cb{n}")),
                (hand.index, "callback_receiver".into()),
            ]
        }
    } else if let Some(index) = world.handling {
        vec![(index, format!("message: GetUniqueId {{ cb{} }}", index + 1))]
    } else {
        Vec::new()
    };
    let running = matches!(world.event_loop, LoopState::Running | LoopState::Parked);

    rsx! {
        div { class: "ut-locals",
            span { class: "ut-locals-head", "locals" }
            if running {
                span { class: "ut-chip", "self.next_id: {world.next_id.unwrap_or_default()}" }
            }
            for (index, label) in chips {
                span { class: "ut-chip {pair_class(index)}", "{label}" }
            }
        }
    }
}

#[component]
fn WorldView(world: World) -> Element {
    let (outcome_class, outcome) = match world.outcome {
        Outcome::Running => ("running", "running"),
        Outcome::Passed => ("passed", "test passed ✓"),
        Outcome::Hangs => ("hangs", "hangs forever ✗"),
    };
    let next_id = world.next_id.unwrap_or_default();
    let (actor_class, actor_where) = match (world.next_id, world.event_loop, world.service) {
        (None, _, _) => ("unborn", "not created yet"),
        (_, _, Binding::Live) => ("home", "in `service`, returned by the loop"),
        (_, _, Binding::Dropped) => ("home", "dropped at the end of the test"),
        (_, LoopState::Running, _) => ("running", "inside event_loop, running"),
        (_, LoopState::Parked, _) => ("parked", "inside event_loop, parked on recv()"),
        _ => ("home", "in `actor`, plain data"),
    };
    let senders = usize::from(world.tx == Binding::Live);
    let receiver = match world.rx {
        Binding::Undeclared => "—",
        Binding::Live => "in the test",
        Binding::Moved => "in event_loop",
        Binding::Dropped => "dropped",
    };

    rsx! {
        div { class: "ut-world",
            span { class: "ut-outcome {outcome_class}", "{outcome}" }

            div { class: "ut-actor {actor_class}",
                div { class: "ut-actor-state", "UniqueIdService {{ next_id: {next_id} }}" }
                div { class: "ut-actor-where", {note(actor_where)} }
            }

            div { class: "ut-chan",
                div { class: "ut-chan-head",
                    if world.capacity == 0 {
                        "no channel yet"
                    } else {
                        "mpsc::channel({world.capacity}) · senders: {senders} · receiver: {receiver}"
                    }
                }
                div { class: "ut-slots",
                    for slot in 0..world.capacity {
                        match world.buffer.get(slot) {
                            Some(&index) => rsx! {
                                span { class: "ut-slot filled {pair_class(index)}", "cb{index + 1}" }
                            },
                            None => rsx! { span { class: "ut-slot" } },
                        }
                    }
                    if let Some(index) = world.send_parked {
                        span { class: "ut-slot parked {pair_class(index)}", "cb{index + 1} ⏸" }
                    }
                }
            }

            div { class: "ut-scope",
                Row { name: "actor", state: binding_text(world.actor, "UniqueIdService"), class: binding_class(world.actor) }
                Row { name: "tx", state: binding_text(world.tx, "Sender<Message>"), class: binding_class(world.tx) }
                Row { name: "rx", state: binding_text(world.rx, "Receiver<Message>"), class: binding_class(world.rx) }
                for index in 0..REQUESTS {
                    Row {
                        name: format!("resp{}", index + 1),
                        state: resp_text(world.resps[index]),
                        class: format!("{} {}", resp_class(world.resps[index]), pair_class(index)),
                    }
                }
                Row { name: "service", state: binding_text(world.service, "UniqueIdService"), class: binding_class(world.service) }
                Row {
                    name: "nums",
                    state: world.nums.map(|[a, b, c]| format!("({a}, {b}, {c})")).unwrap_or_else(|| "—".into()),
                    class: if world.nums.is_some() { "live" } else { "undeclared" },
                }
                Row {
                    name: "asserts",
                    state: format!("{} of 2 passed", world.asserts_passed),
                    class: if world.asserts_passed > 0 { "live" } else { "undeclared" },
                }
            }
        }
    }
}

#[component]
fn Row(name: String, state: String, class: String) -> Element {
    rsx! {
        div { class: "ut-row {class}",
            span { class: "ut-name", "{name}" }
            span { class: "ut-val", "{state}" }
        }
    }
}

fn binding_text(binding: Binding, ty: &str) -> String {
    match binding {
        Binding::Undeclared => "—".into(),
        Binding::Live => ty.into(),
        Binding::Moved => "moved into event_loop".into(),
        Binding::Dropped => "dropped".into(),
    }
}

fn binding_class(binding: Binding) -> &'static str {
    match binding {
        Binding::Undeclared => "undeclared",
        Binding::Live => "live",
        Binding::Moved => "moved",
        Binding::Dropped => "dropped",
    }
}

fn resp_text(resp: Resp) -> String {
    match resp {
        Resp::Undeclared => "—".into(),
        Resp::Empty => "oneshot::Receiver · empty".into(),
        Resp::Holds(value) => format!("oneshot::Receiver · holds {value}"),
        Resp::Joined => "taken by try_join!".into(),
    }
}

fn resp_class(resp: Resp) -> &'static str {
    match resp {
        Resp::Undeclared => "undeclared",
        Resp::Empty => "live",
        Resp::Holds(_) => "live holds",
        Resp::Joined => "moved",
    }
}
