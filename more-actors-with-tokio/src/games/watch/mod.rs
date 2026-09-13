//! The watch minigame: one fixed sender (the presenter), a create phase for
//! the initial value, cloneable receivers everyone can subscribe. Receivers
//! hold the latest value invisibly; "look inside" (`borrow()`) reveals it and
//! — while the read guard is held — blocks the presenter's send, which queues
//! as a pending send until the last borrow drops.

mod layout;
mod state;

use dioxus::prelude::*;

use crate::games::{is_desktop, use_game_connection, GameConnection};
use crate::protocol::{RxInfo, WatchEvent, WatchSnapshot, WatchWire, WATCH_MAX_RX};
use crate::sim::{WatchSim, LOCAL_CONN};
use crate::{AppCtx, GameMode};

use crate::games::palette;
use layout::{rx_center_x, rx_cy, rx_geometry, RX_CTL_RIGHT, RX_RIGHT, TX_CTL_LEFT, TX_LEFT, TX_W};
use state::{empty_snapshot, Flight, FlightKind, WatchState};

fn local_sim() -> WatchSim {
    let mut sim = WatchSim::new();
    sim.set_presenter(Some(LOCAL_CONN));
    sim
}

fn initial_snapshot(mode: GameMode) -> WatchSnapshot {
    match mode {
        GameMode::Local => local_sim().snapshot(),
        GameMode::Remote => empty_snapshot(),
    }
}

/// Local (loopback) driver: handle the wire against the in-browser sim and
/// republish its snapshot, mirroring what the server actor broadcasts.
fn dispatch_local(mut sim: Signal<WatchSim>, chan: WatchState, wire: WatchWire) {
    let events = sim.with_mut(|s| s.handle(&wire, LOCAL_CONN));
    for event in events {
        chan.apply(event);
    }
    chan.set_snapshot(sim.read().snapshot());
}

fn dispatch(conn: GameConnection, sim: Signal<WatchSim>, chan: WatchState, wire: WatchWire) {
    if conn.is_local() {
        dispatch_local(sim, chan, wire);
    } else {
        conn.send(&wire);
    }
}

fn create_channel(
    conn: GameConnection,
    sim: Signal<WatchSim>,
    chan: WatchState,
    mut create_init: Signal<String>,
) {
    let init = create_init().trim().chars().next();
    create_init.set(String::new());
    dispatch(conn, sim, chan, WatchWire::Create { init });
}

fn send_value(
    conn: GameConnection,
    sim: Signal<WatchSim>,
    chan: WatchState,
    mut draft: Signal<String>,
) {
    let ch = draft().trim().chars().next();
    draft.set(String::new());
    if let Some(ch) = ch {
        dispatch(conn, sim, chan, WatchWire::Send { ch });
    }
}

fn restart(conn: GameConnection, ctx: AppCtx, mut sim: Signal<WatchSim>, mut chan: WatchState) {
    if conn.is_local() {
        sim.set(local_sim());
        chan.set_snapshot(sim.read().snapshot());
        chan.flights.set(Vec::new());
        conn.set_status("single-player");
    } else {
        ctx.send(crate::AppUp::Restart {
            game: crate::protocol::Game::Watch,
        });
    }
}

#[derive(Clone, PartialEq)]
struct RxView {
    rx: RxInfo,
    top: f64,
    height: f64,
    label: String,
    color: String,
    text: &'static str,
}

#[derive(Clone, Copy, PartialEq)]
struct CtlView {
    id: u64,
    cy: f64,
    awaiting: bool,
    borrowing: bool,
    mine: bool,
    can_clone: bool,
}

#[component]
pub fn WatchGame() -> Element {
    let ctx: AppCtx = use_context();
    let mode = use_context::<GameMode>();

    let sim = use_signal(local_sim);
    let chan = WatchState {
        snap: use_signal(move || initial_snapshot(mode)),
        flights: use_signal(Vec::new),
        key: use_signal(|| 0),
    };
    let mut my_conn: Signal<Option<u64>> = use_signal(|| None);
    let mut badge = use_signal(|| None::<(String, u64)>);
    let mut create_init = use_signal(String::new);
    let mut draft = use_signal(String::new);

    let conn = use_game_connection::<WatchEvent>("/ws/game/watch", move |event| match event {
        WatchEvent::Hello { conn } => my_conn.set(Some(conn)),
        WatchEvent::SameValue => badge.set(Some(("same".to_string(), chan.next_key()))),
        WatchEvent::SendRefused => badge.set(Some(("error".to_string(), chan.next_key()))),
        WatchEvent::SendBlocked => badge.set(Some(("blocked".to_string(), chan.next_key()))),
        ev => chan.apply(ev),
    });

    // The presenter slot gates Create/Send server-side; desktop clients claim
    // it, last claim wins.
    use_effect(move || {
        if conn.connected() && !conn.is_local() && my_conn().is_some() && is_desktop() {
            conn.send(&WatchWire::ClaimPresenter);
        }
    });

    // transient badges clear themselves shortly after appearing
    use_effect(move || {
        if let Some((_, _key)) = badge() {
            #[cfg(target_arch = "wasm32")]
            {
                let mut badge = badge;
                spawn(async move {
                    gloo_timers::future::TimeoutFuture::new(900).await;
                    badge.with_mut(|b| {
                        if b.as_ref().is_some_and(|(_, k)| *k == _key) {
                            *b = None;
                        }
                    });
                });
            }
        }
    });

    let snapshot = use_memo(move || chan.snap.read().clone());
    let receivers = use_memo(move || snapshot().receivers.clone());
    let flights = use_memo(move || chan.flights.read().clone());
    let my = use_memo(move || my_conn().unwrap_or(LOCAL_CONN));
    let i_am_presenter = use_memo(move || conn.is_local() || snapshot().presenter == Some(my()));
    let my_count = use_memo(move || {
        snapshot()
            .receivers
            .iter()
            .filter(|r| r.owner == my())
            .count()
    });
    let can_create = use_memo(move || my_count() < WATCH_MAX_RX);
    let connected = conn.connected();

    let nodes = use_memo(move || {
        let receivers = receivers();
        receivers
            .iter()
            .enumerate()
            .map(|(i, r)| {
                let (top, height) = rx_geometry(i, receivers.len());
                let label = if r.borrowing {
                    snapshot()
                        .value
                        .map(|c| c.to_string())
                        .unwrap_or_else(|| "…".to_string())
                } else if r.awaiting {
                    "…".to_string()
                } else {
                    "Rx".to_string()
                };
                RxView {
                    rx: *r,
                    top,
                    height,
                    label,
                    color: palette::sender_hex(r.owner),
                    text: palette::sender_text_color(r.owner),
                }
            })
            .collect::<Vec<RxView>>()
    });
    let controls: Memo<Vec<CtlView>> = use_memo(move || {
        let receivers = receivers();
        receivers
            .iter()
            .map(|r| {
                let mine = r.owner == my();
                CtlView {
                    id: r.id,
                    cy: rx_cy(&receivers, r.id),
                    awaiting: r.awaiting,
                    borrowing: r.borrowing,
                    mine,
                    can_clone: can_create(),
                }
            })
            .collect::<Vec<CtlView>>()
    });

    let value_label = use_memo(move || {
        snapshot()
            .value
            .map(|c| c.to_string())
            .unwrap_or_else(|| "…".to_string())
    });
    let pending = use_memo(move || snapshot().pending_send.is_some());

    rsx! {
        div { class: "diagram",
            div { class: "game-header", "watch" }
            div { class: "status", "{conn.status}" }

            if snapshot().created && i_am_presenter() {
                div {
                    class: "tx-ctl",
                    style: "left: {TX_CTL_LEFT}%;",
                    input {
                        class: "field",
                        maxlength: 1,
                        placeholder: "ch",
                        value: draft(),
                        disabled: !connected || pending(),
                        oninput: move |e| draft.set(e.value()),
                        onkeydown: move |e| {
                            if e.key() == dioxus::prelude::Key::Enter {
                                send_value(conn, sim, chan, draft);
                            }
                        },
                    }
                    button {
                        class: "btn clone",
                        disabled: !connected || pending(),
                        onclick: move |_| send_value(conn, sim, chan, draft),
                        "send"
                    }
                }
            }

            if !snapshot().created && i_am_presenter() {
                div { class: "create-row",
                    input {
                        class: "field",
                        maxlength: 1,
                        placeholder: "ch",
                        title: "initial value — a watch channel is created with one",
                        value: create_init(),
                        disabled: !connected,
                        oninput: move |e| create_init.set(e.value()),
                    }
                    button {
                        class: "btn await",
                        disabled: !connected,
                        onclick: move |_| create_channel(conn, sim, chan, create_init),
                        "Create Channel"
                    }
                }
            }

            if snapshot().created {
                div {
                    class: if pending() { "actor tx-node blocked" } else { "actor tx-node" },
                    style: "left: {TX_LEFT}%; width: {TX_W}%;",
                    title: "Sender<T> — the fixed sender screen",
                    "{value_label()}"
                }
                div { class: "actor watch-cell",
                    style: "left: {layout::CELL_LEFT}%; width: {layout::CELL_W}%;",
                    div { class: "cell-val", "{value_label()}" }
                    div { class: "cell-cap", "latest value" }
                }

                if let Some((text, _key)) = badge() {
                    div { class: "note-pill watch-badge", {text} }
                }

                for node in nodes() {
                    ReceiverNode {
                        key: "{node.rx.id}",
                        style_top: node.top,
                        style_height: node.height,
                        label: node.label.clone(),
                        color: node.color.clone(),
                        text: node.text,
                        awaiting: node.rx.awaiting,
                        borrowing: node.rx.borrowing,
                    }
                }

                for view in controls() {
                    if view.mine || i_am_presenter() {
                        ReceiverControls {
                            key: "ctl-{view.id}",
                            view: view,
                            connected: connected,
                            onaction: move |wire: WatchWire| dispatch(conn, sim, chan, wire),
                        }
                    }
                }

                FlightLayer {
                    flights: flights(),
                    flights_receivers: receivers(),
                    onremove: move |key| chan.remove_flight(key),
                }
            }

            div { class: "game-footer",
                div { class: "side-btns",
                    if snapshot().created {
                        button {
                            class: "btn",
                            disabled: !connected || !can_create(),
                            title: "subscribe a new receiver — it instantly sees the current value",
                            onclick: move |_| dispatch(conn, sim, chan, WatchWire::NewReceiver),
                            "＋ receiver"
                        }
                    }
                    if !snapshot().created {
                        span { class: "note-pill", "no channel yet — presenter creates it" }
                    } else if pending() {
                        span { class: "note-pill blocked",
                            "send blocked by {snapshot().receivers.iter().filter(|r| r.borrowing).count()} borrow(s)"
                        }
                    } else if receivers().is_empty() {
                        span { class: "note-pill", "0 receivers — send fails" }
                    } else {
                        span { class: "note-pill", "{receivers().len()} receiver(s)" }
                    }
                }
                button {
                    class: "btn desktop-only",
                    onclick: move |_| restart(conn, ctx, sim, chan),
                    "restart"
                }
            }
        }
    }
}

#[component]
fn ReceiverNode(
    style_top: f64,
    style_height: f64,
    label: String,
    color: String,
    text: &'static str,
    awaiting: bool,
    borrowing: bool,
) -> Element {
    let class = if awaiting {
        "actor rx-node awaiting"
    } else if borrowing {
        "actor rx-node borrowing"
    } else {
        "actor rx-node"
    };
    rsx! {
        div {
            class: class,
            style: "top: {style_top}%; height: {style_height}%; right: {RX_RIGHT}%; --c: {color}; --fg: {text};",
            {label}
        }
    }
}

#[component]
fn ReceiverControls(view: CtlView, connected: bool, onaction: EventHandler<WatchWire>) -> Element {
    let borrow_btn = if view.borrowing {
        "drop borrow"
    } else {
        "look inside"
    };
    let await_btn = if view.awaiting {
        "cancel"
    } else {
        "until change"
    };
    let rx = view.id;
    rsx! {
        div {
            class: "rx-ctl",
            style: "top: {view.cy}%; right: {RX_CTL_RIGHT}%;",
            if view.mine {
                button {
                    class: "btn small",
                    title: "changed(): block until the value differs from what you've seen",
                    disabled: !connected,
                    onclick: move |_| onaction.call(WatchWire::AwaitChange { rx }),
                    {await_btn}
                }
                button {
                    class: "btn small await",
                    title: "borrow(): reveal the contained value while holding a read guard (blocks sending)",
                    disabled: !connected,
                    onclick: move |_| onaction.call(WatchWire::LookInside { rx }),
                    {borrow_btn}
                }
            }
            button {
                class: "btn small clone",
                title: "clone this receiver (restarts awaiting)",
                disabled: !connected || !view.can_clone,
                onclick: move |_| onaction.call(WatchWire::CloneReceiver { rx }),
                "clone"
            }
            if view.mine {
                button {
                    class: "btn small",
                    title: "drop this receiver",
                    disabled: !connected,
                    onclick: move |_| onaction.call(WatchWire::DropReceiver { rx }),
                    "drop"
                }
            }
        }
    }
}

#[component]
fn FlightLayer(
    flights: Vec<Flight>,
    flights_receivers: Vec<RxInfo>,
    onremove: EventHandler<u64>,
) -> Element {
    rsx! {
        for fl in flights {
            span {
                key: "{fl.key}",
                class: if fl.kind == FlightKind::Value { "watch-flight" } else { "watch-flight borrow" },
                style: "--to-x: {rx_center_x()}%; --to-y: {rx_cy(&flights_receivers, fl.rx)}%;",
                onanimationend: move |_| onremove.call(fl.key),
                "{fl.ch.map(|c| c.to_string()).unwrap_or_default()}"
            }
        }
    }
}
