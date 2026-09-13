//! The broadcast minigame: clone senders, subscribe receivers, send without
//! blocking — and fall behind to meet `Lagged(n)`. One ring buffer sits
//! between the senders and the receivers; a full buffer evicts its oldest
//! value.

mod layout;
mod state;

use dioxus::prelude::*;

use crate::games::palette;
use crate::games::{is_desktop, use_game_connection, GameConnection};
use crate::protocol::{
    BroadcastEvent, BroadcastSnapshot, BroadcastWire, RxState, BROADCAST_CAPACITY,
};
use crate::sim::{BroadcastSim, BROADCAST_HOST_CONN, LOCAL_CONN};
use crate::{AppCtx, GameMode};

use layout::{BUFFER_LEFT, BUFFER_SLOT_W, RECEIVERS_RIGHT, SENDERS_LEFT, SENDERS_WIDTH};
use state::{empty_snapshot, ChannelState, Flight};

fn local_sim() -> BroadcastSim {
    let mut sim = BroadcastSim::new();
    // single-player: the local player is the presenter and owns the host seed
    sim.handle(&BroadcastWire::ClaimPresenter, LOCAL_CONN);
    sim
}

fn initial_snapshot(mode: GameMode) -> BroadcastSnapshot {
    match mode {
        GameMode::Local => local_sim().snapshot(),
        GameMode::Remote => empty_snapshot(),
    }
}

/// Local (loopback) driver: handle the wire against the in-browser sim and
/// republish its snapshot, mirroring what the server actor broadcasts.
fn dispatch_local(mut sim: Signal<BroadcastSim>, chan: &ChannelState, wire: BroadcastWire) {
    let events = sim.with_mut(|s| s.handle(&wire, LOCAL_CONN));
    for event in events {
        chan.apply(event);
    }
    chan.set_snapshot(sim.read().snapshot());
}

fn send_from(
    conn: GameConnection,
    sim: Signal<BroadcastSim>,
    chan: &ChannelState,
    mut drafts: Signal<Vec<(u64, char)>>,
    handle: u64,
) {
    let draft = drafts
        .read()
        .iter()
        .find(|(c, _)| *c == handle)
        .map(|(_, ch)| *ch);
    if let Some(ch) = draft {
        let wire = BroadcastWire::Send { conn: handle, ch };
        if conn.is_local() {
            dispatch_local(sim, chan, wire);
        } else {
            conn.send(&wire);
        }
        drafts.with_mut(|d| d.retain(|(c, _)| *c != handle));
    }
}

fn set_draft(mut drafts: Signal<Vec<(u64, char)>>, handle: u64, ch: Option<char>) {
    drafts.with_mut(|d| {
        d.retain(|(c, _)| *c != handle);
        if let Some(ch) = ch {
            d.push((handle, ch));
        }
    });
}

fn restart(
    conn: GameConnection,
    ctx: AppCtx,
    mut sim: Signal<BroadcastSim>,
    mut chan: ChannelState,
) {
    if conn.is_local() {
        sim.set(local_sim());
        chan.set_snapshot(sim.read().snapshot());
        chan.flights.set(Vec::new());
        conn.set_status("single-player");
    } else {
        ctx.send(crate::AppUp::Restart {
            game: "broadcast".to_string(),
        });
    }
}

#[derive(Clone, PartialEq)]
struct SenderView {
    conn: u64,
    owner: u64,
    mine: bool,
    is_host: bool,
    top: f64,
    height: f64,
}

#[derive(Clone, PartialEq)]
struct RxView {
    rx: RxState,
    top: f64,
}

#[component]
pub fn BroadcastGame() -> Element {
    let ctx: AppCtx = use_context();
    let mode = use_context::<GameMode>();

    let sim = use_signal(local_sim);
    let chan = ChannelState {
        snap: use_signal(move || initial_snapshot(mode)),
        flights: use_signal(Vec::new),
        key: use_signal(|| 0),
    };
    let mut my_conn: Signal<Option<u64>> = use_signal(|| match mode {
        GameMode::Local => Some(LOCAL_CONN),
        GameMode::Remote => None,
    });
    let drafts = use_signal(Vec::<(u64, char)>::new);

    let conn = use_game_connection::<BroadcastEvent>("/ws/game/broadcast", move |event| {
        if let BroadcastEvent::Hello { conn } = event {
            my_conn.set(Some(conn));
        } else {
            #[cfg(not(target_arch = "wasm32"))]
            let _ = chan.apply(event);
            #[cfg(target_arch = "wasm32")]
            if let Some(key) = chan.apply(event) {
                // fallback: never let a missed animationend strand a flight
                spawn(async move {
                    gloo_timers::future::TimeoutFuture::new(1500).await;
                    chan.remove_flight(key);
                });
            }
        }
    });

    // Claim the presenter slot: the host seed sender transfers to the
    // presenter, so they can send from it (last claim wins).
    use_effect(move || {
        if conn.connected() && !conn.is_local() && my_conn().is_some() && is_desktop() {
            conn.send(&BroadcastWire::ClaimPresenter);
        }
    });

    let senders = use_memo(move || chan.snap.read().senders.clone());
    let receivers = use_memo(move || chan.snap.read().receivers.clone());
    let flights = use_memo(move || chan.flights.read().clone());
    let my_receiver = use_memo(move || {
        chan.snap
            .read()
            .receivers
            .iter()
            .find(|r| r.owner == my_conn().unwrap_or(0))
            .map(|r| r.receiver)
    });
    let closed = use_memo(move || senders().is_empty());

    let nodes = use_memo(move || {
        let senders = senders();
        senders
            .iter()
            .enumerate()
            .map(|(i, s)| {
                let row = layout::SENDERS_SPAN / senders.len().max(1) as f64;
                let height = row * 0.6;
                SenderView {
                    conn: s.conn,
                    owner: s.owner,
                    mine: s.owner == my_conn().unwrap_or(0) && my_conn().is_some(),
                    is_host: s.conn == BROADCAST_HOST_CONN,
                    top: layout::SENDERS_TOP + i as f64 * row + (row - height) / 2.0,
                    height,
                }
            })
            .collect::<Vec<SenderView>>()
    });
    let rx_views = use_memo(move || {
        let receivers = receivers();
        receivers
            .iter()
            .enumerate()
            .map(|(i, rx)| RxView {
                rx: *rx,
                top: layout::receiver_cy(i, receivers.len()),
            })
            .collect::<Vec<RxView>>()
    });

    let occupancy = use_memo(move || chan.snap.read().buffer.len());

    rsx! {
        div { class: "diagram",
            div { class: "game-header", "broadcast" }
            div { class: "status", "{conn.status}" }

            if closed() {
                div { class: "note-pill broadcast-closed",
                    "channel closed — every sender dropped. restart to play again."
                }
            }

            for view in nodes() {
                div { key: "{view.conn}",
                    div {
                        class: if view.is_host { "actor sender-node host" } else { "actor sender-node" },
                        style: "top: {view.top}%; height: {view.height}%; left: {SENDERS_LEFT}%; width: {SENDERS_WIDTH}%;",
                        title: if view.is_host { "host seed sender" } else if view.mine { "your sender" } else { "another client\'s sender" },
                        { if view.is_host { "host" } else { "Sender" } }
                    }
                    div {
                        class: "sender-btns",
                        style: "top: {view.top + view.height + 1.0}%; left: {SENDERS_LEFT}%; width: {SENDERS_WIDTH}%;",
                        button {
                            class: "btn small",
                            disabled: !conn.connected() || my_receiver().is_some(),
                            title: "subscribe a receiver via this sender — it starts at the tail, so it only sees values sent from now on (one per client)",
                            onclick: move |_| {
                                let wire = BroadcastWire::Subscribe;
                                if conn.is_local() {
                                    dispatch_local(sim, &chan, wire);
                                } else {
                                    conn.send(&wire);
                                }
                            },
                            "subscribe"
                        }
                        button {
                            class: "btn small clone",
                            disabled: !conn.connected(),
                            title: "clone this sender — the clone is yours",
                            onclick: move |_| {
                                let wire = BroadcastWire::CloneSender { source: view.conn };
                                if conn.is_local() {
                                    dispatch_local(sim, &chan, wire);
                                } else {
                                    conn.send(&wire);
                                }
                            },
                            "clone"
                        }
                        if view.mine {
                            {
                                let draft = drafts.read().iter().find(|(c, _)| *c == view.conn)
                                    .map(|(_, ch)| ch.to_string()).unwrap_or_default();
                                let busy = closed();
                                rsx! {
                                    input {
                                        key: "in-{view.conn}",
                                        class: "field",
                                        maxlength: 1,
                                        disabled: busy,
                                        value: draft.to_string(),
                                        oninput: move |e| set_draft(drafts, view.conn, e.value().chars().next()),
                                    }
                                    button {
                                        class: "btn send",
                                        disabled: busy,
                                        onclick: move |_| send_from(conn, sim, &chan, drafts, view.conn),
                                        "send"
                                    }
                                }
                            }
                        }
                    }
                }
            }

            BufferRow { buffer: chan.snap.read().buffer.clone(), tail: chan.snap.read().tail }

            for view in rx_views() {
                ReceiverBox {
                    key: "{view.rx.receiver}",
                    receiver: view.rx.receiver,
                    owner: view.rx.owner,
                    next: view.rx.next,
                    lagged_total: view.rx.lagged_total,
                    last: view.rx.last,
                    error: view.rx.error,
                    waiting: view.rx.waiting,
                    top: view.top,
                    mine: view.rx.owner == my_conn().unwrap_or(0),
                    connected: conn.connected(),
                    onreceive: move |_| {
                        let wire = BroadcastWire::Receive { receiver: view.rx.receiver };
                        if conn.is_local() {
                            dispatch_local(sim, &chan, wire);
                        } else {
                            conn.send(&wire);
                        }
                    },
                    ondrop: move |_| {
                        let wire = BroadcastWire::Unsubscribe { receiver: view.rx.receiver };
                        if conn.is_local() {
                            dispatch_local(sim, &chan, wire);
                        } else {
                            conn.send(&wire);
                        }
                    },
                }
            }

            LinkLayer { senders: senders(), snapshot: chan.snap.read().clone() }

            FlightLayer {
                flights: flights(),
                senders: senders(),
                onremove: move |key| chan.remove_flight(key),
            }

            div { class: "game-footer",
                span { class: "note-pill occupancy",
                    "buffer {occupancy} / {BROADCAST_CAPACITY}"
                }
                if closed() {
                    span { class: "note-pill blocked", "closed" }
                }
                if let Some(receiver) = my_receiver() {
                    span { class: "note-pill", "rx #{receiver}" }
                }
                button {
                    class: "btn desktop-only",
                    disabled: !conn.connected(),
                    onclick: move |_| restart(conn, ctx, sim, chan),
                    "restart"
                }
            }
        }
    }
}

/// Sequence number of the oldest retained value. `tail` counts every value
/// ever sent, so the ring holds seqs `oldest..tail`. Saturating: a snapshot
/// can never have more buffered values than were sent, but the arithmetic
/// must not underflow if one ever does.
fn oldest_seq(tail: u64, buffered: usize) -> u64 {
    tail.saturating_sub(buffered as u64)
}

#[component]
fn BufferRow(buffer: Vec<crate::protocol::BufferChar>, tail: u64) -> Element {
    rsx! {
        div { class: "buffer-label", "channel buffer" }
        for i in 0..BROADCAST_CAPACITY {
            { match buffer.get(i) {
                Some(owned) => rsx! {
                    div {
                        key: "{oldest_seq(tail, buffer.len()) + i as u64}",
                        class: "actor buf-slot filled",
                        style: "left: {layout::slot_left(i)}%; width: {layout::BUFFER_SLOT_W}%; --c: {palette::sender_hex(owned.conn)};",
                        "{owned.ch}"
                    }
                },
                None => rsx! {
                    div {
                        class: "actor buf-slot",
                        key: "empty-{i}",
                        style: "left: {layout::slot_left(i)}%; width: {layout::BUFFER_SLOT_W}%;",
                    }
                },
            } }
        }
        div { class: "buffer-oldest", style: "left: {BUFFER_LEFT}%;", "oldest" }
        div {
            class: "buffer-newest",
            style: "left: {BUFFER_LEFT + (BROADCAST_CAPACITY - 1) as f64 * (BUFFER_SLOT_W + 1.2)}%;",
            "newest"
        }
        div {
            class: "buffer-tail",
            style: "left: {BUFFER_LEFT + tail.min(BROADCAST_CAPACITY as u64) as f64 * (BUFFER_SLOT_W + 1.2)}%;",
            "next →"
        }
    }
}

#[component]
fn ReceiverBox(
    receiver: u64,
    owner: u64,
    next: u64,
    lagged_total: u64,
    last: Option<crate::protocol::BufferChar>,
    error: Option<crate::protocol::BroadcastError>,
    waiting: bool,
    top: f64,
    mine: bool,
    connected: bool,
    onreceive: EventHandler,
    ondrop: EventHandler,
) -> Element {
    let _ = owner;
    // constant node shape: dynamic parts are text/classes only
    let class = if error == Some(crate::protocol::BroadcastError::Closed) {
        "actor rx-b-node closed"
    } else if waiting {
        "actor rx-b-node waiting"
    } else {
        "actor rx-b-node"
    };
    let label = if error == Some(crate::protocol::BroadcastError::Closed) {
        "closed"
    } else if waiting {
        "…"
    } else {
        "Rx"
    };
    let last_text = last
        .map(|o| o.ch.to_string())
        .unwrap_or_else(|| "–".to_string());
    let last_color = last
        .map(|o| palette::sender_hex(o.conn))
        .unwrap_or_else(|| "#999".to_string());
    let (badge, badge_class) = match error {
        Some(crate::protocol::BroadcastError::Lagged(n)) => {
            (format!("lagged {n}"), "lag-badge".to_string())
        }
        Some(crate::protocol::BroadcastError::Closed) => {
            ("closed".to_string(), "lag-badge closed".to_string())
        }
        _ if lagged_total > 0 => (format!("missed {lagged_total}"), "lag-total".to_string()),
        _ => (String::new(), "lag-total".to_string()),
    };
    rsx! {
        div {
            class: class,
            style: "top: {top}%; right: {RECEIVERS_RIGHT}%;",
            div { class: "rx-name", {label} }
            div { class: "rx-last", style: "color: {last_color};", {last_text} }
            div { class: badge_class, {badge} }
            div { class: "rx-next", "next #{next}" }
            if mine {
                div { class: "rx-btns",
                    button {
                        class: "btn small rcv",
                        disabled: !connected || error == Some(crate::protocol::BroadcastError::Closed),
                        title: if waiting { "recv() — blocked, completes on the next send" } else { "recv()" },
                        onclick: move |_| onreceive.call(()),
                        "recv"
                    }
                    button {
                        class: "btn small",
                        disabled: !connected,
                        title: "drop this receiver",
                        onclick: move |_| ondrop.call(()),
                        "drop"
                    }
                }
            }
        }
    }
}

#[component]
fn FlightLayer(
    flights: Vec<Flight>,
    senders: Vec<crate::protocol::SenderInfo>,
    onremove: EventHandler<u64>,
) -> Element {
    rsx! {
        for f in flights {
            span {
                key: "{f.key}",
                class: "bcast-flight",
                style: "--from-x: {SENDERS_LEFT + SENDERS_WIDTH / 2.0}%; --from-y: {flight_start_y(&senders, f.from)}%; color: {palette::sender_hex(f.from)};",
                onanimationend: move |_| onremove.call(f.key),
                "{f.ch}"
            }
        }
    }
}

/// Dashed index lines: each receiver ties to the buffer slot its `next`
/// points at; red when it has fallen behind the oldest value.
#[component]
fn LinkLayer(senders: Vec<crate::protocol::SenderInfo>, snapshot: BroadcastSnapshot) -> Element {
    let oldest = oldest_seq(snapshot.tail, snapshot.buffer.len());
    let lines = snapshot
        .receivers
        .iter()
        .enumerate()
        .map(|(i, rx)| {
            let y = layout::receiver_cy(i, snapshot.receivers.len());
            let x = RECEIVERS_RIGHT + layout::RECEIVERS_WIDTH;
            if snapshot.buffer.is_empty() {
                return (y, x, layout::BUFFER_LEFT, layout::BUFFER_CY, false);
            }
            // `next` is 1-based (the seq this receiver will read next), the
            // ring is indexed from `oldest`. A receiver that subscribed at
            // the tail points one past the newest value and has nothing to
            // read yet, so its line rests on the newest slot.
            let cursor = rx.next.saturating_sub(1);
            let behind = cursor < oldest;
            let slot = (cursor.saturating_sub(oldest) as usize)
                .min(snapshot.buffer.len().saturating_sub(1));
            (
                y,
                x,
                layout::slot_left(slot) + layout::BUFFER_SLOT_W / 2.0,
                layout::BUFFER_CY,
                behind,
            )
        })
        .collect::<Vec<(f64, f64, f64, f64, bool)>>();
    rsx! {
        svg { class: "arrow-layer",
            for (y, x1, x2, y2, behind) in lines {
                line {
                    key: "{y}-{x2}-{behind}",
                    x1: "{x1}%",
                    y1: "{y}%",
                    x2: "{x2}%",
                    y2: "{y2}%",
                    stroke: if behind { "#d32f2f" } else { "#6b6b6b" },
                    "stroke-width": 2,
                    "stroke-dasharray": "5 4",
                }
            }
        }
    }
}

fn flight_start_y(senders: &[crate::protocol::SenderInfo], conn: u64) -> f64 {
    let n = senders.len().max(1) as f64;
    let row = layout::SENDERS_SPAN / n;
    match senders.iter().position(|s| s.conn == conn) {
        Some(i) => {
            let (top, h) = (layout::SENDERS_TOP + i as f64 * row, row * 0.6);
            top + h / 2.0
        }
        None => layout::SENDERS_TOP + layout::SENDERS_SPAN / 2.0,
    }
}
