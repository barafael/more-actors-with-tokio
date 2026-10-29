//! The button and timer futures, and the compositions built on them:
//! `select!` over the two, that select spawned as a task, and that select in
//! a loop.
//!
//! These are the deck's on-ramp. Before any channel appears, you watch one
//! future resolve on its own (the timer), one resolve from real I/O (the
//! button), then both race, then the race repeats forever. By the time an
//! actor shows up, "a loop around a select" is a shape you have already seen
//! move.

use sim_channels::oneshot::OneshotCore;

use crate::clock::Ticking;

/// The longest the timer future can be told to sleep, in seconds. Its dial
/// is one turn of this many seconds.
pub const MAX_WAIT_S: u64 = 10;

/// What the timer sleeps for until someone picks otherwise: long enough to
/// watch, short enough not to stall a talk.
pub const DEFAULT_WAIT_S: u64 = 3;

/// How many completed rounds the loop-select tape shows.
pub const LOOP_HISTORY: usize = 8;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Color {
    Red,
    Green,
    Blue,
}

impl Color {
    pub const ALL: [Color; 3] = [Color::Red, Color::Green, Color::Blue];

    pub fn css_name(self) -> &'static str {
        match self {
            Color::Red => "red",
            Color::Green => "green",
            Color::Blue => "blue",
        }
    }

    pub fn display(self) -> &'static str {
        match self {
            Color::Red => "Red",
            Color::Green => "Green",
            Color::Blue => "Blue",
        }
    }
}

/// Where the button future stands.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum ButtonState {
    #[default]
    Idle,
    Pending,
    Ready(Color),
}

/// The button future: a `oneshot` channel whose value is the colour that
/// resolved it. Activating creates the channel, pressing a button sends the
/// one value it will ever carry.
///
/// `state` is a projection of the core; the channel itself is the truth.
#[derive(Debug, Default)]
pub struct ButtonSim {
    pending: Option<OneshotCore<Color>>,
    state: ButtonState,
}

impl ButtonSim {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn state(&self) -> ButtonState {
        self.state
    }

    /// Create the future. A pending one is not replaced; a resolved one is
    /// consumed, so activating again creates a new one. Whether a future
    /// was created.
    pub fn activate(&mut self) -> bool {
        if self.state == ButtonState::Pending {
            return false;
        }
        self.pending = Some(OneshotCore::new());
        self.state = ButtonState::Pending;
        true
    }

    /// Press a button. One value moves exactly once: a second press finds
    /// the slot full and the send fails, leaving the resolved colour intact.
    /// Whether this press resolved the future.
    pub fn press(&mut self, color: Color) -> bool {
        let Some(core) = self.pending.as_mut() else {
            return false;
        };
        if core.send(color).is_err() {
            return false;
        }
        self.state = ButtonState::Ready(color);
        true
    }
}

/// The timer future: `async { sleep(Duration::from_secs(n)).await; n }`.
/// Pick `n` in `0..=MAX_WAIT_S`, await it, and it resolves `n` seconds later,
/// yielding `n`. Nothing makes it happen but time passing.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct TimerSim {
    /// How long the next await sleeps, in whole seconds.
    wait_s: u64,
    /// The pending future: when it resolves, in sim-clock ms, and the
    /// seconds it was created to sleep. `None` is idle.
    deadline: Option<(f64, u64)>,
    /// The last completed wait, in seconds.
    resolved: Option<u64>,
    now: f64,
}

impl Default for TimerSim {
    fn default() -> Self {
        Self {
            wait_s: DEFAULT_WAIT_S,
            deadline: None,
            resolved: None,
            now: 0.0,
        }
    }
}

impl TimerSim {
    pub fn new() -> Self {
        Self::default()
    }

    /// How long the next await sleeps, in seconds.
    pub fn wait_s(&self) -> u64 {
        self.wait_s
    }

    /// Choose how long the next await sleeps, clamped to `0..=MAX_WAIT_S`.
    /// A pending future keeps the duration it was created with: changing
    /// the argument does not reach into a sleep already under way. Whether
    /// the duration changed.
    pub fn set_wait_s(&mut self, wait_s: u64) -> bool {
        let wait_s = wait_s.min(MAX_WAIT_S);
        let changed = wait_s != self.wait_s;
        self.wait_s = wait_s;
        changed
    }

    /// The pending future's own sleep, in seconds; `None` when idle.
    pub fn sleeping_s(&self) -> Option<u64> {
        self.deadline.map(|(_, secs)| secs)
    }

    pub fn pending(&self) -> bool {
        self.deadline.is_some()
    }

    /// Time left on the pending future, or `None` when idle.
    pub fn remaining_ms(&self) -> Option<f64> {
        self.deadline.map(|(due, _)| (due - self.now).max(0.0))
    }

    /// How far into its sleep the pending future is, in ms; `None` when idle.
    pub fn elapsed_ms(&self) -> Option<f64> {
        self.deadline
            .map(|(due, secs)| (secs as f64 * 1000.0 - (due - self.now)).clamp(0.0, secs as f64 * 1000.0))
    }

    /// What the last completed await yielded, in seconds.
    pub fn waited_s(&self) -> Option<u64> {
        self.resolved
    }

    /// Await the timer: it resolves `wait_s` seconds from now. A pending
    /// future is not restarted by asking again — the one you are awaiting is
    /// the one that resolves. Whether a future was created.
    pub fn activate(&mut self) -> bool {
        if self.deadline.is_some() {
            return false;
        }
        self.deadline = Some((self.now + self.wait_s as f64 * 1000.0, self.wait_s));
        self.resolved = None;
        true
    }

    /// Drop the pending future without awaiting it. Whether one was pending.
    pub fn cancel(&mut self) -> bool {
        self.deadline.take().is_some()
    }

    /// Drop the resolved value and go back to idle, the way awaiting a
    /// future consumes it.
    pub fn consume(&mut self) {
        self.resolved = None;
    }
}

impl Ticking for TimerSim {
    /// The seconds the future slept, on the frame it resolves.
    type Event = u64;

    fn sync_now(&mut self, now_ms: f64) {
        self.now = now_ms;
    }

    fn next_delay_ms(&self) -> Option<f64> {
        self.remaining_ms()
    }

    fn poll_due(&mut self) -> Vec<u64> {
        let Some((due, secs)) = self.deadline else {
            return Vec::new();
        };
        if due > self.now {
            return Vec::new();
        }
        self.deadline = None;
        self.resolved = Some(secs);
        vec![secs]
    }
}

/// Which branch of the `select!` completed first.
///
/// Carries the branch's value, not just its name: the point of the slide is
/// that a select yields whatever the winning future yielded, and the two
/// branches yield different types.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Won {
    Timer { waited_s: u64 },
    Button { color: Color },
}

impl Won {
    pub fn css_name(self) -> &'static str {
        match self {
            Won::Timer { .. } => "timer",
            Won::Button { color } => color.css_name(),
        }
    }
}

/// `select!` over the two futures you have already met: the timer
/// resolves on its own, the button resolves from real I/O, and whichever
/// gets there first wins.
///
/// The losing branch is *dropped*, not awaited — that is the whole point of
/// the beat, and the reason both halves are the real sims rather than a flat
/// state machine with two slots. Cancelling the loser here is one line
/// because the loser is a future that knows how to be cancelled.
#[derive(Debug, Default)]
pub struct SelectSim {
    timer: TimerSim,
    button: ButtonSim,
    winner: Option<Won>,
}

impl SelectSim {
    pub fn new() -> Self {
        Self::default()
    }

    /// Choose the timer branch's sleep for the next race; a live select
    /// keeps the sleep it already holds. Whether it changed.
    pub fn set_wait_s(&mut self, wait_s: u64) -> bool {
        self.timer.set_wait_s(wait_s)
    }

    /// Inside the select, with both branches live.
    ///
    /// Derived rather than stored: arming activates the timer and settling
    /// cancels it, so a separate flag would be a second copy of the same
    /// fact with three places to drift.
    pub fn armed(&self) -> bool {
        self.timer.pending()
    }

    pub fn timer(&self) -> TimerSim {
        self.timer
    }

    /// The branch that won, once one has.
    pub fn winner(&self) -> Option<Won> {
        self.winner
    }

    /// Enter the select: `select!` polls each branch, so entering it is
    /// what creates both futures, the sleep with whatever duration is
    /// chosen. Entering again starts a fresh race.
    pub fn arm(&mut self) {
        self.timer.cancel();
        self.timer.consume();
        self.timer.activate();
        self.button = ButtonSim::new();
        self.button.activate();
        self.winner = None;
    }

    /// Press a button. Decides the race if the select is live and the button
    /// branch has not been dropped. The winner, if this press decided it.
    pub fn press(&mut self, color: Color) -> Option<Won> {
        if !self.armed() || !self.button.press(color) {
            return None;
        }
        Some(self.settle(Won::Button { color }))
    }

    /// Leave the select, dropping both branches.
    pub fn reset(&mut self) {
        self.winner = None;
        self.timer.cancel();
        self.timer.consume();
        self.button = ButtonSim::new();
    }

    /// Whichever branch completed first takes the select; the other is
    /// dropped mid-flight.
    fn settle(&mut self, won: Won) -> Won {
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
                self.timer.cancel();
                self.timer.consume();
            }
        }
        won
    }
}

impl Ticking for SelectSim {
    /// The winner, on the frame the timer branch takes the race.
    type Event = Won;

    fn sync_now(&mut self, now_ms: f64) {
        self.timer.sync_now(now_ms);
    }

    fn next_delay_ms(&self) -> Option<f64> {
        // only the timer branch is clock-driven; the button waits on a click
        self.timer.next_delay_ms()
    }

    fn poll_due(&mut self) -> Vec<Won> {
        self.timer
            .poll_due()
            .into_iter()
            .map(|waited_s| self.settle(Won::Timer { waited_s }))
            .collect()
    }
}

/// The loop-select, handed to `tokio::spawn`: the loop now runs as a task
/// on its own, and the caller keeps only the `JoinHandle`.
///
/// Nothing about the loop changes. What the slide adds is the handle, and
/// what it says about a loop with no `break`: the task never finishes, so
/// the handle never resolves. One spawn is all there is.
#[derive(Debug, Default)]
pub struct SpawnSim {
    looping: LoopSelectSim,
    spawned: bool,
}

impl SpawnSim {
    pub fn new() -> Self {
        Self::default()
    }

    /// Choose the sleep for the loop's coming rounds.
    pub fn set_wait_s(&mut self, wait_s: u64) -> bool {
        self.looping.set_wait_s(wait_s)
    }

    pub fn looping(&self) -> &LoopSelectSim {
        &self.looping
    }

    /// Whether the task has been spawned, and so is running.
    pub fn spawned(&self) -> bool {
        self.spawned
    }

    /// `tokio::spawn` the loop. It starts at once and never stops, so there
    /// is only ever the one task. Whether this call spawned it.
    pub fn spawn(&mut self) -> bool {
        if self.spawned {
            return false;
        }
        self.spawned = true;
        self.looping.start();
        true
    }

    pub fn press(&mut self, color: Color) -> Option<Round> {
        self.looping.press(color)
    }
}

impl Ticking for SpawnSim {
    /// The round the timer branch completed, on that frame.
    type Event = Round;

    fn sync_now(&mut self, now_ms: f64) {
        self.looping.sync_now(now_ms);
    }

    fn next_delay_ms(&self) -> Option<f64> {
        self.looping.next_delay_ms()
    }

    fn poll_due(&mut self) -> Vec<Round> {
        self.looping.poll_due()
    }
}

/// One completed trip around the loop.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Round {
    /// 1-based, and never reused: the tape keys its cells on it.
    pub round: u64,
    pub won: Won,
}

/// The same select, in a loop: when a branch wins, the result is recorded
/// and the select is entered again.
///
/// This is the last slide before an actor appears, and it is deliberately
/// the same picture as one: a piece of state, a loop, and a `select!` that
/// is the only thing that ever touches that state. The only missing piece by
/// the end of it is the inbox — which is the next chapter.
#[derive(Debug, Default)]
pub struct LoopSelectSim {
    select: SelectSim,
    running: bool,
    /// The last `LOOP_HISTORY` rounds, oldest first.
    history: Vec<Round>,
    /// Rounds completed since the loop was built.
    ///
    /// Kept alongside the tape rather than read off it: `Round::round` is
    /// promised never to be reused (the tape keys its cells on it), and the
    /// tape forgets its oldest rounds.
    rounds: u64,
}

impl LoopSelectSim {
    pub fn new() -> Self {
        Self::default()
    }

    /// Choose the sleep for the coming rounds. A round already under way
    /// keeps its own; the next trip around the loop picks this one up.
    pub fn set_wait_s(&mut self, wait_s: u64) -> bool {
        self.select.set_wait_s(wait_s)
    }

    pub fn running(&self) -> bool {
        self.running
    }

    pub fn select(&self) -> &SelectSim {
        &self.select
    }

    pub fn history(&self) -> &[Round] {
        &self.history
    }

    /// Rounds completed, which is also the newest round's number.
    pub fn rounds(&self) -> u64 {
        self.rounds
    }

    /// Enter the loop, which then never stops: there is no `break` in it.
    /// Whether it was not already running.
    pub fn start(&mut self) -> bool {
        if self.running {
            return false;
        }
        self.running = true;
        self.select.arm();
        true
    }

    pub fn press(&mut self, color: Color) -> Option<Round> {
        if !self.running {
            return None;
        }
        let won = self.select.press(color)?;
        Some(self.record(won))
    }

    /// Record a completed round and go around again.
    fn record(&mut self, won: Won) -> Round {
        self.rounds += 1;
        let entry = Round {
            round: self.rounds,
            won,
        };
        self.history.push(entry);
        // The slide shows a running tape, not a transcript: only the last
        // few rounds are legible from the back of a room, and a talk that
        // runs for an hour would otherwise accumulate hundreds.
        if self.history.len() > LOOP_HISTORY {
            self.history.remove(0);
        }
        if self.running {
            self.select.arm();
        }
        entry
    }
}

impl Ticking for LoopSelectSim {
    /// The round the timer branch completed, on that frame.
    type Event = Round;

    fn sync_now(&mut self, now_ms: f64) {
        self.select.sync_now(now_ms);
    }

    fn next_delay_ms(&self) -> Option<f64> {
        if !self.running {
            return None;
        }
        self.select.next_delay_ms()
    }

    fn poll_due(&mut self) -> Vec<Round> {
        if !self.running {
            return Vec::new();
        }
        self.select
            .poll_due()
            .into_iter()
            .map(|won| self.record(won))
            .collect()
    }
}

#[cfg(test)]
mod button_tests {
    use super::*;

    #[test]
    fn button_future_lifecycle() {
        let mut sim = ButtonSim::new();
        assert_eq!(sim.state(), ButtonState::Idle);
        assert!(sim.activate());
        assert_eq!(sim.state(), ButtonState::Pending);
        assert!(sim.press(Color::Green));
        assert_eq!(sim.state(), ButtonState::Ready(Color::Green));
        // one value moves exactly once
        assert!(!sim.press(Color::Red));
        assert_eq!(sim.state(), ButtonState::Ready(Color::Green));
        // a resolved future is consumed; activating creates a new one
        assert!(sim.activate());
        assert_eq!(sim.state(), ButtonState::Pending);
        // pressing while idle does nothing
        let mut idle = ButtonSim::new();
        assert!(!idle.press(Color::Red));
    }

    #[test]
    fn activating_a_pending_future_does_not_replace_it() {
        let mut sim = ButtonSim::new();
        sim.activate();
        assert!(!sim.activate());
    }
}

#[cfg(test)]
mod timer_tests {
    use super::*;

    #[test]
    fn it_sleeps_for_the_chosen_seconds_and_yields_them() {
        let mut timer = TimerSim::new();
        assert!(timer.set_wait_s(7));
        timer.sync_now(3_000.0);
        assert!(timer.activate());
        assert_eq!(timer.remaining_ms(), Some(7_000.0));

        // nothing is due before the deadline
        assert!(timer.tick(9_999.0).is_empty());
        assert_eq!(timer.next_delay_ms(), Some(1.0));
        assert_eq!(timer.elapsed_ms(), Some(6_999.0));

        assert_eq!(timer.tick(10_000.0), vec![7]);
        assert_eq!(timer.waited_s(), Some(7));
    }

    #[test]
    fn the_wait_is_measured_from_the_await() {
        // unlike a wall-clock deadline, the same sleep is the same wait
        // whenever you ask
        for start in [1_000.0, 9_000.0] {
            let mut timer = TimerSim::new();
            timer.sync_now(start);
            timer.activate();
            assert_eq!(timer.remaining_ms(), Some(DEFAULT_WAIT_S as f64 * 1000.0));
        }
    }

    #[test]
    fn the_argument_stays_between_zero_and_ten() {
        let mut timer = TimerSim::new();
        timer.set_wait_s(99);
        assert_eq!(timer.wait_s(), MAX_WAIT_S);
        timer.set_wait_s(0);
        assert_eq!(timer.wait_s(), 0);
        assert!(!timer.set_wait_s(0), "the same value is no change");
    }

    #[test]
    fn sleeping_zero_resolves_on_the_next_poll() {
        let mut timer = TimerSim::new();
        timer.set_wait_s(0);
        timer.sync_now(5_000.0);
        timer.activate();
        assert_eq!(timer.next_delay_ms(), Some(0.0));
        assert_eq!(timer.tick(5_000.0), vec![0]);
    }

    #[test]
    fn a_pending_sleep_keeps_the_length_it_was_created_with() {
        let mut timer = TimerSim::new();
        timer.set_wait_s(4);
        timer.sync_now(0.0);
        timer.activate();
        timer.set_wait_s(9);
        assert_eq!(timer.sleeping_s(), Some(4));
        assert_eq!(timer.tick(4_000.0), vec![4], "the old argument resolves");
        timer.activate();
        assert_eq!(timer.sleeping_s(), Some(9), "the new one is for the next");
    }

    #[test]
    fn the_resolving_frame_is_the_one_that_stops_having_a_delay() {
        let mut timer = TimerSim::new();
        timer.sync_now(0.0);
        timer.activate();
        assert!(timer.next_delay_ms().is_some(), "counting down");

        assert_eq!(timer.tick(3_000.0), vec![3]);
        assert_eq!(timer.next_delay_ms(), None, "and now reads as idle");
        assert_eq!(timer.waited_s(), Some(3), "with the value kept");
    }

    #[test]
    fn polling_twice_at_the_same_instant_resolves_once() {
        // the idempotence the Ticking contract promises drivers
        let mut timer = TimerSim::new();
        timer.sync_now(0.0);
        timer.activate();
        assert_eq!(timer.tick(10_000.0).len(), 1);
        assert!(timer.poll_due().is_empty(), "already resolved");
    }

    #[test]
    fn an_idle_timer_is_not_waiting_on_the_clock() {
        let timer = TimerSim::new();
        assert_eq!(timer.next_delay_ms(), None, "idle waits on a click");
        assert_eq!(timer.elapsed_ms(), None);
    }

    #[test]
    fn re_activating_a_pending_timer_does_not_restart_it() {
        let mut timer = TimerSim::new();
        timer.sync_now(0.0);
        timer.activate();
        timer.sync_now(2_000.0);
        assert!(!timer.activate());
        // still the original deadline, not one 3s from the second ask
        assert_eq!(timer.tick(3_000.0), vec![3]);
    }

    #[test]
    fn cancelling_a_pending_timer_stops_the_clock() {
        let mut timer = TimerSim::new();
        timer.sync_now(0.0);
        timer.activate();
        assert!(timer.cancel());
        assert_eq!(timer.next_delay_ms(), None);
        assert!(
            timer.tick(60_000.0).is_empty(),
            "a dropped future never fires"
        );
    }

    #[test]
    fn cancelling_an_idle_timer_says_nothing() {
        assert!(!TimerSim::new().cancel());
    }
}

#[cfg(test)]
mod select_tests {
    use super::*;

    fn armed() -> SelectSim {
        let mut select = SelectSim::new();
        select.sync_now(0.0);
        select.arm();
        assert!(select.armed());
        select
    }

    #[test]
    fn the_button_can_beat_the_timer() {
        let mut select = armed();
        assert_eq!(
            select.press(Color::Red),
            Some(Won::Button { color: Color::Red })
        );
        // the timer branch was dropped, so it never fires
        assert_eq!(select.next_delay_ms(), None);
        assert!(select.tick(60_000.0).is_empty());
    }

    #[test]
    fn the_timer_can_beat_the_button() {
        let mut select = armed();
        assert_eq!(select.tick(3_000.0), vec![Won::Timer { waited_s: 3 }]);
        // the button branch was dropped: a late press decides nothing
        assert_eq!(select.press(Color::Blue), None);
        assert_eq!(
            select.winner(),
            Some(Won::Timer { waited_s: 3 }),
            "the first winner stands",
        );
        // The winning branch must still carry what it yielded: the slide
        // styles it as the winner and then renders this value inside it.
        assert_eq!(
            select.timer().waited_s(),
            Some(3),
            "a winning timer keeps its duration",
        );
    }

    #[test]
    fn a_press_before_arming_decides_nothing() {
        let mut select = SelectSim::new();
        assert_eq!(select.press(Color::Green), None);
        assert_eq!(select.winner(), None);
    }

    #[test]
    fn an_unarmed_select_is_not_waiting_on_the_clock() {
        assert_eq!(SelectSim::new().next_delay_ms(), None);
    }

    #[test]
    fn arming_again_starts_a_fresh_race() {
        let mut select = armed();
        select.press(Color::Red);
        select.sync_now(1_000.0);
        select.arm();
        assert_eq!(select.winner(), None, "the new race is undecided");
        assert_eq!(select.next_delay_ms(), Some(3_000.0), "a full sleep from now");
        // and its button branch is live again
        assert!(select.press(Color::Green).is_some());
    }

    #[test]
    fn resetting_drops_both_branches() {
        let mut select = armed();
        select.reset();
        assert_eq!(select.next_delay_ms(), None);
        assert!(!select.armed());
    }

    #[test]
    fn a_winner_names_a_css_class_per_branch() {
        // the tape colours cells by branch, so these must not collide
        assert_eq!(Won::Timer { waited_s: 3 }.css_name(), "timer");
        assert_eq!(Won::Button { color: Color::Red }.css_name(), "red");
    }
}

#[cfg(test)]
mod loop_select_tests {
    use super::*;

    fn running() -> LoopSelectSim {
        let mut looping = LoopSelectSim::new();
        looping.sync_now(0.0);
        assert!(looping.start());
        assert!(looping.select().armed());
        looping
    }

    #[test]
    fn a_win_records_a_round_and_re_arms() {
        let mut looping = running();
        assert_eq!(
            looping.tick(3_000.0),
            vec![Round {
                round: 1,
                won: Won::Timer { waited_s: 3 },
            }],
        );
        assert_eq!(looping.rounds(), 1);
        assert!(
            looping.select().armed(),
            "the loop goes around again by itself"
        );
        assert_eq!(looping.next_delay_ms(), Some(3_000.0));
    }

    #[test]
    fn the_loop_keeps_going_around() {
        let mut looping = running();
        looping.tick(3_000.0);
        looping.press(Color::Red);
        looping.tick(6_000.0);

        assert_eq!(looping.rounds(), 3);
        assert_eq!(
            looping
                .history()
                .iter()
                .map(|entry| entry.won)
                .collect::<Vec<_>>(),
            vec![
                Won::Timer { waited_s: 3 },
                Won::Button { color: Color::Red },
                Won::Timer { waited_s: 3 },
            ],
        );
    }

    #[test]
    fn the_tape_keeps_only_the_recent_rounds() {
        let mut looping = running();
        for round in 1..=(LOOP_HISTORY + 3) {
            looping.press(Color::Blue);
            assert_eq!(looping.rounds(), round as u64);
        }
        assert_eq!(looping.history().len(), LOOP_HISTORY);
        assert_eq!(
            looping.history().last().expect("a round was played").round,
            (LOOP_HISTORY + 3) as u64,
            "the tape keeps the newest, not the oldest",
        );
    }

    #[test]
    fn a_press_while_stopped_decides_nothing() {
        let mut looping = LoopSelectSim::new();
        assert_eq!(looping.press(Color::Red), None);
        assert_eq!(looping.rounds(), 0);
    }

    #[test]
    fn starting_a_running_loop_is_a_no_op() {
        assert!(!running().start());
    }

}

#[cfg(test)]
mod spawn_tests {
    use super::*;

    #[test]
    fn nothing_runs_until_spawned() {
        let mut spawn = SpawnSim::new();
        assert!(!spawn.spawned());
        assert_eq!(spawn.next_delay_ms(), None);
        assert_eq!(spawn.press(Color::Red), None, "no task, no select");
    }

    #[test]
    fn spawning_starts_the_loop() {
        let mut spawn = SpawnSim::new();
        spawn.sync_now(0.0);
        assert!(spawn.spawn());
        assert!(spawn.looping().select().armed(), "the task is in its select");
    }

    #[test]
    fn there_is_only_ever_one_task() {
        let mut spawn = SpawnSim::new();
        spawn.spawn();
        assert!(!spawn.spawn());
    }

    #[test]
    fn the_spawned_loop_goes_around_and_never_finishes() {
        let mut spawn = SpawnSim::new();
        spawn.sync_now(0.0);
        spawn.spawn();
        assert_eq!(
            spawn.tick(3_000.0),
            vec![Round {
                round: 1,
                won: Won::Timer { waited_s: 3 },
            }],
        );
        spawn.press(Color::Green);
        assert_eq!(spawn.looping().rounds(), 2);
        assert!(
            spawn.looping().select().armed(),
            "after every round the task is back in its select"
        );
    }
}
