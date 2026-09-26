//! The unit test from "More Actors with Tokio", recorded one line at a time.
//!
//! The test is deterministic — that is the whole point of it — so there is
//! nothing to simulate live. It runs once, here, and every executed line
//! leaves a [`Frame`]: the line, the call it happened in, what that line did
//! and the world after it. Stepping forward, back, into or over is then only
//! moving an index, and every frame can be asserted on below.
//!
//! Two variants break the test on purpose, one per precondition the article
//! leans on: the sender must be dropped before the loop is awaited, and the
//! buffer must hold every message enqueued before the loop starts.

use std::collections::VecDeque;

/// How many ids the test asks for.
pub const REQUESTS: usize = 3;

/// The test as the article writes it, or with one precondition removed.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Variant {
    #[default]
    AsWritten,
    /// `drop(tx)` commented out: the loop can never see `None`.
    ForgetDrop,
    /// `mpsc::channel(2)`: the third send has nowhere to go.
    TooSmall,
}

impl Variant {
    pub const ALL: [Variant; 3] = [Variant::AsWritten, Variant::ForgetDrop, Variant::TooSmall];

    pub fn label(self) -> &'static str {
        match self {
            Variant::AsWritten => "as written",
            Variant::ForgetDrop => "forget drop(tx)",
            Variant::TooSmall => "channel(2)",
        }
    }

    pub fn capacity(self) -> usize {
        match self {
            Variant::TooSmall => 2,
            _ => REQUESTS,
        }
    }

    fn drops_tx(self) -> bool {
        self != Variant::ForgetDrop
    }

    /// The test's source, with this variant's one change applied. Line
    /// numbers are identical across variants so the `L_*` indices hold.
    pub fn test_code(self) -> String {
        let capacity = self.capacity();
        let drop_line = if self.drops_tx() {
            "    drop(tx);"
        } else {
            "    // drop(tx);"
        };
        format!(
            r#"#[tokio::test]
async fn should_increment_unique_id() {{
    let actor = UniqueIdService::new();
    let (tx, rx) = mpsc::channel({capacity});

    let resp1 = UniqueIdService::get_unique_id(&tx).await.unwrap();
    let resp2 = UniqueIdService::get_unique_id(&tx).await.unwrap();
    let resp3 = UniqueIdService::get_unique_id(&tx).await.unwrap();

    // Important for this test:
{drop_line}

    let service = actor.event_loop(rx).await;

    let nums = tokio::try_join!(resp1, resp2, resp3).unwrap();
    assert_eq!(nums, (0, 1, 2));
    assert_eq!(service, UniqueIdService {{ next_id: 3 }});
}}"#
        )
    }
}

// Line indices into `Variant::test_code`, zero-based.
const L_ACTOR: usize = 2;
const L_CHANNEL: usize = 3;
const L_RESP: [usize; REQUESTS] = [5, 6, 7];
const L_DROP: usize = 10;
const L_LOOP: usize = 12;
const L_JOIN: usize = 14;
const L_ASSERT_NUMS: usize = 15;
const L_ASSERT_STATE: usize = 16;
const L_END: usize = 17;

/// The associated function the article uses instead of a handle type.
pub const GET_UNIQUE_ID: &str = r#"pub async fn get_unique_id(
    sender: &mpsc::Sender<Message>,
) -> Option<oneshot::Receiver<u32>> {
    let (callback, callback_receiver) = oneshot::channel();
    let message = Message::GetUniqueId { callback };

    sender.send(message).await.ok()?;
    Some(callback_receiver)
}"#;

const G_ONESHOT: usize = 3;
const G_MESSAGE: usize = 4;
const G_SEND: usize = 6;

/// The event loop as a consuming method, and the handler it calls.
pub const EVENT_LOOP: &str = r#"pub async fn event_loop(
    mut self,
    mut rx: mpsc::Receiver<Message>,
) -> Self {
    while let Some(message) = rx.recv().await {
        self.handle_message(message);
    }
    self
}

fn handle_message(&mut self, message: Message) {
    match message {
        Message::GetUniqueId { callback } => {
            let _ = callback.send(self.next_id);
            self.next_id += 1;
        }
    }
}"#;

const E_ENTER: usize = 0;
const E_RECV: usize = 4;
const E_RETURN: usize = 7;
const E_SEND: usize = 13;
const E_INC: usize = 14;

/// Where a binding in the test's scope stands.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Binding {
    /// Its `let` has not run yet.
    #[default]
    Undeclared,
    Live,
    /// Moved into the event loop by value.
    Moved,
    Dropped,
}

/// One `respN`: the oneshot receiver the test kept instead of awaiting.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Resp {
    #[default]
    Undeclared,
    Empty,
    Holds(u32),
    /// Consumed by `try_join!`.
    Joined,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum LoopState {
    #[default]
    NotStarted,
    Running,
    /// Awaiting `recv()` with nothing able to wake it.
    Parked,
    Returned,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Outcome {
    #[default]
    Running,
    Passed,
    Hangs,
}

/// A message being built inside `get_unique_id`, before it is sent.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct InHand {
    /// Which request, and so which oneshot pair.
    pub index: usize,
    /// Whether `callback` has been moved into `message` yet.
    pub packed: bool,
}

/// Everything the test, the channel and the actor hold after a line ran.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct World {
    pub actor: Binding,
    /// The actor's only state. `None` until `UniqueIdService::new()`.
    pub next_id: Option<u32>,
    pub service: Binding,
    pub tx: Binding,
    pub rx: Binding,
    /// The channel's bound; 0 until the channel exists.
    pub capacity: usize,
    /// Queued messages, oldest first, each named by the request it carries.
    pub buffer: VecDeque<usize>,
    /// A send waiting for room it will never get.
    pub send_parked: Option<usize>,
    pub in_hand: Option<InHand>,
    /// The message the loop has received and not yet answered.
    pub handling: Option<usize>,
    pub resps: [Resp; REQUESTS],
    pub nums: Option<[u32; REQUESTS]>,
    pub asserts_passed: usize,
    pub event_loop: LoopState,
    pub outcome: Outcome,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Callee {
    /// The `n`th call, zero-based.
    GetUniqueId(usize),
    EventLoop,
}

/// A line that ran inside a call the test made.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Call {
    pub callee: Callee,
    pub line: usize,
}

/// One executed line and the world it left behind.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Frame {
    /// The test line that ran, or is running if `call` is set. `None`
    /// before the test starts.
    pub test_line: Option<usize>,
    pub call: Option<Call>,
    /// What the line just did, with `code` in backticks.
    pub note: String,
    pub world: World,
}

/// The next frame at the test's own level: a whole call runs in one step.
/// From inside a call this steps out of it. If the test never gets back to
/// its own level, this is the last frame — where it stopped.
pub fn step_over(frames: &[Frame], at: usize) -> usize {
    frames
        .iter()
        .enumerate()
        .skip(at + 1)
        .find(|(_, frame)| frame.call.is_none())
        .map(|(index, _)| index)
        .unwrap_or(frames.len().saturating_sub(1))
}

struct Tracer {
    world: World,
    frames: Vec<Frame>,
}

impl Tracer {
    fn frame(&mut self, test_line: usize, call: Option<Call>, note: impl Into<String>) {
        self.frames.push(Frame {
            test_line: Some(test_line),
            call,
            note: note.into(),
            world: self.world.clone(),
        });
    }

    /// Runs `get_unique_id(&tx)` for request `index`. False if it parks.
    fn get_unique_id(&mut self, index: usize) -> bool {
        let line = L_RESP[index];
        let n = index + 1;
        let at = |line| {
            Some(Call {
                callee: Callee::GetUniqueId(index),
                line,
            })
        };

        self.world.in_hand = Some(InHand {
            index,
            packed: false,
        });
        self.frame(
            line,
            at(G_ONESHOT),
            format!("A fresh oneshot pair: `callback` will carry answer {n}, `callback_receiver` is where the test will read it."),
        );

        self.world.in_hand = Some(InHand {
            index,
            packed: true,
        });
        self.frame(
            line,
            at(G_MESSAGE),
            "`callback` moves into the message. Whoever handles this message now owns the duty to answer it.",
        );

        if self.world.buffer.len() == self.world.capacity {
            self.world.in_hand = None;
            self.world.send_parked = Some(index);
            self.world.outcome = Outcome::Hangs;
            self.frame(
                line,
                at(G_SEND),
                "The buffer is full and nothing is receiving: the event loop is only awaited further down, after this line. `send().await` parks forever. The test hangs before the actor handles a single message.",
            );
            return false;
        }

        self.world.in_hand = None;
        self.world.buffer.push_back(index);
        let (queued, capacity) = (self.world.buffer.len(), self.world.capacity);
        self.frame(
            line,
            at(G_SEND),
            format!("There is room ({queued} of {capacity} slots now taken), so `send().await` completes at once. The message just sits in the buffer: no task is running the actor."),
        );

        self.world.resps[index] = Resp::Empty;
        let note = if index == 0 {
            "`resp1` is the oneshot receiver, still empty. The test keeps it rather than awaiting it: awaiting now would wait forever, since nothing has handled the message. (`unwrap()` is on the `Option` — `send` only fails once `rx` is gone.)".to_string()
        } else {
            format!("`resp{n}` joins the others: another empty receiver, held for later.")
        };
        self.frame(line, None, note);
        true
    }

    /// Awaits `actor.event_loop(rx)` in place. False if the loop parks.
    fn event_loop(&mut self) -> bool {
        let at = |line| {
            Some(Call {
                callee: Callee::EventLoop,
                line,
            })
        };

        self.world.actor = Binding::Moved;
        self.world.rx = Binding::Moved;
        self.world.event_loop = LoopState::Running;
        self.frame(
            L_LOOP,
            at(E_ENTER),
            "`event_loop` takes `self` and `rx` by value, so `actor` and `rx` move out of the test's scope. Not spawned — just awaited, right here. While it runs, nothing else can touch the state.",
        );

        loop {
            if let Some(index) = self.world.buffer.pop_front() {
                let n = index + 1;
                self.world.handling = Some(index);
                self.frame(
                    L_LOOP,
                    at(E_RECV),
                    format!("`recv()` yields the oldest message: the one carrying callback {n}. First in, first out."),
                );

                let id = self.world.next_id.unwrap_or_default();
                self.world.handling = None;
                self.world.resps[index] = Resp::Holds(id);
                self.frame(
                    L_LOOP,
                    at(E_SEND),
                    format!("`callback.send({id})` lands in `resp{n}` now, although nobody is awaiting it. A oneshot send never waits."),
                );

                self.world.next_id = Some(id + 1);
                self.frame(
                    L_LOOP,
                    at(E_INC),
                    format!("`next_id` is {}. The state changed, and only the loop could have changed it.", id + 1),
                );
            } else if self.world.tx == Binding::Live {
                self.world.event_loop = LoopState::Parked;
                self.world.outcome = Outcome::Hangs;
                self.frame(
                    L_LOOP,
                    at(E_RECV),
                    "The buffer is empty, but `tx` is still alive in the test, so another message could still come: `recv()` parks. The test awaits the loop, the loop awaits the test. Every answer already sits in its `resp` — no assertion will ever look.",
                );
                return false;
            } else {
                self.frame(
                    L_LOOP,
                    at(E_RECV),
                    "Empty buffer and zero senders: no message can ever arrive again, so `recv()` returns `None` and the `while let` ends. Natural actor shutdown — nobody told the actor to stop.",
                );
                break;
            }
        }

        self.world.rx = Binding::Dropped;
        self.world.event_loop = LoopState::Returned;
        self.frame(
            L_LOOP,
            at(E_RETURN),
            "The loop returns `self`. `rx` goes out of scope with it and is dropped — the loop owned it, the loop cleans it up.",
        );

        self.world.service = Binding::Live;
        let next_id = self.world.next_id.unwrap_or_default();
        self.frame(
            L_LOOP,
            None,
            format!("`service` is the actor, back in the test's hands: `UniqueIdService {{ next_id: {next_id} }}`. Returning `Self` is what makes its final state inspectable."),
        );
        true
    }
}

/// Every frame of the test under `variant`, in execution order.
pub fn trace(variant: Variant) -> Vec<Frame> {
    let mut t = Tracer {
        world: World::default(),
        frames: vec![Frame {
            test_line: None,
            call: None,
            note: "Nothing has run yet. No task will be spawned in this test, and nothing will be mocked: the actor talks through a channel, so the test just makes one.".to_string(),
            world: World::default(),
        }],
    };

    t.world.actor = Binding::Live;
    t.world.next_id = Some(0);
    t.frame(
        L_ACTOR,
        None,
        "`actor` is plain data: `UniqueIdService { next_id: 0 }`. No channel handles, no task, no runtime. The actor is its data.",
    );

    let capacity = variant.capacity();
    t.world.tx = Binding::Live;
    t.world.rx = Binding::Live;
    t.world.capacity = capacity;
    t.frame(
        L_CHANNEL,
        None,
        format!("A bounded channel with room for {capacity}. Both ends are local variables of the test. Nothing is receiving yet — nothing is running at all."),
    );

    for index in 0..REQUESTS {
        if !t.get_unique_id(index) {
            return t.frames;
        }
    }

    if variant.drops_tx() {
        t.world.tx = Binding::Dropped;
        t.frame(
            L_DROP,
            None,
            "Zero senders now. The three messages are still buffered: `recv()` hands them over first, and only then returns `None`. This line is what lets the loop end.",
        );
    } else {
        t.frame(
            L_DROP,
            None,
            "Commented out, so `tx` lives until the end of the test — and the end of the test comes after the event loop has been awaited.",
        );
    }

    if !t.event_loop() {
        return t.frames;
    }

    t.world.resps = [Resp::Joined; REQUESTS];
    t.world.nums = Some([0, 1, 2]);
    t.frame(
        L_JOIN,
        None,
        "`try_join!` polls the three receivers. Each already holds its value, so all are ready on the first poll: `(0, 1, 2)`. No sleep, no timeout, no timing at all.",
    );

    t.world.asserts_passed = 1;
    t.frame(
        L_ASSERT_NUMS,
        None,
        "The ids came out in the order the messages went in.",
    );

    t.world.asserts_passed = 2;
    t.frame(
        L_ASSERT_STATE,
        None,
        "And the final state is exactly what three requests should leave behind. The test asserts on the actor itself, not only on what it said.",
    );

    t.world.service = Binding::Dropped;
    t.world.outcome = Outcome::Passed;
    t.frame(
        L_END,
        None,
        "Passed. Same order on every run: every message was enqueued before the loop started, so there was never anything to race.",
    );
    t.frames
}

#[cfg(test)]
mod tests {
    use super::*;

    fn line(code: &str, index: usize) -> &str {
        code.lines().nth(index).unwrap_or_default()
    }

    #[test]
    fn the_line_indices_point_at_the_lines_they_name() {
        for variant in Variant::ALL {
            let code = variant.test_code();
            assert!(line(&code, L_ACTOR).contains("let actor"));
            assert!(line(&code, L_CHANNEL).contains("mpsc::channel("));
            for (n, index) in L_RESP.into_iter().enumerate() {
                assert!(line(&code, index).contains(&format!("resp{}", n + 1)));
            }
            assert!(line(&code, L_DROP).contains("drop(tx)"));
            assert!(line(&code, L_LOOP).contains("event_loop(rx)"));
            assert!(line(&code, L_JOIN).contains("try_join!"));
            assert!(line(&code, L_ASSERT_NUMS).contains("(0, 1, 2)"));
            assert!(line(&code, L_ASSERT_STATE).contains("next_id: 3"));
            assert_eq!(line(&code, L_END), "}");
            assert_eq!(code.lines().count(), L_END + 1);
        }
        assert!(line(GET_UNIQUE_ID, G_ONESHOT).contains("oneshot::channel()"));
        assert!(line(GET_UNIQUE_ID, G_MESSAGE).contains("GetUniqueId { callback }"));
        assert!(line(GET_UNIQUE_ID, G_SEND).contains("send(message)"));
        assert!(line(EVENT_LOOP, E_ENTER).contains("event_loop("));
        assert!(line(EVENT_LOOP, E_RECV).contains("rx.recv().await"));
        assert_eq!(line(EVENT_LOOP, E_RETURN).trim(), "self");
        assert!(line(EVENT_LOOP, E_SEND).contains("callback.send"));
        assert!(line(EVENT_LOOP, E_INC).contains("next_id += 1"));
    }

    #[test]
    fn as_written_the_test_passes_with_the_articles_values() {
        let frames = trace(Variant::AsWritten);
        let last = frames.last().unwrap();
        assert_eq!(last.world.outcome, Outcome::Passed);
        assert_eq!(last.world.nums, Some([0, 1, 2]));
        assert_eq!(last.world.next_id, Some(3));
        assert_eq!(last.world.asserts_passed, 2);
        assert_eq!(last.world.event_loop, LoopState::Returned);
    }

    #[test]
    fn stepping_over_visits_each_line_of_the_test_once() {
        let frames = trace(Variant::AsWritten);
        let mut at = 0;
        let mut lines = vec![frames[at].test_line];
        while at + 1 < frames.len() {
            at = step_over(&frames, at);
            lines.push(frames[at].test_line);
        }
        let expected: Vec<_> = [
            None,
            Some(L_ACTOR),
            Some(L_CHANNEL),
            Some(L_RESP[0]),
            Some(L_RESP[1]),
            Some(L_RESP[2]),
            Some(L_DROP),
            Some(L_LOOP),
            Some(L_JOIN),
            Some(L_ASSERT_NUMS),
            Some(L_ASSERT_STATE),
            Some(L_END),
        ]
        .into();
        assert_eq!(lines, expected);
    }

    #[test]
    fn the_loop_answers_in_fifo_order_before_anyone_awaits() {
        let frames = trace(Variant::AsWritten);
        let answered: Vec<_> = frames
            .iter()
            .filter(|frame| {
                frame.call
                    == Some(Call {
                        callee: Callee::EventLoop,
                        line: E_SEND,
                    })
            })
            .map(|frame| frame.world.resps)
            .collect();
        use Resp::{Empty, Holds};
        assert_eq!(
            answered,
            [
                [Holds(0), Empty, Empty],
                [Holds(0), Holds(1), Empty],
                [Holds(0), Holds(1), Holds(2)],
            ]
        );
    }

    #[test]
    fn nothing_runs_the_actor_until_the_loop_is_awaited() {
        let frames = trace(Variant::AsWritten);
        for frame in frames.iter().take_while(|f| f.test_line != Some(L_LOOP)) {
            assert_eq!(frame.world.next_id.unwrap_or_default(), 0);
            assert_eq!(frame.world.event_loop, LoopState::NotStarted);
        }
    }

    #[test]
    fn forgetting_the_drop_parks_the_loop_with_every_answer_delivered() {
        let frames = trace(Variant::ForgetDrop);
        let last = frames.last().unwrap();
        assert_eq!(last.world.outcome, Outcome::Hangs);
        assert_eq!(last.world.event_loop, LoopState::Parked);
        assert_eq!(last.world.tx, Binding::Live);
        assert_eq!(
            last.world.resps,
            [Resp::Holds(0), Resp::Holds(1), Resp::Holds(2)]
        );
        assert_eq!(last.world.asserts_passed, 0);
    }

    #[test]
    fn a_buffer_too_small_parks_the_third_send_before_the_actor_runs() {
        let frames = trace(Variant::TooSmall);
        let last = frames.last().unwrap();
        assert_eq!(last.world.outcome, Outcome::Hangs);
        assert_eq!(last.world.send_parked, Some(2));
        assert_eq!(last.world.buffer, VecDeque::from([0, 1]));
        assert_eq!(last.world.event_loop, LoopState::NotStarted);
        assert_eq!(
            last.call,
            Some(Call {
                callee: Callee::GetUniqueId(2),
                line: G_SEND,
            })
        );
    }

    #[test]
    fn stepping_over_a_call_that_never_returns_stops_where_it_parked() {
        for variant in [Variant::ForgetDrop, Variant::TooSmall] {
            let frames = trace(variant);
            let mut at = 0;
            while at + 1 < frames.len() {
                at = step_over(&frames, at);
            }
            assert_eq!(frames[at].world.outcome, Outcome::Hangs);
            assert_eq!(step_over(&frames, at), at);
        }
    }

    #[test]
    fn only_the_last_frame_has_an_outcome_and_every_frame_explains_itself() {
        for variant in Variant::ALL {
            let frames = trace(variant);
            let (last, rest) = frames.split_last().unwrap();
            assert_ne!(last.world.outcome, Outcome::Running, "{variant:?}");
            assert!(rest.iter().all(|f| f.world.outcome == Outcome::Running));
            assert!(frames.iter().all(|f| !f.note.is_empty()));
        }
    }
}
