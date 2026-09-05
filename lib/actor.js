/*
 * Actor — reusable "Collector" actor component.
 * A rounded-rect-labelled oval pinned below an anchor point (the receiver dot
 * sits on the oval's top point). Draws the inner buffer slots, the oval
 * outline with a "Task" pill at ~2 o'clock, a boxed received-chars display,
 * a "Business Logic" label, and a "Collector" label below the oval.
 * All interior layout scales with a pop-in `scale` about the anchor.
 * The component is stateless for the minigame state: pass items/received in.
 * Geometry helpers slotXY()/receivedXY() let callers aim flights at the same
 * positions the drawing uses.
 */
(function (global) {
  'use strict';

  const TAU = Math.PI * 2;

  const LAYOUT = {
    bufferY: 36, // top of the slot row, relative to the anchor (oval top point)
    slotW: 20,
    slotH: 22,
    gap: 5,
    boxY: 62, // top of the received-chars box
    boxW: 120,
    boxH: 32,
    receivedY: 78, // vertical center of the box / received chars
    businessY: 108, // 'Business Logic' label center
    labelDx: 22, // 'Collector' label distance below the oval bottom
  };

  function createActor(opts) {
    const cfg = Object.assign({ width: 230, height: 130, cap: 5, bg: '#fff8e1' }, opts);
    const halfW = cfg.width / 2;
    const halfH = cfg.height / 2;
    const slotsTotal = cfg.cap * LAYOUT.slotW + (cfg.cap - 1) * LAYOUT.gap;

    function drawBuffer(ctx, dots) {
      const x0 = -slotsTotal / 2;
      const y0 = LAYOUT.bufferY;
      for (let i = 0; i < cfg.cap; i++) {
        const x = x0 + i * (LAYOUT.slotW + LAYOUT.gap);
        const filled = i < dots.length;
        ctx.strokeStyle = filled ? '#000' : '#bbb';
        ctx.lineWidth = 2;
        ctx.fillStyle = filled ? '#fff' : 'rgba(0,0,0,0)';
        ctx.beginPath();
        ctx.roundRect(x, y0, LAYOUT.slotW, LAYOUT.slotH, 4);
        ctx.fill();
        ctx.stroke();
        if (filled) {
          ctx.fillStyle = '#000';
          ctx.font = 'bold 14px sans-serif';
          ctx.textAlign = 'center';
          ctx.textBaseline = 'middle';
          ctx.fillText(dots[i], x + LAYOUT.slotW / 2, y0 + LAYOUT.slotH / 2);
        }
      }
    }

    // Draw just the buffer slot row at the anchor. Used when no consumer actor
    // is spawned yet — the channel's buffer region always exists, actor or not.
    function drawBufferOnly(ctx, o) {
      const s = o.scale || 1;
      ctx.save();
      ctx.translate(o.x, o.y);
      ctx.scale(s, s);
      drawBuffer(ctx, o.items || []);
      ctx.restore();
    }

    // o = { x, y, scale?, items?, received? } where x,y is the anchor (top point).
    function draw(ctx, o) {
      const s = o.scale || 1;
      ctx.save();
      ctx.translate(o.x, o.y);
      ctx.scale(s, s);

      drawBuffer(ctx, o.items || []);

      // oval outline
      ctx.strokeStyle = '#000';
      ctx.lineWidth = 3;
      ctx.beginPath();
      ctx.ellipse(0, halfH, halfW, halfH, 0, 0, TAU);
      ctx.stroke();

      // "Task" pill centered on the rightmost point of the oval outline
      const lx = halfW;
      const ly = halfH;
      ctx.font = 'bold 14px sans-serif';
      ctx.textAlign = 'center';
      ctx.textBaseline = 'middle';
      const tw = ctx.measureText('Task').width;
      const pad = 5;
      ctx.fillStyle = cfg.bg;
      ctx.beginPath();
      ctx.roundRect(lx - tw / 2 - pad, ly - 11 - pad, tw + pad * 2, 22 + pad * 2, 6);
      ctx.fill();
      ctx.strokeStyle = '#000';
      ctx.lineWidth = 1.5;
      ctx.stroke();
      ctx.fillStyle = '#000';
      ctx.fillText('Task', lx, ly);

      // received-chars box
      ctx.strokeStyle = '#000';
      ctx.lineWidth = 2;
      ctx.beginPath();
      ctx.roundRect(-LAYOUT.boxW / 2, LAYOUT.boxY, LAYOUT.boxW, LAYOUT.boxH, 6);
      ctx.stroke();
      const rc = (o.received || []).join('').slice(-9);
      if (rc) {
        ctx.fillStyle = '#000';
        ctx.font = 'bold 18px monospace';
        ctx.textAlign = 'center';
        ctx.textBaseline = 'middle';
        ctx.fillText(rc, 0, LAYOUT.receivedY);
      }

      // business logic label
      ctx.fillStyle = '#000';
      ctx.font = 'bold 14px sans-serif';
      ctx.textAlign = 'center';
      ctx.textBaseline = 'middle';
      ctx.fillText('Business Logic', 0, LAYOUT.businessY);

      // collector label below the oval
      ctx.font = 'bold 18px sans-serif';
      ctx.textBaseline = 'baseline';
      ctx.fillText('Collector', 0, cfg.height + LAYOUT.labelDx);

      ctx.restore();
    }

    return {
      config: cfg,
      halfW,
      halfH,
      // anchor (receiver-dot position) for a given oval center
      topPoint(cx, cy) {
        return { x: cx, y: cy - halfH };
      },
      // oval center for a given anchor position
      center(ax, ay) {
        return { x: ax, y: ay + halfH };
      },
      // position of buffer-slot `idx` for a given anchor
      slotXY(ax, ay, idx) {
        return {
          x: ax + idx * (LAYOUT.slotW + LAYOUT.gap) - slotsTotal / 2 + LAYOUT.slotW / 2,
          y: ay + LAYOUT.bufferY + LAYOUT.slotH / 2,
        };
      },
      // position of the received-chars display for a given anchor
      receivedXY(ax, ay) {
        return { x: ax, y: ay + LAYOUT.receivedY };
      },
      draw,
      drawBufferOnly,
    };
  }

  const Actor = { create: createActor };
  if (typeof module !== 'undefined' && module.exports) module.exports = Actor;
  global.Actor = global.Actor || Actor;
})(typeof window !== 'undefined' ? window : globalThis);
