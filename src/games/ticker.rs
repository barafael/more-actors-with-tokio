//! The browser half of the tick contract: a signal that follows the clock.
//!
//! On the server, a ticking sim is driven by a `select!` branch that sleeps
//! until the next deadline. In the browser there is no runtime to sleep in,
//! so the same job is done by a frame loop — and unlike the server's, it must
//! run even in remote mode, because the seconds dial has to keep sweeping
//! between snapshots rather than jumping each time one arrives.
//!
//! That split is deliberate. The *authority* over when a future resolves is
//! the actor; this only animates the gap. A client whose frame loop stalls
//! draws a stuttering dial and still agrees with everyone else about who won
//! the race.

use dioxus::prelude::*;

use crate::clock::Ticking;
use crate::sim::now_ms;

/// How often the dial is redrawn, in milliseconds.
///
/// Not a frame rate: the hand sweeps a 60-second dial, so ~10fps is already
/// smoother than the eye needs at the back of a room, and a phone in
/// someone's pocket should not burn a core on it.
const FRAME_MS: u32 = 100;

/// A signal carrying monotonic milliseconds, updated continuously.
///
/// Call once per game component; the loop stops when the component unmounts.
pub fn use_clock() -> Signal<f64> {
    let mut clock = use_signal(now_ms);

    use_future(move || async move {
        loop {
            sleep_ms(FRAME_MS).await;
            clock.set(now_ms());
        }
    });

    clock
}

/// Drive a local (single-player) sim from the clock signal.
///
/// The remote counterpart of this is the actor's timer branch. Here the
/// component re-runs whenever `clock` changes, polls whatever the clock made
/// due, and hands the events to `apply` — the same events the socket would
/// have delivered, so the rendering path is shared.
pub fn use_local_tick<S, F>(mut sim: Signal<S>, clock: Signal<f64>, mut apply: F)
where
    S: Ticking + 'static,
    F: FnMut(S::Event) + 'static,
{
    use_effect(move || {
        let now = clock();
        let events = sim.with_mut(|sim| sim.tick(now));
        for event in events {
            apply(event);
        }
    });
}

/// Sleep without pulling an async runtime into the client.
///
/// Native builds (the SSR export binary) have no timer here and no need for
/// one: they render one frame and exit, so the loop simply never advances.
async fn sleep_ms(ms: u32) {
    #[cfg(target_arch = "wasm32")]
    gloo_timers::future::TimeoutFuture::new(ms).await;
    #[cfg(not(target_arch = "wasm32"))]
    {
        let _ = ms;
        std::future::pending::<()>().await;
    }
}
