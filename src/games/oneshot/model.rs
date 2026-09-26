//! One `oneshot` channel, held in both hands: an exercise in ownership.
//!
//! Every action consumes something. `tx.send(v)` takes `tx` by value, so a
//! sender sends once, whatever the outcome. `rx.await` takes `rx` by value,
//! so dropping that future drops the receiver with it. Dropping either half
//! is how the other half learns the conversation is over: a send to a
//! dropped receiver hands the value back, an await on a dropped sender
//! yields `RecvError`.
//!
//! The channel itself is an [`OneshotCore`]; this only tracks which handles
//! the player still owns and records what each call returned.

use sim_channels::oneshot::{OneshotCore, PollRecv};

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Tx {
    /// No channel yet.
    #[default]
    None,
    Held,
    /// Moved into `send`: gone, whatever it returned.
    Spent,
    Dropped,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Rx {
    #[default]
    None,
    Held,
    /// Moved into a pending `rx.await`.
    Awaiting,
    /// The await finished: `Ok(v)` or `Err(RecvError)`.
    Done(Result<char, ()>),
    Dropped,
}

/// What the slot inside the channel holds, for drawing it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Slot {
    Empty,
    Holds(char),
    Taken,
    Closed,
}

/// One call and what it returned, as the player would read it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Line {
    pub call: String,
    pub result: String,
    pub ok: bool,
}

#[derive(Debug, Default)]
pub struct OneshotModel {
    core: Option<OneshotCore<char>>,
    /// The value in flight, for drawing: the core does not lend it out.
    sent: Option<char>,
    pub tx: Tx,
    pub rx: Rx,
    pub log: Vec<Line>,
}

/// How many lines of history to keep.
const LOG_LINES: usize = 4;

impl OneshotModel {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn exists(&self) -> bool {
        self.core.is_some()
    }

    /// Both halves consumed: nothing left to do but make another channel.
    pub fn finished(&self) -> bool {
        self.exists() && !matches!(self.tx, Tx::Held) && !matches!(self.rx, Rx::Held | Rx::Awaiting)
    }

    pub fn slot(&self) -> Slot {
        match (&self.core, self.sent, self.rx) {
            (None, _, _) => Slot::Empty,
            (Some(core), Some(value), _) if core.has_value() => Slot::Holds(value),
            (Some(_), _, Rx::Done(Ok(_))) => Slot::Taken,
            (Some(core), _, _) if core.is_closed() || matches!(self.tx, Tx::Dropped) => {
                Slot::Closed
            }
            _ => Slot::Empty,
        }
    }

    fn log(&mut self, call: &str, result: impl Into<String>, ok: bool) {
        self.log.push(Line {
            call: call.to_string(),
            result: result.into(),
            ok,
        });
        let excess = self.log.len().saturating_sub(LOG_LINES);
        self.log.drain(..excess);
    }

    pub fn create(&mut self) {
        *self = Self {
            core: Some(OneshotCore::new()),
            tx: Tx::Held,
            rx: Rx::Held,
            log: std::mem::take(&mut self.log),
            sent: None,
        };
        self.log("oneshot::channel()", "(tx, rx)", true);
    }

    pub fn send(&mut self, value: char) {
        let Some(core) = self.core.as_mut().filter(|_| self.tx == Tx::Held) else {
            return;
        };
        self.tx = Tx::Spent;
        // `send` consumed the sender, so it is gone whichever way this goes
        core.sender_gone();
        let call = format!("tx.send('{value}')");
        match core.send(value) {
            Ok(()) => {
                self.sent = Some(value);
                self.log(&call, "Ok(())", true);
                self.wake();
            }
            Err(error) => {
                let back = error.0;
                self.log(&call, format!("Err('{back}') — handed back"), false);
            }
        }
    }

    pub fn drop_tx(&mut self) {
        let Some(core) = self.core.as_mut().filter(|_| self.tx == Tx::Held) else {
            return;
        };
        core.sender_gone();
        self.tx = Tx::Dropped;
        self.log("drop(tx)", "()", true);
        self.wake();
    }

    pub fn await_rx(&mut self) {
        if self.rx != Rx::Held || self.core.is_none() {
            return;
        }
        self.rx = Rx::Awaiting;
        self.wake();
        if self.rx == Rx::Awaiting {
            self.log("rx.await", "Pending…", true);
        }
    }

    /// Drop the pending `rx.await` future — and the receiver it owns.
    pub fn drop_future(&mut self) {
        if self.rx != Rx::Awaiting {
            return;
        }
        if let Some(core) = self.core.as_mut() {
            core.receiver_gone();
        }
        self.rx = Rx::Dropped;
        self.log("drop(rx.await)", "rx went with it", false);
    }

    pub fn drop_rx(&mut self) {
        let Some(core) = self.core.as_mut().filter(|_| self.rx == Rx::Held) else {
            return;
        };
        core.receiver_gone();
        self.rx = Rx::Dropped;
        self.log("drop(rx)", "()", true);
    }

    /// A pending await re-polls whenever the other half does something.
    fn wake(&mut self) {
        if self.rx != Rx::Awaiting {
            return;
        }
        let Some(core) = self.core.as_mut() else {
            return;
        };
        match core.poll_recv() {
            PollRecv::Pending => {}
            PollRecv::Ready => {
                let value = core.try_recv().ok();
                if let Some(value) = value {
                    self.rx = Rx::Done(Ok(value));
                    self.log("rx.await", format!("Ok('{value}')"), true);
                }
            }
            PollRecv::Closed => {
                self.rx = Rx::Done(Err(()));
                self.log("rx.await", "Err(RecvError)", false);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fresh() -> OneshotModel {
        let mut model = OneshotModel::new();
        model.create();
        model
    }

    fn last(model: &OneshotModel) -> &str {
        &model.log.last().unwrap().result
    }

    #[test]
    fn send_then_await_moves_one_value_and_consumes_both_halves() {
        let mut model = fresh();
        model.send('a');
        assert_eq!(model.tx, Tx::Spent);
        assert_eq!(model.slot(), Slot::Holds('a'), "it waits in the channel");
        model.await_rx();
        assert_eq!(model.rx, Rx::Done(Ok('a')));
        assert_eq!(model.slot(), Slot::Taken);
        assert!(model.finished());
    }

    #[test]
    fn an_early_await_parks_until_the_send() {
        let mut model = fresh();
        model.await_rx();
        assert_eq!(model.rx, Rx::Awaiting);
        model.send('b');
        assert_eq!(model.rx, Rx::Done(Ok('b')));
    }

    #[test]
    fn a_sender_sends_once() {
        let mut model = fresh();
        model.send('a');
        model.send('b');
        assert_eq!(model.log.len(), 2, "the second send had no sender to call");
    }

    #[test]
    fn sending_to_a_dropped_receiver_hands_the_value_back() {
        let mut model = fresh();
        model.drop_rx();
        model.send('c');
        assert_eq!(last(&model), "Err('c') — handed back");
        assert_eq!(model.tx, Tx::Spent, "the sender is consumed either way");
        assert!(model.finished());
    }

    #[test]
    fn dropping_the_sender_wakes_the_receiver_with_an_error() {
        let mut model = fresh();
        model.await_rx();
        model.drop_tx();
        assert_eq!(model.rx, Rx::Done(Err(())));
        assert_eq!(model.slot(), Slot::Closed);
    }

    #[test]
    fn awaiting_after_the_sender_is_gone_fails_at_once() {
        let mut model = fresh();
        model.drop_tx();
        model.await_rx();
        assert_eq!(last(&model), "Err(RecvError)");
    }

    #[test]
    fn dropping_the_await_drops_the_receiver_it_owned() {
        let mut model = fresh();
        model.await_rx();
        model.drop_future();
        assert_eq!(model.rx, Rx::Dropped);
        model.send('d');
        assert_eq!(last(&model), "Err('d') — handed back");
    }

    #[test]
    fn a_new_channel_keeps_the_history_short() {
        let mut model = fresh();
        for _ in 0..5 {
            model.create();
        }
        assert_eq!(model.log.len(), LOG_LINES);
        assert_eq!(model.tx, Tx::Held);
    }
}
