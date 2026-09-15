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

/// Milliseconds into the current minute, from the wall clock.
///
/// The dial is read straight off the local wall clock rather than off a
/// snapshot's `wall_ms`. A snapshot only arrives when something happens,
/// which is precisely when a swept hand is least informative — and since the
/// actor derives its own dial from the same wall clock, the two agree to
/// within the room's clock skew without any interpolation.
pub fn wall_now_ms() -> f64 {
    #[cfg(target_arch = "wasm32")]
    {
        // `Date.now()` without a js-sys dependency: the time origin plus the
        // performance clock is the same wall clock.
        with_performance(|performance| performance.time_origin() + performance.now())
            .rem_euclid(60_000.0)
    }
    #[cfg(not(target_arch = "wasm32"))]
    {
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|elapsed| elapsed.as_millis() as f64)
            .unwrap_or_default()
            .rem_euclid(60_000.0)
    }
}

/// The offset that maps the monotonic sim clock onto the wall dial.
///
/// Both the server actor and a single-player browser sim need this, and they
/// need to agree, so it lives here rather than in either one's module.
pub fn wall_offset_ms() -> f64 {
    (wall_now_ms() - crate::sim::now_ms()).rem_euclid(60_000.0)
}

/// Run `read` against the browser's `Performance`, fetching it once.
///
/// `window()` and `performance()` are process-constant but each crossing of
/// the JS boundary costs; this is on every frame of the clock loop, so the
/// handle is cached. wasm is single-threaded, hence `thread_local!`.
#[cfg(target_arch = "wasm32")]
pub(crate) fn with_performance(read: impl Fn(&web_sys::Performance) -> f64) -> f64 {
    thread_local! {
        static PERFORMANCE: Option<web_sys::Performance> =
            web_sys::window().and_then(|window| window.performance());
    }
    PERFORMANCE.with(|performance| performance.as_ref().map(read).unwrap_or_default())
}
