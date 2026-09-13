//! A blocking broadcast channel: every value is delivered to *every*
//! receiver, each with its own cursor.
//!
//! Mirrors `tokio::sync::broadcast`: [`broadcast::channel`] takes a capacity
//! and keeps a ring of the most recent values. A receiver that falls further
//! behind than the capacity sees [`RecvError::Lagged`] on its next `recv`
//! and skips to the oldest retained value. `send` never blocks; it fails
//! only when no receivers remain, handing the value back in [`SendError`].
//!
//! ```
//! use sim_channels::broadcast;
//!
//! let (tx, mut rx1) = broadcast::channel::<char>(4);
//! let mut rx2 = rx1.clone();
//! tx.send('a').expect("receivers alive");
//! assert_eq!(rx1.try_recv(), Ok('a'));
//! assert_eq!(rx2.try_recv(), Ok('a'));
//! ```
//!
//! The [`BroadcastCore`] state machine underneath is a plain value that game
//! drivers and tests can step through without threads.

use std::collections::VecDeque;
use std::sync::Arc;

use crate::waiter::{Lock, WaiterId, WaiterIds};

// ---- core ----

/// One receiver slot inside the core: the next sequence number this receiver
/// has not seen yet.
#[derive(Debug)]
struct Slot {
    next_seq: u64,
    waiting: Option<WaiterId>,
    alive: bool,
}

/// The pure state machine behind a broadcast channel.
#[derive(Debug)]
pub struct BroadcastCore<T> {
    capacity: usize,
    ring: VecDeque<(u64, T)>,
    next_seq: u64,
    senders: usize,
    slots: Vec<Slot>,
    live_receivers: usize,
    waiters: WaiterIds,
}

/// Outcome of the core receive polls — the predicate behind a blocking
/// `recv`.
#[derive(Debug, PartialEq, Eq)]
pub enum BroadcastPoll<T> {
    /// The receiver's next value, taken from the ring.
    Value(T),
    /// The receiver is caught up; keep waiting with this identity.
    Empty { waiter: WaiterId },
    /// The receiver fell behind the ring; it skipped this many values and
    /// now starts at the oldest retained one.
    Lagged { skipped: u64 },
    /// No senders remain and the receiver is caught up.
    Closed,
}

impl<T> BroadcastCore<T> {
    /// A channel keeping the most recent `capacity` values.
    ///
    /// # Panics
    ///
    /// Panics if `capacity` is zero.
    pub fn new(capacity: usize) -> Self {
        assert!(capacity >= 1, "broadcast capacity must be at least 1");
        Self {
            capacity,
            ring: VecDeque::new(),
            next_seq: 0,
            senders: 1,
            slots: Vec::new(),
            live_receivers: 0,
            waiters: WaiterIds::default(),
        }
    }

    /// Publish a value to every receiver. Returns the number of receivers
    /// that will see it, or the value back when nobody is listening.
    pub fn send(&mut self, value: T) -> Result<usize, SendError<T>> {
        if self.live_receivers == 0 {
            return Err(SendError(value));
        }
        self.ring.push_back((self.next_seq, value));
        self.next_seq += 1;
        while self.ring.len() > self.capacity {
            self.ring.pop_front();
        }
        Ok(self.live_receivers)
    }

    /// Add a sender handle (clone).
    pub fn add_sender(&mut self) {
        self.senders += 1;
    }

    /// Drop a sender handle.
    pub fn drop_sender(&mut self) {
        self.senders = self.senders.saturating_sub(1);
    }

    /// Subscribe a receiver starting at the *current* tail: new receivers
    /// never see history.
    pub fn subscribe(&mut self) -> usize {
        self.add_slot(self.next_seq)
    }

    /// Clone a receiver: the clone continues from the source's position,
    /// like `Receiver: Clone` in tokio.
    pub fn clone_receiver(&mut self, source: usize) -> Option<usize> {
        let next_seq = self
            .slots
            .get(source)?
            .alive
            .then_some(self.slots[source].next_seq)?;
        Some(self.add_slot(next_seq))
    }

    /// Drop a receiver slot.
    pub fn drop_receiver(&mut self, id: usize) {
        if let Some(slot) = self.slots.get_mut(id) {
            if slot.alive {
                slot.alive = false;
                slot.waiting = None;
                self.live_receivers -= 1;
            }
        }
    }

    fn add_slot(&mut self, next_seq: u64) -> usize {
        if let Some((id, slot)) = self
            .slots
            .iter_mut()
            .enumerate()
            .find(|(_, slot)| !slot.alive)
        {
            *slot = Slot {
                next_seq,
                waiting: None,
                alive: true,
            };
            self.live_receivers += 1;
            return id;
        }
        self.slots.push(Slot {
            next_seq,
            waiting: None,
            alive: true,
        });
        self.live_receivers += 1;
        self.slots.len() - 1
    }

    fn oldest_seq(&self) -> u64 {
        self.ring
            .front()
            .map(|(seq, _)| *seq)
            .unwrap_or(self.next_seq)
    }

    /// True once the last sender handle is gone.
    pub fn is_closed(&self) -> bool {
        self.senders == 0
    }

    // ---- introspection ----

    /// Ring capacity.
    pub fn capacity(&self) -> usize {
        self.capacity
    }

    /// Values currently retained in the ring.
    pub fn len(&self) -> usize {
        self.ring.len()
    }

    /// True when the ring holds no values.
    pub fn is_empty(&self) -> bool {
        self.ring.is_empty()
    }

    /// Sequence number the next sent value will get.
    pub fn next_seq(&self) -> u64 {
        self.next_seq
    }

    /// Number of live sender handles.
    pub fn sender_count(&self) -> usize {
        self.senders
    }

    /// Number of live receiver handles.
    pub fn receiver_count(&self) -> usize {
        self.live_receivers
    }

    /// The receiver's cursor position, if it is alive.
    pub fn receiver_seq(&self, id: usize) -> Option<u64> {
        let slot = self.slots.get(id)?;
        slot.alive.then_some(slot.next_seq)
    }
}

impl<T: Clone> BroadcastCore<T> {
    /// Poll a receiver, registering it as waiting when caught up.
    pub fn poll_recv(&mut self, id: usize) -> BroadcastPoll<T> {
        self.poll_inner(id, RecvRegister::Reuse)
    }

    /// Re-check a waiting receiver's poll.
    pub fn poll_recv_wait(&mut self, id: usize, waiter: WaiterId) -> BroadcastPoll<T> {
        self.poll_inner(id, RecvRegister::Pin(waiter))
    }

    fn poll_inner(&mut self, id: usize, register: RecvRegister) -> BroadcastPoll<T> {
        // snapshot everything that does not depend on the slot, so the slot
        // mutations below cannot fight the borrow checker
        let (oldest, tail, no_senders, cursor, parked) = {
            let Some(slot) = self.slots.get(id).filter(|slot| slot.alive) else {
                return BroadcastPoll::Closed;
            };
            (
                self.oldest_seq(),
                self.next_seq,
                self.senders == 0,
                slot.next_seq,
                slot.waiting,
            )
        };
        if cursor < oldest {
            let slot = self.slots.get_mut(id).expect("slot checked live above");
            slot.next_seq = oldest;
            slot.waiting = None;
            return BroadcastPoll::Lagged {
                skipped: oldest - cursor,
            };
        }
        if cursor == tail {
            if no_senders {
                let slot = self.slots.get_mut(id).expect("slot checked live above");
                slot.waiting = None;
                return BroadcastPoll::Closed;
            }
            let waiter = match register {
                RecvRegister::Reuse => parked.unwrap_or_else(|| self.waiters.alloc()),
                RecvRegister::Pin(waiter) => waiter,
            };
            let slot = self.slots.get_mut(id).expect("slot checked live above");
            slot.waiting = Some(waiter);
            return BroadcastPoll::Empty { waiter };
        }
        let value = self.ring[(cursor - oldest) as usize].1.clone();
        let slot = self.slots.get_mut(id).expect("slot checked live above");
        slot.next_seq += 1;
        slot.waiting = None;
        BroadcastPoll::Value(value)
    }

    /// Take the receiver's next value without waiting. A receiver that fell
    /// behind reports [`TryRecvError::Lagged`] instead of a value.
    pub fn try_recv(&mut self, id: usize) -> Result<T, TryRecvError> {
        let (oldest, tail, no_senders, cursor) = {
            let Some(slot) = self.slots.get(id).filter(|slot| slot.alive) else {
                return Err(TryRecvError::Closed);
            };
            (
                self.oldest_seq(),
                self.next_seq,
                self.senders == 0,
                slot.next_seq,
            )
        };
        if cursor < oldest {
            let slot = self.slots.get_mut(id).expect("slot checked live above");
            slot.next_seq = oldest;
            slot.waiting = None;
            return Err(TryRecvError::Lagged(oldest - cursor));
        }
        if cursor == tail {
            let slot = self.slots.get_mut(id).expect("slot checked live above");
            slot.waiting = None;
            return if no_senders {
                Err(TryRecvError::Closed)
            } else {
                Err(TryRecvError::Empty)
            };
        }
        let value = self.ring[(cursor - oldest) as usize].1.clone();
        let slot = self.slots.get_mut(id).expect("slot checked live above");
        slot.next_seq += 1;
        slot.waiting = None;
        Ok(value)
    }
}

#[derive(Debug, Clone, Copy)]
enum RecvRegister {
    Reuse,
    Pin(WaiterId),
}

// ---- errors ----

/// A send failed because no receivers remain; the value comes back.
#[derive(Debug, PartialEq, Eq)]
pub struct SendError<T>(pub T);

/// [`Receiver::recv`] failed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RecvError {
    /// Every sender is gone and the receiver is caught up.
    Closed,
    /// The receiver fell behind by this many values and skipped them.
    Lagged(u64),
}

/// [`Receiver::try_recv`] failed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TryRecvError {
    /// No new value yet, and senders may still send.
    Empty,
    /// Every sender is gone and the receiver is caught up.
    Closed,
    /// The receiver fell behind by this many values and skipped them.
    Lagged(u64),
}

// ---- handles ----

struct Shared<T> {
    lock: Lock<BroadcastCore<T>>,
}

/// The publishing half. Cheap to clone.
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
        let mut guard = self.shared.lock.lock();
        guard.drop_sender();
        drop(guard);
        self.shared.lock.notify_all();
    }
}

/// The receiving half. Cloning continues from the same position.
pub struct Receiver<T> {
    shared: Arc<Shared<T>>,
    id: usize,
}

impl<T> Clone for Receiver<T> {
    fn clone(&self) -> Self {
        let mut guard = self.shared.lock.lock();
        let id = guard
            .clone_receiver(self.id)
            .expect("cloning a live receiver");
        drop(guard);
        Receiver {
            shared: self.shared.clone(),
            id,
        }
    }
}

impl<T> Drop for Receiver<T> {
    fn drop(&mut self) {
        let mut guard = self.shared.lock.lock();
        guard.drop_receiver(self.id);
    }
}

/// Create a broadcast channel keeping the most recent `capacity` values.
///
/// # Panics
///
/// Panics if `capacity` is zero.
pub fn channel<T: Clone>(capacity: usize) -> (Sender<T>, Receiver<T>) {
    let shared = Arc::new(Shared {
        lock: Lock::new(BroadcastCore::new(capacity)),
    });
    let id = shared.lock.lock().subscribe();
    (
        Sender {
            shared: shared.clone(),
        },
        Receiver { shared, id },
    )
}

impl<T> Sender<T> {
    /// Publish a value to every receiver. Returns the receiver count.
    pub fn send(&self, value: T) -> Result<usize, SendError<T>> {
        let mut guard = self.shared.lock.lock();
        let result = guard.send(value);
        drop(guard);
        self.shared.lock.notify_all();
        result
    }

    /// Subscribe a new receiver starting at the current tail.
    pub fn subscribe(&self) -> Receiver<T> {
        let mut guard = self.shared.lock.lock();
        let id = guard.subscribe();
        drop(guard);
        Receiver {
            shared: self.shared.clone(),
            id,
        }
    }

    /// Number of live receivers.
    pub fn receiver_count(&self) -> usize {
        self.shared.lock.lock().receiver_count()
    }

    /// Number of live sender handles (including this one).
    pub fn sender_count(&self) -> usize {
        self.shared.lock.lock().sender_count()
    }

    /// Number of values currently retained.
    pub fn len(&self) -> usize {
        self.shared.lock.lock().len()
    }

    /// True when the ring holds no values.
    pub fn is_empty(&self) -> bool {
        self.shared.lock.lock().is_empty()
    }

    /// True once the last receiver is gone: `send` will fail.
    pub fn is_closed(&self) -> bool {
        self.shared.lock.lock().receiver_count() == 0
    }
}

impl<T: Clone> Receiver<T> {
    /// Take this receiver's next value, blocking while caught up.
    #[cfg(not(target_arch = "wasm32"))]
    pub fn recv(&mut self) -> Result<T, RecvError> {
        let mut guard = self.shared.lock.lock();
        let mut waiter = None;
        loop {
            let poll = match waiter {
                None => guard.poll_recv(self.id),
                Some(waiter) => guard.poll_recv_wait(self.id, waiter),
            };
            match poll {
                BroadcastPoll::Value(value) => return Ok(value),
                BroadcastPoll::Lagged { skipped } => return Err(RecvError::Lagged(skipped)),
                BroadcastPoll::Closed => return Err(RecvError::Closed),
                BroadcastPoll::Empty { waiter: fresh } => waiter = Some(fresh),
            }
            guard = self.shared.lock.wait(guard);
        }
    }

    /// Take this receiver's next value without blocking.
    pub fn try_recv(&mut self) -> Result<T, TryRecvError> {
        self.shared.lock.lock().try_recv(self.id)
    }

    /// Ring capacity.
    pub fn capacity(&self) -> usize {
        self.shared.lock.lock().capacity()
    }
}

#[cfg(test)]
mod core_tests {
    use super::*;

    #[test]
    fn capacity_must_be_nonzero() {
        assert!(std::panic::catch_unwind(|| BroadcastCore::<char>::new(0)).is_err());
    }

    #[test]
    fn every_receiver_sees_every_value() {
        let mut core = BroadcastCore::new(8);
        let a = core.subscribe();
        let b = core.subscribe();
        assert_eq!(core.send('x'), Ok(2));

        assert_eq!(core.poll_recv(a), BroadcastPoll::Value('x'));
        assert_eq!(core.try_recv(b), Ok('x'));
        assert_eq!(core.receiver_seq(a), Some(core.next_seq()));
        assert_eq!(core.try_recv(b), Err(TryRecvError::Empty));
    }

    #[test]
    fn subscribers_start_at_the_tail_not_in_history() {
        let mut core = BroadcastCore::new(8);
        assert_eq!(core.send('a'), Err(SendError('a')), "nobody is listening");
        assert_eq!(core.send('b'), Err(SendError('b')));
        assert_eq!(core.next_seq(), 0, "failed sends publish nothing");

        let late = core.subscribe();
        assert_eq!(core.receiver_seq(late), Some(0), "starts at the tail");
        assert_eq!(core.send('c'), Ok(1));
        assert_eq!(core.try_recv(late), Ok('c'));

        // adding a sender back reopens the channel
        assert_eq!(core.sender_count(), 1);
        core.drop_sender();
        assert!(core.is_closed());
        core.add_sender();
        assert!(!core.is_closed());
    }

    #[test]
    fn clone_continues_from_the_source_position() {
        let mut core = BroadcastCore::new(8);
        let a = core.subscribe();
        assert_eq!(core.send('1'), Ok(1));
        let b = core.clone_receiver(a).expect("clone live receiver");
        assert_eq!(
            core.receiver_seq(b),
            core.receiver_seq(a),
            "clone copies the source's cursor"
        );

        assert_eq!(core.try_recv(a), Ok('1'));
        assert_eq!(core.try_recv(b), Ok('1'), "both cursors advance alone");
        assert_eq!(core.try_recv(a), Err(TryRecvError::Empty));
        assert_eq!(core.try_recv(b), Err(TryRecvError::Empty));
    }

    #[test]
    fn falling_behind_lags_then_resumes_at_oldest() {
        let mut core = BroadcastCore::new(2);
        let rx = core.subscribe();
        for ch in 'a'..='d' {
            core.send(ch).expect("receiver alive");
        }
        assert_eq!(core.len(), 2, "only the newest two are retained");
        assert_eq!(core.receiver_seq(rx), Some(0));

        assert_eq!(core.try_recv(rx), Err(TryRecvError::Lagged(2)));
        assert_eq!(core.receiver_seq(rx), Some(2), "cursor skipped to oldest");
        assert_eq!(core.try_recv(rx), Ok('c'));
        assert_eq!(core.try_recv(rx), Ok('d'));
        assert_eq!(core.try_recv(rx), Err(TryRecvError::Empty));

        // the poll path reports lag the same way
        let rx2 = core.subscribe();
        for ch in 'e'..='g' {
            core.send(ch).expect("receiver alive");
        }
        assert_eq!(core.poll_recv(rx2), BroadcastPoll::Lagged { skipped: 1 });
        assert_eq!(core.poll_recv(rx2), BroadcastPoll::Value('f'));
        assert_eq!(core.poll_recv(rx2), BroadcastPoll::Value('g'));
    }

    #[test]
    fn caught_up_receiver_waits_and_then_wakes() {
        let mut core = BroadcastCore::new(4);
        let rx = core.subscribe();
        let waiter = match core.poll_recv(rx) {
            BroadcastPoll::Empty { waiter } => waiter,
            other => panic!("expected Empty, got {other:?}"),
        };
        assert_eq!(
            core.poll_recv_wait(rx, waiter),
            BroadcastPoll::Empty { waiter },
            "recheck keeps the waiter"
        );
        core.send('a').expect("receiver alive");
        assert_eq!(core.poll_recv_wait(rx, waiter), BroadcastPoll::Value('a'));
    }

    #[test]
    fn closed_once_caught_up_and_senders_gone() {
        let mut core = BroadcastCore::new(4);
        let rx = core.subscribe();
        core.send('a').expect("receiver alive");
        core.drop_sender();
        assert!(core.is_closed());

        assert_eq!(core.try_recv(rx), Ok('a'), "retained values stay readable");
        assert_eq!(core.try_recv(rx), Err(TryRecvError::Closed));
        assert_eq!(core.poll_recv(rx), BroadcastPoll::Closed);
    }

    #[test]
    fn dropped_receiver_slot_is_reused() {
        let mut core = BroadcastCore::new(4);
        let rx = core.subscribe();
        core.drop_receiver(rx);
        assert_eq!(core.receiver_count(), 0);
        assert_eq!(core.send('a'), Err(SendError('a')));
        assert_eq!(core.receiver_seq(rx), None);

        let fresh = core.subscribe();
        assert_eq!(fresh, rx, "dead slots are recycled");
        assert_eq!(core.receiver_count(), 1);
    }
}

#[cfg(not(target_arch = "wasm32"))]
#[cfg(test)]
mod handle_tests {
    use super::*;
    use std::thread;

    #[test]
    fn recv_blocks_until_a_send_arrives() {
        let (tx, mut rx) = channel::<char>(4);
        let reader = thread::spawn(move || rx.recv());
        thread::sleep(std::time::Duration::from_millis(20));
        tx.send('a').expect("receiver alive");
        assert_eq!(reader.join().expect("reader thread"), Ok('a'));
    }

    #[test]
    fn recv_reports_lag_after_overflow() {
        let (tx, mut rx) = channel::<char>(1);
        let mut reader_rx = rx.clone();
        let (got_tx, got_rx) = std::sync::mpsc::channel();
        let reader = thread::spawn(move || {
            let first = reader_rx.recv();
            got_tx.send(()).expect("report progress");
            let second = reader_rx.recv();
            (first, second)
        });
        tx.send('a').expect("receiver alive");
        // wait until the reader consumed 'a', so the overflow below is
        // deterministic
        got_rx.recv().expect("reader progress");
        tx.send('b').expect("receiver alive");
        tx.send('c').expect("receiver alive");
        let (first, second) = reader.join().expect("reader thread");
        assert_eq!(first, Ok('a'));
        assert_eq!(second, Err(RecvError::Lagged(1)));
        // the untouched original handle lags by everything it never read
        assert_eq!(rx.try_recv(), Err(TryRecvError::Lagged(2)));
        assert_eq!(rx.try_recv(), Ok('c'), "resumed at the oldest retained");
    }

    #[test]
    fn send_fails_once_receivers_are_gone() {
        let (tx, rx) = channel::<char>(4);
        assert_eq!(tx.receiver_count(), 1);
        drop(rx);
        assert!(tx.is_closed());
        assert_eq!(tx.send('a'), Err(SendError('a')));
    }

    #[test]
    fn dropping_senders_closes_parked_receivers() {
        let (tx, rx) = channel::<char>(4);
        let mut rx2 = rx.clone();
        assert_eq!(tx.sender_count(), 1);
        let reader = thread::spawn(move || {
            let _ = rx2.recv();
            rx2.recv()
        });
        thread::sleep(std::time::Duration::from_millis(20));
        drop(tx);
        assert_eq!(
            reader.join().expect("reader thread"),
            Err(RecvError::Closed)
        );
    }

    #[test]
    fn clones_share_and_advance_their_own_cursors() {
        let (tx, mut rx1) = channel::<char>(4);
        let mut rx2 = rx1.clone();
        tx.send('a').expect("receivers alive");
        assert_eq!(rx1.try_recv(), Ok('a'));
        assert_eq!(rx2.try_recv(), Ok('a'));
        assert_eq!(rx1.try_recv(), Err(TryRecvError::Empty));
        assert_eq!(rx2.try_recv(), Err(TryRecvError::Empty));
    }
}
