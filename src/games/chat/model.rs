//! Budget Chat (Protohackers 3) with the lock taken out, as a board you step
//! through by hand.
//!
//! The original keeps the member list in an `Arc<Mutex<HashMap>>` that every
//! connection task locks, beside one `broadcast` channel all of them share.
//! Here the list has an owner instead: a room actor. Connections ask it to
//! join, speak and leave through its bounded `mpsc` inbox; it answers a
//! join over a `oneshot` and announces everything on the `broadcast`, of
//! which it holds the only sender. Nobody locks anything, because nobody
//! but the room can reach the names.
//!
//! A join is answered with the member list *and* a receiver the room
//! subscribes after announcing the join — so the newcomer does not hear its
//! own arrival, which the original arranged with `resubscribe()`.
//!
//! Every `click_*` is one iteration of one actor's loop and returns the
//! hops it made, for the view to animate. Channels are the `sim-channels`
//! cores.

use std::collections::{BTreeMap, VecDeque};

use sim_channels::broadcast::{BroadcastCore, TryRecvError};
use sim_channels::mpsc::{MpscCore, SendOffer, SendPoll};
use sim_channels::oneshot::OneshotCore;
use sim_channels::WaiterId;

/// Room in the room actor's inbox.
pub const INBOX_CAPACITY: usize = 3;
/// How many announcements the broadcast ring keeps (budget_chat: 256).
pub const BROADCAST_CAPACITY: usize = 4;
/// How many lines a client may type ahead of its connection reading them.
pub const TYPE_AHEAD: usize = 3;
/// How many received lines a client's terminal shows.
pub const TRANSCRIPT: usize = 4;

pub const WELCOME: &str = "Welcome to budgetchat! What shall I call you?";

/// The people at the keyboards, and what each will type: a name first,
/// then a few lines.
pub const USERS: [(&str, [&str; 3]); 4] = [
    ("alice", ["hi all", "seen dave?", "gotta go"]),
    ("bob", ["hey alice", "nope", "lol"]),
    ("carol", ["hello!", "what's up?", "brb"]),
    ("dave", ["sorry, lagging", "back", "bye"]),
];

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum RoomMsg {
    /// `name` wants in; the answer goes on `replies[conn]`.
    Join {
        conn: usize,
        name: String,
    },
    Say {
        conn: usize,
        text: String,
    },
    Leave {
        conn: usize,
    },
}

impl RoomMsg {
    pub fn conn(&self) -> usize {
        match self {
            RoomMsg::Join { conn, .. } | RoomMsg::Say { conn, .. } | RoomMsg::Leave { conn } => {
                *conn
            }
        }
    }

    pub fn label(&self) -> String {
        match self {
            RoomMsg::Join { name, .. } => format!("join {name}"),
            RoomMsg::Say { text, .. } => text.clone(),
            RoomMsg::Leave { .. } => "leave".to_string(),
        }
    }
}

/// The room's answer to a join: who else is here, and a receiver.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Welcome {
    pub others: Vec<String>,
    pub rx: usize,
}

/// What a parked connection goes on to do once its send goes through.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum After {
    /// Await the room's answer to its join.
    Join,
    /// Back to `select!`, holding this receiver.
    Chat(usize),
    /// Close, dropping this receiver.
    Leave(usize),
}

/// Where one connection task is. Its name is always its user's: the first
/// line its client types.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Conn {
    /// Waiting for the client to type a name.
    Naming,
    /// Sent its join; awaiting the room's oneshot.
    Joining,
    /// In the room: `select!` over the client and the broadcast.
    Chatting {
        rx: usize,
    },
    /// Parked sending into the full room inbox.
    Parked {
        waiter: WaiterId,
        then: After,
    },
    Closed,
}

impl Conn {
    /// Whether it has given its name and not yet closed.
    pub fn named(&self) -> bool {
        !matches!(self, Conn::Naming | Conn::Closed)
    }
}

/// One client: a terminal, the lines typed but not yet read by the server,
/// and what the server wrote back.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Client {
    /// Lines of the script typed so far: 0 is the name.
    pub typed: usize,
    pub socket: VecDeque<String>,
    pub hung_up: bool,
    pub transcript: VecDeque<String>,
}

impl Client {
    fn new() -> Self {
        Self {
            typed: 0,
            socket: VecDeque::new(),
            hung_up: false,
            transcript: VecDeque::from([WELCOME.to_string()]),
        }
    }

    fn hear(&mut self, line: String) {
        self.transcript.push_back(line);
        while self.transcript.len() > TRANSCRIPT {
            self.transcript.pop_front();
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Node {
    Client(usize),
    Conn(usize),
    Inbox,
    Room,
    Ring,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Cargo {
    /// A chat line or announcement, in the colour of who said it.
    Line(usize),
    /// Joins, leaves and the oneshot answer.
    Control,
    /// Something lost or refused.
    Lost,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Hop {
    pub from: Node,
    pub to: Node,
    pub label: String,
    pub cargo: Cargo,
    pub leg: u8,
}

/// What one click did: the hops it made and what to say about it.
pub type Step = crate::games::step::Step<Hop>;

fn hop(from: Node, to: Node, label: impl Into<String>, cargo: Cargo, leg: u8) -> Hop {
    Hop {
        from,
        to,
        label: label.into(),
        cargo,
        leg,
    }
}

/// A broadcast line, remembering who said it for its colour.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Announcement {
    pub from: usize,
    pub text: String,
}

pub struct Chat {
    pub clients: [Client; USERS.len()],
    conns: [Conn; USERS.len()],
    inbox: MpscCore<RoomMsg>,
    ring: BroadcastCore<Announcement>,
    /// The oneshot each joining connection awaits.
    replies: [Option<OneshotCore<Welcome>>; USERS.len()],
    /// The room's state: who is in, by connection. Owned, never shared.
    names: BTreeMap<usize, String>,
    /// Which `select!` branch a connection tries first next time.
    client_first: [bool; USERS.len()],
}

impl Default for Chat {
    fn default() -> Self {
        Self::new()
    }
}

impl Chat {
    pub fn new() -> Self {
        let mut inbox = MpscCore::new(INBOX_CAPACITY);
        for _ in 1..USERS.len() {
            inbox.add_sender();
        }
        Self {
            clients: std::array::from_fn(|_| Client::new()),
            conns: std::array::from_fn(|_| Conn::Naming),
            inbox,
            ring: BroadcastCore::new(BROADCAST_CAPACITY),
            replies: Default::default(),
            names: BTreeMap::new(),
            client_first: [true; USERS.len()],
        }
    }

    // ---- reading the board ----

    pub fn conn(&self, index: usize) -> &Conn {
        &self.conns[index]
    }

    pub fn inbox(&self) -> Vec<RoomMsg> {
        self.inbox.buffer().cloned().collect()
    }

    pub fn inbox_parked(&self) -> Vec<RoomMsg> {
        self.inbox.blocked().map(|(_, m)| m.clone()).collect()
    }

    /// The ring, oldest first, with each value's sequence number.
    pub fn ring(&self) -> Vec<(u64, Announcement)> {
        self.ring.ring().map(|(seq, a)| (seq, a.clone())).collect()
    }

    pub fn names(&self) -> Vec<String> {
        self.names.values().cloned().collect()
    }

    /// Announcements waiting for a connection, and whether some of them
    /// have already been overwritten.
    pub fn unread(&self, index: usize) -> Option<(u64, bool)> {
        let rx = self.rx_of(index)?;
        let cursor = self.ring.receiver_seq(rx)?;
        let oldest = self
            .ring
            .ring()
            .next()
            .map_or(self.ring.next_seq(), |(s, _)| s);
        Some((self.ring.next_seq() - cursor, cursor < oldest))
    }

    pub fn stuck(&self, index: usize) -> bool {
        match &self.conns[index] {
            Conn::Parked { waiter, .. } => self.inbox.blocked().any(|(w, _)| w == *waiter),
            _ => false,
        }
    }

    pub fn reply_ready(&self, index: usize) -> bool {
        self.replies[index].as_ref().is_some_and(|r| r.has_value())
    }

    fn rx_of(&self, index: usize) -> Option<usize> {
        match &self.conns[index] {
            Conn::Chatting { rx }
            | Conn::Parked {
                then: After::Chat(rx) | After::Leave(rx),
                ..
            } => Some(*rx),
            _ => None,
        }
    }

    // ---- clients ----

    /// The next line this client will type, if any.
    pub fn next_line(&self, index: usize) -> Option<&'static str> {
        let (name, lines) = USERS[index];
        let client = &self.clients[index];
        if client.hung_up {
            return None;
        }
        match client.typed {
            0 => Some(name),
            n => lines.get(n - 1).copied(),
        }
    }

    pub fn can_type(&self, index: usize) -> bool {
        self.next_line(index).is_some() && self.clients[index].socket.len() < TYPE_AHEAD
    }

    /// The client types its next line into the socket. Nothing reads it
    /// until its connection is stepped.
    pub fn click_type(&mut self, index: usize) -> Step {
        let Some(line) = self.next_line(index).filter(|_| self.can_type(index)) else {
            return Step::note("Nothing more to type.");
        };
        let client = &mut self.clients[index];
        client.typed += 1;
        client.socket.push_back(line.to_string());
        Step::note(format!(
            "{} types \u{201c}{line}\u{201d}. It waits in the socket until the connection reads it.",
            USERS[index].0
        ))
    }

    /// The client closes its terminal: the connection reads EOF next.
    pub fn click_hang_up(&mut self, index: usize) -> Step {
        self.clients[index].hung_up = true;
        Step::note(format!(
            "{} hangs up. Their connection sees EOF the next time it reads.",
            USERS[index].0
        ))
    }

    // ---- connections ----

    pub fn click_conn(&mut self, index: usize) -> Step {
        match self.conns[index] {
            Conn::Naming => self.read_name(index),
            Conn::Joining => self.read_welcome(index),
            Conn::Chatting { rx } => self.select(index, rx),
            Conn::Parked { waiter, then } => match self.inbox.poll_send(waiter) {
                SendPoll::Pending => Step::note(
                    "Still parked: the room's inbox is full, so this connection hears nothing either.",
                ),
                _ => {
                    self.then(index, then);
                    Step::note(match then {
                        After::Join => "The join is in: now it awaits the room's answer on the oneshot.",
                        After::Chat(_) => "The send returned: back in select!.",
                        After::Leave(_) => "The leave is in; the connection closes and drops its receiver.",
                    })
                }
            },
            Conn::Closed => Step::note("This connection has closed."),
        }
    }

    fn read_name(&mut self, index: usize) -> Step {
        let client = &mut self.clients[index];
        let Some(name) = client.socket.pop_front() else {
            if client.hung_up {
                self.conns[index] = Conn::Closed;
                return Step::note(
                    "EOF before a name: the connection just closes. The room never knew.",
                );
            }
            return Step::note("reader.next(): no name typed yet, so it would wait here.");
        };
        self.replies[index] = Some(OneshotCore::new());
        let mut step = self.send(
            index,
            RoomMsg::Join {
                conn: index,
                name: name.clone(),
            },
            After::Join,
        );
        step.note = format!(
            "The connection reads the name {name} and asks the room to join — no lock, a message. {}",
            step.note
        )
        .trim_end()
        .to_string();
        step
    }

    fn read_welcome(&mut self, index: usize) -> Step {
        let reply = self.replies[index].as_mut().and_then(|r| r.try_recv().ok());
        let Some(Welcome { others, rx }) = reply else {
            return Step::note("rx.await: the room has not answered the join yet.");
        };
        self.replies[index] = None;
        let line = if others.is_empty() {
            "* The room contains: nobody".to_string()
        } else {
            format!("* The room contains: {}", others.join(", "))
        };
        self.clients[index].hear(line.clone());
        self.conns[index] = Conn::Chatting { rx };
        Step {
            hops: vec![hop(Node::Conn(index), Node::Client(index), line, Cargo::Control, 0)],
            note: "The room's answer arrives: who else is here, and a receiver for everything said from now on.".to_string(),
        }
    }

    /// One `select!` over the client's socket and the broadcast. With both
    /// ready, the branches take turns: tokio picks at random.
    fn select(&mut self, index: usize, rx: usize) -> Step {
        let client_ready = !self.clients[index].socket.is_empty() || self.clients[index].hung_up;
        let ring_ready = self.unread(index).is_some_and(|(unread, _)| unread > 0);
        let take_client = match (client_ready, ring_ready) {
            (false, false) => {
                return Step::note(
                    "select!: nothing typed, nothing announced — it would wait on both.",
                )
            }
            (true, false) => true,
            (false, true) => false,
            (true, true) => self.client_first[index],
        };
        self.client_first[index] = !take_client;
        if take_client {
            self.take_from_client(index, rx)
        } else {
            self.take_from_ring(index, rx)
        }
    }

    fn take_from_client(&mut self, index: usize, rx: usize) -> Step {
        let (msg, then, said) = match self.clients[index].socket.pop_front() {
            // EOF: say goodbye through the room, then close
            None => (
                RoomMsg::Leave { conn: index },
                After::Leave(rx),
                "EOF: the connection tells the room it is leaving and closes, dropping its receiver.".to_string(),
            ),
            Some(text) => (
                RoomMsg::Say {
                    conn: index,
                    text: text.clone(),
                },
                After::Chat(rx),
                format!("The connection reads \u{201c}{text}\u{201d} and hands it to the room to announce."),
            ),
        };
        let mut step = self.send(index, msg, then);
        if step.note.is_empty() {
            step.note = said;
        }
        step
    }

    fn take_from_ring(&mut self, index: usize, rx: usize) -> Step {
        let name = USERS[index].0;
        match self.ring.try_recv(rx) {
            Ok(Announcement { from, text }) => {
                // the original's echo suppression: skip lines it said itself
                if text.starts_with(&format!("[{name}]")) {
                    return Step {
                        hops: vec![hop(Node::Ring, Node::Conn(index), text, Cargo::Line(from), 0)],
                        note: "Its own line comes back on the broadcast: recognised by the [name] prefix and not written.".to_string(),
                    };
                }
                self.clients[index].hear(text.clone());
                Step {
                    hops: vec![
                        hop(Node::Ring, Node::Conn(index), text.clone(), Cargo::Line(from), 0),
                        hop(Node::Conn(index), Node::Client(index), text, Cargo::Line(from), 1),
                    ],
                    note: format!("{name}'s connection takes the next announcement off the broadcast and writes it to the terminal."),
                }
            }
            Err(TryRecvError::Lagged(skipped)) => Step {
                hops: vec![hop(Node::Ring, Node::Conn(index), format!("Lagged({skipped})"), Cargo::Lost, 0)],
                note: format!(
                    "Lagged({skipped}): the ring moved on without {name}. `Ok(msg) = rx.recv()` does not match an error, so select! skips the branch — {} simply gone.",
                    if skipped == 1 { "that line is".to_string() } else { format!("those {skipped} lines are") }
                ),
            },
            Err(TryRecvError::Empty | TryRecvError::Closed) => {
                Step::note("Nothing on the broadcast after all.")
            }
        }
    }

    /// Send into the room's inbox, then go on to `then` — or park there,
    /// with `then` kept for when the send goes through. The note is empty
    /// unless it parked.
    fn send(&mut self, index: usize, msg: RoomMsg, then: After) -> Step {
        let label = msg.label();
        let cargo = match msg {
            RoomMsg::Say { .. } => Cargo::Line(index),
            _ => Cargo::Control,
        };
        // a line or a name was read off the socket first; a leave was not
        let mut hops = Vec::new();
        if !matches!(msg, RoomMsg::Leave { .. }) {
            hops.push(hop(
                Node::Client(index),
                Node::Conn(index),
                label.clone(),
                cargo,
                0,
            ));
        }
        let leg = hops.len() as u8;
        hops.push(hop(Node::Conn(index), Node::Inbox, label, cargo, leg));
        let note = match self.inbox.offer_send(msg) {
            SendOffer::Blocked { waiter } => {
                self.conns[index] = Conn::Parked { waiter, then };
                "The room's inbox is full: the connection parks, and while it waits it reads nothing — not even the broadcast."
            }
            SendOffer::Accepted | SendOffer::Rejected(_) => {
                self.then(index, then);
                ""
            }
        };
        Step {
            hops,
            note: note.to_string(),
        }
    }

    /// Carry on after a send went through.
    fn then(&mut self, index: usize, then: After) {
        self.conns[index] = match then {
            After::Join => Conn::Joining,
            After::Chat(rx) => Conn::Chatting { rx },
            After::Leave(rx) => {
                self.ring.drop_receiver(rx);
                Conn::Closed
            }
        };
    }

    // ---- the room ----

    /// The room takes one message from its inbox.
    pub fn click_room(&mut self) -> Step {
        let Ok(msg) = self.inbox.try_recv() else {
            return Step::note("The room's inbox is empty: it would wait in rx.recv().");
        };
        let conn = msg.conn();
        let mut hops = vec![hop(Node::Inbox, Node::Room, msg.label(), Cargo::Control, 0)];
        let note = match msg {
            RoomMsg::Join { name, .. } => {
                let others: Vec<String> = self.names.values().cloned().collect();
                self.names.insert(conn, name.clone());
                let line = format!("* {name} has joined the room");
                self.announce(conn, line.clone(), &mut hops);
                // subscribed after the announcement: the newcomer does not
                // hear itself arrive
                let rx = self.ring.subscribe();
                if let Some(reply) = self.replies[conn].as_mut() {
                    reply.send(Welcome { others, rx }).ok();
                }
                hops.push(hop(
                    Node::Room,
                    Node::Conn(conn),
                    "room list",
                    Cargo::Control,
                    1,
                ));
                format!("The room adds {name} to its names — its own map, no lock — announces the arrival, and answers on the oneshot.")
            }
            RoomMsg::Say { text, .. } => {
                let name = self.names.get(&conn).cloned().unwrap_or_default();
                self.announce(conn, format!("[{name}] {text}"), &mut hops);
                format!("The room announces [{name}] {text} to everyone subscribed.")
            }
            RoomMsg::Leave { .. } => {
                let name = self.names.remove(&conn).unwrap_or_default();
                self.announce(conn, format!("* {name} has left the room"), &mut hops);
                format!("{name} is gone from the room's names, and everyone hears it.")
            }
        };
        Step { hops, note }
    }

    fn announce(&mut self, from: usize, text: String, hops: &mut Vec<Hop>) {
        hops.push(hop(
            Node::Room,
            Node::Ring,
            text.clone(),
            Cargo::Line(from),
            1,
        ));
        // with nobody subscribed yet the value comes back; the room shrugs
        self.ring.send(Announcement { from, text }).ok();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const ALICE: usize = 0;
    const BOB: usize = 1;
    const CAROL: usize = 2;

    /// Type a name, and step the connection and room until it has joined.
    fn join(chat: &mut Chat, who: usize) {
        chat.click_type(who);
        chat.click_conn(who);
        chat.click_room();
        chat.click_conn(who);
        assert!(
            matches!(chat.conn(who), Conn::Chatting { .. }),
            "{who} joined"
        );
    }

    fn heard(chat: &Chat, who: usize) -> Vec<String> {
        chat.clients[who].transcript.iter().cloned().collect()
    }

    #[test]
    fn a_join_is_answered_with_who_else_is_here() {
        let mut chat = Chat::new();
        join(&mut chat, ALICE);
        join(&mut chat, BOB);
        assert_eq!(
            heard(&chat, BOB).last().unwrap(),
            "* The room contains: alice"
        );
        assert_eq!(chat.names(), ["alice", "bob"]);
    }

    #[test]
    fn the_newcomer_does_not_hear_its_own_arrival_but_others_do() {
        let mut chat = Chat::new();
        join(&mut chat, ALICE);
        join(&mut chat, BOB);
        assert_eq!(chat.unread(BOB), Some((0, false)));
        assert_eq!(chat.unread(ALICE), Some((1, false)));
        chat.click_conn(ALICE);
        assert_eq!(
            heard(&chat, ALICE).last().unwrap(),
            "* bob has joined the room"
        );
    }

    #[test]
    fn a_line_reaches_everyone_else_and_its_echo_is_suppressed() {
        let mut chat = Chat::new();
        join(&mut chat, ALICE);
        join(&mut chat, BOB);
        chat.click_conn(ALICE); // hears bob join
        chat.click_type(ALICE); // "hi all"
        chat.click_conn(ALICE);
        chat.click_room();
        chat.click_conn(BOB);
        assert_eq!(heard(&chat, BOB).last().unwrap(), "[alice] hi all");
        let before = heard(&chat, ALICE);
        let step = chat.click_conn(ALICE);
        assert!(step.note.contains("own line"));
        assert_eq!(heard(&chat, ALICE), before);
    }

    #[test]
    fn a_connection_that_is_not_stepped_falls_behind_and_loses_lines() {
        let mut chat = Chat::new();
        join(&mut chat, ALICE);
        join(&mut chat, BOB);
        // bob chats while alice's connection sits still
        for _ in 0..3 {
            chat.click_conn(BOB); // drains anything for bob, or reads a line
            chat.click_type(BOB);
            chat.click_conn(BOB);
            chat.click_room();
        }
        join(&mut chat, CAROL);
        let (unread, lagging) = chat.unread(ALICE).unwrap();
        assert!(unread > BROADCAST_CAPACITY as u64, "got {unread}");
        assert!(lagging);
        let step = chat.click_conn(ALICE);
        assert!(step.note.starts_with("Lagged("), "{}", step.note);
        assert_eq!(chat.unread(ALICE).map(|(_, lagging)| lagging), Some(false));
    }

    #[test]
    fn a_full_inbox_parks_a_connection_until_the_room_receives() {
        let mut chat = Chat::new();
        for who in 0..USERS.len() {
            chat.click_type(who);
            chat.click_conn(who);
        }
        assert!(chat.stuck(3), "the fourth join finds the inbox full");
        assert_eq!(chat.inbox().len(), INBOX_CAPACITY);
        chat.click_room();
        assert!(!chat.stuck(3));
        let step = chat.click_conn(3);
        assert!(step.note.contains("join is in"));
    }

    #[test]
    fn hanging_up_leaves_the_room() {
        let mut chat = Chat::new();
        join(&mut chat, ALICE);
        join(&mut chat, BOB);
        chat.click_hang_up(BOB);
        chat.click_conn(BOB);
        assert_eq!(chat.conn(BOB), &Conn::Closed);
        chat.click_room();
        assert_eq!(chat.names(), ["alice"]);
        chat.click_conn(ALICE); // bob joined
        chat.click_conn(ALICE); // bob left
        assert_eq!(
            heard(&chat, ALICE).last().unwrap(),
            "* bob has left the room"
        );
    }

    #[test]
    fn with_both_ready_select_takes_turns() {
        let mut chat = Chat::new();
        join(&mut chat, ALICE);
        join(&mut chat, BOB);
        chat.click_type(ALICE);
        chat.click_type(ALICE);
        // alice has a typed line and bob's join to hear
        let first = chat.click_conn(ALICE);
        let second = chat.click_conn(ALICE);
        assert_ne!(first.hops[0].from, second.hops[0].from, "one of each");
    }

    #[test]
    fn a_client_cannot_type_further_ahead_than_the_socket_holds() {
        let mut chat = Chat::new();
        for _ in 0..TYPE_AHEAD {
            chat.click_type(ALICE);
        }
        assert!(!chat.can_type(ALICE));
    }
}
