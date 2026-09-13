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

    pub fn apply(mut self, event: BroadcastEvent) -> Option<u64> {
        if let BroadcastEvent::InFlight { conn, ch } = event {
            let key = *self.key.read() + 1;
            self.key.set(key);
            self.flights.with_mut(|f| {
                f.push(Flight {
                    key,
                    from: conn,
                    ch,
                })
            });
            return Some(key);
        }
        if let BroadcastEvent::Snapshot { state } = event {
            self.snap.set(state);
        }
        // everything else is covered by the Snapshot that follows
        None
    }
}
