//! The mpsc minigame: every connected client is a `Sender<T>` handle drawn as
//! a colored node, cloning creates extra handles for the presenter, values
//! fly along the arrows, a full buffer blocks (gray) senders until a receive
//! frees a slot. Blocked senders wake in FIFO order, like tokio's.

mod layout;
mod state;

use dioxus::prelude::*;

use crate::games::{is_desktop, use_game_connection, GameConnection};
use crate::protocol::{MpscEvent, MpscSnapshot, MpscWire, SenderInfo, MPSC_CAPACITY};
use crate::sim::{now_ms, MpscSim, LOCAL_CONN};
use crate::{AppCtx, GameMode};

use crate::games::palette;
use layout::{ARROW_X0, ARROW_X1, RECEIVER_CY};
use state::{ChannelState, Flight};

fn local_sim() -> MpscSim {
    #[cfg(target_arch = "wasm32")]
    let flight_ms = crate::protocol::MPSC_FLIGHT_MS as f64;
    #[cfg(not(target_arch = "wasm32"))]
    let flight_ms = 0.0;
    let mut sim = MpscSim::new(flight_ms);
    sim.add_sender(LOCAL_CONN, LOCAL_CONN);
    sim
}

fn initial_snapshot(mode: GameMode) -> MpscSnapshot {
    match mode {
        GameMode::Local => {
            let mut sim = local_sim();
            sim.sync_now(now_ms());
            sim.snapshot()
        }
        GameMode::Remote => state::empty_snapshot(),
    }
}

/// Local (loopback) driver: handle the wire against the in-browser sim and
/// republish its snapshot, mirroring what the server actor broadcasts.
fn dispatch_local(mut sim: Signal<MpscSim>, chan: ChannelState, wire: MpscWire) {
    let events = sim.with_mut(|s| {
        s.sync_now(now_ms());
        let events = s.handle(&wire, LOCAL_CONN);
        for event in &events {
            chan.apply(event.clone());
        }
        s.poll_due()
    });
    for event in events {
        chan.apply(event);
    }
    chan.set_snapshot(sim.read().snapshot());
}

/// Route a wire to the in-browser sim (single-player) or over the game
/// socket. Every call site goes through here so the two modes cannot drift.
fn dispatch(conn: GameConnection, sim: Signal<MpscSim>, chan: ChannelState, wire: MpscWire) {
    if conn.is_local() {
        dispatch_local(sim, chan, wire);
    } else {
        conn.send(&wire);
    }
}

fn send_from(
    conn: GameConnection,
    sim: Signal<MpscSim>,
    chan: ChannelState,
    mut drafts: Signal<Vec<(u64, char)>>,
    sender: u64,
) {
    let draft = drafts
        .read()
        .iter()
        .find(|(c, _)| *c == sender)
        .map(|(_, ch)| *ch);
    if let Some(ch) = draft {
        dispatch(conn, sim, chan, MpscWire::Send { conn: sender, ch });
        drafts.with_mut(|d| d.retain(|(c, _)| *c != sender));
    }
}

fn set_draft(mut drafts: Signal<Vec<(u64, char)>>, sender: u64, ch: Option<char>) {
    drafts.with_mut(|d| {
        d.retain(|(c, _)| *c != sender);
        if let Some(ch) = ch {
            d.push((sender, ch));
        }
    });
}

fn restart(conn: GameConnection, ctx: AppCtx, mut sim: Signal<MpscSim>, mut chan: ChannelState) {
    if conn.is_local() {
        sim.set(local_sim());
        chan.set_snapshot(sim.read().snapshot());
        chan.flights.set(Vec::new());
        conn.set_status("single-player");
    } else {
        ctx.send(crate::AppUp::Restart {
            game: crate::protocol::Game::Mpsc,
        });
    }
}

#[component]
pub fn MpscGame() -> Element {
    let ctx: AppCtx = use_context();
    let mode = use_context::<GameMode>();

    let sim = use_signal(local_sim);
    let chan = ChannelState {
        snap: use_signal(move || initial_snapshot(mode)),
        flights: use_signal(Vec::new),
        key: use_signal(|| 0u64),
    };
    let mut my_conn: Signal<Option<u64>> = use_signal(|| None);
    let drafts = use_signal(Vec::<(u64, char)>::new);
    let mut next_local_conn = use_signal(|| LOCAL_CONN + 1);

    let conn = use_game_connection::<MpscEvent>("/ws/game/mpsc", move |event| {
        if let MpscEvent::Hello { conn } = event {
            my_conn.set(Some(conn));
        } else {
            chan.apply(event);
        }
    });

    // The presenter slot gates cloning server-side; desktop clients claim it,
    // last claim wins.
    use_effect(move || {
        if conn.connected() && !conn.is_local() && my_conn().is_some() && is_desktop() {
            conn.send(&MpscWire::ClaimPresenter);
        }
    });

    // Local flights land on a tick, driven by the sim's own clock. The task
    // is spawned through dioxus so it is cancelled when the component
    // unmounts; a bare loop would keep ticking against dead signals.
    #[cfg(target_arch = "wasm32")]
    use_future(move || {
        let mut sim = sim;
        let chan = chan;
        async move {
            if !conn.is_local() {
                return;
            }
            loop {
                gloo_timers::future::TimeoutFuture::new(50).await;
                let landed = sim.with_mut(|s| {
                    s.sync_now(now_ms());
                    s.poll_due()
                });
                let landed_any = !landed.is_empty();
                for event in landed {
                    chan.apply(event);
                }
                if landed_any {
                    chan.set_snapshot(sim.read().snapshot());
                }
            }
        }
    });

    let senders = use_memo(move || chan.snap.read().senders.clone());
    let flights = use_memo(move || chan.flights.read().clone());
    let nodes = use_memo(move || {
        let snap = chan.snap.read();
        let flights = chan.flights.read();
        snap.senders
            .iter()
            .enumerate()
            .map(|(i, s)| {
                let (top, height) = layout::sender_geometry(i, snap.senders.len());
                let queued = snap
                    .blocked_sends
                    .iter()
                    .find(|b| b.conn == s.conn)
                    .map(|pending| pending.ch);
                // a value in hand outranks the handle's name: while a send
                // is parked or in flight, the node shows what it is carrying
                let label = queued
                    .or_else(|| flights.iter().find(|f| f.conn == s.conn).map(|f| f.ch))
                    .map(|ch| ch.to_string())
                    .unwrap_or_else(|| format!("tx #{}", s.conn));
                NodeView {
                    sender: *s,
                    label,
                    top,
                    height,
                }
            })
            .collect::<Vec<NodeView>>()
    });
    let controls = use_memo(move || {
        let snap = chan.snap.read();
        let drafts = drafts.read();
        // Before `Hello` arrives this client has no identity yet. Falling
        // back to 0 would match LOCAL_CONN / the broadcast host handle and
        // briefly show another party's controls as our own.
        let my = my_conn();
        snap.senders
            .iter()
            .filter(|s| Some(s.owner) == my)
            .map(|s| ControlsView {
                conn: s.conn,
                blocked: s.blocked,
                cy: layout::sender_cy(&snap.senders, s.conn),
                draft: drafts
                    .iter()
                    .find(|(c, _)| *c == s.conn)
                    .map(|(_, ch)| ch.to_string())
                    .unwrap_or_default(),
            })
            .collect::<Vec<ControlsView>>()
    });

    let blocked_count = use_memo(move || {
        chan.snap
            .read()
            .senders
            .iter()
            .filter(|s| s.blocked)
            .count()
    });

    rsx! {
        div { class: "diagram",
            div { class: "game-header", "mpsc" }
            div { class: "status", "{conn.status}" }

            ArrowLayer { senders: senders() }

            for node in nodes() {
                SenderNode { key: "{node.sender.conn}", node: node }
            }

            FlightLayer { flights: flights(), senders: senders() }

            ReceiverPanel {
                snapshot: chan.snap.read().clone(),
                connected: conn.connected(),
                onreceive: move |_| dispatch(conn, sim, chan, MpscWire::Receive),
            }

            for view in controls() {
                SenderControls {
                    key: "{view.conn}",
                    conn: view.conn,
                    blocked: view.blocked,
                    cy: view.cy,
                    draft: view.draft,
                    ondraft: move |ch| set_draft(drafts, view.conn, ch),
                    onsend: move |_| send_from(conn, sim, chan, drafts, view.conn),
                }
            }

            div { class: "game-footer",
                if chan.snap.read().waiting_receive {
                    span { class: "note-pill waiting", "receiver waiting…" }
                } else if blocked_count() > 0 {
                    span { class: "note-pill blocked", "{blocked_count} sender(s) blocked" }
                } else {
                    span {}
                }
                button {
                    class: "btn desktop-only",
                    onclick: move |_| {
                        if conn.is_local() {
                            let new = next_local_conn();
                            next_local_conn.set(new + 1);
                            dispatch_local(sim, chan, MpscWire::CloneSender { conn: new });
                        } else {
                            conn.send(&MpscWire::CloneSender { conn: 0 });
                        }
                    },
                    "+ clone sender"
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

#[derive(Clone, PartialEq)]
struct NodeView {
    sender: SenderInfo,
    label: String,
    top: f64,
    height: f64,
}

#[component]
fn SenderNode(node: NodeView) -> Element {
    let s = node.sender;
    let title = if s.owner == s.conn {
        "connected player"
    } else {
        "cloned sender"
    };
    rsx! {
        div {
            class: if s.blocked { "actor sender-node blocked" } else { "actor sender-node" },
            style: "top: {node.top}%; height: {node.height}%; --c: {palette::sender_hex(s.conn)}; --fg: {palette::sender_text_color(s.conn)};",
            title: title,
            "{node.label}"
        }
    }
}

#[derive(Clone, PartialEq)]
struct ControlsView {
    conn: u64,
    blocked: bool,
    cy: f64,
    draft: String,
}

#[component]
fn SenderControls(
    conn: u64,
    blocked: bool,
    cy: f64,
    draft: String,
    ondraft: EventHandler<Option<char>>,
    onsend: EventHandler,
) -> Element {
    rsx! {
        div {
            class: "sender-ctl",
            style: "top: {cy}%; right: {layout::CONTROLS_RIGHT}%;",
            input {
                class: "field",
                maxlength: 1,
                disabled: blocked,
                value: draft,
                oninput: move |e| ondraft.call(e.value().chars().next()),
            }
            button {
                class: "btn send",
                disabled: blocked,
                onclick: move |_| onsend.call(()),
                "send"
            }
        }
    }
}

#[component]
fn ArrowLayer(senders: Vec<SenderInfo>) -> Element {
    let markers = (0..palette::PALETTE_LEN)
        .map(|idx| {
            let c = colorous::TABLEAU10[idx];
            (idx, format!("#{:02x}{:02x}{:02x}", c.r, c.g, c.b))
        })
        .collect::<Vec<(usize, String)>>();
    rsx! {
        svg { class: "arrow-layer",
            defs {
                for (idx, fill) in markers {
                    marker {
                        key: "{idx}",
                        id: "arrowhead-{idx}",
                        "markerWidth": 4,
                        "markerHeight": 4,
                        "refX": 2.6,
                        "refY": 2,
                        orient: "auto",
                        path { d: "M0,0 L4,2 L0,4 Z", fill: fill }
                    }
                }
            }
            for s in &senders {
                line {
                    key: "{s.conn}",
                    x1: "{ARROW_X0}%",
                    y1: "{layout::sender_cy(&senders, s.conn)}%",
                    x2: "{ARROW_X1}%",
                    y2: "{RECEIVER_CY}%",
                    stroke: palette::sender_hex(s.conn),
                    "stroke-width": 3,
                    "marker-end": "url(#arrowhead-{palette::color_index(s.conn)})",
                }
            }
        }
    }
}

#[component]
fn FlightLayer(flights: Vec<Flight>, senders: Vec<SenderInfo>) -> Element {
    rsx! {
        for f in flights {
            span {
                key: "{f.key}",
                class: "flight",
                style: "--fy: {layout::sender_cy(&senders, f.conn)}%; color: {palette::sender_hex(f.conn)};",
                "{f.ch}"
            }
        }
    }
}

#[component]
fn ReceiverPanel(snapshot: MpscSnapshot, connected: bool, onreceive: EventHandler) -> Element {
    let count = snapshot.senders.len();
    let occupancy = snapshot.buffer.len() + snapshot.in_flight;
    rsx! {
        div { class: "receiver-group",
            div {
                class: if snapshot.waiting_receive { "actor receiver waiting" } else { "actor receiver" },
                title: "{count} connected sender(s)",
                span { class: "count", "{count}" }
                span { class: "count-caption", "senders" }
            }
            div { class: "buffer",
                for (i, owned) in snapshot.buffer.iter().enumerate() {
                    div {
                        key: "{owned.conn}-{owned.ch}-{i}",
                        class: "slot filled",
                        style: "color: {palette::sender_hex(owned.conn)};",
                        "{owned.ch}"
                    }
                }
                for i in snapshot.buffer.len()..MPSC_CAPACITY {
                    div { class: "slot", key: "empty-{i}" }
                }
            }
            div { class: "recv-row",
                button {
                    class: "btn rcv",
                    disabled: !connected,
                    onclick: move |_| onreceive.call(()),
                    "Receive"
                }
                span { class: "recv-label",
                    { match snapshot.last_received {
                        Some(owned) => rsx! {
                            span { style: "color: {palette::sender_hex(owned.conn)};", "→ {owned.ch}" }
                        },
                        None => rsx! { "→ –" },
                    } }
                }
                span { class: "note-pill occupancy", "buffer {occupancy} / {MPSC_CAPACITY}" }
            }
        }
    }
}
