//! A blocking multi-producer, single-consumer channel.
//!
//! Mirrors `tokio::sync::mpsc`: [`mpsc::channel`] takes a capacity and makes
//! `send` block while the buffer is full (tokio's async `send` waits instead
//! of parking — the observable semantics are the same), `try_send` never
//! waits, and a value sent on a closed channel comes back inside
//! [`SendError`]. [`mpsc::unbounded_channel`] is the capacity-less twin.
//!
//! Teaching beats, now as plain API semantics:
//!
//! * a send that hits a full buffer parks **holding its value** in a FIFO
//!   queue inside the core ([`SendOffer::Blocked`]); the value only enters
//!   the buffer when a receive frees a slot,
//! * every receive wakes exactly one blocked sender — the one at the front
//!   of the queue (FIFO; tokio's wake order is unspecified, the games pick
//!   whatever order they want to show),
//! * a receiver waiting on an empty channel takes the next value directly,
//! * dropping a sender discards nothing in flight but stops new sends; the
//!   receiver drains the buffer and then sees [`TryRecvError::Disconnected`],
//! * [`MpscCore::cancel_send`] models the minigame's "client disconnects
//!   while blocked": the queued value is handed back.
//!
//! ```
//! use sim_channels::mpsc;
//!
//! let (tx, rx) = mpsc::channel::<char>(2);
//! tx.send('a').expect("sent");
//! assert_eq!(rx.try_recv(), Ok('a'));
//! ```
//!
//! The [`MpscCore`] state machine underneath is a plain value that game
//! drivers and tests can step through without threads.

use std::collections::VecDeque;
use std::sync::Arc;

use crate::waiter::{Lock, WaiterId, WaiterIds};

// ---- core ----

/// One send parked on a full buffer. The value moves in at
/// [`MpscCore::offer_send`] and back out when the send completes, fails or is
/// cancelled.
#[derive(Debug)]
struct BlockedSend<T> {
    waiter: WaiterId,
    value: Option<T>,
    state: BlockedState,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum BlockedState {
    /// Waiting for capacity.
    Queued,
    /// Value already moved into the buffer.
    Completed,
    /// Channel closed; the value is handed back to the sender.
    Failed,
}

/// The pure state machine behind a bounded or unbounded mpsc channel.
#[derive(Debug)]
pub struct MpscCore<T> {
    capacity: usize,
    buffer: VecDeque<T>,
    blocked: VecDeque<BlockedSend<T>>,
    waiting_receiver: Option<WaiterId>,
    senders: usize,
    receiver_present: bool,
    closed: bool,
    waiters: WaiterIds,
}

/// Outcome of [`MpscCore::offer_send`] — the modeling of one blocking send.
#[derive(Debug, PartialEq, Eq)]
pub enum SendOffer<T> {
    /// The value is in the buffer.
    Accepted,
    /// The buffer is full; the value is parked with this identity.
    Blocked { waiter: WaiterId },
    /// The channel is closed; the value comes back.
    Rejected(T),
}

/// Re-check of a parked send, via [`MpscCore::poll_send`].
#[derive(Debug, PartialEq, Eq)]
pub enum SendPoll<T> {
    /// The value is in the buffer; the sender may proceed.
    Accepted,
    /// Still parked.
    Pending,
    /// The channel closed while parked; the value comes back.
    Failed(T),
    /// The parked send was cancelled with
    /// [`MpscCore::cancel_send`] (core drivers only; live handles never
    /// observe this).
    Cancelled,
}

/// Outcome of [`MpscCore::poll_recv`] — the predicate behind a blocking
/// `recv`.
#[derive(Debug, PartialEq, Eq)]
pub enum RecvPoll<T> {
    /// A value was taken from the buffer.
    Value(T),
    /// The buffer is empty; keep waiting with this identity.
    Empty { waiter: WaiterId },
    /// The channel is closed and fully drained.
    Disconnected,
}

impl<T> MpscCore<T> {
    /// A channel with room for `capacity` buffered values. Unbounded
    /// channels use [`usize::MAX`].
    ///
    /// # Panics
    ///
    /// Panics if `capacity` is zero.
    pub fn new(capacity: usize) -> Self {
        assert!(capacity >= 1, "mpsc capacity must be at least 1");
        Self {
            capacity,
            buffer: VecDeque::new(),
            blocked: VecDeque::new(),
            waiting_receiver: None,
            senders: 1,
            receiver_present: true,
            closed: false,
            waiters: WaiterIds::default(),
        }
    }

    /// Model one blocking send.
    pub fn offer_send(&mut self, value: T) -> SendOffer<T> {
        if self.closed {
            return SendOffer::Rejected(value);
        }
        if self.buffer.len() < self.capacity {
            self.buffer.push_back(value);
            return SendOffer::Accepted;
        }
        let waiter = self.waiters.alloc();
        self.blocked.push_back(BlockedSend {
            waiter,
            value: Some(value),
            state: BlockedState::Queued,
        });
        SendOffer::Blocked { waiter }
    }

    /// Re-check a parked send.
    pub fn poll_send(&mut self, waiter: WaiterId) -> SendPoll<T> {
        let Some(idx) = self
            .blocked
            .iter()
            .position(|blocked| blocked.waiter == waiter)
        else {
            return SendPoll::Cancelled;
        };
        match self.blocked[idx].state {
            BlockedState::Queued => SendPoll::Pending,
            BlockedState::Completed => {
                self.blocked.remove(idx);
                SendPoll::Accepted
            }
            BlockedState::Failed => {
                let value = self.blocked.remove(idx).and_then(|b| b.value);
                SendPoll::Failed(value.expect("failed send keeps its value"))
            }
        }
    }

    /// Non-blocking send: never parks a value, never joins the queue.
    pub fn try_send(&mut self, value: T) -> Result<(), TrySendError<T>> {
        if self.closed {
            return Err(TrySendError::Closed(value));
        }
        if self.buffer.len() >= self.capacity {
            return Err(TrySendError::Full(value));
        }
        self.buffer.push_back(value);
        Ok(())
    }

    /// Poll for a value, registering the receiver as waiting when empty.
    pub fn poll_recv(&mut self) -> RecvPoll<T> {
        let waiter = *self
            .waiting_receiver
            .get_or_insert_with(|| self.waiters.alloc());
        self.poll_recv_inner(Some(waiter))
    }

    /// Re-check a waiting receiver's poll.
    pub fn poll_recv_wait(&mut self, waiter: WaiterId) -> RecvPoll<T> {
        self.poll_recv_inner(Some(waiter))
    }

    fn poll_recv_inner(&mut self, waiter: Option<WaiterId>) -> RecvPoll<T> {
        if let Some(value) = self.buffer.pop_front() {
            self.complete_blocked_if_room();
            if self.waiting_receiver == waiter {
                self.waiting_receiver = None;
            }
            return RecvPoll::Value(value);
        }
        let Some(waiter) = waiter else {
            return RecvPoll::Disconnected;
        };
        if self.closed || self.senders == 0 {
            self.waiting_receiver = None;
            return RecvPoll::Disconnected;
        }
        self.waiting_receiver = Some(waiter);
        RecvPoll::Empty { waiter }
    }

    /// Non-blocking receive.
    pub fn try_recv(&mut self) -> Result<T, TryRecvError> {
        match self.buffer.pop_front() {
            Some(value) => {
                self.complete_blocked_if_room();
                Ok(value)
            }
            None if self.closed || self.senders == 0 => Err(TryRecvError::Disconnected),
            None => Err(TryRecvError::Empty),
        }
    }

    /// Close the receiving half: buffered values stay drainable, parked
    /// sends fail with their values back, new sends are rejected.
    pub fn close(&mut self) {
        self.closed = true;
        for blocked in &mut self.blocked {
            if blocked.state == BlockedState::Queued {
                blocked.state = BlockedState::Failed;
            }
        }
    }

    /// A `Sender` handle appeared (clone or fresh connection).
    pub fn add_sender(&mut self) {
        self.senders += 1;
    }

    /// A `Sender` handle was dropped. The last one disconnects the channel
    /// once the buffer drains.
    pub fn drop_sender(&mut self) {
        self.senders = self.senders.saturating_sub(1);
    }

    /// The receiver handle was dropped: the channel closes.
    pub fn drop_receiver(&mut self) {
        self.receiver_present = false;
        self.close();
    }

    /// Cancel a parked send, handing its value back. Core-driver modeling
    /// for "the handle was dropped while blocked"; live handles cannot
    /// trigger this because a parked `send` borrows its handle.
    pub fn cancel_send(&mut self, waiter: WaiterId) -> Option<T> {
        let idx = self
            .blocked
            .iter()
            .position(|blocked| blocked.waiter == waiter)?;
        let blocked = self.blocked.remove(idx)?;
        blocked.value
    }

    /// As soon as buffer room frees up, the front-most queued send proceeds
    /// into the buffer (FIFO).
    fn complete_blocked_if_room(&mut self) {
        while let Some(front) = self.blocked.front() {
            let eligible = front.state == BlockedState::Queued
                && !self.closed
                && self.buffer.len() < self.capacity;
            if !eligible {
                break;
            }
            let value = self
                .blocked
                .front_mut()
                .and_then(|front| front.value.take())
                .expect("queued send keeps its value");
            self.blocked.front_mut().expect("checked front").state = BlockedState::Completed;
            self.buffer.push_back(value);
        }
    }

    // ---- introspection ----

    /// Configured capacity (`usize::MAX` when unbounded).
    pub fn capacity(&self) -> usize {
        self.capacity
    }

    /// Values sitting in the buffer.
    pub fn len(&self) -> usize {
        self.buffer.len()
    }

    /// True when no value is buffered or parked.
    pub fn is_empty(&self) -> bool {
        self.buffer.is_empty() && self.blocked.is_empty()
    }

    /// Number of parked sends.
    pub fn blocked_len(&self) -> usize {
        self.blocked.len()
    }

    /// Parked sends in queue order — the FIFO wake order.
    pub fn blocked_waiters(&self) -> Vec<WaiterId> {
        self.blocked.iter().map(|blocked| blocked.waiter).collect()
    }

    /// Identity of the receiver currently waiting on an empty channel.
    pub fn waiting_receiver(&self) -> Option<WaiterId> {
        self.waiting_receiver
    }

    /// True once `close` happened or the receiver is gone.
    pub fn is_closed(&self) -> bool {
        self.closed
    }

    /// Number of live `Sender` handles.
    pub fn sender_count(&self) -> usize {
        self.senders
    }

    /// True while a `Receiver` handle exists.
    pub fn has_receiver(&self) -> bool {
        self.receiver_present
    }
}

// ---- errors ----

/// A send failed because the channel was closed; the value comes back.
#[derive(Debug, PartialEq, Eq)]
pub struct SendError<T>(pub T);

/// [`Sender::try_send`] failed.
#[derive(Debug, PartialEq, Eq)]
pub enum TrySendError<T> {
    /// The buffer is full; nothing was sent.
    Full(T),
    /// The channel is closed; the value comes back.
    Closed(T),
}

/// The sender is gone and the buffer is drained.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RecvError;

/// [`Receiver::try_recv`] failed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TryRecvError {
    /// No value buffered, and senders may still send.
    Empty,
    /// The channel is closed and fully drained.
    Disconnected,
}

// ---- shared plumbing ----

struct Shared<T> {
    lock: Lock<MpscCore<T>>,
}

fn sender_gone<T>(shared: &Shared<T>) {
    let mut guard = shared.lock.lock();
    guard.drop_sender();
    drop(guard);
    shared.lock.notify_all();
}

// ---- bounded handles ----

/// The sending half of a bounded channel. Cheap to clone; the channel
/// closes once the last clone is dropped.
pub struct Sender<T> {
    shared: Arc<Shared<T>>,
}

impl<T> Clone for Sender<T> {
    fn clone(&self) -> Self {
        self.shared.lock.lock().add_sender();
        Sender {
            shared: self.shared.clone(),
        }
    }
}

impl<T> Drop for Sender<T> {
    fn drop(&mut self) {
        sender_gone(&self.shared);
    }
}

/// The receiving half of a bounded channel. Not cloneable: single consumer.
pub struct Receiver<T> {
    shared: Arc<Shared<T>>,
}

/// Create a bounded mpsc channel.
///
/// # Panics
///
/// Panics if `capacity` is zero (tokio parity).
pub fn channel<T>(capacity: usize) -> (Sender<T>, Receiver<T>) {
    assert!(capacity >= 1, "mpsc capacity must be at least 1");
    let shared = Arc::new(Shared {
        lock: Lock::new(MpscCore::new(capacity)),
    });
    (
        Sender {
            shared: shared.clone(),
        },
        Receiver { shared },
    )
}

impl<T> Sender<T> {
    /// Send a value, blocking while the buffer is full.
    #[cfg(not(target_arch = "wasm32"))]
    pub fn send(&self, value: T) -> Result<(), SendError<T>> {
        let mut guard = self.shared.lock.lock();
        let waiter = match guard.offer_send(value) {
            SendOffer::Accepted => {
                drop(guard);
                self.shared.lock.notify_all();
                return Ok(());
            }
            SendOffer::Blocked { waiter } => waiter,
            SendOffer::Rejected(value) => return Err(SendError(value)),
        };
        loop {
            match guard.poll_send(waiter) {
                SendPoll::Accepted => {
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

    /// Send a value without blocking; a full buffer returns it.
    pub fn try_send(&self, value: T) -> Result<(), TrySendError<T>> {
        let mut guard = self.shared.lock.lock();
        let result = guard.try_send(value);
        drop(guard);
        if result.is_ok() {
            self.shared.lock.notify_all();
        }
        result
    }

    /// Configured capacity.
    pub fn capacity(&self) -> usize {
        self.shared.lock.lock().capacity()
    }

    /// True once `send` can no longer succeed.
    pub fn is_closed(&self) -> bool {
        self.shared.lock.lock().is_closed()
    }

    /// True if both handles were cloned from the same `channel` call.
    pub fn same_channel(&self, other: &Sender<T>) -> bool {
        Arc::ptr_eq(&self.shared, &other.shared)
    }
}

impl<T> Receiver<T> {
    /// Take the next value, blocking while the buffer is empty.
    #[cfg(not(target_arch = "wasm32"))]
    pub fn recv(&self) -> Result<T, RecvError> {
        let mut guard = self.shared.lock.lock();
        let mut waiter = None;
        loop {
            let poll = match waiter {
                None => guard.poll_recv(),
                Some(waiter) => guard.poll_recv_wait(waiter),
            };
            match poll {
                RecvPoll::Value(value) => {
                    drop(guard);
                    self.shared.lock.notify_all();
                    return Ok(value);
                }
                RecvPoll::Disconnected => return Err(RecvError),
                RecvPoll::Empty { waiter: fresh } => waiter = Some(fresh),
            }
            guard = self.shared.lock.wait(guard);
        }
    }

    /// Take the next value without blocking.
    pub fn try_recv(&self) -> Result<T, TryRecvError> {
        let mut guard = self.shared.lock.lock();
        let result = guard.try_recv();
        drop(guard);
        if result.is_ok() {
            self.shared.lock.notify_all();
        }
        result
    }

    /// Close the channel: buffered values stay drainable, parked sends fail.
    pub fn close(&self) {
        let mut guard = self.shared.lock.lock();
        guard.close();
        drop(guard);
        self.shared.lock.notify_all();
    }

    /// Number of buffered values.
    pub fn len(&self) -> usize {
        self.shared.lock.lock().len()
    }

    /// True when nothing is buffered or parked.
    pub fn is_empty(&self) -> bool {
        self.shared.lock.lock().is_empty()
    }

    /// Configured capacity.
    pub fn capacity(&self) -> usize {
        self.shared.lock.lock().capacity()
    }

    /// Number of live `Sender` handles.
    pub fn sender_count(&self) -> usize {
        self.shared.lock.lock().sender_count()
    }

    /// True if the two handles belong to the same channel.
    pub fn same_channel(&self, other: &Receiver<T>) -> bool {
        Arc::ptr_eq(&self.shared, &other.shared)
    }
}

impl<T> Drop for Receiver<T> {
    fn drop(&mut self) {
        let mut guard = self.shared.lock.lock();
        guard.drop_receiver();
        drop(guard);
        self.shared.lock.notify_all();
    }
}

// ---- unbounded handles ----

/// The sending half of an unbounded channel.
pub struct UnboundedSender<T> {
    shared: Arc<Shared<T>>,
}

impl<T> Clone for UnboundedSender<T> {
    fn clone(&self) -> Self {
        self.shared.lock.lock().add_sender();
        UnboundedSender {
            shared: self.shared.clone(),
        }
    }
}

impl<T> Drop for UnboundedSender<T> {
    fn drop(&mut self) {
        sender_gone(&self.shared);
    }
}

/// The receiving half of an unbounded channel.
pub struct UnboundedReceiver<T> {
    shared: Arc<Shared<T>>,
}

/// Create an unbounded mpsc channel: `send` never blocks on capacity.
pub fn unbounded_channel<T>() -> (UnboundedSender<T>, UnboundedReceiver<T>) {
    let shared = Arc::new(Shared {
        lock: Lock::new(MpscCore::new(usize::MAX)),
    });
    (
        UnboundedSender {
            shared: shared.clone(),
        },
        UnboundedReceiver { shared },
    )
}

impl<T> UnboundedSender<T> {
    /// Send a value. Only fails on a closed channel.
    pub fn send(&self, value: T) -> Result<(), SendError<T>> {
        let mut guard = self.shared.lock.lock();
        let result = guard.offer_send(value);
        drop(guard);
        self.shared.lock.notify_all();
        match result {
            SendOffer::Accepted => Ok(()),
            SendOffer::Rejected(value) => Err(SendError(value)),
            SendOffer::Blocked { .. } => unreachable!("unbounded channels never fill up"),
        }
    }

    /// Non-blocking alias of [`UnboundedSender::send`]; only fails on a
    /// closed channel.
    pub fn try_send(&self, value: T) -> Result<(), TrySendError<T>> {
        self.send(value)
            .map_err(|SendError(value)| TrySendError::Closed(value))
    }

    /// True once `send` can no longer succeed.
    pub fn is_closed(&self) -> bool {
        self.shared.lock.lock().is_closed()
    }

    /// True if both handles were cloned from the same `unbounded_channel`
    /// call.
    pub fn same_channel(&self, other: &UnboundedSender<T>) -> bool {
        Arc::ptr_eq(&self.shared, &other.shared)
    }
}

impl<T> UnboundedReceiver<T> {
    /// Take the next value, blocking while the buffer is empty.
    #[cfg(not(target_arch = "wasm32"))]
    pub fn recv(&self) -> Result<T, RecvError> {
        let mut guard = self.shared.lock.lock();
        let mut waiter = None;
        loop {
            let poll = match waiter {
                None => guard.poll_recv(),
                Some(waiter) => guard.poll_recv_wait(waiter),
            };
            match poll {
                RecvPoll::Value(value) => {
                    drop(guard);
                    self.shared.lock.notify_all();
                    return Ok(value);
                }
                RecvPoll::Disconnected => return Err(RecvError),
                RecvPoll::Empty { waiter: fresh } => waiter = Some(fresh),
            }
            guard = self.shared.lock.wait(guard);
        }
    }

    /// Take the next value without blocking.
    pub fn try_recv(&self) -> Result<T, TryRecvError> {
        let mut guard = self.shared.lock.lock();
        let result = guard.try_recv();
        drop(guard);
        if result.is_ok() {
            self.shared.lock.notify_all();
        }
        result
    }

    /// Close the channel: buffered values stay drainable.
    pub fn close(&self) {
        let mut guard = self.shared.lock.lock();
        guard.close();
        drop(guard);
        self.shared.lock.notify_all();
    }

    /// Number of buffered values.
    pub fn len(&self) -> usize {
        self.shared.lock.lock().len()
    }

    /// True when nothing is buffered.
    pub fn is_empty(&self) -> bool {
        self.shared.lock.lock().is_empty()
    }
}

impl<T> Drop for UnboundedReceiver<T> {
    fn drop(&mut self) {
        let mut guard = self.shared.lock.lock();
        guard.drop_receiver();
        drop(guard);
        self.shared.lock.notify_all();
    }
}

#[cfg(test)]
mod core_tests {
    use super::*;

    const CAP: usize = 3;

    fn core() -> MpscCore<char> {
        MpscCore::new(CAP)
    }

    #[test]
    fn capacity_must_be_nonzero() {
        let result = std::panic::catch_unwind(|| MpscCore::<char>::new(0));
        assert!(result.is_err());
    }

    #[test]
    fn sends_land_in_the_buffer_until_it_is_full() {
        let mut core = core();
        for ch in 'a'..'d' {
            assert_eq!(core.offer_send(ch), SendOffer::Accepted);
        }
        assert_eq!(core.len(), CAP);
        assert!(
            matches!(core.offer_send('d'), SendOffer::Blocked { .. }),
            "a full buffer parks the send, it does not reject it"
        );
    }

    #[test]
    fn full_buffer_parks_sends_fifo_and_wakes_in_order() {
        let mut core = core();
        for ch in 'a'..'d' {
            let _ = core.offer_send(ch);
        }
        let first = match core.offer_send('x') {
            SendOffer::Blocked { waiter } => waiter,
            other => panic!("expected Blocked, got {other:?}"),
        };
        let second = match core.offer_send('y') {
            SendOffer::Blocked { waiter } => waiter,
            other => panic!("expected Blocked, got {other:?}"),
        };
        assert_eq!(core.blocked_waiters(), vec![first, second]);
        assert_eq!(core.blocked_len(), 2);
        assert_eq!(core.len(), CAP, "parked values do not enter the buffer");

        // one receive frees one slot; the front of the queue proceeds
        assert_eq!(core.try_recv(), Ok('a'));
        assert_eq!(core.poll_send(first), SendPoll::Accepted);
        assert_eq!(core.poll_send(second), SendPoll::Pending);
        assert_eq!(core.len(), CAP, "the woken value is already buffered");
        assert_eq!(core.blocked_waiters(), vec![second]);

        // the next receive frees the second parked send
        assert_eq!(core.try_recv(), Ok('b'));
        assert_eq!(core.poll_send(second), SendPoll::Accepted);
        assert_eq!(core.blocked_waiters(), Vec::<WaiterId>::new());
    }

    #[test]
    fn waiting_receiver_takes_the_next_value_directly() {
        let mut core = core();
        let waiter = match core.poll_recv() {
            RecvPoll::Empty { waiter } => waiter,
            other => panic!("expected Empty, got {other:?}"),
        };
        assert_eq!(core.waiting_receiver(), Some(waiter));

        assert_eq!(core.offer_send('a'), SendOffer::Accepted);
        assert_eq!(core.poll_recv_wait(waiter), RecvPoll::Value('a'));
        assert_eq!(core.waiting_receiver(), None, "waiter cleared on success");
        assert_eq!(core.len(), 0, "waiting receiver bypasses the buffer");
    }

    #[test]
    fn close_fails_parked_sends_and_returns_their_values() {
        let mut core = core();
        for ch in 'a'..'d' {
            let _ = core.offer_send(ch);
        }
        let waiter = match core.offer_send('x') {
            SendOffer::Blocked { waiter } => waiter,
            other => panic!("expected Blocked, got {other:?}"),
        };
        core.close();
        assert_eq!(core.poll_send(waiter), SendPoll::Failed('x'));

        // buffered values remain drainable, then disconnected
        assert_eq!(core.try_recv(), Ok('a'));
        assert_eq!(core.try_recv(), Ok('b'));
        assert_eq!(core.try_recv(), Ok('c'));
        assert_eq!(core.try_recv(), Err(TryRecvError::Disconnected));
    }

    #[test]
    fn close_rejects_new_sends_but_drains() {
        let mut core = core();
        let _ = core.offer_send('a');
        core.close();
        assert!(core.is_closed());
        assert_eq!(core.offer_send('b'), SendOffer::Rejected('b'));
        assert_eq!(core.try_send('b'), Err(TrySendError::Closed('b')));
        assert_eq!(core.try_recv(), Ok('a'));
        assert_eq!(core.try_recv(), Err(TryRecvError::Disconnected));
    }

    #[test]
    fn dropping_the_receiver_closes_the_channel() {
        let mut core = core();
        assert!(core.has_receiver());
        core.drop_receiver();
        assert!(!core.has_receiver());
        assert!(core.is_closed());
        assert_eq!(core.offer_send('a'), SendOffer::Rejected('a'));
    }

    #[test]
    fn last_sender_drop_disconnects_once_drained() {
        let mut core = core();
        let _ = core.offer_send('a');
        core.add_sender();
        assert_eq!(core.sender_count(), 2);
        core.drop_sender();
        core.drop_sender();
        assert_eq!(core.sender_count(), 0);
        assert_eq!(core.try_recv(), Ok('a'), "buffer drains after disconnect");
        assert_eq!(core.try_recv(), Err(TryRecvError::Disconnected));
        assert_eq!(core.poll_recv(), RecvPoll::Disconnected);
    }

    #[test]
    fn cancel_send_hands_the_parked_value_back() {
        let mut core = core();
        for ch in 'a'..'d' {
            let _ = core.offer_send(ch);
        }
        let waiter = match core.offer_send('x') {
            SendOffer::Blocked { waiter } => waiter,
            other => panic!("expected Blocked, got {other:?}"),
        };
        assert_eq!(core.cancel_send(waiter), Some('x'));
        assert_eq!(core.blocked_len(), 0);
        assert_eq!(core.poll_send(waiter), SendPoll::Cancelled);
        assert_eq!(core.cancel_send(waiter), None);
    }

    #[test]
    fn unbounded_channels_never_park() {
        let mut core: MpscCore<char> = MpscCore::new(usize::MAX);
        for ch in 'a'..'f' {
            assert_eq!(core.offer_send(ch), SendOffer::Accepted);
        }
        assert_eq!(core.len(), 5);
        assert_eq!(core.capacity(), usize::MAX);
    }
}

#[cfg(test)]
mod try_send_tests {
    use super::*;

    #[test]
    fn try_send_reports_full_without_parking() {
        let mut core = MpscCore::new(1);
        assert_eq!(core.try_send('a'), Ok(()));
        assert_eq!(core.try_send('b'), Err(TrySendError::Full('b')));
        assert_eq!(core.blocked_len(), 0);
    }
}

#[cfg(not(target_arch = "wasm32"))]
#[cfg(test)]
mod handle_tests {
    use super::*;
    use std::thread;

    #[test]
    fn send_blocks_on_full_until_a_receive_frees_a_slot() {
        let (tx, rx) = channel::<char>(1);
        tx.send('a').expect("fits");

        let writer = thread::spawn(move || {
            tx.send('b').expect("unblocked by receive");
        });
        thread::sleep(std::time::Duration::from_millis(20));
        assert_eq!(rx.try_recv(), Ok('a'));
        writer.join().expect("writer thread");
        assert_eq!(rx.try_recv(), Ok('b'));
    }

    #[test]
    fn recv_blocks_on_empty_until_a_send_arrives() {
        let (tx, rx) = channel::<char>(1);
        let reader = thread::spawn(move || rx.recv());
        thread::sleep(std::time::Duration::from_millis(20));
        tx.send('a').expect("sent");
        assert_eq!(reader.join().expect("reader thread"), Ok('a'));
    }

    #[test]
    fn close_fails_a_blocked_sender() {
        let (tx, rx) = channel::<char>(1);
        tx.send('a').expect("fits");
        let writer = thread::spawn(move || tx.send('b'));
        thread::sleep(std::time::Duration::from_millis(20));
        rx.close();
        assert_eq!(writer.join().expect("writer thread"), Err(SendError('b')));
        assert_eq!(rx.try_recv(), Ok('a'), "buffered values survive close");
    }

    #[test]
    fn dropping_the_receiver_fails_a_blocked_sender() {
        let (tx, rx) = channel::<char>(1);
        tx.send('a').expect("fits");
        let writer = thread::spawn(move || tx.send('b'));
        thread::sleep(std::time::Duration::from_millis(20));
        drop(rx);
        assert_eq!(writer.join().expect("writer thread"), Err(SendError('b')));
    }

    #[test]
    fn last_sender_drop_disconnects_a_waiting_receiver() {
        let (tx, rx) = channel::<char>(1);
        let reader = thread::spawn(move || rx.recv());
        thread::sleep(std::time::Duration::from_millis(20));
        drop(tx);
        assert_eq!(reader.join().expect("reader thread"), Err(RecvError));
    }

    #[test]
    fn clones_count_and_same_channel_matches() {
        let (tx, rx) = channel::<char>(1);
        let tx2 = tx.clone();
        assert!(tx.same_channel(&tx2));
        assert_eq!(rx.sender_count(), 2);
        drop(tx2);
        assert_eq!(rx.sender_count(), 1);
        assert_eq!(tx.capacity(), 1);
        assert_eq!(rx.capacity(), 1);

        let (other_tx, _other_rx) = channel::<char>(1);
        assert!(!tx.same_channel(&other_tx));
    }

    #[test]
    fn unbounded_send_never_blocks() {
        let (tx, rx) = unbounded_channel::<char>();
        for ch in 'a'..'e' {
            tx.send(ch).expect("unbounded");
        }
        assert_eq!(rx.len(), 4);
        for ch in 'a'..'e' {
            assert_eq!(rx.try_recv(), Ok(ch));
        }
    }
}
