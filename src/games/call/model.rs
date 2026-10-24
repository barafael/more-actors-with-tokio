//! Call and response: an actor's `mpsc` inbox carrying `oneshot` callbacks.
//!
//! The recipe's `get_unique_id`, played out. Every requester sends
//! `GetUniqueId { callback }` into the actor's bounded inbox and then awaits
//! its `oneshot::Receiver`. The player is also the actor's event loop —
//! nothing gets answered unless they receive a message and use its callback
//! — so the game shows the price of the pattern the article warns about:
//! every caller is blocked on one loop's attention.
//!
//! The inbox is an [`MpscCore`] and each callback a [`OneshotCore`], so a
//! full inbox parks senders FIFO, a dropped receiver makes the actor's send
//! fail with the value handed back, and a dropped callback resolves the
//! receiver with `RecvError` — all by the cores' rules, not by this file's.

use std::collections::BTreeMap;

use sim_channels::mpsc::{MpscCore, RecvPoll, SendOffer, SendPoll};
use sim_channels::oneshot::{OneshotCore, TryRecvError};
use sim_channels::WaiterId;

/// How many requests the actor's inbox buffers before a requester parks.
pub const CAPACITY: usize = 4;

/// The requesters, each holding a clone of the actor's `Sender`.
pub const TASKS: [u64; 3] = [1, 2, 3];

/// One `GetUniqueId { callback }` message, named by the requester who sent
/// it. `listening` is false once the requester dropped its
/// `oneshot::Receiver`: the callback still travels, but a send on it will
/// fail.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Request {
    pub task: u64,
    pub listening: bool,
}

/// Where one requester's `get_unique_id(&tx).await` stands.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum CallPhase {
    #[default]
    Idle,
    /// `send().await` parked on the full inbox.
    Sending,
    /// Sent; awaiting the oneshot receiver.
    Awaiting,
    /// The reply arrived.
    Got(u32),
    /// The actor dropped the callback: the receiver yields `RecvError`.
    Closed,
    /// The requester dropped its receiver and stopped waiting.
    GaveUp,
}

impl CallPhase {
    /// Whether this is a call still waiting on the actor.
    pub fn waiting(self) -> bool {
        matches!(self, CallPhase::Sending | CallPhase::Awaiting)
    }
}

/// What the actor's last use of a callback yielded.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CallOutcome {
    /// `callback.send(id)` returned `Ok(())`.
    Replied { task: u64, id: u32 },
    /// `callback.send(id)` returned `Err(id)`: the receiver was dropped.
    Unheard { task: u64, id: u32 },
    /// The callback was dropped unused.
    Dropped { task: u64 },
}

impl CallOutcome {
    pub fn task(self) -> u64 {
        match self {
            CallOutcome::Replied { task, .. }
            | CallOutcome::Unheard { task, .. }
            | CallOutcome::Dropped { task } => task,
        }
    }
}

/// The message in the inbox: which request, from whom. Its callback lives in
/// [`CallSim::callbacks`] under the same `req`, standing in for the
/// `oneshot::Sender` the message owns.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Message {
    req: u64,
    task: u64,
}

#[derive(Debug)]
struct Requester {
    task: u64,
    phase: CallPhase,
    /// The outstanding request and when it started.
    current: Option<(u64, f64)>,
    /// Set while this requester's send is parked on the full inbox.
    parked: Option<WaiterId>,
}

pub struct CallSim {
    now: f64,
    inbox: MpscCore<Message>,
    /// Each request's oneshot. Present from `request` until both halves are
    /// gone; the receiver half is the requester, the sender half travels
    /// inside the message.
    callbacks: BTreeMap<u64, OneshotCore<u32>>,
    requesters: Vec<Requester>,
    in_hand: Option<Message>,
    next_id: u32,
    next_req: u64,
    last: Option<CallOutcome>,
}

impl Default for CallSim {
    fn default() -> Self {
        Self::new(CAPACITY)
    }
}

impl CallSim {
    pub fn new(capacity: usize) -> Self {
        // the core counts the sender `channel()` returns; each further
        // requester holds a clone
        let mut inbox = MpscCore::new(capacity);
        for _ in 1..TASKS.len() {
            inbox.add_sender();
        }
        let requesters = TASKS
            .map(|task| Requester {
                task,
                phase: CallPhase::Idle,
                current: None,
                parked: None,
            })
            .into();
        Self {
            now: 0.0,
            inbox,
            callbacks: BTreeMap::new(),
            requesters,
            in_hand: None,
            next_id: 0,
            next_req: 1,
            last: None,
        }
    }

    pub fn sync_now(&mut self, now: f64) {
        self.now = now;
    }

    pub fn capacity(&self) -> usize {
        self.inbox.capacity()
    }

    /// The actor's state: the id the next reply will carry.
    pub fn next_id(&self) -> u32 {
        self.next_id
    }

    /// The inbox, oldest first.
    pub fn queue(&self) -> Vec<Request> {
        self.inbox.buffer().map(|m| self.view(m)).collect()
    }

    /// Sends parked on the full inbox, in wake order.
    pub fn parked(&self) -> Vec<Request> {
        self.inbox.blocked().map(|(_, m)| self.view(m)).collect()
    }

    /// The message the actor has received and not yet answered.
    pub fn in_hand(&self) -> Option<Request> {
        self.in_hand.as_ref().map(|m| self.view(m))
    }

    pub fn last(&self) -> Option<CallOutcome> {
        self.last
    }

    /// Each requester's phase, and since when its current call has waited.
    pub fn requesters(&self) -> impl Iterator<Item = (u64, CallPhase, Option<f64>)> + '_ {
        self.requesters
            .iter()
            .map(|r| (r.task, r.phase, r.current.map(|(_, since)| since)))
    }

    fn view(&self, message: &Message) -> Request {
        Request {
            task: message.task,
            listening: self
                .callbacks
                .get(&message.req)
                .is_some_and(|callback| !callback.is_abandoned()),
        }
    }

    fn requester(&mut self, task: u64) -> Option<&mut Requester> {
        self.requesters.iter_mut().find(|r| r.task == task)
    }

    /// A requester: `get_unique_id(&tx).await`, then await the reply.
    pub fn request(&mut self, task: u64) {
        let Some(index) = self.requesters.iter().position(|r| r.task == task) else {
            return;
        };
        // One call at a time per requester: it is awaiting the one it made.
        if self.requesters[index].phase.waiting() {
            return;
        }
        let req = self.next_req;
        self.next_req += 1;
        self.callbacks.insert(req, OneshotCore::new());
        let offer = self.inbox.offer_send(Message { req, task });
        let requester = &mut self.requesters[index];
        requester.current = Some((req, self.now));
        match offer {
            SendOffer::Accepted => requester.phase = CallPhase::Awaiting,
            SendOffer::Blocked { waiter } => {
                requester.phase = CallPhase::Sending;
                requester.parked = Some(waiter);
            }
            // the actor's receiver never drops in this game
            SendOffer::Rejected(_) => {
                requester.phase = CallPhase::Closed;
                requester.current = None;
            }
        }
    }

    /// A requester: drop the receiver (or the parked send) and stop waiting.
    pub fn give_up(&mut self, task: u64) {
        let Some(requester) = self.requester(task) else {
            return;
        };
        let parked = requester.parked.take();
        let current = requester.current.take();
        if !requester.phase.waiting() {
            return;
        }
        requester.phase = CallPhase::GaveUp;
        // A parked send is a future that owns the message: dropping it takes
        // the message, callback and all, back out of the queue.
        if let Some(waiter) = parked {
            self.inbox.cancel_send(waiter);
            if let Some((req, _)) = current {
                self.callbacks.remove(&req);
            }
            return;
        }
        // Otherwise the message is already delivered or in hand; only the
        // receiver drops, and the callback travels on regardless.
        if let Some((req, _)) = current {
            if let Some(callback) = self.callbacks.get_mut(&req) {
                callback.receiver_gone();
            }
        }
    }

    /// The actor: `rx.recv().await` one message into hand.
    pub fn recv(&mut self) {
        if self.in_hand.is_some() {
            return;
        }
        let RecvPoll::Value(message) = self.inbox.poll_recv() else {
            return;
        };
        self.in_hand = Some(message);
        self.wake_parked();
    }

    /// A receive frees a slot and the core moves the longest-parked send
    /// into it; tell that requester its `send().await` returned.
    fn wake_parked(&mut self) {
        for index in 0..self.requesters.len() {
            let Some(waiter) = self.requesters[index].parked else {
                continue;
            };
            if self.inbox.poll_send(waiter) == SendPoll::Accepted {
                let requester = &mut self.requesters[index];
                requester.parked = None;
                requester.phase = CallPhase::Awaiting;
            }
        }
    }

    /// The actor: `callback.send(self.next_id)`, then `next_id += 1`.
    pub fn reply(&mut self) {
        let Some(message) = self.in_hand.take() else {
            return;
        };
        let id = self.next_id;
        // `let _ = callback.send(self.next_id); self.next_id += 1;` — the
        // article's handler, which counts up whether anyone heard or not.
        self.next_id += 1;
        let Some(mut callback) = self.callbacks.remove(&message.req) else {
            return;
        };
        if callback.send(id).is_err() {
            self.last = Some(CallOutcome::Unheard {
                task: message.task,
                id,
            });
            return;
        }
        self.last = Some(CallOutcome::Replied {
            task: message.task,
            id,
        });
        if let Ok(value) = callback.try_recv() {
            self.settle(message, CallPhase::Got(value));
        }
    }

    /// The actor: drop the callback without answering.
    pub fn drop_callback(&mut self) {
        let Some(message) = self.in_hand.take() else {
            return;
        };
        self.last = Some(CallOutcome::Dropped { task: message.task });
        let Some(mut callback) = self.callbacks.remove(&message.req) else {
            return;
        };
        callback.sender_gone();
        if callback.try_recv() == Err(TryRecvError::Closed) {
            self.settle(message, CallPhase::Closed);
        }
    }

    /// Resolve the requester waiting on `message`, if it still is.
    fn settle(&mut self, message: Message, phase: CallPhase) {
        let Some(requester) = self.requester(message.task) else {
            return;
        };
        if requester.phase == CallPhase::Awaiting
            && requester.current.map(|(req, _)| req) == Some(message.req)
        {
            requester.phase = phase;
            requester.current = None;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn phase(sim: &CallSim, task: u64) -> CallPhase {
        sim.requesters()
            .find(|(t, _, _)| *t == task)
            .map(|(_, phase, _)| phase)
            .unwrap()
    }

    fn queued(sim: &CallSim) -> Vec<u64> {
        sim.queue().iter().map(|r| r.task).collect()
    }

    #[test]
    fn requests_queue_in_order_and_are_answered_with_rising_ids() {
        let mut sim = CallSim::new(4);
        sim.request(2);
        sim.request(1);
        assert_eq!(queued(&sim), [2, 1]);
        assert_eq!(phase(&sim, 1), CallPhase::Awaiting);

        for _ in 0..2 {
            sim.recv();
            sim.reply();
        }
        assert_eq!(phase(&sim, 2), CallPhase::Got(0));
        assert_eq!(phase(&sim, 1), CallPhase::Got(1));
        assert_eq!(sim.next_id(), 2);
    }

    #[test]
    fn a_full_inbox_parks_the_next_requester_until_a_recv() {
        let mut sim = CallSim::new(1);
        sim.request(1);
        sim.request(2);
        assert_eq!(phase(&sim, 2), CallPhase::Sending);
        assert_eq!(sim.parked().len(), 1);

        sim.recv();
        assert_eq!(
            phase(&sim, 2),
            CallPhase::Awaiting,
            "the freed slot took it"
        );
        assert_eq!(queued(&sim), [2]);
    }

    #[test]
    fn a_dropped_callback_resolves_the_receiver_with_an_error() {
        let mut sim = CallSim::new(4);
        sim.request(1);
        sim.recv();
        sim.drop_callback();
        assert_eq!(phase(&sim, 1), CallPhase::Closed);
        assert_eq!(sim.last(), Some(CallOutcome::Dropped { task: 1 }));
        assert_eq!(sim.next_id(), 0, "no id was spent");
    }

    #[test]
    fn replying_to_a_requester_who_gave_up_hands_the_value_back() {
        let mut sim = CallSim::new(4);
        sim.request(1);
        sim.give_up(1);
        assert_eq!(phase(&sim, 1), CallPhase::GaveUp);
        assert!(!sim.queue()[0].listening, "the message still travels");

        sim.recv();
        sim.reply();
        assert_eq!(sim.last(), Some(CallOutcome::Unheard { task: 1, id: 0 }));
        assert_eq!(sim.next_id(), 1, "the handler counts up regardless");
        assert_eq!(phase(&sim, 1), CallPhase::GaveUp);
    }

    #[test]
    fn giving_up_a_parked_send_takes_the_message_back_out() {
        let mut sim = CallSim::new(1);
        sim.request(1);
        sim.request(2);
        sim.give_up(2);
        assert!(sim.parked().is_empty());
        sim.recv();
        assert!(queued(&sim).is_empty(), "nothing moved into the freed slot");
    }

    #[test]
    fn a_requester_can_ask_again_once_answered() {
        let mut sim = CallSim::new(4);
        sim.request(1);
        sim.request(1);
        assert_eq!(queued(&sim).len(), 1, "one call at a time");
        sim.recv();
        sim.reply();
        sim.request(1);
        assert_eq!(phase(&sim, 1), CallPhase::Awaiting);
    }

    #[test]
    fn the_wait_counts_from_the_request() {
        let mut sim = CallSim::new(1);
        sim.sync_now(1_000.0);
        sim.request(1);
        sim.sync_now(2_500.0);
        sim.request(2);
        let since: Vec<Option<f64>> = sim.requesters().map(|(_, _, since)| since).collect();
        assert_eq!(since, [Some(1_000.0), Some(2_500.0), None]);
    }
}
