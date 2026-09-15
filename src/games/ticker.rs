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
    use_frame_clock().0
}

/// The clock, plus whether it has actually started ticking.
///
/// The flag is what separates "rendering on the server" from "running in the
/// browser", and it has to be a runtime value rather than a
/// `cfg!(target_arch)`: the live fullstack server renders the first paint
/// natively and the wasm client then hydrates it, so a build-target check
/// would have the two disagree — which is precisely the hydration mismatch
/// it looks like it prevents. `use_future` does not poll during server
/// rendering, so `live` is false exactly there.
pub fn use_frame_clock() -> (Signal<f64>, Signal<bool>) {
    let mut clock = use_signal(now_ms);
    let mut live = use_signal(|| false);

    use_future(move || async move {
        loop {
            // Both flags flip only after a frame has elapsed. The client's
            // first render has to reproduce the server's HTML exactly or
            // hydration mismatches, so the sweep starts one frame late
            // rather than on the render that adopts the DOM.
            sleep_ms(FRAME_MS).await;
            if !live() {
                live.set(true);
            }
            clock.set(now_ms());
        }
    });

    (clock, live)
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
        // Nothing pending means nothing the clock can do, so leave the sim
        // untouched. `with_mut` marks the signal dirty whether or not it
        // changed anything, which would re-render every subscriber ten times
        // a second — for an idle game, and for every game in remote mode,
        // where the local sim is never armed at all.
        if sim.peek().next_delay_ms().is_none() {
            return;
        }
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

/// Subscribe the calling component to a signal without using its value.
///
/// Reading a signal inside a component body is what makes Dioxus re-run that
/// body when the signal changes. Where the value itself is not wanted — a
/// component that redraws each frame from another source — the read still
/// has to happen, and `let _ = clock();` reads as a line that does nothing
/// and invites deletion. This says what it is for.
pub fn subscribe<T: Clone + 'static>(signal: Signal<T>) {
    let _subscribed = signal();
}
