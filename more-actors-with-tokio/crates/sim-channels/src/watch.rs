//! A blocking watch channel: a single shared value, one [`Sender`], many
//! [`Receiver`]s that observe changes through a version counter.
//!
//! Mirrors `tokio::sync::watch`: [`watch::channel`] starts from an initial
//! value, [`Sender::send`] replaces the value and marks it changed, and
//! [`Receiver::changed`] waits for the next change — completing instantly
//! when the receiver has already fallen behind. Sending fails with
//! [`SendError`] once no receivers remain, and `send_if_modified` dedups
//! (tokio's plain `send` always marks changed, equal value or not).
//!
//! The teaching beat kept from the deck: [`Receiver::borrow`] hands out a
//! read guard, and while any guard is alive a `send` parks behind it and
//! commits when the last guard drops — at core level this is the FIFO
//! `queue` of parked sends. Concurrent parked sends commit in queue order,
//! like tokio's write lock would order them.
//!
//! ```
//! use sim_channels::watch;
//!
//! let (tx, mut rx) = watch::channel('a');
//! assert!(!rx.has_changed());
//! tx.send('b').expect("receiver alive");
//! assert!(rx.has_changed());
//! assert_eq!(*rx.borrow_and_update(), 'b');
//! assert!(!rx.has_changed());
//! ```
//!
//! The [`WatchCore`] state machine underneath is a plain value that game
//! drivers and tests can step through without threads.

use std::collections::VecDeque;
#[cfg(not(target_arch = "wasm32"))]
use std::ops::Deref;
use std::sync::Arc;

#[cfg(not(target_arch = "wasm32"))]
use std::sync::MutexGuard;

use crate::waiter::{Lock, WaiterId, WaiterIds};

// ---- core ----

/// One receiver slot inside the core. Ids are 1-based so that the slot can
/// be found at index `id - 1`; freed ids are recycled.
#[derive(Debug)]
struct Rx {
    version: u64,
    awaiting: bool,
    waiter: Option<WaiterId>,
    alive: bool,
}

/// A send parked behind outstanding borrow guards.
#[derive(Debug)]
struct Pending<T> {
    waiter: WaiterId,
    kind: PendingKind,
    value: Option<T>,
    old: Option<T>,
    state: PendingState,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum PendingKind {
    /// Fails when all receivers vanish meanwhile.
    Send,
    /// [`WatchCore::send_replace`]: never fails, also records the old value.
    Replace,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum PendingState {
    /// Waiting for the last guard to drop.
    Queued,
    /// Committed; the sender may proceed.
    Committed,
    /// All receivers vanished meanwhile; the value goes back.
    Failed,
}

/// The pure state machine behind a watch channel.
#[derive(Debug)]
pub struct WatchCore<T> {
    value: T,
    version: u64,
    readers: usize,
    queue: VecDeque<Pending<T>>,
    receivers: Vec<Rx>,
    live_receivers: usize,
    sender_alive: bool,
    waiters: WaiterIds,
}

/// Outcome of [`WatchCore::send`] — the modeling of one blocking send.
#[derive(Debug, PartialEq, Eq)]
pub enum SendOffer<T> {
    /// The value was committed immediately.
    Sent,
    /// Borrow guards are out; the value is parked with this identity.
    Blocked { waiter: WaiterId },
    /// No receivers remain; the value comes back.
    Rejected(T),
}

/// Re-check of a parked send, via [`WatchCore::poll_send`].
#[derive(Debug, PartialEq, Eq)]
pub enum SendPoll<T> {
    /// The value was committed; the sender may proceed.
    Sent,
    /// Still parked behind a borrow guard.
    Pending,
    /// All receivers vanished; the value comes back.
    Failed(T),
    /// The parked send was cancelled (core drivers only).
    Cancelled,
}

/// Outcome of [`WatchCore::send_replace`].
#[derive(Debug, PartialEq, Eq)]
pub enum ReplaceOffer<T> {
    /// The new value was committed immediately; the old one is returned.
    Replaced(T),
    /// The new value is parked behind borrow guards.
    Queued { waiter: WaiterId },
}

/// Re-check of a parked replace, via [`WatchCore::poll_replace`].
#[derive(Debug, PartialEq, Eq)]
pub enum ReplacePoll<T> {
    /// The value was committed; the previously visible value is returned.
    Replaced(T),
    /// Still parked behind a borrow guard.
    Pending,
    /// The parked replace was cancelled (core drivers only).
    Cancelled,
}

/// Outcome of [`WatchCore::end_borrow`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BorrowRelease {
    /// More guards are out, or nothing was parked.
    Released,
    /// Parked sends committed as the guards went away.
    Resolved,
    /// At least one parked send failed: all receivers vanished meanwhile.
    Refused,
}

/// Outcome of the core `changed` polls — the predicate behind a blocking
/// `changed`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ChangePoll {
    /// The receiver was behind and has caught up.
    Immediate,
    /// Up to date; keep waiting with this identity.
    Blocked { waiter: WaiterId },
    /// The sender is gone and nothing new will arrive.
    Closed,
}

impl<T> WatchCore<T> {
    /// A channel holding `initial` at version 0, with one receiver already
    /// subscribed (tokio's `watch::channel` returns a receiver too).
    pub fn new(initial: T) -> Self {
        let mut core = Self {
            value: initial,
            version: 0,
            readers: 0,
            queue: VecDeque::new(),
            receivers: Vec::new(),
            live_receivers: 0,
            sender_alive: true,
            waiters: WaiterIds::default(),
        };
        core.subscribe();
        core
    }

    /// Send a replacement value.
    pub fn send(&mut self, value: T) -> SendOffer<T> {
        if self.live_receivers == 0 {
            return SendOffer::Rejected(value);
        }
        if self.readers == 0 && self.queue.is_empty() {
            let _ = self.swap(value);
            return SendOffer::Sent;
        }
        let waiter = self.waiters.alloc();
        self.queue.push_back(Pending {
            waiter,
            kind: PendingKind::Send,
            value: Some(value),
            old: None,
            state: PendingState::Queued,
        });
        SendOffer::Blocked { waiter }
    }

    /// Replace the value; never fails, even with zero receivers.
    pub fn send_replace(&mut self, value: T) -> ReplaceOffer<T> {
        if self.readers == 0 && self.queue.is_empty() {
            let old = self.swap(value);
            return ReplaceOffer::Replaced(old);
        }
        let waiter = self.waiters.alloc();
        self.queue.push_back(Pending {
            waiter,
            kind: PendingKind::Replace,
            value: Some(value),
            old: None,
            state: PendingState::Queued,
        });
        ReplaceOffer::Queued { waiter }
    }

    /// Re-check a parked send, resolving it once the guards are gone.
    pub fn poll_send(&mut self, waiter: WaiterId) -> SendPoll<T> {
        let Some(idx) = self.position(waiter) else {
            return SendPoll::Cancelled;
        };
        if self.queue[idx].state == PendingState::Queued {
            self.drain_if_clear();
        }
        match self.queue[idx].state {
            PendingState::Queued => SendPoll::Pending,
            PendingState::Committed => {
                self.queue.remove(idx);
                SendPoll::Sent
            }
            PendingState::Failed => {
                let pending = self.queue.remove(idx).expect("entry exists");
                SendPoll::Failed(pending.value.expect("failed send keeps its value"))
            }
        }
    }

    /// Re-check a parked replace.
    pub fn poll_replace(&mut self, waiter: WaiterId) -> ReplacePoll<T> {
        let Some(idx) = self.position(waiter) else {
            return ReplacePoll::Cancelled;
        };
        if self.queue[idx].state == PendingState::Queued {
            self.drain_if_clear();
        }
        match self.queue[idx].state {
            PendingState::Queued => ReplacePoll::Pending,
            PendingState::Committed => {
                let pending = self.queue.remove(idx).expect("entry exists");
                ReplacePoll::Replaced(pending.old.expect("replace records the old value"))
            }
            PendingState::Failed => unreachable!("replace never fails"),
        }
    }

    /// Cancel a parked send or replace, handing its value back. Core-driver
    /// modeling for "the presenter went away while blocked".
    pub fn cancel_send(&mut self, waiter: WaiterId) -> Option<T> {
        let idx = self.position(waiter)?;
        self.queue.remove(idx).and_then(|pending| pending.value)
    }

    /// Apply a modification when no guards are out and nothing is parked;
    /// otherwise hand the closure back. `Ok(true)` marks the value changed.
    pub fn try_modify<F: FnOnce(&mut T) -> bool>(&mut self, f: F) -> Result<bool, F> {
        if self.readers > 0 || !self.queue.is_empty() {
            return Err(f);
        }
        let changed = f(&mut self.value);
        if changed {
            self.version += 1;
            self.catch_up_awaiting();
        }
        Ok(changed)
    }

    /// Mark the value changed without modifying it.
    pub fn mark_changed(&mut self) {
        self.version += 1;
        self.catch_up_awaiting();
    }

    /// A borrow guard was taken.
    pub fn begin_borrow(&mut self) {
        self.readers += 1;
    }

    /// A borrow guard was dropped. Parked sends commit in queue order as
    /// the last guard goes away.
    pub fn end_borrow(&mut self) -> BorrowRelease {
        self.readers = self.readers.saturating_sub(1);
        if self.readers > 0 || self.queue.is_empty() {
            return BorrowRelease::Released;
        }
        self.drain_if_clear();
        if self
            .queue
            .iter()
            .any(|pending| pending.state == PendingState::Failed)
        {
            BorrowRelease::Refused
        } else {
            BorrowRelease::Resolved
        }
    }

    /// The last [`Sender`] handle was dropped.
    pub fn drop_sender(&mut self) {
        self.sender_alive = false;
    }

    // ---- internals ----

    fn position(&self, waiter: WaiterId) -> Option<usize> {
        self.queue
            .iter()
            .position(|pending| pending.waiter == waiter)
    }

    fn swap(&mut self, value: T) -> T {
        let old = std::mem::replace(&mut self.value, value);
        self.version += 1;
        self.catch_up_awaiting();
        old
    }

    /// Wake every receiver parked in `changed`.
    fn catch_up_awaiting(&mut self) {
        let current = self.version;
        for rx in &mut self.receivers {
            if rx.awaiting {
                rx.awaiting = false;
                rx.version = current;
                rx.waiter = None;
            }
        }
    }

    /// Commit parked sends in queue order. Only meaningful when the last
    /// guard is gone.
    fn drain_if_clear(&mut self) {
        if self.readers > 0 || self.queue.is_empty() {
            return;
        }
        // phase 1: decide and take the queued values, in queue order
        let mut commits: Vec<(usize, T)> = Vec::new();
        for (idx, pending) in self.queue.iter_mut().enumerate() {
            if pending.state != PendingState::Queued {
                continue;
            }
            if pending.kind == PendingKind::Send && self.live_receivers == 0 {
                pending.state = PendingState::Failed;
                continue;
            }
            let value = pending.value.take().expect("queued op keeps its value");
            commits.push((idx, value));
        }
        // phase 2: commit in queue order
        for (idx, value) in commits {
            let old = self.swap(value);
            let pending = self.queue.get_mut(idx).expect("entry still exists");
            pending.old = Some(old);
            pending.state = PendingState::Committed;
        }
    }

    // ---- receivers ----

    /// Subscribe a receiver at the current version.
    pub fn subscribe(&mut self) -> u64 {
        self.add_rx(self.version)
    }

    /// Clone a receiver: the clone inherits the source's seen-version but
    /// starts fresh (not awaiting).
    pub fn clone_receiver(&mut self, source: u64) -> Option<u64> {
        let version = self.rx(source)?.version;
        Some(self.add_rx(version))
    }

    fn add_rx(&mut self, version: u64) -> u64 {
        let slot = Rx {
            version,
            awaiting: false,
            waiter: None,
            alive: true,
        };
        if let Some(idx) = self.receivers.iter().position(|rx| !rx.alive) {
            self.receivers[idx] = slot;
            self.live_receivers += 1;
            return idx as u64 + 1;
        }
        self.receivers.push(slot);
        self.live_receivers += 1;
        self.receivers.len() as u64
    }

    fn rx(&self, id: u64) -> Option<&Rx> {
        self.receivers
            .get(id.checked_sub(1)? as usize)
            .filter(|rx| rx.alive)
    }

    fn rx_mut(&mut self, id: u64) -> Option<&mut Rx> {
        self.receivers
            .get_mut(id.checked_sub(1)? as usize)
            .filter(|rx| rx.alive)
    }

    /// Drop a receiver slot.
    pub fn drop_receiver(&mut self, id: u64) {
        if let Some(rx) = self.rx_mut(id) {
            rx.alive = false;
            rx.awaiting = false;
            rx.waiter = None;
            self.live_receivers -= 1;
        }
    }

    /// Poll a receiver's `changed`.
    pub fn poll_changed(&mut self, id: u64) -> ChangePoll {
        self.poll_changed_inner(id, None)
    }

    /// Re-check a waiting receiver's `changed`.
    pub fn poll_changed_wait(&mut self, id: u64, waiter: WaiterId) -> ChangePoll {
        self.poll_changed_inner(id, Some(waiter))
    }

    fn poll_changed_inner(&mut self, id: u64, waiter: Option<WaiterId>) -> ChangePoll {
        let (seen, current) = {
            let Some(rx) = self.rx(id) else {
                return ChangePoll::Closed;
            };
            (rx.version, self.version)
        };
        if seen < current {
            let rx = self.rx_mut(id).expect("receiver checked live above");
            rx.version = current;
            rx.awaiting = false;
            rx.waiter = None;
            return ChangePoll::Immediate;
        }
        if let Some(waiter) = waiter {
            // recheck: a commit catching this receiver up clears `awaiting`
            let still_parked = self
                .rx(id)
                .is_some_and(|rx| rx.awaiting && rx.waiter == Some(waiter));
            if !still_parked {
                return ChangePoll::Immediate;
            }
            if !self.sender_alive {
                return ChangePoll::Closed;
            }
            return ChangePoll::Blocked { waiter };
        }
        if !self.sender_alive {
            return ChangePoll::Closed;
        }
        let parked = self.rx(id).and_then(|rx| rx.waiter);
        let waiter = parked.unwrap_or_else(|| self.waiters.alloc());
        let rx = self.rx_mut(id).expect("receiver checked live above");
        rx.awaiting = true;
        rx.waiter = Some(waiter);
        ChangePoll::Blocked { waiter }
    }

    /// Cancel an outstanding `changed` (the minigame's toggle-off).
    pub fn cancel_changed(&mut self, id: u64) {
        if let Some(rx) = self.rx_mut(id) {
            rx.awaiting = false;
            rx.waiter = None;
        }
    }

    /// Reset the receiver's change flag: it counts as up to date.
    pub fn mark_unchanged(&mut self, id: u64) {
        let current = self.version;
        if let Some(rx) = self.rx_mut(id) {
            rx.version = current;
            rx.awaiting = false;
            rx.waiter = None;
        }
    }

    /// Mark the current version as seen (tokio's `borrow_and_update`).
    pub fn mark_seen(&mut self, id: u64) {
        let current = self.version;
        if let Some(rx) = self.rx_mut(id) {
            rx.version = current;
        }
    }

    // ---- introspection ----

    /// The current value.
    pub fn value(&self) -> &T {
        &self.value
    }

    /// The current version.
    pub fn version(&self) -> u64 {
        self.version
    }

    /// Number of borrow guards currently out.
    pub fn readers(&self) -> usize {
        self.readers
    }

    /// Live receiver ids, ascending. Ids are 1-based and are recycled once a
    /// receiver is dropped, so callers holding an id across a drop must
    /// re-check it with [`WatchCore::contains`].
    pub fn receiver_ids(&self) -> impl Iterator<Item = u64> + '_ {
        self.receivers
            .iter()
            .enumerate()
            .filter(|(_, rx)| rx.alive)
            .map(|(index, _)| index as u64 + 1)
    }

    /// Values of the parked sends, in FIFO commit order.
    pub fn pending_values(&self) -> impl Iterator<Item = &T> {
        self.queue.iter().filter_map(|pending| pending.value.as_ref())
    }

    /// Parked sends in queue order — the FIFO commit order.
    pub fn pending_waiters(&self) -> Vec<WaiterId> {
        self.queue.iter().map(|pending| pending.waiter).collect()
    }

    /// Whether the value differs from what the receiver last saw.
    pub fn has_changed(&self, id: u64) -> bool {
        self.rx(id).is_some_and(|rx| rx.version < self.version)
    }

    /// Whether the receiver is parked in `changed`.
    pub fn is_awaiting(&self, id: u64) -> bool {
        self.rx(id).is_some_and(|rx| rx.awaiting)
    }

    /// The receiver's seen-version, if it is alive.
    pub fn receiver_version(&self, id: u64) -> Option<u64> {
        Some(self.rx(id)?.version)
    }

    /// Number of live receiver handles.
    pub fn receiver_count(&self) -> usize {
        self.live_receivers
    }

    /// True if the receiver slot is live.
    pub fn contains(&self, id: u64) -> bool {
        self.rx(id).is_some()
    }
}

// ---- errors ----

/// The send failed because no receivers remain; the value comes back.
#[derive(Debug, PartialEq, Eq)]
pub struct SendError<T>(pub T);

/// [`Receiver::changed`] failed: the sender is gone.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RecvError;

// ---- handles ----

struct Shared<T> {
    lock: Lock<WatchCore<T>>,
}

/// The sending half. Not cloneable: one writer, like tokio.
pub struct Sender<T> {
    shared: Arc<Shared<T>>,
}

/// The receiving half. Cheap to clone; the clone has seen the current
/// value.
pub struct Receiver<T> {
    shared: Arc<Shared<T>>,
    rx: u64,
}

impl<T> Clone for Receiver<T> {
    fn clone(&self) -> Self {
        let mut guard = self.shared.lock.lock();
        let rx = guard
            .clone_receiver(self.rx)
            .expect("cloning a live receiver");
        drop(guard);
        Receiver {
            shared: self.shared.clone(),
            rx,
        }
    }
}

impl<T> Drop for Receiver<T> {
    fn drop(&mut self) {
        self.shared.lock.lock().drop_receiver(self.rx);
    }
}

impl<T> Drop for Sender<T> {
    fn drop(&mut self) {
        let mut guard = self.shared.lock.lock();
        guard.drop_sender();
        drop(guard);
        self.shared.lock.notify_all();
    }
}

/// Create a watch channel from an initial value.
pub fn channel<T>(initial: T) -> (Sender<T>, Receiver<T>) {
    let shared = Arc::new(Shared {
        lock: Lock::new(WatchCore::new(initial)),
    });
    (
        Sender {
            shared: shared.clone(),
        },
        Receiver { shared, rx: 1 },
    )
}

impl<T> Sender<T> {
    /// Replace the value, blocking while borrow guards are out. Fails if no
    /// receivers remain.
    #[cfg(not(target_arch = "wasm32"))]
    pub fn send(&self, value: T) -> Result<(), SendError<T>> {
        let mut guard = self.shared.lock.lock();
        let waiter = match guard.send(value) {
            SendOffer::Sent => {
                drop(guard);
                self.shared.lock.notify_all();
                return Ok(());
            }
            SendOffer::Rejected(value) => return Err(SendError(value)),
            SendOffer::Blocked { waiter } => waiter,
        };
        loop {
            match guard.poll_send(waiter) {
                SendPoll::Sent => {
                    drop(guard);
                    self.shared.lock.notify_all();
                    return Ok(());
                }
                SendPoll::Pending => {}
                SendPoll::Failed(value) => return Err(SendError(value)),
                SendPoll::Cancelled => unreachable!("a live handle's send is never cancelled"),
            }
            guard = self.shared.lock.wait(guard);
        }
    }

    /// Replace the value and return the old one; never fails. Blocks while
    /// borrow guards are out.
    #[cfg(not(target_arch = "wasm32"))]
    pub fn send_replace(&self, value: T) -> T {
        let mut guard = self.shared.lock.lock();
        let waiter = match guard.send_replace(value) {
            ReplaceOffer::Replaced(old) => {
                drop(guard);
                self.shared.lock.notify_all();
                return old;
            }
            ReplaceOffer::Queued { waiter } => waiter,
        };
        loop {
            match guard.poll_replace(waiter) {
                ReplacePoll::Replaced(old) => {
                    drop(guard);
                    self.shared.lock.notify_all();
                    return old;
                }
                ReplacePoll::Pending => {}
                ReplacePoll::Cancelled => {
                    unreachable!("a live handle's replace is never cancelled")
                }
            }
            guard = self.shared.lock.wait(guard);
        }
    }

    /// Modify the value, blocking while borrow guards are out. Always marks
    /// the value changed, even with zero receivers.
    #[cfg(not(target_arch = "wasm32"))]
    pub fn send_modify(&self, f: impl FnOnce(&mut T)) {
        self.modify(move |value| {
            f(value);
            true
        });
    }

    /// Modify the value, marking it changed only when the closure returns
    /// true. Blocks while borrow guards are out.
    #[cfg(not(target_arch = "wasm32"))]
    pub fn send_if_modified(&self, f: impl FnOnce(&mut T) -> bool) -> bool {
        self.modify(f)
    }

    #[cfg(not(target_arch = "wasm32"))]
    fn modify<F: FnOnce(&mut T) -> bool>(&self, mut f: F) -> bool {
        let mut guard = self.shared.lock.lock();
        loop {
            match guard.try_modify(f) {
                Ok(changed) => {
                    drop(guard);
                    self.shared.lock.notify_all();
                    return changed;
                }
                Err(back) => f = back,
            }
            guard = self.shared.lock.wait(guard);
        }
    }

    /// Mark the value changed without modifying it.
    pub fn mark_changed(&self) {
        let mut guard = self.shared.lock.lock();
        guard.mark_changed();
        drop(guard);
        self.shared.lock.notify_all();
    }

    /// Subscribe a new receiver at the current version.
    pub fn subscribe(&self) -> Receiver<T> {
        let mut guard = self.shared.lock.lock();
        let rx = guard.subscribe();
        drop(guard);
        Receiver {
            shared: self.shared.clone(),
            rx,
        }
    }

    /// Number of live receivers.
    pub fn receiver_count(&self) -> usize {
        self.shared.lock.lock().receiver_count()
    }

    /// True once no receivers remain: `send` will fail.
    pub fn is_empty(&self) -> bool {
        self.shared.lock.lock().receiver_count() == 0
    }

    /// True if the receiver is still alive.
    pub fn contains(&self, rx: &Receiver<T>) -> bool {
        self.shared.lock.lock().contains(rx.rx)
    }
}

impl<T> Receiver<T> {
    /// Take a read guard. While any guard is alive, sends park behind it.
    #[cfg(not(target_arch = "wasm32"))]
    pub fn borrow(&self) -> WatchRef<'_, T> {
        self.make_ref(false)
    }

    /// Take a read guard and mark the current version as seen.
    #[cfg(not(target_arch = "wasm32"))]
    pub fn borrow_and_update(&self) -> WatchRef<'_, T> {
        self.make_ref(true)
    }

    #[cfg(not(target_arch = "wasm32"))]
    fn make_ref(&self, mark_seen: bool) -> WatchRef<'_, T> {
        let mut guard = self.shared.lock.lock();
        guard.begin_borrow();
        if mark_seen {
            guard.mark_seen(self.rx);
        }
        WatchRef {
            lock: &self.shared.lock,
            guard: Some(guard),
        }
    }

    /// Wait for the next change. Completes instantly when the receiver has
    /// already fallen behind.
    #[cfg(not(target_arch = "wasm32"))]
    pub fn changed(&mut self) -> Result<(), RecvError> {
        let mut guard = self.shared.lock.lock();
        let mut waiter = None;
        loop {
            let poll = match waiter {
                None => guard.poll_changed(self.rx),
                Some(waiter) => guard.poll_changed_wait(self.rx, waiter),
            };
            match poll {
                ChangePoll::Immediate => return Ok(()),
                ChangePoll::Closed => return Err(RecvError),
                ChangePoll::Blocked { waiter: fresh } => waiter = Some(fresh),
            }
            guard = self.shared.lock.wait(guard);
        }
    }

    /// Whether the value changed since the receiver last looked.
    pub fn has_changed(&self) -> bool {
        self.shared.lock.lock().has_changed(self.rx)
    }

    /// Mark the current value as seen without borrowing it.
    pub fn mark_unchanged(&self) {
        self.shared.lock.lock().mark_unchanged(self.rx);
    }

    /// Number of live receivers.
    pub fn receiver_count(&self) -> usize {
        self.shared.lock.lock().receiver_count()
    }

    /// 1 while the sender is alive, 0 once it is gone.
    pub fn sender_count(&self) -> usize {
        usize::from(self.shared.lock.lock().contains(self.rx))
    }
}

/// A read guard from [`Receiver::borrow`] or
/// [`Receiver::borrow_and_update`]. While alive, sends park behind it.
#[cfg(not(target_arch = "wasm32"))]
pub struct WatchRef<'a, T> {
    lock: &'a Lock<WatchCore<T>>,
    guard: Option<MutexGuard<'a, WatchCore<T>>>,
}

#[cfg(not(target_arch = "wasm32"))]
impl<T> Deref for WatchRef<'_, T> {
    type Target = T;

    fn deref(&self) -> &T {
        self.guard.as_ref().expect("guard lives until drop").value()
    }
}

#[cfg(not(target_arch = "wasm32"))]
impl<T> Drop for WatchRef<'_, T> {
    fn drop(&mut self) {
        if let Some(mut guard) = self.guard.take() {
            guard.end_borrow();
            drop(guard);
            self.lock.notify_all();
        }
    }
}

#[cfg(test)]
mod core_tests {
    use super::*;

    fn core() -> (WatchCore<char>, u64) {
        let mut core = WatchCore::new('a');
        let rx = core.subscribe();
        (core, rx)
    }

    #[test]
    fn starts_at_the_initial_value_with_one_receiver() {
        let (mut core, rx) = core();
        assert_eq!(*core.value(), 'a');
        assert_eq!(core.version(), 0);
        assert_eq!(core.receiver_count(), 2, "the channel receiver plus ours");
        assert!(!core.has_changed(rx));
        assert!(matches!(core.poll_changed(rx), ChangePoll::Blocked { .. }));
    }

    #[test]
    fn send_commits_bumps_version_and_marks_changed() {
        let (mut core, rx) = core();
        assert_eq!(core.send('b'), SendOffer::Sent);
        assert_eq!(*core.value(), 'b');
        assert_eq!(core.version(), 1);
        assert!(core.has_changed(rx));
        assert_eq!(core.poll_changed(rx), ChangePoll::Immediate);
        assert!(!core.has_changed(rx));
    }

    #[test]
    fn send_without_receivers_is_rejected() {
        let (mut core, rx) = core();
        core.drop_receiver(rx);
        core.drop_receiver(1);
        assert_eq!(core.receiver_count(), 0);
        assert_eq!(core.send('b'), SendOffer::Rejected('b'));
        assert_eq!(*core.value(), 'a', "nothing committed");
        // send_replace never fails, even alone
        assert_eq!(core.send_replace('c'), ReplaceOffer::Replaced('a'));
        assert_eq!(*core.value(), 'c');
    }

    #[test]
    fn changed_wakes_on_the_next_send() {
        let (mut core, rx) = core();
        let waiter = match core.poll_changed(rx) {
            ChangePoll::Blocked { waiter } => waiter,
            other => panic!("expected Blocked, got {other:?}"),
        };
        assert!(core.is_awaiting(rx));
        assert_eq!(core.send('b'), SendOffer::Sent);
        assert!(!core.is_awaiting(rx), "the send caught the receiver up");
        assert_eq!(core.poll_changed_wait(rx, waiter), ChangePoll::Immediate);
    }

    #[test]
    fn stale_changed_resolves_immediately() {
        let (mut core, rx) = core();
        assert_eq!(core.send('b'), SendOffer::Sent);
        assert_eq!(core.poll_changed(rx), ChangePoll::Immediate);
    }

    #[test]
    fn changed_can_be_cancelled() {
        let (mut core, rx) = core();
        assert!(matches!(core.poll_changed(rx), ChangePoll::Blocked { .. }));
        core.cancel_changed(rx);
        assert!(!core.is_awaiting(rx));
    }

    #[test]
    fn borrow_blocks_send_until_the_last_guard_drops() {
        let (mut core, _rx) = core();
        core.begin_borrow();
        core.begin_borrow();
        let waiter = core.send('b').waiter();
        assert_eq!(core.pending_waiters(), vec![waiter]);
        assert_eq!(*core.value(), 'a', "not committed while borrowed");

        // releasing one guard keeps the send parked
        assert_eq!(core.end_borrow(), BorrowRelease::Released);
        assert_eq!(*core.value(), 'a');

        assert_eq!(core.end_borrow(), BorrowRelease::Resolved);
        assert_eq!(*core.value(), 'b');
        assert_eq!(core.version(), 1);
    }

    #[test]
    fn parked_sends_commit_in_fifo_order() {
        let (mut core, _rx) = core();
        core.begin_borrow();
        let first = core.send('x').waiter();
        let second = core.send('y').waiter();
        assert_eq!(core.pending_waiters(), vec![first, second]);

        core.end_borrow();
        assert_eq!(*core.value(), 'y', "both committed in order");
        assert_eq!(core.version(), 2);
        assert_eq!(core.poll_send(first), SendPoll::Sent);
        assert_eq!(core.poll_send(second), SendPoll::Sent);
        assert_eq!(core.pending_waiters(), Vec::<WaiterId>::new());
    }

    #[test]
    fn releasing_the_last_guard_refuses_when_receivers_vanished() {
        let (mut core, rx) = core();
        core.begin_borrow();
        let waiter = core.send('b').waiter();
        core.drop_receiver(rx);
        core.drop_receiver(1);
        assert_eq!(core.end_borrow(), BorrowRelease::Refused);
        assert_eq!(core.poll_send(waiter), SendPoll::Failed('b'));
        assert_eq!(*core.value(), 'a', "nothing committed");
    }

    #[test]
    fn cancel_send_hands_the_parked_value_back() {
        let (mut core, _rx) = core();
        core.begin_borrow();
        let waiter = core.send('b').waiter();
        assert_eq!(core.cancel_send(waiter), Some('b'));
        assert_eq!(core.poll_send(waiter), SendPoll::Cancelled);
        assert_eq!(core.cancel_send(waiter), None);
    }

    #[test]
    fn try_modify_dedups_and_bumps() {
        let (mut core, rx) = core();
        let dedup = core.try_modify(|value| {
            let changed = *value != 'a';
            if changed {
                *value = 'a';
            }
            changed
        });
        assert!(matches!(dedup, Ok(false)), "equal value does not change");
        assert_eq!(core.version(), 0);
        assert!(!core.has_changed(rx));

        let modified = core.try_modify(|value| {
            *value = 'b';
            true
        });
        assert!(matches!(modified, Ok(true)));
        assert_eq!(core.version(), 1);
        assert!(core.has_changed(rx));
    }

    #[test]
    fn try_modify_hands_the_closure_back_while_borrowed() {
        let (mut core, _rx) = core();
        core.begin_borrow();
        let blocked = core.try_modify(|value: &mut char| {
            *value = 'b';
            true
        });
        assert!(blocked.is_err(), "blocked by the borrow guard");
        core.end_borrow();
        let applied = core.try_modify(|value| {
            *value = 'b';
            true
        });
        assert!(matches!(applied, Ok(true)));
    }

    #[test]
    fn send_replace_queues_and_returns_the_old_value() {
        let (mut core, _rx) = core();
        core.begin_borrow();
        let waiter = match core.send_replace('b') {
            ReplaceOffer::Queued { waiter } => waiter,
            other => panic!("expected Queued, got {other:?}"),
        };
        assert_eq!(core.poll_replace(waiter), ReplacePoll::Pending);
        core.end_borrow();
        assert_eq!(core.poll_replace(waiter), ReplacePoll::Replaced('a'));
        assert_eq!(*core.value(), 'b');
        assert_eq!(core.version(), 1);
    }

    #[test]
    fn clone_inherits_the_seen_version_but_not_awaiting() {
        let (mut core, rx) = core();
        assert!(matches!(core.poll_changed(rx), ChangePoll::Blocked { .. }));
        let clone = core.clone_receiver(rx).expect("clone live receiver");
        assert_eq!(core.receiver_version(clone), core.receiver_version(rx));
        assert!(!core.is_awaiting(clone), "clones start fresh");

        // a send wakes the source; the clone has not caught up until it
        // calls changed() itself
        assert_eq!(core.send('b'), SendOffer::Sent);
        assert!(!core.is_awaiting(rx));
        assert!(core.has_changed(clone));
        assert_eq!(core.poll_changed(clone), ChangePoll::Immediate);
    }

    #[test]
    fn sender_gone_closes_changed() {
        let (mut core, rx) = core();
        core.drop_sender();
        assert_eq!(core.poll_changed(rx), ChangePoll::Closed);

        // a stale receiver catches up first, then sees the close
        assert_eq!(core.send('b'), SendOffer::Sent);
        core.drop_sender();
        assert_eq!(core.poll_changed(rx), ChangePoll::Immediate);
        assert_eq!(core.poll_changed(rx), ChangePoll::Closed);
    }

    #[test]
    fn mark_changed_bumps_without_modifying() {
        let (mut core, rx) = core();
        core.mark_changed();
        assert_eq!(core.version(), 1);
        assert_eq!(*core.value(), 'a');
        assert!(core.has_changed(rx));
    }

    impl<T> SendOffer<T> {
        fn waiter(&self) -> WaiterId {
            if let SendOffer::Blocked { waiter } = self {
                *waiter
            } else {
                panic!("expected Blocked");
            }
        }
    }

    #[test]
    fn receiver_ids_lists_live_receivers_ascending() {
        let mut core = WatchCore::new('a');
        let second = core.subscribe();
        let third = core.subscribe();
        assert_eq!(core.receiver_ids().collect::<Vec<_>>(), [1, second, third]);

        core.drop_receiver(second);
        assert_eq!(core.receiver_ids().collect::<Vec<_>>(), [1, third]);
    }

    #[test]
    fn pending_values_lists_parked_sends_in_commit_order() {
        let mut core = WatchCore::new('a');
        core.begin_borrow();
        let SendOffer::Blocked { .. } = core.send('x') else {
            panic!("a borrow guard parks the send");
        };
        let SendOffer::Blocked { .. } = core.send('y') else {
            panic!("a borrow guard parks the send");
        };
        assert_eq!(core.pending_values().collect::<Vec<_>>(), [&'x', &'y']);
    }
}

#[cfg(not(target_arch = "wasm32"))]
#[cfg(test)]
mod handle_tests {
    use super::*;
    use std::thread;

    #[test]
    fn changed_blocks_until_a_send_arrives() {
        let (tx, rx) = channel('a');
        let mut reader_rx = rx.clone();
        let reader = thread::spawn(move || reader_rx.changed());
        thread::sleep(std::time::Duration::from_millis(20));
        tx.send('b').expect("receiver alive");
        assert_eq!(reader.join().expect("reader thread"), Ok(()));
        assert_eq!(*rx.borrow(), 'b');
    }

    #[test]
    fn send_blocks_while_a_guard_is_out_and_flushes_on_drop() {
        let (tx, rx) = channel('a');
        let writer = thread::spawn(move || tx.send('b'));
        {
            let guard = rx.borrow();
            thread::sleep(std::time::Duration::from_millis(20));
            assert_eq!(*guard, 'a', "the send has not committed yet");
        }
        let _ = writer.join().expect("writer thread");
        assert_eq!(*rx.borrow_and_update(), 'b');
        assert!(!rx.has_changed(), "borrow_and_update marks seen");
    }

    #[test]
    fn send_fails_when_receivers_are_gone() {
        let (tx, rx) = channel('a');
        drop(rx);
        assert!(tx.is_empty());
        assert_eq!(tx.send('b'), Err(SendError('b')));
    }

    #[test]
    fn sender_gone_fails_a_parked_changed() {
        let (tx, mut rx) = channel('a');
        let reader = thread::spawn(move || rx.changed());
        thread::sleep(std::time::Duration::from_millis(20));
        drop(tx);
        assert_eq!(reader.join().expect("reader thread"), Err(RecvError));
    }

    #[test]
    fn send_if_modified_dedups_equal_values() {
        let (tx, rx) = channel('a');
        assert!(!tx.send_if_modified(|value| {
            let changed = *value != 'a';
            if changed {
                *value = 'z';
            }
            changed
        }));
        assert!(!rx.has_changed());
        assert!(tx.send_if_modified(|value| {
            *value = 'b';
            true
        }));
        assert!(rx.has_changed());
    }

    #[test]
    fn clones_start_seen_and_advance_alone() {
        let (tx, rx) = channel('a');
        let rx2 = rx.clone();
        assert!(!rx2.has_changed(), "the clone has seen the current value");
        tx.send('b').expect("receivers alive");
        assert!(rx.has_changed());
        assert!(rx2.has_changed());
        let _ = rx2.borrow_and_update();
        assert!(!rx2.has_changed());
        assert!(rx.has_changed(), "cursors advance independently");
    }

    #[test]
    fn send_modify_never_fails_without_receivers() {
        let (tx, rx) = channel('a');
        drop(rx);
        tx.send_modify(|value| *value = 'b');
        assert_eq!(*tx.subscribe().borrow(), 'b');
    }
}
