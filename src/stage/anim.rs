//! Tween animator — a Rust port of `lib/anim.js` (the standalone games'
//! animation engine). Pure state, no DOM: the caller drives `update(dt)`
//! from a frame loop and reacts in the step/done callbacks.
//!
//! A tween runs `0.0 → 1.0` over its duration. A flight is a tween whose
//! step moves a point from a fixed start toward a `target()` that is
//! re-evaluated every frame, so glyphs track nodes while they are dragged.

/// Linear interpolation.
pub fn lerp(a: f64, b: f64, p: f64) -> f64 {
    a + (b - a) * p
}

/// Cubic ease-in-out (anim.js `easeInOut`).
pub fn ease_in_out(t: f64) -> f64 {
    if t < 0.5 {
        4.0 * t * t * t
    } else {
        1.0 - (-2.0 * t + 2.0).powi(3) / 2.0
    }
}

/// Overshooting ease-out (anim.js `easeOutBack`) for pop-in effects.
pub fn ease_out_back(t: f64) -> f64 {
    let c1 = 1.701_58;
    let c3 = c1 + 1.0;
    1.0 + c3 * (t - 1.0).powi(3) + c1 * (t - 1.0).powi(2)
}

type Step = Box<dyn FnMut(f64)>;
type Done = Box<dyn FnOnce()>;

struct Tween {
    pos: f64,
    dur_ms: f64,
    step: Option<Step>,
    done: Option<Done>,
}

/// Drives tweens in `update(dt_ms)` steps. Order is FIFO: a tween started
/// earlier steps before a later one, so flights land in launch order.
#[derive(Default)]
pub struct Animator {
    tweens: Vec<Tween>,
}

impl Animator {
    pub fn new() -> Self {
        Self::default()
    }

    /// Run `step(p)` for `p: 0→1` over `dur_ms`, then `done`.
    pub fn tween(&mut self, dur_ms: f64, step: impl FnMut(f64) + 'static, done: Done) {
        self.tweens.push(Tween {
            pos: 0.0,
            dur_ms: dur_ms.max(1.0),
            step: Some(Box::new(step)),
            done: Some(done),
        });
    }

    /// Move a point from `from` toward `target()` — re-evaluated every
    /// step, so the glyph chases a node that is being dragged. `on_move`
    /// receives the eased position; `done` runs on arrival.
    pub fn fly(
        &mut self,
        from: (f64, f64),
        dur_ms: f64,
        target: impl Fn() -> (f64, f64) + 'static,
        mut on_move: impl FnMut(f64, f64) + 'static,
        done: Done,
    ) {
        self.tween(
            dur_ms,
            move |p| {
                let q = ease_in_out(p);
                let (tx, ty) = target();
                on_move(lerp(from.0, tx, q), lerp(from.1, ty, q));
            },
            done,
        );
    }

    /// Advance every tween by `dt_ms`. Finishing tweens step at `p = 1`
    /// before their `done` runs.
    pub fn update(&mut self, dt_ms: f64) {
        for i in (0..self.tweens.len()).rev() {
            let tw = &mut self.tweens[i];
            tw.pos = (tw.pos + dt_ms / tw.dur_ms).min(1.0);
            if let Some(step) = tw.step.as_mut() {
                step(tw.pos);
            }
            if tw.pos >= 1.0 {
                let tw = self.tweens.remove(i);
                if let Some(done) = tw.done {
                    done();
                }
            }
        }
    }

    /// Drop every tween without running their `done`s (despawn).
    pub fn clear(&mut self) {
        self.tweens.clear();
    }

    /// True while any tween is running.
    pub fn is_running(&self) -> bool {
        !self.tweens.is_empty()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::Cell;
    use std::rc::Rc;

    #[test]
    fn easing_endpoints_and_shapes() {
        assert_eq!(ease_in_out(0.0), 0.0);
        assert_eq!(ease_in_out(1.0), 1.0);
        assert!(ease_in_out(0.25) < 0.25, "slow start");
        assert!(ease_in_out(0.75) > 0.75, "fast end");

        assert_eq!(ease_out_back(1.0), 1.0);
        assert!(
            ease_out_back(0.8) > 1.0,
            "easeOutBack overshoots before settling"
        );
        assert_eq!(lerp(10.0, 20.0, 0.5), 15.0);
    }

    #[test]
    fn tween_steps_to_one_then_runs_done() {
        let steps = Rc::new(Cell::new(0.0));
        let done = Rc::new(Cell::new(false));
        let mut anim = Animator::new();
        anim.tween(
            100.0,
            {
                let steps = steps.clone();
                move |p| steps.set(p)
            },
            {
                let done = done.clone();
                Box::new(move || done.set(true))
            },
        );
        anim.update(50.0);
        assert!(!done.get());
        assert!((steps.get() - 0.5).abs() < 1e-9);
        anim.update(50.0);
        assert_eq!(steps.get(), 1.0);
        assert!(done.get(), "done runs on the finishing update");
        assert!(!anim.is_running());
    }

    #[test]
    fn flight_chases_a_moving_target_and_finishes_at_it() {
        let target = Rc::new(Cell::new(100.0));
        let pos = Rc::new(Cell::new(0.0));
        let arrived = Rc::new(Cell::new(0.0));
        let mut anim = Animator::new();
        anim.fly(
            (0.0, 0.0),
            100.0,
            {
                let target = target.clone();
                move || (target.get(), 0.0)
            },
            {
                let pos = pos.clone();
                move |x, _y| pos.set(x)
            },
            {
                let arrived = arrived.clone();
                Box::new(move || arrived.set(pos.get()))
            },
        );
        // the target slides away mid-flight; the glyph must chase it
        target.set(200.0);
        anim.update(90.0);
        assert!(pos.get() > 90.0, "eased past the old midpoint");
        anim.update(10.0);
        assert_eq!(arrived.get(), 200.0, "arrival uses the freshest target");
    }

    #[test]
    fn update_clamps_a_huge_dt_and_clear_drops_tweens() {
        let done = Rc::new(Cell::new(false));
        let mut anim = Animator::new();
        anim.tween(
            1000.0,
            |_| {},
            {
                let done = done.clone();
                Box::new(move || done.set(true))
            },
        );
        anim.update(10_000.0);
        assert!(done.get(), "a throttled frame must not jump past done");

        let mut anim = Animator::new();
        anim.tween(100.0, |_| {}, Box::new(|| panic!("cleared tween ran done")));
        anim.clear();
        assert!(!anim.is_running());
        anim.update(1000.0);
    }

    #[test]
    fn duration_is_never_zero() {
        let mut anim = Animator::new();
        anim.tween(0.0, |_| {}, Box::new(|| {}));
        anim.update(1.0);
        assert!(!anim.is_running(), "finished despite zero duration");
    }
}
