//! The button-future minigame: one future, three buttons, real I/O.

use dioxus::prelude::*;

use crate::games::{use_game_connection, GameConnection};
use crate::protocol::{ButtonEvent, ButtonState, ButtonWire, Color};
use crate::sim::ButtonSim;
use crate::AppCtx;

fn apply_button_event(mut state: Signal<ButtonState>, event: ButtonEvent) {
    match event {
        ButtonEvent::Snapshot { state: new_state } => state.set(new_state),
        ButtonEvent::Activated => state.set(ButtonState::Pending),
        ButtonEvent::Resolved { color } => state.set(ButtonState::Ready(color)),
    }
}

fn dispatch(
    conn: GameConnection,
    mut sim: Signal<ButtonSim>,
    state: Signal<ButtonState>,
    wire: ButtonWire,
) {
    if conn.is_local() {
        for event in sim.with_mut(|s| s.handle(&wire)) {
            apply_button_event(state, event);
        }
    } else {
        conn.send(&wire);
    }
}

fn restart(
    conn: GameConnection,
    ctx: AppCtx,
    mut sim: Signal<ButtonSim>,
    mut state: Signal<ButtonState>,
) {
    if conn.is_local() {
        sim.set(ButtonSim::new());
        state.set(ButtonState::Idle);
        conn.set_status("single-player");
    } else {
        ctx.send(crate::AppUp::Restart {
            game: "button".to_string(),
        });
    }
}

#[component]
pub fn ButtonGame() -> Element {
    let ctx: AppCtx = use_context();
    let sim = use_signal(ButtonSim::new);
    let state = use_signal(|| ButtonState::Idle);

    let conn = use_game_connection::<ButtonEvent>("/ws/game/button", move |event| {
        apply_button_event(state, event)
    });

    let dot_class = match state() {
        ButtonState::Idle => "future-pad".to_string(),
        ButtonState::Pending => "future-pad pending".to_string(),
        ButtonState::Ready(_) => "future-pad resolved".to_string(),
    };
    let dot_label = match state() {
        ButtonState::Idle => "idle",
        ButtonState::Pending => "pending",
        ButtonState::Ready(_) => "resolved",
    };
    let resolved = match state() {
        ButtonState::Ready(color) => Some(color),
        _ => None,
    };

    rsx! {
        div { class: "diagram",
            div { class: "game-header", "button-future" }
            div { class: "status", "{conn.status}" }

            // the future, as an actor dot
            div { class: dot_class, {dot_label} }

            // resolved: arrow to the yielded value, right of the consumed future
            if let Some(color) = resolved {
                div { class: "future-arrow" }
                div { class: "result-pad {color.css_name()}", "{color.css_name()}" }
            }

            div { class: "create-row",
                if state() != ButtonState::Pending {
                    button {
                        class: "btn await",
                        disabled: !conn.connected(),
                        onclick: move |_| dispatch(conn, sim, state, ButtonWire::Activate),
                        "Create Future"
                    }
                }
            }

            div { class: "press-row",
                for color in [Color::Red, Color::Green, Color::Blue] {
                    button {
                        class: "press-pad {color.css_name()}",
                        disabled: state() != ButtonState::Pending || !conn.connected(),
                        title: "{color.display()}",
                        onclick: move |_| dispatch(conn, sim, state, ButtonWire::Press { color }),
                        ""
                    }
                }
            }

            div { class: "game-footer",
                if matches!(state(), ButtonState::Ready(_)) {
                    span { class: "note-pill blocked", "the resolved future is consumed — start a new one" }
                } else {
                    span {}
                }
                button {
                    class: "btn desktop-only",
                    onclick: move |_| restart(conn, ctx, sim, state),
                    "restart"
                }
            }
        }
    }
}
