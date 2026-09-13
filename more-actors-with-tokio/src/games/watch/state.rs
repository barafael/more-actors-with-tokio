//! Client-side view of the watch channel. Authoritative state is the last
//! `Snapshot` — every other event is a cosmetic trigger for flights and never
//! mutates channel state, so the client cannot drift from the server.

use dioxus::prelude::*;

use crate::protocol::{WatchEvent, WatchSnapshot};

/// What a flight animates: a changed value fanning out from the cell, or a
/// gray borrow/subscribe transfer.
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum FlightKind {
    Value,
    Borrow,
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub struct Flight {
    pub key: u64,
    pub kind: FlightKind,
    pub rx: u64,
    pub ch: Option<char>,
}

#[derive(Clone, Copy)]
pub struct WatchState {
    pub snap: Signal<WatchSnapshot>,
    pub flights: Signal<Vec<Flight>>,
    /// Monotonic flight-key generator (per client; flights are cosmetic).
    pub key: Signal<u64>,
}

pub fn empty_snapshot() -> WatchSnapshot {
    WatchSnapshot {
        created: false,
        value: None,
        version: 0,
        pending_send: None,
        presenter: None,
        receivers: Vec::new(),
    }
}

impl WatchState {
    pub fn set_snapshot(mut self, snap: WatchSnapshot) {
        self.snap.set(snap);
    }

    pub fn next_key(mut self) -> u64 {
        let key = (self.key)() + 1;
        self.key.set(key);
        key
    }

    fn push_flight(mut self, kind: FlightKind, rx: u64, ch: Option<char>) {
        let key = self.next_key();
        self.flights
            .with_mut(|f| f.push(Flight { key, kind, rx, ch }));
    }

    pub fn remove_flight(mut self, key: u64) {
        self.flights.with_mut(|f| f.retain(|fl| fl.key != key));
    }

    pub fn apply(mut self, event: WatchEvent) {
        match event {
            WatchEvent::Changed { ch, .. } => {
                let targets: Vec<u64> = self.snap.read().receivers.iter().map(|r| r.id).collect();
                for rx in targets {
                    self.push_flight(FlightKind::Value, rx, Some(ch));
                }
            }
            // a fresh receiver instantly starts at the current value
            WatchEvent::ReceiverAdded { id, .. } => {
                let value = self.snap.read().value;
                self.push_flight(FlightKind::Borrow, id, value);
            }
            // a stale changed() completes at once: the value reaches that
            // one receiver without any send happening
            WatchEvent::ChangedImmediately { rx } => {
                let value = self.snap.read().value;
                self.push_flight(FlightKind::Value, rx, value);
            }
            WatchEvent::BorrowFlight { rx } => {
                let value = self.snap.read().value;
                self.push_flight(FlightKind::Borrow, rx, value);
            }
            WatchEvent::Snapshot { state } => self.snap.set(state),
            // everything else is covered by the Snapshot that follows
            WatchEvent::Hello { .. }
            | WatchEvent::Created
            | WatchEvent::SameValue
            | WatchEvent::SendRefused
            | WatchEvent::SendBlocked
            | WatchEvent::ReceiverRemoved { .. } => {}
        }
    }
}
