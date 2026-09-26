//! Call and response: an actor's `mpsc` inbox carrying `oneshot` callbacks.
//!
//! The recipe's `get_unique_id`, played by the room. Every phone is a
//! requester: it sends `GetUniqueId { callback }` into the actor's bounded
//! inbox and then awaits its `oneshot::Receiver`. The presenter *is* the
//! actor's event loop — nothing gets answered unless they receive a message
//! and use its callback — so the room feels the price of the pattern the
//! article warns about: every caller is blocked on one loop's attention.
//!
//! The inbox is an [`MpscCore`] and each callback a [`OneshotCore`], so a
//! full inbox parks senders FIFO, a dropped receiver makes the actor's send
//! fail with the value handed back, and a dropped callback resolves the
//! receiver with `RecvError` — all by the cores' rules, not by this file's.

use std::collections::BTreeMap;

use sim_channels::mpsc::{MpscCore, RecvPoll, SendOffer, SendPoll};
use sim_channels::oneshot::{OneshotCore, TryRecvError};
use sim_channels::WaiterId;

use crate::protocol::{CallOutcome, CallPhase, CallRequest, CallSnapshot, CallTask, CallWire};

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
    owner: u64,
    phase: CallPhase,
    /// The outstanding request and when it started.
    current: Option<(u64, f64)>,
    /// Set while this requester's send is parked on the full inbox.
    parked: Option<WaiterId>,
}

pub struct CallSim {
    now: f64,
    inbox: MpscCore<Message>,
    /// Each request's oneshot. Present from `Request` until both halves are
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
        Self::new(crate::protocol::CALL_CAPACITY)
    }
}

impl CallSim {
    pub fn new(capacity: usize) -> Self {
        Self {
            now: 0.0,
            inbox: MpscCore::new(capacity),
            callbacks: BTreeMap::new(),
            requesters: Vec::new(),
            in_hand: None,
            next_id: 0,
            next_req: 1,
            last: None,
        }
    }

    pub fn sync_now(&mut self, now: f64) {
        self.now = now;
    }

    pub fn has_task(&self, task: u64) -> bool {
        self.requesters.iter().any(|r| r.task == task)
    }

    /// A requester appears holding a clone of the actor's `Sender`.
    pub fn add_task(&mut self, task: u64, owner: u64) {
        if self.has_task(task) {
            return;
        }
        self.inbox.add_sender();
        self.requesters.push(Requester {
            task,
            owner,
            phase: CallPhase::Idle,
            current: None,
            parked: None,
        });
    }

    /// A requester goes away: its receiver drops and a parked send is
    /// cancelled, as when a task is dropped mid-await.
    pub fn remove_task(&mut self, task: u64) {
        self.give_up(task);
        if let Some(index) = self.requesters.iter().position(|r| r.task == task) {
            self.requesters.remove(index);
            self.inbox.drop_sender();
        }
    }

    fn requester(&mut self, task: u64) -> Option<&mut Requester> {
        self.requesters.iter_mut().find(|r| r.task == task)
    }

    /// Apply a wire. `host` is whether the caller is the actor's loop —
    /// the presenter in the talk, the lone player in the export.
    pub fn handle(&mut self, task: u64, host: bool, wire: CallWire) {
        match wire {
            CallWire::Request => self.request(task),
            CallWire::GiveUp => self.give_up(task),
            CallWire::Recv if host => self.recv(),
            CallWire::Reply if host => self.reply(),
            CallWire::DropCallback if host => self.drop_callback(),
            CallWire::Recv | CallWire::Reply | CallWire::DropCallback => {}
        }
    }

    fn request(&mut self, task: u64) {
        let Some(index) = self.requesters.iter().position(|r| r.task == task) else {
            return;
        };
        // One call at a time per requester: it is awaiting the one it made.
        if matches!(
            self.requesters[index].phase,
            CallPhase::Sending | CallPhase::Awaiting
        ) {
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

    fn give_up(&mut self, task: u64) {
        let Some(requester) = self.requesters.iter_mut().find(|r| r.task == task) else {
            return;
        };
        let parked = requester.parked.take();
        let current = requester.current.take();
        if !matches!(requester.phase, CallPhase::Sending | CallPhase::Awaiting) {
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

    fn recv(&mut self) {
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

    fn reply(&mut self) {
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

    fn drop_callback(&mut self) {
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

    fn listening(&self, req: u64) -> bool {
        self.callbacks
            .get(&req)
            .is_some_and(|callback| !callback.is_abandoned())
    }

    pub fn snapshot(&self) -> CallSnapshot {
        let view = |message: &Message| CallRequest {
            task: message.task,
            listening: self.listening(message.req),
        };
        CallSnapshot {
            capacity: self.inbox.capacity(),
            next_id: self.next_id,
            queue: self.inbox.buffer().map(view).collect(),
            parked: self
                .inbox
                .blocked()
                .map(|(_, message)| view(message))
                .collect(),
            in_hand: self.in_hand.as_ref().map(view),
            tasks: self
                .requesters
                .iter()
                .map(|r| CallTask {
                    task: r.task,
                    owner: r.owner,
                    phase: r.phase,
                    for_ms: r
                        .current
                        .map(|(_, since)| (self.now - since).max(0.0))
                        .unwrap_or_default(),
                })
                .collect(),
            last: self.last,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn room(capacity: usize, tasks: &[u64]) -> CallSim {
        let mut sim = CallSim::new(capacity);
        for &task in tasks {
            sim.add_task(task, task);
        }
        sim
    }

    fn phase(sim: &CallSim, task: u64) -> CallPhase {
        sim.snapshot()
            .tasks
            .iter()
            .find(|t| t.task == task)
            .map(|t| t.phase)
            .unwrap()
    }

    fn queued(sim: &CallSim) -> Vec<u64> {
        sim.snapshot().queue.iter().map(|r| r.task).collect()
    }

    #[test]
    fn requests_queue_in_order_and_are_answered_with_rising_ids() {
        let mut sim = room(4, &[1, 2]);
        sim.handle(2, false, CallWire::Request);
        sim.handle(1, false, CallWire::Request);
        assert_eq!(queued(&sim), [2, 1]);
        assert_eq!(phase(&sim, 1), CallPhase::Awaiting);

        for _ in 0..2 {
            sim.handle(0, true, CallWire::Recv);
            sim.handle(0, true, CallWire::Reply);
        }
        assert_eq!(phase(&sim, 2), CallPhase::Got(0));
        assert_eq!(phase(&sim, 1), CallPhase::Got(1));
        assert_eq!(sim.snapshot().next_id, 2);
    }

    #[test]
    fn only_the_host_runs_the_actor() {
        let mut sim = room(4, &[1]);
        sim.handle(1, false, CallWire::Request);
        sim.handle(1, false, CallWire::Recv);
        sim.handle(1, false, CallWire::Reply);
        assert_eq!(phase(&sim, 1), CallPhase::Awaiting);
        assert!(sim.snapshot().in_hand.is_none());
    }

    #[test]
    fn a_full_inbox_parks_the_next_requester_until_a_recv() {
        let mut sim = room(1, &[1, 2]);
        sim.handle(1, false, CallWire::Request);
        sim.handle(2, false, CallWire::Request);
        assert_eq!(phase(&sim, 2), CallPhase::Sending);
        assert_eq!(sim.snapshot().parked.len(), 1);

        sim.handle(0, true, CallWire::Recv);
        assert_eq!(
            phase(&sim, 2),
            CallPhase::Awaiting,
            "the freed slot took it"
        );
        assert_eq!(queued(&sim), [2]);
    }

    #[test]
    fn a_dropped_callback_resolves_the_receiver_with_an_error() {
        let mut sim = room(4, &[1]);
        sim.handle(1, false, CallWire::Request);
        sim.handle(0, true, CallWire::Recv);
        sim.handle(0, true, CallWire::DropCallback);
        assert_eq!(phase(&sim, 1), CallPhase::Closed);
        assert_eq!(sim.snapshot().last, Some(CallOutcome::Dropped { task: 1 }));
        assert_eq!(sim.snapshot().next_id, 0, "no id was spent");
    }

    #[test]
    fn replying_to_a_requester_who_gave_up_hands_the_value_back() {
        let mut sim = room(4, &[1]);
        sim.handle(1, false, CallWire::Request);
        sim.handle(1, false, CallWire::GiveUp);
        assert_eq!(phase(&sim, 1), CallPhase::GaveUp);
        assert!(
            !sim.snapshot().queue[0].listening,
            "the message still travels"
        );

        sim.handle(0, true, CallWire::Recv);
        sim.handle(0, true, CallWire::Reply);
        assert_eq!(
            sim.snapshot().last,
            Some(CallOutcome::Unheard { task: 1, id: 0 })
        );
        assert_eq!(
            sim.snapshot().next_id,
            1,
            "the handler counts up regardless"
        );
        assert_eq!(phase(&sim, 1), CallPhase::GaveUp);
    }

    #[test]
    fn giving_up_a_parked_send_takes_the_message_back_out() {
        let mut sim = room(1, &[1, 2]);
        sim.handle(1, false, CallWire::Request);
        sim.handle(2, false, CallWire::Request);
        sim.handle(2, false, CallWire::GiveUp);
        assert!(sim.snapshot().parked.is_empty());
        sim.handle(0, true, CallWire::Recv);
        assert!(queued(&sim).is_empty(), "nothing moved into the freed slot");
    }

    #[test]
    fn a_requester_can_ask_again_once_answered() {
        let mut sim = room(4, &[1]);
        sim.handle(1, false, CallWire::Request);
        sim.handle(1, false, CallWire::Request);
        assert_eq!(queued(&sim).len(), 1, "one call at a time");
        sim.handle(0, true, CallWire::Recv);
        sim.handle(0, true, CallWire::Reply);
        sim.handle(1, false, CallWire::Request);
        assert_eq!(phase(&sim, 1), CallPhase::Awaiting);
    }

    #[test]
    fn a_departing_requester_leaves_its_message_behind_unheard() {
        let mut sim = room(4, &[1]);
        sim.handle(1, false, CallWire::Request);
        sim.remove_task(1);
        sim.handle(0, true, CallWire::Recv);
        sim.handle(0, true, CallWire::Reply);
        assert_eq!(
            sim.snapshot().last,
            Some(CallOutcome::Unheard { task: 1, id: 0 })
        );
    }

    #[test]
    fn the_wait_counts_from_the_request() {
        let mut sim = room(1, &[1, 2]);
        sim.sync_now(1_000.0);
        sim.handle(1, false, CallWire::Request);
        sim.handle(2, false, CallWire::Request);
        sim.sync_now(4_000.0);
        let snap = sim.snapshot();
        assert!(snap.tasks.iter().all(|t| t.for_ms == 3_000.0));
    }
}
