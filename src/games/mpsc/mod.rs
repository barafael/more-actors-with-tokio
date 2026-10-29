//! The mpsc minigame: every `Sender<T>` handle is a colored node, cloning
//! creates extra handles, values fly along the arrows, and a full buffer
//! parks (gray) senders until a receive frees a slot. Parked senders wake in
//! FIFO order, like tokio's.

mod layout;
mod model;

use dioxus::prelude::*;

use crate::games::ticker::{act_now, use_clock, use_tick};
use crate::games::{palette, Waited};
use layout::{ARROW_X0, ARROW_X1, RECEIVER_CY};
use model::{MpscSim, Sender, Value, CAPACITY, ORIGINAL};

fn fresh_sim() -> MpscSim {
    // The export renders one frame natively and never ticks, so a flight
    // there would hang mid-air in the snapshot; land them at once instead.
    MpscSim::new(if cfg!(target_arch = "wasm32") {
        model::FLIGHT_MS
    } else {
        0.0
    })
}

#[component]
pub fn MpscGame() -> Element {
    let mut sim = use_signal(fresh_sim);
    // While a send is parked its node counts the wait; the badge reads the
    // frame clock itself, so only it redraws. Flights land on the same clock.
    let clock = use_clock();
    use_tick(sim, clock);
    // what each handle's input box holds: view state, not channel state
    let mut drafts = use_signal(Vec::<(u64, char)>::new);

    let channel = sim.read();
    let senders = channel.senders();
    let handles: Vec<u64> = senders.iter().map(|s| s.handle).collect();
    let parked_sends = channel.parked_sends();
    let flights: Vec<(u64, Value)> = channel.flights().map(|f| (f.key, f.value)).collect();
    let buffer = channel.buffer();
    let occupancy = channel.occupancy();
    let waiting = channel.waiting_receive();
    let last = channel.last_received();
    let closed = channel.closed();
    drop(channel);
    let buffer_empty = buffer.is_empty() && flights.is_empty();
    let parked_count = senders.iter().filter(|s| s.parked_since.is_some()).count();

    let draft_of = move |handle: u64| {
        drafts
            .read()
            .iter()
            .find(|(h, _)| *h == handle)
            .map(|(_, ch)| *ch)
    };
    let mut set_draft = move |handle: u64, ch: Option<char>| {
        drafts.with_mut(|d| {
            d.retain(|(h, _)| *h != handle);
            if let Some(ch) = ch {
                d.push((handle, ch));
            }
        });
    };
    let mut send_from = move |handle: u64| {
        if let Some(ch) = draft_of(handle) {
            act_now(sim, |s| s.send(handle, ch));
            set_draft(handle, None);
        }
    };

    rsx! {
        div {
            class: "diagram mpsc-game",
            // where the receiver box sits, shared with the arrows' ends
            style: "--rx: {ARROW_X1}%; --ry: {RECEIVER_CY}%;",
            div { class: "game-header", "mpsc" }

            ArrowLayer { handles: handles.clone() }

            for (i, sender) in senders.iter().copied().enumerate() {
                SenderNode {
                    key: "{sender.handle}",
                    sender,
                    // a value in hand outranks the handle's name: while a
                    // send is parked or in flight, the node shows what it is
                    // carrying
                    label: parked_sends
                        .iter()
                        .chain(flights.iter().map(|(_, v)| v))
                        .find(|v| v.from == sender.handle)
                        .map(|v| v.ch.to_string())
                        .unwrap_or_else(|| format!("tx #{}", sender.handle)),
                    geometry: layout::sender_geometry(i, senders.len()),
                    clock,
                }
            }

            for (key, value) in flights.iter().copied() {
                span {
                    key: "{key}",
                    class: "flight",
                    style: "--fy: {layout::sender_cy(&handles, value.from)}%; color: {palette::sender_hex(value.from)};",
                    "{value.ch}"
                }
            }

            ReceiverPanel {
                senders: senders.len(),
                buffer,
                occupancy,
                waiting,
                last,
                onreceive: move |_| act_now(sim, |s| s.receive()),
            }

            for sender in senders.iter().copied() {
                div {
                    key: "{sender.handle}",
                    class: "sender-ctl",
                    style: "top: {layout::sender_cy(&handles, sender.handle)}%; right: {layout::CONTROLS_RIGHT}%;",
                    input {
                        class: "field",
                        maxlength: 1,
                        disabled: sender.parked_since.is_some(),
                        value: draft_of(sender.handle).map(String::from).unwrap_or_default(),
                        oninput: move |e| set_draft(sender.handle, e.value().chars().next()),
                        // typing here is not deck navigation
                        onkeydown: move |e| e.stop_propagation(),
                    }
                    button {
                        class: "btn send",
                        disabled: sender.parked_since.is_some(),
                        onclick: move |_| send_from(sender.handle),
                        "send"
                    }
                    button {
                        class: "btn small",
                        title: "drop(tx): a parked send is cancelled, and when the last sender goes the channel closes",
                        onclick: move |_| {
                            act_now(sim, |s| s.drop_sender(sender.handle));
                            set_draft(sender.handle, None);
                        },
                        "drop"
                    }
                }
            }

            div { class: "game-footer",
                if closed && buffer_empty {
                    span { class: "note-pill blocked", "every sender dropped — recv() yields None" }
                } else if closed {
                    span { class: "note-pill", "every sender dropped — the buffer still drains" }
                } else if waiting {
                    span { class: "note-pill waiting", "receiver waiting…" }
                } else if parked_count > 0 {
                    span { class: "note-pill blocked", "{parked_count} sender(s) parked" }
                } else {
                    span {}
                }
                // cloning needs a live handle to clone; once every sender
                // is dropped, the channel can never get one back
                button {
                    class: "btn",
                    disabled: closed,
                    onclick: move |_| {
                        sim.with_mut(|s| s.clone_sender(s.senders()[0].handle));
                    },
                    "+ clone sender"
                }
                button {
                    class: "btn",
                    onclick: move |_| {
                        sim.set(fresh_sim());
                        drafts.set(Vec::new());
                    },
                    "restart"
                }
            }
        }
    }
}

#[component]
fn SenderNode(sender: Sender, label: String, geometry: (f64, f64), clock: Signal<f64>) -> Element {
    let (top, height) = geometry;
    let handle = sender.handle;
    rsx! {
        div {
            class: if sender.parked_since.is_some() { "actor sender-node blocked" } else { "actor sender-node" },
            style: "top: {top}%; height: {height}%; --c: {palette::sender_hex(handle)}; --fg: {palette::sender_text_color(handle)};",
            title: if handle == ORIGINAL { "the sender mpsc::channel returned" } else { "a cloned sender" },
            "{label}"
            if let Some(since) = sender.parked_since {
                Waited { since, clock }
            }
        }
    }
}

#[component]
fn ArrowLayer(handles: Vec<u64>) -> Element {
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
                        // the tip, so the head ends on the receiver's edge
                        "refX": 4,
                        "refY": 2,
                        orient: "auto",
                        path { d: "M0,0 L4,2 L0,4 Z", fill: fill }
                    }
                }
            }
            for handle in handles.iter().copied() {
                line {
                    key: "{handle}",
                    x1: "{ARROW_X0}%",
                    y1: "{layout::sender_cy(&handles, handle)}%",
                    x2: "{ARROW_X1}%",
                    y2: "{RECEIVER_CY}%",
                    stroke: palette::sender_hex(handle),
                    "stroke-width": 3,
                    "marker-end": "url(#arrowhead-{palette::color_index(handle)})",
                }
            }
        }
    }
}

#[component]
fn ReceiverPanel(
    senders: usize,
    buffer: Vec<Value>,
    occupancy: usize,
    waiting: bool,
    last: Option<Value>,
    onreceive: EventHandler,
) -> Element {
    rsx! {
        div { class: "receiver-group",
            div {
                class: if waiting { "actor receiver waiting" } else { "actor receiver" },
                title: "{senders} live sender(s)",
                span { class: "count", "{senders}" }
                span { class: "count-caption", "senders" }
            }
            div { class: "buffer",
                for (i, value) in buffer.iter().enumerate() {
                    div {
                        key: "{value.from}-{value.ch}-{i}",
                        class: "slot filled",
                        style: "color: {palette::sender_hex(value.from)};",
                        "{value.ch}"
                    }
                }
                for i in buffer.len()..CAPACITY {
                    div { class: "slot", key: "empty-{i}" }
                }
            }
            div { class: "recv-row",
                button {
                    class: "btn rcv",
                    onclick: move |_| onreceive.call(()),
                    "Receive"
                }
                span { class: "recv-label",
                    match last {
                        Some(value) => rsx! {
                            span { style: "color: {palette::sender_hex(value.from)};", "→ {value.ch}" }
                        },
                        None => rsx! { "→ –" },
                    }
                }
                span { class: "note-pill occupancy", "buffer {occupancy} / {CAPACITY}" }
            }
        }
    }
}
