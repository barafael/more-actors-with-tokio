//! The budget chat minigame: a chat room with an owner instead of a lock.
//!
//! Protohackers 3 kept its member list in an `Arc<Mutex<HashMap>>`; here a
//! room actor owns it, and every connection reaches it by message. Four
//! clients type, four connection tasks each `select!` between their client
//! and the broadcast, and the room announces. Nothing moves on its own:
//! clicking an actor runs one iteration of its loop. Leave a connection
//! unclicked while the others talk and watch it fall off the end of the
//! ring.
//!
//! Single-player, in the talk as in the export.

mod model;

use dioxus::prelude::*;

use crate::games::board::{at_style, edge, use_stepper, ChannelBox, Chip, Flight, FlightLayer};
use crate::games::palette;
use model::{
    Announcement, Cargo, Chat, Conn, Hop, Node, RoomMsg, BROADCAST_CAPACITY, INBOX_CAPACITY, USERS,
};

const ROW_Y: [f64; 4] = [13.0, 37.0, 61.0, 85.0];
const CLIENT_X: f64 = 12.0;
const CONN_X: f64 = 37.0;
const INBOX: (f64, f64) = (58.0, 15.0);
const ROOM: (f64, f64) = (82.0, 17.0);
const RING: (f64, f64) = (72.0, 66.0);

fn position(node: Node) -> (f64, f64) {
    match node {
        Node::Client(i) => (CLIENT_X, ROW_Y[i]),
        Node::Conn(i) => (CONN_X, ROW_Y[i]),
        Node::Inbox => INBOX,
        Node::Room => ROOM,
        Node::Ring => RING,
    }
}

fn user_hex(user: usize) -> String {
    palette::sender_hex(user as u64 + 4)
}

fn flight(hop: &Hop) -> Flight {
    let (color, control) = match hop.cargo {
        Cargo::Line(user) => (user_hex(user), false),
        Cargo::Control => ("var(--color-blue)".to_string(), true),
        Cargo::Lost => ("var(--color-red)".to_string(), true),
    };
    Flight {
        from: position(hop.from),
        to: position(hop.to),
        label: hop.label.clone(),
        color,
        control,
        leg: hop.leg,
    }
}

const START: &str =
    "Click an actor to run one step of its loop. Start by typing a name into a client.";

#[component]
pub fn ChatGame() -> Element {
    let board = use_stepper(Chat::new, START, flight);
    let chat = board.state.read();

    rsx! {
        div { class: "diagram board-game chat-game",
            div { class: "game-header", "budget chat" }
            div { class: "status", "single-player · click an actor to step it" }

            div { class: "bd-board",
                svg { class: "arrow-layer bd-edges", view_box: "0 0 100 100", preserve_aspect_ratio: "none",
                    for i in 0..USERS.len() {
                        {edge(position(Node::Client(i)), position(Node::Conn(i)), false)}
                        {edge(position(Node::Conn(i)), INBOX, false)}
                        {edge(RING, position(Node::Conn(i)), true)}
                    }
                    {edge(INBOX, ROOM, false)}
                    {edge(ROOM, RING, false)}
                }

                for (i, (name, _)) in USERS.iter().enumerate() {
                    ClientNode {
                        key: "c{i}",
                        index: i,
                        name: name.to_string(),
                        transcript: chat.clients[i].transcript.iter().cloned().collect::<Vec<_>>(),
                        socket: chat.clients[i].socket.iter().cloned().collect::<Vec<_>>(),
                        next: chat.next_line(i).map(str::to_string),
                        can_type: chat.can_type(i),
                        hung_up: chat.clients[i].hung_up,
                        ontype: move |_| board.step(|c| c.click_type(i)),
                        onhangup: move |_| board.step(|c| c.click_hang_up(i)),
                    }
                }
                for i in 0..USERS.len() {
                    ConnNode {
                        key: "n{i}",
                        index: i,
                        state: *chat.conn(i),
                        stuck: chat.stuck(i),
                        reply_ready: chat.reply_ready(i),
                        unread: chat.unread(i),
                        onstep: move |_| board.step(|c| c.click_conn(i)),
                    }
                }

                ChannelBox {
                    at: INBOX,
                    title: "mpsc · the room's inbox",
                    capacity: INBOX_CAPACITY,
                    chips: chat.inbox().iter().map(msg_chip).collect::<Vec<_>>(),
                    parked: chat.inbox_parked().iter().map(msg_chip).collect::<Vec<_>>(),
                }

                button {
                    class: "bd-actor ch-room",
                    style: at_style(ROOM),
                    onclick: move |_| board.step(|c| c.click_room()),
                    div { class: "bd-head", "room" }
                    div { class: "bd-doing", "owns the names" }
                    div { class: "ch-names",
                        if chat.names().is_empty() {
                            span { class: "bd-hint", "nobody here yet" }
                        }
                        for name in chat.names() {
                            span { class: "ch-name", "{name}" }
                        }
                    }
                }

                Ring { values: chat.ring() }

                FlightLayer { flights: board.flights }
            }

            div { class: "bd-note",
                span { class: "note-pill", "{board.note}" }
            }

            div { class: "game-footer",
                span { class: "dim", "the original's Arc<Mutex<HashMap>> is now the room's own map · the room holds the only broadcast sender" }
                button { class: "btn", onclick: move |_| board.reset(Chat::new()), "reset" }
            }
        }
    }
}

fn msg_chip(msg: &RoomMsg) -> Chip {
    Chip {
        label: msg.label(),
        color: match msg {
            RoomMsg::Say { conn, .. } => user_hex(*conn),
            _ => "var(--color-blue)".to_string(),
        },
    }
}

/// The broadcast ring: the last few announcements, with their sequence
/// numbers, so a receiver's lag can be read off against them.
#[component]
fn Ring(values: Vec<(u64, Announcement)>) -> Element {
    rsx! {
        div { class: "bd-channel ch-ring", style: at_style(RING),
            span { class: "bd-channel-title", "broadcast · announcements ({BROADCAST_CAPACITY} kept)" }
            div { class: "ch-ring-slots",
                for slot in 0..BROADCAST_CAPACITY {
                    match values.get(slot) {
                        Some((seq, value)) => rsx! {
                            span { class: "bd-slot filled ch-ring-slot", style: "--c: {user_hex(value.from)};",
                                span { class: "ch-seq", "#{seq}" }
                                "{value.text}"
                            }
                        },
                        None => rsx! { span { class: "bd-slot ch-ring-slot" } },
                    }
                }
            }
        }
    }
}

#[allow(clippy::too_many_arguments)]
#[component]
fn ClientNode(
    index: usize,
    name: String,
    transcript: Vec<String>,
    socket: Vec<String>,
    next: Option<String>,
    can_type: bool,
    hung_up: bool,
    ontype: EventHandler<MouseEvent>,
    onhangup: EventHandler<MouseEvent>,
) -> Element {
    rsx! {
        div {
            class: if hung_up { "bd-actor ch-client gone" } else { "bd-actor ch-client" },
            style: "{at_style(position(Node::Client(index)))} --c: {user_hex(index)};",
            div { class: "bd-head", "{name}'s terminal" }
            div { class: "ch-transcript",
                for line in transcript.iter() {
                    div { class: "ch-line", "{line}" }
                }
            }
            div { class: "ch-typing",
                if let Some(line) = next {
                    button { class: "bd-chip", disabled: !can_type, onclick: move |e| ontype.call(e), "type \u{201c}{line}\u{201d}" }
                }
                if !hung_up {
                    button { class: "bd-chip ch-hangup", onclick: move |e| onhangup.call(e), "hang up" }
                }
            }
            if !socket.is_empty() {
                div { class: "bd-parked",
                    span { class: "bd-hint", "unread by the server" }
                    for line in socket.iter() {
                        span { class: "bd-slot parked", style: "--c: {user_hex(index)};", "{line}" }
                    }
                }
            }
        }
    }
}

#[component]
fn ConnNode(
    index: usize,
    state: Conn,
    stuck: bool,
    reply_ready: bool,
    unread: Option<(u64, bool)>,
    onstep: EventHandler<MouseEvent>,
) -> Element {
    let (class, doing) = match &state {
        Conn::Naming => ("", "reading a name".to_string()),
        Conn::Parked { .. } if stuck => (" parked", "parked: inbox full".to_string()),
        Conn::Parked { .. } => (" held", "send went through · click".to_string()),
        Conn::Joining if reply_ready => (" held", "answer ready".to_string()),
        Conn::Joining => ("", "awaiting the room".to_string()),
        Conn::Chatting { .. } => (" running", "in select!".to_string()),
        Conn::Closed => (" gone", "closed".to_string()),
    };
    let title = if state.named() {
        format!("connection · {}", USERS[index].0)
    } else {
        format!("connection #{}", index + 1)
    };
    rsx! {
        button {
            class: "bd-actor ch-conn{class}",
            style: at_style(position(Node::Conn(index))),
            onclick: move |e| onstep.call(e),
            div { class: "bd-head", "{title}" }
            div { class: "bd-doing", "{doing}" }
            if let Some((count, lagging)) = unread {
                span {
                    class: if lagging { "ch-unread lagging" } else { "ch-unread" },
                    if lagging { "{count} behind · lagging" } else { "{count} unread" }
                }
            }
        }
    }
}
