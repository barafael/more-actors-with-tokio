//! The watch minigame: one sender, one shared cell, and cloneable receivers.
//! Receivers hold the latest value invisibly; "look inside" (`borrow()`)
//! reveals it and — while the read guard is held — blocks the send, which
//! waits as a pending send until the last borrow drops.

mod layout;
mod model;

use dioxus::prelude::*;

use crate::games::palette;
use crate::games::ticker::sleep_ms;
use layout::{rx_center_x, rx_cy, rx_geometry, RX_RIGHT, TX_CTL_LEFT, TX_LEFT, TX_W};
use model::{FlightKind, Receiver, WatchSim};

fn fresh_sim() -> WatchSim {
    // Open on a live channel with two receivers. An empty stage reads as a
    // broken export, and restart returns here too.
    WatchSim::new('c', 2)
}

#[component]
pub fn WatchGame() -> Element {
    let mut sim = use_signal(fresh_sim);
    let mut draft = use_signal(String::new);
    let mut send = move || {
        if let Some(ch) = draft().trim().chars().next() {
            sim.with_mut(|s| s.send(ch));
        }
        draft.set(String::new());
    };

    let channel = sim.read();
    let value = channel.value();
    let pending = channel.pending_send().is_some();
    let receivers = channel.receivers();
    let flights = channel.flights().to_vec();
    let notice = channel.notice();
    let can_subscribe = channel.can_subscribe();
    drop(channel);
    let borrows = receivers.iter().filter(|r| r.borrowing).count();
    let ids: Vec<u64> = receivers.iter().map(|r| r.id).collect();

    // a notice clears itself shortly after appearing
    let notice_key = use_memo(move || sim.read().notice().map(|(_, key)| key));
    use_effect(move || {
        if let Some(key) = notice_key() {
            spawn(async move {
                sleep_ms(900).await;
                sim.with_mut(|s| s.clear_notice(key));
            });
        }
    });

    rsx! {
        div { class: "diagram watch-game",
            div { class: "game-header", "watch" }

            div {
                class: "tx-ctl",
                style: "left: {TX_CTL_LEFT}%;",
                input {
                    class: "field",
                    maxlength: 1,
                    placeholder: "ch",
                    value: draft(),
                    disabled: pending,
                    oninput: move |e| draft.set(e.value()),
                    onkeydown: move |e| {
                        if e.key() == Key::Enter {
                            send();
                        }
                    },
                }
                button {
                    class: "btn clone",
                    disabled: pending,
                    onclick: move |_| send(),
                    "send"
                }
            }

            div {
                class: if pending { "actor tx-node blocked" } else { "actor tx-node" },
                style: "left: {TX_LEFT}%; width: {TX_W}%;",
                title: "Sender<T>",
                "{value}"
            }
            div { class: "actor watch-cell",
                style: "left: {layout::CELL_LEFT}%; width: {layout::CELL_W}%;",
                div { class: "cell-val", "{value}" }
                div { class: "cell-cap", "latest value" }
            }

            if let Some((notice, _)) = notice {
                div { class: "note-pill watch-badge", {notice.label()} }
            }

            for (i, rx) in receivers.iter().copied().enumerate() {
                ReceiverCard {
                    key: "{rx.id}",
                    rx,
                    geometry: rx_geometry(i, receivers.len()),
                    // what the receiver shows: the value only while it holds
                    // a guard on it; idle, the card already says which
                    // receiver this is
                    face: if rx.borrowing {
                        value.to_string()
                    } else if rx.awaiting {
                        "…".to_string()
                    } else {
                        String::new()
                    },
                    can_clone: can_subscribe,
                    sim,
                }
            }

            for flight in flights {
                span {
                    key: "{flight.key}",
                    class: if flight.kind == FlightKind::Value { "watch-flight" } else { "watch-flight borrow" },
                    style: "--to-x: {rx_center_x()}%; --to-y: {rx_cy(&ids, flight.rx)}%;",
                    onanimationend: move |_| sim.with_mut(|s| s.land(flight.key)),
                    "{flight.ch}"
                }
            }

            div { class: "game-footer",
                div { class: "side-btns",
                    button {
                        class: "btn",
                        disabled: !can_subscribe,
                        title: "subscribe a new receiver — it instantly sees the current value",
                        onclick: move |_| {
                            sim.with_mut(|s| s.subscribe());
                        },
                        "＋ receiver"
                    }
                    if pending {
                        span { class: "note-pill blocked", "send blocked by {borrows} borrow(s)" }
                    } else if receivers.is_empty() {
                        span { class: "note-pill", "0 receivers — send fails" }
                    } else {
                        span { class: "note-pill", "{receivers.len()} receiver(s)" }
                    }
                }
                button {
                    class: "btn",
                    onclick: move |_| {
                        sim.set(fresh_sim());
                        draft.set(String::new());
                    },
                    "restart"
                }
            }
        }
    }
}

/// One receiver, drawn as a self-contained card: identity, what it is doing
/// right now, and the buttons that operate it. Keeping the controls inside
/// the card is what stops them colliding with the cell as receivers pile up.
#[component]
fn ReceiverCard(
    rx: Receiver,
    geometry: (f64, f64),
    face: String,
    can_clone: bool,
    sim: Signal<WatchSim>,
) -> Element {
    let (top, height) = geometry;
    let id = rx.id;
    // by receiver: two receivers painted alike would make the fan-out
    // unreadable
    let (color, text) = (palette::receiver_hex(id), palette::receiver_text_color(id));
    let class = if rx.awaiting {
        "actor rx-card awaiting"
    } else if rx.borrowing {
        "actor rx-card borrowing"
    } else {
        "actor rx-card"
    };
    // what this receiver is doing, in the language of the API it models
    let (state, state_class) = if rx.awaiting {
        ("blocked in changed()", "rx-state blocked")
    } else if rx.borrowing {
        ("holding borrow()", "rx-state borrowing")
    } else {
        ("idle", "rx-state")
    };
    rsx! {
        div {
            class,
            style: "top: {top}%; height: {height}%; right: {RX_RIGHT}%; --c: {color}; --fg: {text};",
            div { class: "rx-id",
                span { class: "rx-tag", "rx #{id}" }
                span { class: "rx-seen", "seen v{rx.version}" }
                span { class: state_class, {state} }
            }
            div { class: "rx-face", "{face}" }
            div { class: "rx-btns",
                button {
                    class: "btn small",
                    title: "changed(): block until the value differs from what this receiver has seen",
                    onclick: move |_| sim.with_mut(|s| s.toggle_changed(id)),
                    if rx.awaiting { "cancel" } else { "until change" }
                }
                button {
                    class: "btn small await",
                    title: "borrow(): hold a read guard on the value (blocks the sender)",
                    onclick: move |_| sim.with_mut(|s| s.toggle_borrow(id)),
                    if rx.borrowing { "drop borrow" } else { "look inside" }
                }
                button {
                    class: "btn small clone",
                    title: "clone this receiver (inherits the seen version, starts idle)",
                    disabled: !can_clone,
                    onclick: move |_| {
                        sim.with_mut(|s| s.clone_receiver(id));
                    },
                    "clone"
                }
                button {
                    class: "btn small",
                    title: "drop this receiver",
                    onclick: move |_| sim.with_mut(|s| s.drop_receiver(id)),
                    "drop"
                }
            }
        }
    }
}
