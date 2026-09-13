//! Client-side view of the broadcast channel. Authoritative state is the
//! last `Snapshot`; the only ephemeral extras are send flights (pure
//! cosmetics over the already-settled state).

use dioxus::prelude::*;

use crate::protocol::{BroadcastEvent, BroadcastSnapshot};

/// A send animation: a value flying from a sender into the buffer tail.
#[derive(Clone, Copy, PartialEq)]
pub struct Flight {
    pub key: u64,
    pub from: u64,
    pub ch: char,
}

#[derive(Clone, Copy)]
pub struct ChannelState {
    pub snap: Signal<BroadcastSnapshot>,
    pub flights: Signal<Vec<Flight>>,
    /// Monotonic flight-key generator (per client; flights are cosmetic).
    pub key: Signal<u64>,
}

pub fn empty_snapshot() -> BroadcastSnapshot {
    BroadcastSnapshot {
        senders: Vec::new(),
        buffer: Vec::new(),
        tail: 0,
        receivers: Vec::new(),
        presenter: None,
    }
}

impl ChannelState {
    pub fn set_snapshot(mut self, snap: BroadcastSnapshot) {
        self.snap.set(snap);
    }

    pub fn remove_flight(mut self, key: u64) {
        self.flights.with_mut(|f| f.retain(|fl| fl.key != key));
    }

    /// Returns the key of a flight this event started, if any.
    ///
    /// Matched exhaustively on purpose: a new event must be classified here
    /// rather than silently ignored by a catch-all.
    pub fn apply(mut self, event: BroadcastEvent) -> Option<u64> {
        match event {
            BroadcastEvent::InFlight { conn, ch } => {
                let key = *self.key.read() + 1;
                self.key.set(key);
                self.flights.with_mut(|f| {
                    f.push(Flight {
                        key,
                        from: conn,
                        ch,
                    })
                });
                Some(key)
            }
            BroadcastEvent::Snapshot { state } => {
                self.snap.set(state);
                None
            }
            // covered by the Snapshot that follows in the same batch
            BroadcastEvent::Hello { .. }
            | BroadcastEvent::SenderJoined { .. }
            | BroadcastEvent::SenderLeft { .. }
            | BroadcastEvent::ReceiverAdded { .. }
            | BroadcastEvent::ReceiverRemoved { .. }
            | BroadcastEvent::Evicted { .. }
            | BroadcastEvent::Received { .. }
            | BroadcastEvent::Lagged { .. } => None,
        }
    }
}
