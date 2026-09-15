//! The timer future, and the two compositions built on it: `select!` over a
//! timer and a button, and that same select in a loop.
//!
//! These are the deck's on-ramp. Before any channel appears, the audience
//! watches one future resolve on its own (the timer), one resolve from real
//! I/O (the button), then both race, then the race repeats forever. By the
//! time an actor shows up, "a loop around a select" is a shape they have
//! already seen move.

use crate::clock::Ticking;
use crate::protocol::{
    ButtonWire, LoopSelectEvent, LoopSelectSnapshot, LoopSelectWire, SelectEvent, SelectSnapshot,
    SelectWinner, SelectWire, TimerEvent, TimerSnapshot, TimerWire, Won, LOOP_SELECT_HISTORY,
    TIMER_PERIOD_S,
};
use crate::sim::ButtonSim;

/// Milliseconds in the timer's period.
const PERIOD_MS: f64 = TIMER_PERIOD_S as f64 * 1000.0;

/// The timer future: activate it and it resolves at the next wall-clock
/// instant whose seconds are a multiple of ten, yielding how long it waited.
///
/// The period is deliberately wall-clock aligned rather than a plain
/// "sleep 10s". Aligning it is what makes the wait *visible*: the slide shows
/// a seconds-of-the-minute dial, the audience can see the deadline coming,
/// and — the actual lesson — activating at 0:07 waits three seconds while
/// activating at 0:11 waits nine. A future's completion is an event in the
/// world, not a stopwatch that starts when you ask.
#[derive(Debug, Clone, PartialEq)]
pub struct TimerSim {
    /// When the pending future resolves, in sim-clock ms. `None` is idle.
    deadline: Option<f64>,
    /// When the pending future was activated, so the wait can be reported.
    started: Option<f64>,
    /// The last completed wait, in seconds.
    resolved: Option<f64>,
    now: f64,
    /// Offset between the sim clock (monotonic, arbitrary origin) and the
    /// seconds-of-the-minute the room's clocks show.
    ///
    /// The sim clock starts wherever the process started, so "a multiple of
    /// ten" in sim time means nothing on the wall. The driver supplies this
    /// once so the server and every phone align to the same dial.
    epoch_offset_ms: f64,
}

impl Default for TimerSim {
    fn default() -> Self {
        Self::new(0.0)
    }
}

impl TimerSim {
    /// `epoch_offset_ms` maps sim time onto wall time: the wall-clock
    /// milliseconds-since-the-minute at sim time zero.
    pub fn new(epoch_offset_ms: f64) -> Self {
        Self {
            deadline: None,
            started: None,
            resolved: None,
            now: 0.0,
            epoch_offset_ms,
        }
    }

    /// Where the dial's hand points: milliseconds into the current minute.
    pub fn wall_ms(&self) -> f64 {
        (self.now + self.epoch_offset_ms).rem_euclid(60_000.0)
    }

    /// The next instant, at or after `from`, whose seconds are a multiple of
    /// the period.
    ///
    /// Strictly after: activating exactly on the tick waits a full period
    /// rather than resolving instantly, which is both what an aligned sleep
    /// does and the only answer that does not make the game look broken.
    fn next_boundary(&self, from: f64) -> f64 {
        let wall = from + self.epoch_offset_ms;
        let elapsed = wall.rem_euclid(PERIOD_MS);
        from + (PERIOD_MS - elapsed)
    }

    pub fn handle(&mut self, wire: &TimerWire) -> Vec<TimerEvent> {
        match wire {
            // A pending future is not restarted by asking again: the one you
            // are awaiting is the one that resolves.
            TimerWire::Activate if self.deadline.is_none() => {
                let deadline = self.next_boundary(self.now);
                self.deadline = Some(deadline);
                self.started = Some(self.now);
                self.resolved = None;
                vec![TimerEvent::Activated {
                    waiting_ms: deadline - self.now,
                }]
            }
            TimerWire::Activate => Vec::new(),
            TimerWire::Cancel => {
                let was_pending = self.deadline.take().is_some();
                self.started = None;
                if was_pending {
                    vec![TimerEvent::Cancelled]
                } else {
                    Vec::new()
                }
            }
        }
    }

    pub fn snapshot(&self) -> TimerSnapshot {
        TimerSnapshot {
            pending: self.deadline.is_some(),
            wall_ms: self.wall_ms(),
            remaining_ms: self.deadline.map(|due| (due - self.now).max(0.0)),
            waited_s: self.resolved,
        }
    }

    /// Drop the resolved value and go back to idle, the way awaiting a
    /// future consumes it.
    pub fn consume(&mut self) {
        self.resolved = None;
    }
}

impl Ticking for TimerSim {
    type Event = TimerEvent;

    fn sync_now(&mut self, now_ms: f64) {
        self.now = now_ms;
    }

    fn next_delay_ms(&self) -> Option<f64> {
        self.deadline.map(|due| (due - self.now).max(0.0))
    }

    fn poll_due(&mut self) -> Vec<TimerEvent> {
        let Some(due) = self.deadline else {
            return Vec::new();
        };
        if due > self.now {
            return Vec::new();
        }
        self.deadline = None;
        let waited_s = self
            .started
            .take()
            .map(|started| (due - started) / 1000.0)
            .unwrap_or(TIMER_PERIOD_S as f64);
        self.resolved = Some(waited_s);
        vec![TimerEvent::Resolved { waited_s }]
    }
}

/// `select!` over the two futures the audience has already met: the timer
/// resolves on its own, the button resolves from real I/O, and whichever
/// gets there first wins.
///
/// The losing branch is *dropped*, not awaited — that is the whole point of
/// the beat, and the reason both halves are the real sims rather than a flat
/// state machine with two slots. Cancelling the loser here is one line
/// because the loser is a future that knows how to be cancelled.
pub struct SelectSim {
    timer: TimerSim,
    button: ButtonSim,
    winner: Option<Won>,
}

impl Default for SelectSim {
    fn default() -> Self {
        Self::new(0.0)
    }
}

impl SelectSim {
    pub fn new(epoch_offset_ms: f64) -> Self {
        Self {
            timer: TimerSim::new(epoch_offset_ms),
            button: ButtonSim::new(),
            winner: None,
        }
    }

    /// Arm both branches: `select!` polls each of them, so entering the
    /// select is what creates both futures.
    fn arm(&mut self) -> Vec<SelectEvent> {
        self.timer.handle(&TimerWire::Cancel);
        self.timer.consume();
        self.timer.handle(&TimerWire::Activate);
        self.button.handle(&ButtonWire::Activate);
        self.winner = None;
        vec![SelectEvent::Armed]
    }

    /// Whichever branch completed first takes the select; the other is
    /// dropped mid-flight.
    fn settle(&mut self, won: Won) -> Vec<SelectEvent> {
        self.winner = Some(won);
        // Cancel the loser, exactly as `select!` drops the branch it did not
        // take: the timer stops counting, the button stops accepting presses.
        //
        // Only the loser. A timer that won has just put its duration in
        // `resolved`, and that value is what the slide exists to show —
        // consuming it here would style the branch as the winner while
        // rendering an empty one.
        match won {
            Won::Timer { .. } => self.button = ButtonSim::new(),
            Won::Button { .. } => {
                self.timer.handle(&TimerWire::Cancel);
                self.timer.consume();
            }
        }
        vec![SelectEvent::Won { won }]
    }

    pub fn handle(&mut self, wire: &SelectWire) -> Vec<SelectEvent> {
        match wire {
            SelectWire::Arm => self.arm(),
            SelectWire::Press { color } if self.armed() => {
                let resolved = self
                    .button
                    .handle(&ButtonWire::Press { color: *color })
                    .into_iter()
                    .any(|event| matches!(event, crate::protocol::ButtonEvent::Resolved { .. }));
                if resolved {
                    self.settle(Won::Button { color: *color })
                } else {
                    Vec::new()
                }
            }
            SelectWire::Press { .. } => Vec::new(),
            SelectWire::Reset => {
                self.winner = None;
                self.timer.handle(&TimerWire::Cancel);
                self.timer.consume();
                self.button = ButtonSim::new();
                vec![SelectEvent::Reset]
            }
        }
    }

    /// Inside the select, with both branches live.
    ///
    /// Derived rather than stored: arming activates the timer and settling
    /// cancels it, so a separate flag would be a second copy of the same
    /// fact with three places to drift.
    pub fn armed(&self) -> bool {
        self.timer.snapshot().pending
    }

    pub fn snapshot(&self) -> SelectSnapshot {
        SelectSnapshot {
            armed: self.armed(),
            timer: self.timer.snapshot(),
            button: self.button.state,
            winner: self.winner,
        }
    }

    /// The branch that won, for the loop to record.
    pub fn winner(&self) -> Option<Won> {
        self.winner
    }
}

impl Ticking for SelectSim {
    type Event = SelectEvent;

    fn sync_now(&mut self, now_ms: f64) {
        self.timer.sync_now(now_ms);
    }

    fn next_delay_ms(&self) -> Option<f64> {
        if !self.armed() {
            return None;
        }
        // only the timer branch is clock-driven; the button waits on a wire
        self.timer.next_delay_ms()
    }

    fn poll_due(&mut self) -> Vec<SelectEvent> {
        if !self.armed() {
            return Vec::new();
        }
        let mut events = Vec::new();
        for event in self.timer.poll_due() {
            if let TimerEvent::Resolved { waited_s } = event {
                events.extend(self.settle(Won::Timer { waited_s }));
            }
        }
        events
    }
}

/// The same select, in a loop: when a branch wins, the result is recorded
/// and the select is entered again.
///
/// This is the last slide before an actor appears, and it is deliberately
/// the same picture as one: a piece of state, a loop, and a `select!` that
/// is the only thing that ever touches that state. The only missing piece by
/// the end of it is the inbox — which is the next chapter.
pub struct LoopSelectSim {
    select: SelectSim,
    running: bool,
    history: Vec<SelectWinner>,
    /// Rounds completed since the actor started.
    ///
    /// Kept alongside the tape rather than read off it: `SelectWinner.round`
    /// is promised never to be reused (the client keys its list on it), and
    /// emptying the tape must not restart the numbering.
    rounds: u64,
}

impl Default for LoopSelectSim {
    fn default() -> Self {
        Self::new(0.0)
    }
}

impl LoopSelectSim {
    pub fn new(epoch_offset_ms: f64) -> Self {
        Self {
            select: SelectSim::new(epoch_offset_ms),
            running: false,
            history: Vec::new(),
            rounds: 0,
        }
    }

    /// Record a completed round and go around again.
    fn record(&mut self, won: Won) -> Vec<LoopSelectEvent> {
        self.rounds += 1;
        let entry = SelectWinner {
            round: self.rounds,
            won,
        };
        self.history.push(entry);
        // The slide shows a running tape, not a transcript: only the last
        // few rounds are legible from the back of a room, and a talk that
        // runs for an hour would otherwise accumulate hundreds.
        if self.history.len() > LOOP_SELECT_HISTORY {
            self.history.remove(0);
        }
        let mut events = vec![LoopSelectEvent::Completed { winner: entry }];
        if self.running {
            events.extend(
                self.select
                    .handle(&SelectWire::Arm)
                    .into_iter()
                    .map(LoopSelectEvent::Select),
            );
        }
        events
    }

    pub fn handle(&mut self, wire: &LoopSelectWire) -> Vec<LoopSelectEvent> {
        match wire {
            LoopSelectWire::Start if !self.running => {
                self.running = true;
                let mut events = vec![LoopSelectEvent::Started];
                events.extend(
                    self.select
                        .handle(&SelectWire::Arm)
                        .into_iter()
                        .map(LoopSelectEvent::Select),
                );
                events
            }
            LoopSelectWire::Start => Vec::new(),
            // Breaking out of the loop leaves the current select dropped
            // mid-await, which is what returning from the loop body does.
            LoopSelectWire::Stop => {
                self.running = false;
                self.select.handle(&SelectWire::Reset);
                vec![LoopSelectEvent::Stopped]
            }
            LoopSelectWire::Press { color } => {
                if !self.running {
                    return Vec::new();
                }
                let mut events: Vec<LoopSelectEvent> = self
                    .select
                    .handle(&SelectWire::Press { color: *color })
                    .into_iter()
                    .map(LoopSelectEvent::Select)
                    .collect();
                if let Some(won) = self.select.winner() {
                    events.extend(self.record(won));
                }
                events
            }
            LoopSelectWire::Clear => {
                self.history.clear();
                vec![LoopSelectEvent::Cleared]
            }
        }
    }

    /// Rounds completed since the actor started.
    pub fn rounds(&self) -> u64 {
        self.rounds
    }

    pub fn snapshot(&self) -> LoopSelectSnapshot {
        LoopSelectSnapshot {
            running: self.running,
            select: self.select.snapshot(),
            history: self.history.clone(),
            rounds: self.rounds(),
        }
    }
}

impl Ticking for LoopSelectSim {
    type Event = LoopSelectEvent;

    fn sync_now(&mut self, now_ms: f64) {
        self.select.sync_now(now_ms);
    }

    fn next_delay_ms(&self) -> Option<f64> {
        if !self.running {
            return None;
        }
        self.select.next_delay_ms()
    }

    fn poll_due(&mut self) -> Vec<LoopSelectEvent> {
        if !self.running {
            return Vec::new();
        }
        let mut events: Vec<LoopSelectEvent> = self
            .select
            .poll_due()
            .into_iter()
            .map(LoopSelectEvent::Select)
            .collect();
        if let Some(won) = self.select.winner() {
            events.extend(self.record(won));
        }
        events
    }
}

#[cfg(test)]
mod timer_tests {
    use super::*;

    /// A timer aligned to the minute: sim time zero is the top of a minute.
    fn timer() -> TimerSim {
        TimerSim::new(0.0)
    }

    #[test]
    fn activating_waits_until_the_next_multiple_of_ten() {
        let mut timer = timer();
        timer.sync_now(3_000.0);
        let events = timer.handle(&TimerWire::Activate);
        assert_eq!(
            events,
            vec![TimerEvent::Activated {
                waiting_ms: 7_000.0
            }]
        );

        // nothing is due before the boundary
        assert!(timer.tick(9_999.0).is_empty());
        assert_eq!(timer.next_delay_ms(), Some(1.0));

        let events = timer.tick(10_000.0);
        assert_eq!(events, vec![TimerEvent::Resolved { waited_s: 7.0 }]);
        assert!(timer.snapshot().waited_s.is_some());
    }

    #[test]
    fn the_wait_depends_on_when_you_asked() {
        // the beat of the slide: same future, same period, different wait
        let mut early = timer();
        early.sync_now(1_000.0);
        assert_eq!(
            early.handle(&TimerWire::Activate),
            vec![TimerEvent::Activated {
                waiting_ms: 9_000.0
            }],
        );

        let mut late = timer();
        late.sync_now(9_000.0);
        assert_eq!(
            late.handle(&TimerWire::Activate),
            vec![TimerEvent::Activated {
                waiting_ms: 1_000.0
            }],
        );
    }

    #[test]
    fn activating_exactly_on_the_boundary_waits_a_whole_period() {
        // resolving instantly would make the game look broken, and an
        // aligned sleep does not do it either
        let mut timer = timer();
        timer.sync_now(10_000.0);
        assert_eq!(
            timer.handle(&TimerWire::Activate),
            vec![TimerEvent::Activated {
                waiting_ms: 10_000.0
            }],
        );
    }

    #[test]
    fn the_resolving_frame_is_the_one_that_stops_having_a_delay() {
        // The local driver publishes a snapshot whenever a tick produced
        // events, precisely because it cannot publish on "still pending":
        // the deadline clears on the same frame the value appears.
        let mut timer = TimerSim::new(0.0);
        timer.sync_now(3_000.0);
        timer.handle(&TimerWire::Activate);
        assert!(timer.next_delay_ms().is_some(), "counting down");

        let events = timer.tick(10_000.0);
        assert_eq!(events, vec![TimerEvent::Resolved { waited_s: 7.0 }]);
        assert_eq!(timer.next_delay_ms(), None, "and now reads as idle");
        assert_eq!(
            timer.snapshot().waited_s,
            Some(7.0),
            "so the value is only reachable via the frame that produced it",
        );
    }

    #[test]
    fn polling_twice_at_the_same_instant_resolves_once() {
        // the idempotence the Ticking contract promises drivers
        let mut timer = timer();
        timer.sync_now(0.0);
        timer.handle(&TimerWire::Activate);
        assert_eq!(timer.tick(10_000.0).len(), 1);
        assert!(timer.poll_due().is_empty(), "already resolved");
    }

    #[test]
    fn an_idle_timer_is_not_waiting_on_the_clock() {
        let mut timer = timer();
        assert_eq!(timer.next_delay_ms(), None, "idle waits on a wire");
        timer.sync_now(0.0);
        timer.handle(&TimerWire::Activate);
        assert_eq!(timer.next_delay_ms(), Some(10_000.0));
    }

    #[test]
    fn re_activating_a_pending_timer_does_not_restart_it() {
        let mut timer = timer();
        timer.sync_now(3_000.0);
        timer.handle(&TimerWire::Activate);
        assert!(timer.handle(&TimerWire::Activate).is_empty());
        // still the original deadline, not one 10s from the second ask
        assert_eq!(
            timer.tick(10_000.0),
            vec![TimerEvent::Resolved { waited_s: 7.0 }]
        );
    }

    #[test]
    fn cancelling_a_pending_timer_stops_the_clock() {
        let mut timer = timer();
        timer.sync_now(0.0);
        timer.handle(&TimerWire::Activate);
        assert_eq!(
            timer.handle(&TimerWire::Cancel),
            vec![TimerEvent::Cancelled]
        );
        assert_eq!(timer.next_delay_ms(), None);
        assert!(
            timer.tick(60_000.0).is_empty(),
            "a dropped future never fires"
        );
    }

    #[test]
    fn cancelling_an_idle_timer_says_nothing() {
        let mut timer = timer();
        assert!(timer.handle(&TimerWire::Cancel).is_empty());
    }

    #[test]
    fn the_dial_wraps_at_a_minute() {
        let mut timer = timer();
        timer.sync_now(61_500.0);
        assert_eq!(timer.wall_ms(), 1_500.0);
    }

    #[test]
    fn the_epoch_offset_aligns_the_dial_to_the_wall() {
        // sim zero happens 4s into a minute: the first boundary is 6s later
        let mut timer = TimerSim::new(4_000.0);
        timer.sync_now(0.0);
        assert_eq!(timer.wall_ms(), 4_000.0);
        assert_eq!(
            timer.handle(&TimerWire::Activate),
            vec![TimerEvent::Activated {
                waiting_ms: 6_000.0
            }],
        );
    }
}

#[cfg(test)]
mod select_tests {
    use super::*;
    use crate::protocol::Color;

    fn armed() -> SelectSim {
        let mut select = SelectSim::new(0.0);
        select.sync_now(0.0);
        assert_eq!(select.handle(&SelectWire::Arm), vec![SelectEvent::Armed]);
        select
    }

    #[test]
    fn the_button_can_beat_the_timer() {
        let mut select = armed();
        let events = select.handle(&SelectWire::Press { color: Color::Red });
        assert_eq!(
            events,
            vec![SelectEvent::Won {
                won: Won::Button { color: Color::Red },
            }],
        );
        // the timer branch was dropped, so it never fires
        assert_eq!(select.next_delay_ms(), None);
        assert!(select.tick(60_000.0).is_empty());
    }

    #[test]
    fn the_timer_can_beat_the_button() {
        let mut select = armed();
        let events = select.tick(10_000.0);
        assert_eq!(
            events,
            vec![SelectEvent::Won {
                won: Won::Timer { waited_s: 10.0 },
            }],
        );
        // the button branch was dropped: a late press decides nothing
        assert!(select
            .handle(&SelectWire::Press { color: Color::Blue })
            .is_empty());
        let snapshot = select.snapshot();
        assert_eq!(
            snapshot.winner,
            Some(Won::Timer { waited_s: 10.0 }),
            "the first winner stands",
        );
        // The winning branch must still carry what it yielded: the slide
        // styles it as the winner and then renders this value inside it.
        assert_eq!(
            snapshot.timer.waited_s,
            Some(10.0),
            "a winning timer keeps its duration",
        );
    }

    #[test]
    fn a_press_before_arming_decides_nothing() {
        let mut select = SelectSim::new(0.0);
        assert!(select
            .handle(&SelectWire::Press {
                color: Color::Green
            })
            .is_empty());
        assert_eq!(select.snapshot().winner, None);
    }

    #[test]
    fn an_unarmed_select_is_not_waiting_on_the_clock() {
        let select = SelectSim::new(0.0);
        assert_eq!(select.next_delay_ms(), None);
    }

    #[test]
    fn arming_again_starts_a_fresh_race() {
        let mut select = armed();
        select.handle(&SelectWire::Press { color: Color::Red });
        select.sync_now(1_000.0);
        select.handle(&SelectWire::Arm);
        assert_eq!(select.snapshot().winner, None, "the new race is undecided");
        assert_eq!(select.next_delay_ms(), Some(9_000.0));
    }

    #[test]
    fn resetting_drops_both_branches() {
        let mut select = armed();
        assert_eq!(select.handle(&SelectWire::Reset), vec![SelectEvent::Reset]);
        assert_eq!(select.next_delay_ms(), None);
        assert!(!select.snapshot().armed);
    }
}

#[cfg(test)]
mod loop_select_tests {
    use super::*;
    use crate::protocol::Color;

    fn running() -> LoopSelectSim {
        let mut looping = LoopSelectSim::new(0.0);
        looping.sync_now(0.0);
        let events = looping.handle(&LoopSelectWire::Start);
        assert_eq!(
            events,
            vec![
                LoopSelectEvent::Started,
                LoopSelectEvent::Select(SelectEvent::Armed),
            ],
        );
        looping
    }

    #[test]
    fn a_win_records_a_round_and_re_arms() {
        let mut looping = running();
        let events = looping.tick(10_000.0);
        assert_eq!(
            events,
            vec![
                LoopSelectEvent::Select(SelectEvent::Won {
                    won: Won::Timer { waited_s: 10.0 },
                }),
                LoopSelectEvent::Completed {
                    winner: SelectWinner {
                        round: 1,
                        won: Won::Timer { waited_s: 10.0 },
                    },
                },
                LoopSelectEvent::Select(SelectEvent::Armed),
            ],
            "the loop goes around again by itself",
        );

        let snapshot = looping.snapshot();
        assert_eq!(snapshot.rounds, 1);
        assert!(snapshot.select.armed, "already awaiting the next race");
        assert_eq!(looping.next_delay_ms(), Some(10_000.0));
    }

    #[test]
    fn the_loop_keeps_going_around() {
        let mut looping = running();
        looping.tick(10_000.0);
        looping.handle(&LoopSelectWire::Press { color: Color::Red });
        looping.tick(20_000.0);

        let snapshot = looping.snapshot();
        assert_eq!(snapshot.rounds, 3);
        assert_eq!(
            snapshot
                .history
                .iter()
                .map(|entry| entry.won)
                .collect::<Vec<_>>(),
            vec![
                Won::Timer { waited_s: 10.0 },
                Won::Button { color: Color::Red },
                Won::Timer { waited_s: 10.0 },
            ],
        );
    }

    #[test]
    fn the_tape_keeps_only_the_recent_rounds() {
        let mut looping = running();
        for round in 1..=(LOOP_SELECT_HISTORY + 3) {
            looping.handle(&LoopSelectWire::Press { color: Color::Blue });
            assert_eq!(looping.snapshot().rounds, round as u64);
        }
        let snapshot = looping.snapshot();
        assert_eq!(snapshot.history.len(), LOOP_SELECT_HISTORY);
        assert_eq!(
            snapshot.history.last().expect("a round was played").round,
            (LOOP_SELECT_HISTORY + 3) as u64,
            "the tape keeps the newest, not the oldest",
        );
    }

    #[test]
    fn stopping_breaks_out_and_drops_the_pending_select() {
        let mut looping = running();
        assert_eq!(
            looping.handle(&LoopSelectWire::Stop),
            vec![LoopSelectEvent::Stopped],
        );
        assert_eq!(looping.next_delay_ms(), None);
        assert!(looping.tick(60_000.0).is_empty(), "a stopped loop is inert");
        assert!(!looping.snapshot().select.armed);
    }

    #[test]
    fn a_press_while_stopped_decides_nothing() {
        let mut looping = LoopSelectSim::new(0.0);
        assert!(looping
            .handle(&LoopSelectWire::Press { color: Color::Red })
            .is_empty());
        assert_eq!(looping.snapshot().rounds, 0);
    }

    #[test]
    fn starting_a_running_loop_is_a_no_op() {
        let mut looping = running();
        assert!(looping.handle(&LoopSelectWire::Start).is_empty());
    }

    #[test]
    fn clearing_the_tape_does_not_restart_round_numbering() {
        // `SelectWinner.round` is promised never to be reused — the client
        // keys its tape on it, and a repeated key lets Dioxus reuse a stale
        // node — so emptying the tape must not reset the counter.
        let mut looping = running();
        looping.handle(&LoopSelectWire::Press { color: Color::Red });
        looping.handle(&LoopSelectWire::Clear);
        looping.handle(&LoopSelectWire::Press { color: Color::Blue });

        let snapshot = looping.snapshot();
        assert_eq!(snapshot.rounds, 2, "the counter survives the clear");
        assert_eq!(
            snapshot.history.first().map(|entry| entry.round),
            Some(2),
            "the round after a clear is numbered 2, not 1",
        );
    }

    #[test]
    fn clearing_empties_the_tape_without_stopping_the_loop() {
        let mut looping = running();
        looping.tick(10_000.0);
        assert_eq!(
            looping.handle(&LoopSelectWire::Clear),
            vec![LoopSelectEvent::Cleared],
        );
        let snapshot = looping.snapshot();
        assert!(snapshot.history.is_empty());
        assert!(
            snapshot.running,
            "clearing the tape does not break the loop"
        );
    }
}
