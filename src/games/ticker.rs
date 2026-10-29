//! The browser half of the tick contract: a signal that follows the clock.
//!
//! A ticking sim reports when it next wants polling; in the browser there is
//! no runtime to sleep in, so the job is done by a frame loop. It keeps
//! running even while no sim wants polling, so a game can redraw from it
//! whenever it likes.
//!
//! That split is deliberate. The *authority* over when a future resolves is
//! the sim; this only animates the gap. A frame loop that stalls
//! draws a stuttering dial and still agrees with the sim about who won
//! the race.

use dioxus::prelude::*;

use crate::clock::now_ms;
use crate::clock::Ticking;

/// How often the dial is redrawn, in milliseconds.
///
/// Not a frame rate: the hand sweeps a ten-second dial, so 20fps moves it
/// under two degrees a frame, smoother than the eye follows from a seat,
/// without burning a core.
const FRAME_MS: u32 = 50;

/// A signal carrying monotonic milliseconds, updated continuously.
///
/// Call once per game component; the loop stops when the component unmounts.
pub fn use_clock() -> Signal<f64> {
    use_frame_clock().0
}

/// The clock, plus whether it has actually started ticking.
///
/// The flag is what separates "rendering for the export" from "running in the
/// browser", and it has to be a runtime value rather than a
/// `cfg!(target_arch)`: the SSR export renders the first paint natively and
/// the wasm client then hydrates it, so a build-target check would have the
/// two disagree — which is precisely the hydration mismatch it looks like it
/// prevents. `use_future` does not poll during server rendering, so `live` is
/// false exactly there.
pub fn use_frame_clock() -> (Signal<f64>, Signal<bool>) {
    let mut clock = use_signal(now_ms);
    let mut live = use_signal(|| false);

    use_future(move || async move {
        loop {
            // Both flags flip only after a frame has elapsed. The client's
            // first render has to reproduce the exported HTML exactly or
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

/// Drive a local sim from the clock signal.
///
/// The component re-runs whenever `clock` changes, polls whatever the clock
/// made due, and hands the events to `apply` — the same events a driver would
/// have produced, so the rendering path is shared.
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
        // a second — for an idle game, where the local sim is never armed.
        if sim.peek().next_delay_ms().is_none() {
            return;
        }
        let events = sim.with_mut(|sim| sim.tick(now));
        for event in events {
            apply(event);
        }
    });
}

/// [`use_local_tick`] for a sim whose clock-driven changes need no cue
/// beyond the state itself: the component reads the sim, and the tick's
/// `with_mut` is what re-renders it.
pub fn use_tick<S: Ticking + 'static>(sim: Signal<S>, clock: Signal<f64>) {
    use_local_tick(sim, clock, |_| {});
}

/// Act on a ticking sim at the current instant.
///
/// A click happens between frames, so the sim's clock is up to a frame
/// stale; anything it stamps or schedules has to be measured from now.
pub fn act_now<S: Ticking + 'static, R>(mut sim: Signal<S>, act: impl FnOnce(&mut S) -> R) -> R {
    sim.with_mut(|sim| {
        sim.sync_now(now_ms());
        act(sim)
    })
}

/// Sleep without pulling an async runtime into the client.
///
/// Native builds (the SSR export binary) have no timer here and no need for
/// one: they render one frame and exit, so the loop simply never advances.
pub async fn sleep_ms(ms: u32) {
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
