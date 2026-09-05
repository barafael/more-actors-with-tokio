/*
 * Anim — reusable animation/tween library.
 * Exposes: lerp, easeInOut, easeOutBack, createAnimator(), createFlight(anim, opts).
 * An animator drives tweens in its update(dt) step (dt in seconds).
 * A flight moves a marker from a fixed start toward a dynamic target() point
 * each frame; target() is re-evaluated per frame so anchored flights track
 * moving objects.
 */
(function (global) {
  'use strict';

  function lerp(a, b, p) {
    return a + (b - a) * p;
  }

  function easeInOut(t) {
    return t < 0.5 ? 4 * t * t * t : 1 - Math.pow(-2 * t + 2, 3) / 2;
  }

  function easeOutBack(t) {
    const c1 = 1.70158,
      c3 = c1 + 1;
    return 1 + c3 * Math.pow(t - 1, 3) + c1 * Math.pow(t - 1, 2);
  }

  function createAnimator() {
    const tweens = [];
    return {
      tween(dur, step, done) {
        tweens.push({ pos: 0, dur: Math.max(1, dur), step, done });
      },
      update(dt) {
        for (let i = tweens.length - 1; i >= 0; i--) {
          const tw = tweens[i];
          tw.pos = Math.min(1, tw.pos + (dt * 1000) / tw.dur);
          if (tw.step) tw.step(tw.pos);
          if (tw.pos >= 1) {
            tweens.splice(i, 1);
            if (tw.done) tw.done();
          }
        }
      },
      clear() {
        tweens.length = 0;
      },
    };
  }

  function createFlight(anim, o) {
    anim.tween(
      o.dur,
      (t) => {
        const q = easeInOut(t);
        const tg = o.target();
        o.onUpdate(lerp(o.fromX, tg.x, q), lerp(o.fromY, tg.y, q));
      },
      o.done,
    );
  }

  const Anim = { lerp, easeInOut, easeOutBack, createAnimator, createFlight };
  if (typeof module !== 'undefined' && module.exports) module.exports = Anim;
  global.Anim = global.Anim || Anim;
})(typeof window !== 'undefined' ? window : globalThis);
