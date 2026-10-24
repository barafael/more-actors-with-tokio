//! The broadcast minigame: clone senders, subscribe receivers, send without
//! blocking — and fall behind to meet `Lagged(n)`. One ring buffer sits
//! between the senders and the receivers; a full buffer evicts its oldest
//! value.

mod layout;
mod model;

use dioxus::prelude::*;

use crate::games::palette;
use crate::games::ticker::sleep_ms;
use layout::{BUFFER_LEFT, RECEIVERS_RIGHT, SENDERS_LEFT, SENDERS_WIDTH};
use model::{BroadcastSim, Receiver, RecvError, Value, CAPACITY, ORIGINAL};

#[component]
pub fn BroadcastGame() -> Element {
    let mut sim = use_signal(BroadcastSim::new);
    // what each handle's input box holds: view state, not channel state
    let mut drafts = use_signal(Vec::<(u64, char)>::new);
    // Rapid-fire values for the burst button, so digits stay unique within
    // a burst and across bursts until the counter wraps at 10^4.
    let mut burst_counter = use_signal(|| 0u32);

    let channel = sim.read();
    let senders = channel.senders().to_vec();
    let receivers = channel.receivers().to_vec();
    let buffer = channel.buffer();
    let tail = channel.tail();
    let flights = channel.flights().to_vec();
    let refused = channel.refused().is_some();
    let closed = channel.closed();
    let can_subscribe = channel.can_subscribe();
    drop(channel);

    // a refusal clears itself shortly after appearing
    let refused_key = use_memo(move || sim.read().refused());
    use_effect(move || {
        if let Some(key) = refused_key() {
            spawn(async move {
                sleep_ms(1600).await;
                sim.with_mut(|s| s.clear_refused(key));
            });
        }
    });

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

    rsx! {
        div { class: "diagram broadcast-game",
            div { class: "game-header", "broadcast" }

            if closed {
                div { class: "note-pill broadcast-closed",
                    "channel closed — every sender dropped. restart to play again."
                }
            }

            for (i, handle) in senders.iter().copied().enumerate() {
                SenderNode {
                    key: "{handle}",
                    handle,
                    geometry: layout::sender_geometry(i, senders.len()),
                    draft: draft_of(handle).map(String::from).unwrap_or_default(),
                    can_subscribe,
                    ondraft: move |ch| set_draft(handle, ch),
                    onsend: move |_| {
                        if let Some(ch) = draft_of(handle) {
                            sim.with_mut(|s| s.send(handle, ch));
                            set_draft(handle, None);
                        }
                    },
                    sim,
                }
            }

            BufferRow { buffer: buffer.clone(), tail }

            for (i, rx) in receivers.iter().copied().enumerate() {
                ReceiverBox {
                    key: "{rx.id}",
                    rx,
                    top: layout::receiver_cy(i, receivers.len()),
                    sim,
                }
            }

            LinkLayer { receivers: receivers.clone(), buffered: buffer.len(), tail }

            for flight in flights {
                span {
                    key: "{flight.key}",
                    class: "bcast-flight",
                    style: "--from-x: {SENDERS_LEFT + SENDERS_WIDTH / 2.0}%; --from-y: {layout::sender_cy(&senders, flight.value.from)}%; color: {palette::sender_hex(flight.value.from)};",
                    onanimationend: move |_| sim.with_mut(|s| s.land(flight.key)),
                    "{flight.value.ch}"
                }
            }

            div { class: "game-footer",
                span { class: "note-pill occupancy", "buffer {buffer.len()} / {CAPACITY}" }
                if refused {
                    span { class: "note-pill refused", "no receivers — send failed" }
                }
                if closed {
                    span { class: "note-pill blocked", "closed" }
                }
                button {
                    class: "btn",
                    disabled: closed,
                    title: "seven rapid sends from the first sender — overflows the ring, evicts the oldest values, and strands any receiver that has not kept up",
                    onclick: move |_| {
                        let start = burst_counter();
                        sim.with_mut(|s| {
                            let Some(&sender) = s.senders().first() else {
                                return;
                            };
                            for i in 0..7u32 {
                                let ch = char::from_digit((start + i) % 10, 10).unwrap();
                                s.send(sender, ch);
                            }
                        });
                        burst_counter.set((start + 7) % 10_000);
                    },
                    "burst ×7"
                }
                button {
                    class: "btn",
                    onclick: move |_| {
                        sim.set(BroadcastSim::new());
                        drafts.set(Vec::new());
                    },
                    "restart"
                }
            }
        }
    }
}

#[component]
fn SenderNode(
    handle: u64,
    geometry: (f64, f64),
    draft: String,
    can_subscribe: bool,
    ondraft: EventHandler<Option<char>>,
    onsend: EventHandler,
    sim: Signal<BroadcastSim>,
) -> Element {
    let (top, height) = geometry;
    let original = handle == ORIGINAL;
    rsx! {
        div {
            class: if original { "actor sender-node original" } else { "actor sender-node" },
            style: "top: {top}%; height: {height}%; left: {SENDERS_LEFT}%; width: {SENDERS_WIDTH}%;",
            title: if original { "the sender broadcast::channel returned" } else { "a cloned sender" },
            if original { "tx" } else { "tx #{handle}" }
        }
        div {
            class: "sender-btns",
            style: "top: {top + height + 1.0}%; left: {SENDERS_LEFT}%; width: {SENDERS_WIDTH}%;",
            button {
                class: "btn small",
                disabled: !can_subscribe,
                title: "subscribe another receiver via this sender — like tx.subscribe(), every click adds an independent receiver that starts at the tail and only sees values sent from now on",
                onclick: move |_| {
                    sim.with_mut(|s| s.subscribe());
                },
                "subscribe"
            }
            button {
                class: "btn small clone",
                title: "clone this sender",
                onclick: move |_| {
                    sim.with_mut(|s| s.clone_sender(handle));
                },
                "clone"
            }
            button {
                class: "btn small",
                title: "drop(tx): when the last sender goes, the channel closes — after its receivers drain what is buffered",
                onclick: move |_| sim.with_mut(|s| s.drop_sender(handle)),
                "drop"
            }
            input {
                class: "field",
                maxlength: 1,
                value: draft,
                oninput: move |e| ondraft.call(e.value().chars().next()),
            }
            button {
                class: "btn send",
                onclick: move |_| onsend.call(()),
                "send"
            }
        }
    }
}

/// Sequence number of the oldest retained value. `tail` counts every value
/// ever sent, so the ring holds seqs `oldest..tail`. Saturating: there can
/// never be more buffered values than were sent, but the arithmetic must
/// not underflow if there ever are.
fn oldest_seq(tail: u64, buffered: usize) -> u64 {
    tail.saturating_sub(buffered as u64)
}

#[component]
fn BufferRow(buffer: Vec<Value>, tail: u64) -> Element {
    rsx! {
        div { class: "buffer-label", "channel buffer" }
        for i in 0..CAPACITY {
            match buffer.get(i) {
                Some(value) => rsx! {
                    div {
                        key: "{oldest_seq(tail, buffer.len()) + i as u64}",
                        class: "actor buf-slot filled",
                        style: "left: {layout::slot_left(i)}%; width: {layout::BUFFER_SLOT_W}%; --c: {palette::sender_hex(value.from)};",
                        "{value.ch}"
                    }
                },
                None => rsx! {
                    div {
                        class: "actor buf-slot",
                        key: "empty-{i}",
                        style: "left: {layout::slot_left(i)}%; width: {layout::BUFFER_SLOT_W}%;",
                    }
                },
            }
        }
        div { class: "buffer-oldest", style: "left: {BUFFER_LEFT}%;", "oldest" }
        div {
            class: "buffer-newest",
            style: "left: {layout::slot_left(CAPACITY - 1)}%;",
            "newest"
        }
        div {
            class: "buffer-tail",
            style: "left: {layout::slot_left(tail.min(CAPACITY as u64) as usize)}%;",
            "next →"
        }
    }
}

#[component]
fn ReceiverBox(rx: Receiver, top: f64, sim: Signal<BroadcastSim>) -> Element {
    let id = rx.id;
    let closed = rx.error == Some(RecvError::Closed);
    // constant node shape: dynamic parts are text/classes only
    let (class, label) = if closed {
        ("actor rx-b-node closed", "closed")
    } else if rx.waiting {
        ("actor rx-b-node waiting", "…")
    } else {
        ("actor rx-b-node", "Rx")
    };
    let last_text = rx
        .last
        .map(|v| v.ch.to_string())
        .unwrap_or_else(|| "–".to_string());
    let last_color = rx
        .last
        .map(|v| palette::sender_hex(v.from))
        .unwrap_or_else(|| "#999".to_string());
    let (badge, badge_class) = match rx.error {
        Some(RecvError::Lagged(n)) => (format!("lagged {n}"), "lag-badge"),
        Some(RecvError::Closed) => ("closed".to_string(), "lag-badge closed"),
        None if rx.lagged_total > 0 => (format!("missed {}", rx.lagged_total), "lag-total"),
        None => (String::new(), "lag-total"),
    };
    rsx! {
        div {
            class,
            style: "top: {top}%; right: {RECEIVERS_RIGHT}%;",
            div { class: "rx-name", {label} }
            div { class: "rx-last", style: "color: {last_color};", {last_text} }
            div { class: badge_class, {badge} }
            div { class: "rx-next", "next #{rx.next}" }
            div { class: "rx-btns",
                button {
                    class: "btn small rcv",
                    disabled: closed,
                    title: if rx.waiting { "recv() — waiting, completes on the next send" } else { "recv()" },
                    onclick: move |_| {
                        sim.with_mut(|s| s.recv(id));
                    },
                    "recv"
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

/// Dashed index lines: each receiver ties to the buffer slot its `next`
/// points at; red when it has fallen behind the oldest value.
#[component]
fn LinkLayer(receivers: Vec<Receiver>, buffered: usize, tail: u64) -> Element {
    let oldest = oldest_seq(tail, buffered);
    let lines = receivers
        .iter()
        .enumerate()
        .map(|(i, rx)| {
            let y = layout::receiver_cy(i, receivers.len());
            let x = RECEIVERS_RIGHT + layout::RECEIVERS_WIDTH;
            if buffered == 0 {
                return (y, x, layout::BUFFER_LEFT, layout::BUFFER_CY, false);
            }
            // `next` is 1-based (the seq this receiver will read next), the
            // ring is indexed from `oldest`. A receiver that subscribed at
            // the tail points one past the newest value and has nothing to
            // read yet, so its line rests on the newest slot.
            let cursor = rx.next.saturating_sub(1);
            let behind = cursor < oldest;
            let slot = (cursor.saturating_sub(oldest) as usize).min(buffered - 1);
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
