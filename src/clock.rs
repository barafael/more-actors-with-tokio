//! The tick contract: what it takes for a sim to be driven by a clock as
//! well as by wires.
//!
//! Every game before the timer was purely reactive — a wire arrived, state
//! changed, events came back. A future that resolves on its own is the first
//! thing in the deck that happens *because time passed*, and `select!` over
//! two of them is the first thing that needs two clocks at once.
//!
//! [`MpscSim`](crate::sim::MpscSim) already grew this shape by hand for its
//! flight animation: sync the clock, ask how long until something is due,
//! then collect whatever came due. This states that shape once so the timer,
//! select and loop-select sims share it, and so a driver can hold
//! `&mut dyn Ticking<Event = _>` without caring which game it is.
//!
//! The contract deliberately has no `sleep` in it. A sim never waits; it
//! reports *when* it would next like to be polled and the driver — a tokio
//! `select!` on the server, a `requestAnimationFrame` loop in the browser —
//! decides how to wait. That is what keeps the sims testable by stepping a
//! number forward instead of by sleeping.

/// A sim whose state can advance because time passed.
///
/// Drivers use the three methods as a cycle: [`sync_now`](Ticking::sync_now)
/// to tell the sim what time it is, [`poll_due`](Ticking::poll_due) to
/// collect whatever that made happen, and
/// [`next_delay_ms`](Ticking::next_delay_ms) to learn how long to wait
/// before doing it again.
pub trait Ticking {
    /// What this sim emits when time moves it.
    type Event;

    /// Tell the sim what time it is now. Monotonic milliseconds, from
    /// [`now_ms`](crate::sim::now_ms).
    fn sync_now(&mut self, now_ms: f64);

    /// How long until this sim has something to do, or `None` if it is
    /// waiting on a wire rather than on the clock.
    ///
    /// A driver that gets `None` must park until a wire arrives; one that
    /// gets `Some(0.0)` has work due right now.
    fn next_delay_ms(&self) -> Option<f64>;

    /// Collect everything that came due at the synced time.
    ///
    /// Must be idempotent: calling it twice without moving the clock
    /// forward yields events the first time and nothing the second.
    fn poll_due(&mut self) -> Vec<Self::Event>;

    /// Sync and poll in one step, which is what every driver actually wants.
    fn tick(&mut self, now_ms: f64) -> Vec<Self::Event> {
        self.sync_now(now_ms);
        self.poll_due()
    }
}

/// The earliest of two optional delays.
///
/// `select!` over two futures waits for whichever is ready first, so a sim
/// that owns two tickers wants the sooner of their deadlines — and `None`,
/// meaning "not waiting on the clock", must lose to any real delay rather
/// than swallowing it.
pub fn sooner(left: Option<f64>, right: Option<f64>) -> Option<f64> {
    match (left, right) {
        (Some(left), Some(right)) => Some(left.min(right)),
        (delay, None) | (None, delay) => delay,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sooner_picks_the_nearer_deadline() {
        assert_eq!(sooner(Some(5.0), Some(9.0)), Some(5.0));
        assert_eq!(sooner(Some(9.0), Some(5.0)), Some(5.0));
    }

    #[test]
    fn a_sim_waiting_on_a_wire_does_not_swallow_a_real_deadline() {
        // None means "wake me when a wire arrives", not "wake me never":
        // it must not out-vote a branch that genuinely has work due.
        assert_eq!(sooner(None, Some(5.0)), Some(5.0));
        assert_eq!(sooner(Some(5.0), None), Some(5.0));
        assert_eq!(sooner(None, None), None);
    }
}
