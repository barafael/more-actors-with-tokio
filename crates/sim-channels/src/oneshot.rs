//! A blocking one-shot channel: exactly one value travels from one
//! [`Sender`] to one [`Receiver`].
//!
//! Mirrors `tokio::sync::oneshot`: `channel()` hands out the two halves, the
//! sender consumes itself on `send`, and the receiver consumes itself on
//! `recv`. Dropping the receiver before the value is sent makes `send` fail
//! with [`SendError`]; dropping the sender before sending makes `recv` fail
//! with [`RecvError`].
//!
//! ```
//! use sim_channels::oneshot;
//!
//! let (tx, rx) = oneshot::channel::<char>();
//! tx.send('a').expect("receiver is alive");
//! assert_eq!(rx.recv(), Ok('a'));
//! ```
//!
//! The [`OneshotCore`] state machine underneath is a plain value that game
//! drivers and tests can step through without threads; its [`PollRecv`]
//! outcome doubles as the predicate for the blocking `recv`.

use std::sync::Arc;

use crate::waiter::Lock;

// ---- core ----

/// The pure state machine behind the oneshot channel.
///
/// The slot holds `Empty` until a value is sent, then `Value`, then `Taken`
/// once received. The terminal `Dropped` state records a receiver that went
/// away without ever receiving.
#[derive(Debug)]
pub struct OneshotCore<T> {
    slot: Slot<T>,
    sender_alive: bool,
    receiver_alive: bool,
}

#[derive(Debug)]
enum Slot<T> {
    Empty,
    Value(T),
    Taken,
    Dropped,
}

/// Outcome of [`OneshotCore::poll_recv`], the predicate behind a blocking
/// `recv`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PollRecv {
    /// A value is ready and was taken.
    Ready,
    /// No value yet; keep waiting.
    Pending,
    /// The sender is gone (or the channel was closed): no value will ever
    /// arrive.
    Closed,
}

impl<T> Default for OneshotCore<T> {
    fn default() -> Self {
        Self::new()
    }
}

impl<T> OneshotCore<T> {
    pub fn new() -> Self {
        Self {
            slot: Slot::Empty,
            sender_alive: true,
            receiver_alive: true,
        }
    }

    /// Send the one value. Fails if the receiver is gone or a value was
    /// already sent.
    pub fn send(&mut self, value: T) -> Result<(), SendError<T>> {
        if !matches!(self.slot, Slot::Empty) {
            return Err(SendError(value));
        }
        self.slot = Slot::Value(value);
        Ok(())
    }

    /// Take the value if it is there, without waiting.
    pub fn try_recv(&mut self) -> Result<T, TryRecvError> {
        match self.slot {
            Slot::Value(_) => match std::mem::replace(&mut self.slot, Slot::Taken) {
                Slot::Value(value) => Ok(value),
                _ => unreachable!("slot was Value"),
            },
            Slot::Empty if self.sender_alive => Err(TryRecvError::Empty),
            _ => Err(TryRecvError::Closed),
        }
    }

    /// Non-consuming poll used by the blocking `recv`: `PollRecv::Ready`
    /// means a subsequent [`OneshotCore::try_recv`] will succeed.
    pub fn poll_recv(&mut self) -> PollRecv {
        match self.slot {
            Slot::Empty if self.sender_alive => PollRecv::Pending,
            Slot::Empty => PollRecv::Closed,
            Slot::Value(_) => PollRecv::Ready,
            Slot::Taken | Slot::Dropped => PollRecv::Closed,
        }
    }

    /// The receiver half closes without receiving: further sends fail with
    /// [`SendError`] and the channel reports [`OneshotCore::is_abandoned`].
    pub fn close(&mut self) {
        if matches!(self.slot, Slot::Empty | Slot::Value(_)) {
            self.slot = Slot::Dropped;
        }
    }

    /// The last [`Sender`] handle was dropped.
    pub fn sender_gone(&mut self) {
        self.sender_alive = false;
    }

    /// The last [`Receiver`] handle was dropped (same as
    /// [`OneshotCore::close`]).
    pub fn receiver_gone(&mut self) {
        self.receiver_alive = false;
        self.close();
    }

    /// True once a `send` can no longer succeed.
    pub fn is_closed(&self) -> bool {
        !matches!(self.slot, Slot::Empty) || !self.receiver_alive
    }

    /// True if the receiver went away without ever receiving a value.
    pub fn is_abandoned(&self) -> bool {
        matches!(self.slot, Slot::Dropped)
    }

    /// True while the value sits in the channel, unreceived.
    pub fn has_value(&self) -> bool {
        matches!(self.slot, Slot::Value(_))
    }
}

// ---- errors ----

/// The receiver is gone: the value comes back.
#[derive(Debug, PartialEq, Eq)]
pub struct SendError<T>(pub T);

/// The channel was closed without a value ever being sent.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RecvError;

/// [`Receiver::try_recv`] failed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TryRecvError {
    /// No value yet, and the sender may still send one.
    Empty,
    /// The sender is gone (or the value was already received).
    Closed,
}

// ---- handles ----

struct Shared<T> {
    lock: Lock<OneshotCore<T>>,
}

/// The sending half. `send` consumes it: exactly one value.
pub struct Sender<T> {
    shared: Arc<Shared<T>>,
}

/// The receiving half. `recv` consumes it: exactly one read.
pub struct Receiver<T> {
    shared: Arc<Shared<T>>,
}

/// Create a oneshot channel holding a single value.
pub fn channel<T>() -> (Sender<T>, Receiver<T>) {
    let shared = Arc::new(Shared {
        lock: Lock::new(OneshotCore::new()),
    });
    (
        Sender {
            shared: shared.clone(),
        },
        Receiver { shared },
    )
}

impl<T> Sender<T> {
    /// Send the value, consuming the sender. Fails if the receiver is gone,
    /// handing the value back.
    pub fn send(self, value: T) -> Result<(), SendError<T>> {
        let mut guard = self.shared.lock.lock();
        match guard.send(value) {
            Ok(()) => {
                drop(guard);
                self.shared.lock.notify_all();
                Ok(())
            }
            Err(SendError(value)) => Err(SendError(value)),
        }
    }

    /// True once a `send` can no longer succeed.
    pub fn is_closed(&self) -> bool {
        self.shared.lock.lock().is_closed()
    }

    /// True if the receiver went away without ever receiving a value.
    pub fn is_abandoned(&self) -> bool {
        self.shared.lock.lock().is_abandoned()
    }
}

impl<T> Drop for Sender<T> {
    fn drop(&mut self) {
        let mut guard = self.shared.lock.lock();
        guard.sender_gone();
        drop(guard);
        self.shared.lock.notify_all();
    }
}

impl<T> Receiver<T> {
    /// Wait for the value, consuming the receiver. Fails if the sender is
    /// gone without sending.
    #[cfg(not(target_arch = "wasm32"))]
    pub fn recv(self) -> Result<T, RecvError> {
        // Take the shared state, then suppress the shell's `Drop`: a
        // consuming `recv` must not look like "the receiver was dropped".
        let shared = self.shared.clone();
        // `_this` stays alive to the end of the fn, but never runs `Drop`.
        let _this = std::mem::ManuallyDrop::new(self);
        let mut guard = shared.lock.lock();
        loop {
            match guard.poll_recv() {
                PollRecv::Ready => {
                    let value = guard.try_recv().map_err(|_| RecvError);
                    guard.receiver_gone();
                    drop(guard);
                    shared.lock.notify_all();
                    return value;
                }
                PollRecv::Closed => {
                    guard.receiver_gone();
                    drop(guard);
                    shared.lock.notify_all();
                    return Err(RecvError);
                }
                PollRecv::Pending => {}
            }
            guard = shared.lock.wait(guard);
        }
    }

    /// Take the value if it has already arrived, without waiting.
    pub fn try_recv(&mut self) -> Result<T, TryRecvError> {
        self.shared.lock.lock().try_recv()
    }

    /// Close the channel without receiving: pending sends fail.
    pub fn close(&mut self) {
        let mut guard = self.shared.lock.lock();
        guard.close();
        drop(guard);
        self.shared.lock.notify_all();
    }
}

impl<T> Drop for Receiver<T> {
    fn drop(&mut self) {
        let mut guard = self.shared.lock.lock();
        guard.receiver_gone();
        drop(guard);
        self.shared.lock.notify_all();
    }
}

#[cfg(test)]
mod core_tests {
    use super::*;

    #[test]
    fn value_lands_and_is_consumed_once() {
        let mut core = OneshotCore::new();
        assert_eq!(core.poll_recv(), PollRecv::Pending);
        assert_eq!(core.try_recv(), Err(TryRecvError::Empty));

        assert_eq!(core.send('a'), Ok(()));
        assert!(core.has_value());
        assert_eq!(core.try_recv(), Ok('a'));
        assert_eq!(core.try_recv(), Err(TryRecvError::Closed));
        assert_eq!(core.poll_recv(), PollRecv::Closed);
    }

    #[test]
    fn receiver_gone_before_send_fails_the_send() {
        let mut core = OneshotCore::new();
        core.receiver_gone();
        assert!(core.is_closed());
        assert_eq!(core.send('a'), Err(SendError('a')));
        assert_eq!(core.try_recv(), Err(TryRecvError::Closed));
    }

    #[test]
    fn close_abandons_even_a_sent_value() {
        let mut core = OneshotCore::new();
        assert_eq!(core.send('a'), Ok(()));
        assert!(!core.is_abandoned(), "not abandoned until closed");
        core.close();
        assert!(core.is_abandoned());
        assert_eq!(core.try_recv(), Err(TryRecvError::Closed));
    }

    #[test]
    fn sender_gone_leaves_a_sent_value_receivable() {
        let mut core = OneshotCore::new();
        assert_eq!(core.send('a'), Ok(()));
        core.sender_gone();
        assert_eq!(core.poll_recv(), PollRecv::Ready, "value still lands");
        assert_eq!(core.try_recv(), Ok('a'));

        let mut empty = OneshotCore::<char>::new();
        empty.sender_gone();
        assert_eq!(empty.poll_recv(), PollRecv::Closed);
        assert_eq!(empty.try_recv(), Err(TryRecvError::Closed));
    }

    #[test]
    fn second_send_fails_at_core_level() {
        let mut core = OneshotCore::new();
        assert_eq!(core.send('a'), Ok(()));
        assert_eq!(core.send('b'), Err(SendError('b')));
        assert_eq!(core.try_recv(), Ok('a'));
    }
}

#[cfg(not(target_arch = "wasm32"))]
#[cfg(test)]
mod handle_tests {
    use super::*;
    use std::sync::mpsc as std_mpsc;
    use std::thread;

    #[test]
    fn recv_blocks_until_send_arrives() {
        let (tx, rx) = channel::<char>();
        let (done_tx, done_rx) = std_mpsc::channel();
        let reader = thread::spawn(move || {
            let value = rx.recv();
            done_tx.send(()).expect("report done");
            value
        });
        // give the reader a chance to park, then resolve the channel
        thread::sleep(std::time::Duration::from_millis(20));
        assert!(done_rx
            .recv_timeout(std::time::Duration::from_millis(1))
            .is_err());
        tx.send('a').expect("receiver alive");
        assert_eq!(reader.join().expect("reader thread"), Ok('a'));
    }

    #[test]
    fn dropping_the_sender_wakes_an_empty_recv() {
        let (tx, rx) = channel::<char>();
        let reader = thread::spawn(move || rx.recv());
        thread::sleep(std::time::Duration::from_millis(20));
        drop(tx);
        assert_eq!(reader.join().expect("reader thread"), Err(RecvError));
    }

    #[test]
    fn send_fails_when_receiver_was_dropped() {
        let (tx, rx) = channel::<char>();
        drop(rx);
        assert!(tx.is_closed());
        assert!(tx.is_abandoned());
        assert_eq!(tx.send('a'), Err(SendError('a')));
    }

    #[test]
    fn try_recv_after_close_reports_closed() {
        let (_tx, mut rx) = channel::<char>();
        rx.close();
        assert_eq!(rx.try_recv(), Err(TryRecvError::Closed));
    }
}
