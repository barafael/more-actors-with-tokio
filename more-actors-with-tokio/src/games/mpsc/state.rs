//! Client-side view of the channel. Authoritative state is the last
//! `Snapshot` — every other event is a cosmetic trigger for animations and
//! never mutates channel state, so the client cannot drift from the server.

use dioxus::prelude::*;

use crate::protocol::{MpscEvent, MpscSnapshot};

/// A value on its way from a sender to the receiver (pure cosmetics).
#[derive(Clone, Copy, PartialEq)]
pub struct Flight {
    pub ch: char,
    pub conn: u64,
}

#[derive(Clone, Copy)]
pub struct ChannelState {
    pub snap: Signal<MpscSnapshot>,
    pub flights: Signal<Vec<Flight>>,
}

pub fn empty_snapshot() -> MpscSnapshot {
    MpscSnapshot {
        senders: Vec::new(),
        buffer: Vec::new(),
        blocked_sends: Vec::new(),
        in_flight: 0,
        waiting_receive: false,
        last_received: None,
    }
}

impl ChannelState {
    pub fn set_snapshot(mut self, snap: MpscSnapshot) {
        self.snap.set(snap);
    }

    pub fn apply(mut self, event: MpscEvent) {
        match event {
            MpscEvent::Snapshot { state } => self.snap.set(state),
            MpscEvent::InFlight { conn, ch } => {
                self.flights.with_mut(|f| f.push(Flight { ch, conn }));
            }
            MpscEvent::Consumed { conn, ch } => {
                self.flights
                    .with_mut(|f| f.retain(|fl| *fl != Flight { ch, conn }));
            }
            // Joins/leaves are covered by the Snapshot that follows them in
            // the same broadcast batch.
            MpscEvent::Hello { .. }
            | MpscEvent::SenderJoined { .. }
            | MpscEvent::SenderLeft { .. } => {}
        }
    }
}
