/*
 * Collector Actor minigame — composition layer.
 * Reuses the Anim, Channel and Actor libraries.
 * Top-level bindings (actor, dot, senders, ch, ...) stay inspectable so the
 * minigame can be driven and asserted from the outside.
 */
'use strict';

const c = document.getElementById('c');
const ctx = c.getContext('2d');
const toolbar = document.getElementById('toolbar');
const senderOverlay = document.getElementById('sender-overlay');

const BG = '#fff8e1';
const TAU = Math.PI * 2;
const DOT_R = 30;
const SEND_R = 55;
const CAP = 5;

const TRAVEL_MS = 650;
const POP_MS = 450;
const SHRINK_MS = 400;
const DESPAWN_MS = 500;

const ANGLE = (-5 * Math.PI) / 6; // Receive button sits over the oval outline at ~10 o'clock (upper-left shoulder)

const anim = Anim.createAnimator();
const ch = Channel.create({ cap: CAP });
const collector = Actor.create({ width: 230, height: 130, cap: CAP, bg: BG });

let actor = null; // {x, y, scale} center of the collector oval, or null
let dot = null; // {x, y, scale} receiver dot at the oval's top point
let senders = []; // [{x, y, scale, blocked?, pendingChar?}]
let received = []; // chars collected into the actor
let phase = 'idle'; // 'idle' | 'active'
const senderButtonEls = new Map(); // sender -> {wrap, input, clone, drop}
let receiveBtn = null; // actor's Receive button
let closeBtn = null; // receiver's Close button
let closed = false; // receiver closed -> buffer kept, senders cannot send
let pendingReceive = false; // Receive pressed while buffer empty: blocks until a char arrives
const flights = []; // airborne char glyphs
let inFlightReceives = 0;
let legendEl = null; // "what means what" legend panel

function usable() {
  return phase === 'active' && dot;
}

// ---- canvas / viewport ----
let W, H;
function resize() {
  W = window.innerWidth;
  H = window.innerHeight;
  c.width = W;
  c.height = H;
  updateHudPositions();
}
window.addEventListener('resize', resize);
resize();

// ---- drag ----
let drag = null; // {type:'actor'|'sender'|'dot', idx, offX, offY}

// ---- pop-in helpers ----
function popIn(obj, dur) {
  obj.scale = obj.scale || 0.02;
  anim.tween(
    dur,
    (t) => {
      obj.scale = Math.max(0.02, Anim.easeOutBack(t));
    },
    () => {
      obj.scale = 1;
    },
  );
}

function popInSender(s) {
  s.scale = s.scale || 0.02;
  const els = senderButtonEls.get(s);
  anim.tween(
    POP_MS,
    (t) => {
      s.scale = Math.max(0.02, Anim.easeOutBack(t));
      if (els) els.wrap.style.opacity = String(Math.min(1, s.scale));
    },
    () => {
      if (els) els.wrap.style.opacity = '1';
    },
  );
}

// ---- html buttons ----
// Step 1: create the channel (receiver dot + one sender). No actor yet.
const createBtn = document.createElement('button');
createBtn.className = 'btn';
createBtn.textContent = 'Create Channel';
createBtn.onclick = () => {
  const cy = clamp(
    (H * 5) / 6,
    (H * 2) / 3 + DOT_R + collector.halfH + 10,
    H - DOT_R - collector.halfH - 10,
  );
  dot = { x: W / 2, y: cy - collector.halfH };
  senders = [{ x: clamp(dot.x + 190, 90, W - 90), y: clamp(H * 0.3, 90, (H * 2) / 3) }];
  phase = 'active';
  closed = false;
  pendingReceive = false;
  ensureReceiveBtn();
  ensureCloseBtn();
  ensureLegend();
  createSenderUI(senders[0]);
  popIn(dot, POP_MS);
  popInSender(senders[0]);
  updateButtons();
};

// Step 2: spawn the collector actor below the dot, centered in the lower third.
const spawnBtn = document.createElement('button');
spawnBtn.className = 'btn';
spawnBtn.textContent = 'Spawn Actor';
spawnBtn.onclick = () => {
  if (phase !== 'active' || actor || senders.length === 0) return;
  actor = spawnActorPos(dot);
  popIn(actor, POP_MS);
  if (pendingReceive) doReceive(); // a pre-actor receive resolves once the actor exists
  updateButtons();
};

toolbar.appendChild(createBtn);
toolbar.appendChild(spawnBtn);

// ---- receive button (always present while active, left of dot, over oval line) ----
function ensureReceiveBtn() {
  if (receiveBtn) return;
  receiveBtn = document.createElement('button');
  receiveBtn.className = 'btn rcv';
  receiveBtn.textContent = 'Receive';
  receiveBtn.title = 'receive the next char into the collector (blocks while empty)';
  receiveBtn.onclick = () => doReceive();
  senderOverlay.appendChild(receiveBtn);
}

// ---- close button (right of the receiver dot) ----
function ensureCloseBtn() {
  if (closeBtn) return;
  closeBtn = document.createElement('button');
  closeBtn.className = 'btn small close';
  closeBtn.textContent = 'Close';
  closeBtn.title =
    'close the receiver: buffer content stays, every sender turns gray and can no longer send';
  closeBtn.onclick = () => closeReceiver();
  senderOverlay.appendChild(closeBtn);
}

// ---- sender ui ----
function createSenderUI(s) {
  const wrap = document.createElement('div');
  wrap.className = 'side-btns';

  const input = document.createElement('input');
  input.className = 'field';
  input.maxLength = 1;
  input.title = 'type a char to send';

  const clone = document.createElement('button');
  clone.className = 'btn small clone';
  clone.textContent = 'Clone';

  const drop = document.createElement('button');
  drop.className = 'btn small drop';
  drop.textContent = 'Drop';

  input.addEventListener('input', () => {
    if (s.blocked) {
      input.value = s.pendingChar || '';
      return;
    }
    const val = input.value;
    if (!val) return;
    input.value = '';
    launchSendFlight(s, val);
  });

  clone.onclick = () => {
    const target = placeNextTo(s);
    const start = { x: s.x, y: s.y };
    const ns = { x: start.x, y: start.y };
    senders.push(ns);
    createSenderUI(ns);
    if (closed) {
      ns.blocked = true;
      const nels = senderButtonEls.get(ns);
      if (nels) {
        nels.input.disabled = true;
      }
    }
    anim.tween(
      TRAVEL_MS,
      (t) => {
        const q = Anim.easeInOut(t);
        ns.x = Anim.lerp(start.x, target.x, q);
        ns.y = Anim.lerp(start.y, target.y, q);
        updateHudPositions();
      },
      () => {
        ns.x = target.x;
        ns.y = target.y;
        updateButtons();
      },
    );
    updateButtons();
  };

  drop.onclick = () => {
    cancelFlightsForSender(s);
    ch.removeBlocked(s);
    const els = senderButtonEls.get(s);
    const finish = () => {
      removeSenderUI(s);
      senders = senders.filter((o) => o !== s);
      if (senders.length === 0) animateDespawn();
      else updateButtons();
    };
    anim.tween(
      SHRINK_MS,
      (t) => {
        const q = Anim.easeInOut(t);
        s.scale = 1 - q;
        if (els) els.wrap.style.opacity = String(Math.max(0.25, 1 - q));
      },
      finish,
    );
  };

  wrap.append(input, clone, drop);
  senderOverlay.appendChild(wrap);
  senderButtonEls.set(s, { wrap, input, clone, drop });
}

function removeSenderUI(s) {
  const els = senderButtonEls.get(s);
  if (els) els.wrap.remove();
  senderButtonEls.delete(s);
}

// ---- flights ----
function drawFlights() {
  for (const f of flights) {
    ctx.fillStyle = '#fff';
    ctx.strokeStyle = '#333';
    ctx.lineWidth = 2;
    ctx.beginPath();
    ctx.arc(f.x, f.y, 12, 0, TAU);
    ctx.fill();
    ctx.stroke();
    ctx.fillStyle = '#000';
    ctx.font = 'bold 14px sans-serif';
    ctx.textAlign = 'center';
    ctx.textBaseline = 'middle';
    ctx.fillText(f.ch, f.x, f.y);
  }
}

function launchSendFlight(s, val) {
  if (!usable()) return;
  if (closed) {
    s.blocked = true;
    const els = senderButtonEls.get(s);
    if (els) els.input.disabled = true;
    return;
  }
  const idx = ch.length + ch.reserved;
  if (!ch.launchSend(val)) {
    s.blocked = true;
    s.pendingChar = val;
    ch.block(val, s);
    const els = senderButtonEls.get(s);
    if (els) els.input.value = val;
    updateButtons();
    return;
  }
  const sx0 = s.x,
    sy0 = s.y;
  const f = { ch: val, x: sx0, y: sy0 };
  flights.push(f);
  const target = () => (usable() ? collector.slotXY(dot.x, dot.y, idx) : { x: f.x, y: f.y });
  Anim.createFlight(anim, {
    fromX: sx0,
    fromY: sy0,
    dur: TRAVEL_MS,
    target,
    onUpdate: (x, y) => {
      f.x = x;
      f.y = y;
    },
    done: () => {
      if (!usable()) return;
      const i = flights.indexOf(f);
      if (i < 0) {
        ch.release();
        return;
      }
      flights.splice(i, 1);
      ch.land(val);
      if (pendingReceive) doReceive();
      updateButtons();
    },
  });
}

function launchDeliveryFlight(tx) {
  const s = tx.meta;
  const idx = ch.length + ch.reserved;
  if (!ch.launchSend(tx.ch)) {
    ch.block(tx.ch, tx.meta);
    return;
  }
  const sentEls = senderButtonEls.get(s);
  if (sentEls) sentEls.input.value = ''; // source char gone once movement starts
  const sx0 = s.x,
    sy0 = s.y;
  const f = { ch: tx.ch, x: sx0, y: sy0, delivery: true, sender: s };
  flights.push(f);
  const target = () => (usable() ? collector.slotXY(dot.x, dot.y, idx) : { x: f.x, y: f.y });
  Anim.createFlight(anim, {
    fromX: sx0,
    fromY: sy0,
    dur: TRAVEL_MS,
    target,
    onUpdate: (x, y) => {
      f.x = x;
      f.y = y;
    },
    done: () => {
      const i = flights.indexOf(f);
      if (i < 0) return;
      flights.splice(i, 1);
      if (!usable() || f.cancelled || closed) {
        ch.release();
        return;
      }
      ch.land(tx.ch);
      s.pendingChar = null;
      s.blocked = false;
      const els = senderButtonEls.get(s);
      if (els) els.input.value = '';
      if (pendingReceive) doReceive();
      updateButtons();
    },
  });
}

function tryDeliverPending() {
  if (closed) return;
  let guard = 0;
  while (guard++ < 20) {
    const tx = ch.pickBlocked();
    if (!tx) break;
    launchDeliveryFlight(tx);
  }
}

function cancelFlightsForSender(s) {
  for (const f of flights) if (f.delivery && f.sender === s) f.cancelled = true;
}

// Receive is never disabled: it can always be pressed and simply becomes an
// async block when it cannot complete right away. A press while the buffer is
// empty (or before the actor exists, or while another receive is in flight)
// only queues a pending receive that auto-delivers as soon as a char lands.
function doReceive() {
  if (inFlightReceives > 0) {
    pendingReceive = true;
    updateButtons();
    return;
  }
  if (!actor || ch.length === 0) {
    pendingReceive = true;
    updateButtons();
    return;
  }
  launchReceiveFlight();
}

function launchReceiveFlight() {
  inFlightReceives++;
  pendingReceive = false;
  const start = collector.slotXY(dot.x, dot.y, 0);
  const f = { ch: ch.first, x: start.x, y: start.y };
  flights.push(f);
  const target = () => (usable() ? collector.receivedXY(dot.x, dot.y) : { x: f.x, y: f.y });
  Anim.createFlight(anim, {
    fromX: start.x,
    fromY: start.y,
    dur: TRAVEL_MS,
    target,
    onUpdate: (x, y) => {
      f.x = x;
      f.y = y;
    },
    done: () => {
      if (!usable()) return;
      const i = flights.indexOf(f);
      if (i < 0) {
        inFlightReceives--;
        return;
      }
      flights.splice(i, 1);
      inFlightReceives--;
      const val = ch.receive();
      if (val !== null) received.push(val);
      tryDeliverPending();
      if (pendingReceive && ch.length > 0) launchReceiveFlight();
      updateButtons();
    },
  });
}

// Closing the receiver keeps the buffer content; senders gray out and can't send at all.
function closeReceiver() {
  if (phase !== 'active' || closed || !dot) return;
  closed = true;
  pendingReceive = false;
  for (const s of senders) {
    s.blocked = true;
    s.pendingChar = null;
    ch.removeBlocked(s);
    const els = senderButtonEls.get(s);
    if (els) {
      els.input.disabled = true;
      els.input.value = '';
    }
  }
  updateButtons();
}

// ---- legend (what means what) ----
function ensureLegend() {
  if (legendEl) return;
  const row = (size, color, label) => {
    const div = document.createElement('div');
    div.className = 'legend-row';
    const dot = document.createElement('span');
    dot.className = 'legend-circle';
    dot.style.width = size;
    dot.style.height = size;
    dot.style.background = color;
    const lab = document.createElement('span');
    lab.className = 'legend-label';
    lab.textContent = label;
    div.append(dot, lab);
    return div;
  };
  const el = document.createElement('div');
  el.id = 'legend';
  el.appendChild(row('30px', '#2e7d32', 'Sender (Tx) — type a char to send'));
  el.appendChild(
    row('30px', '#9e9e9e', 'Sender blocked — channel full (or receiver closed), cannot send'),
  );
  el.appendChild(row('16px', '#000000', 'Receiver (single consumer)'));
  document.body.appendChild(el);
  legendEl = el;
}

// ---- hud positioning ----
function updateHudPositions() {
  senderButtonEls.forEach((els, s) => {
    const sw = els.wrap.offsetWidth || 150;
    const sh = els.wrap.offsetHeight || 26;
    els.wrap.style.left = s.x - sw / 2 + 'px';
    els.wrap.style.top = s.y - SEND_R - sh - 8 + 'px';
  });
  if (receiveBtn && dot) {
    const bw = receiveBtn.offsetWidth || 86;
    const bh = receiveBtn.offsetHeight || 36;
    const ox = dot.x + collector.halfW * Math.cos(ANGLE);
    const oy = dot.y + collector.halfH + collector.halfH * Math.sin(ANGLE);
    receiveBtn.style.left = ox - bw / 2 + 'px';
    receiveBtn.style.top = oy - bh / 2 + 'px';
  }
  if (closeBtn && dot) {
    const bw = closeBtn.offsetWidth || 64;
    const bh = closeBtn.offsetHeight || 30;
    closeBtn.style.left = dot.x + DOT_R + 10 + 'px';
    closeBtn.style.top = dot.y - bh / 2 + 'px';
  }
}

function updateButtons() {
  createBtn.disabled = phase !== 'idle';
  spawnBtn.disabled = !(phase === 'active' && actor === null && senders.length > 0);
  if (receiveBtn) {
    receiveBtn.disabled = false;
    receiveBtn.classList.toggle('waiting', pendingReceive);
  }
  if (closeBtn) closeBtn.disabled = phase !== 'active' || closed || !dot;
  updateHudPositions();
}

// ---- life cycle ----
function animateDespawn() {
  flights.length = 0;
  inFlightReceives = 0;
  if (!actor) {
    doDespawn();
    return;
  }
  anim.tween(
    DESPAWN_MS,
    (t) => {
      const q = Anim.easeInOut(t);
      actor.scale = 1 - q;
      dot.scale = Math.max(0, 1 - q);
      if (receiveBtn) receiveBtn.style.opacity = String(1 - q);
    },
    doDespawn,
  );
}

function doDespawn() {
  for (const els of senderButtonEls.values()) els.wrap.remove();
  senderButtonEls.clear();
  if (receiveBtn) {
    receiveBtn.style.opacity = '1';
    receiveBtn.remove();
    receiveBtn = null;
  }
  if (closeBtn) {
    closeBtn.style.opacity = '1';
    closeBtn.remove();
    closeBtn = null;
  }
  if (legendEl) {
    legendEl.remove();
    legendEl = null;
  }
  actor = null;
  dot = null;
  senders = [];
  received = [];
  closed = false;
  pendingReceive = false;
  ch.reset();
  inFlightReceives = 0;
  flights.length = 0;
  anim.clear();
  phase = 'idle';
  updateButtons();
}

function placeNextTo(s) {
  const idx = s._clones || 0;
  s._clones = idx + 1;
  const d = 160;
  const cand = [
    { x: s.x + d, y: s.y },
    { x: s.x - d, y: s.y },
    { x: s.x, y: s.y + d },
    { x: s.x, y: s.y - d },
  ][idx % 4];
  return { x: clamp(cand.x, 90, W - 90), y: clamp(cand.y, 90, (H * 2) / 3) };
}

function spawnActorPos(d) {
  return collector.center(d.x, d.y);
}

function clamp(v, lo, hi) {
  return Math.max(lo, Math.min(hi, v));
}

// ---- draw ----
function drawArrow(x1, y1, x2, y2) {
  const dx = x2 - x1,
    dy = y2 - y1;
  const len = Math.hypot(dx, dy);
  if (len < 1) return;
  const ux = dx / len,
    uy = dy / len;
  const sx = x1 + ux * SEND_R;
  const sy = y1 + uy * SEND_R;
  const ex = x2 - ux * DOT_R;
  const ey = y2 - uy * DOT_R;
  ctx.strokeStyle = '#333';
  ctx.lineWidth = 2;
  ctx.beginPath();
  ctx.moveTo(sx, sy);
  ctx.lineTo(ex, ey);
  ctx.stroke();
  const as = 10;
  const a1x = ex - ux * as - uy * as * 0.6;
  const a1y = ey - uy * as + ux * as * 0.6;
  const a2x = ex - ux * as + uy * as * 0.6;
  const a2y = ey - uy * as - ux * as * 0.6;
  ctx.fillStyle = '#333';
  ctx.beginPath();
  ctx.moveTo(ex, ey);
  ctx.lineTo(a1x, a1y);
  ctx.lineTo(a2x, a2y);
  ctx.closePath();
  ctx.fill();
}

function drawSender(s) {
  const r = SEND_R * (s.scale || 1);
  ctx.fillStyle = s.blocked ? '#9e9e9e' : '#2e7d32';
  ctx.beginPath();
  ctx.arc(s.x, s.y, r, 0, TAU);
  ctx.fill();
  ctx.strokeStyle = '#1b5e20';
  ctx.lineWidth = 2;
  ctx.stroke();
  ctx.fillStyle = '#fff';
  ctx.font = 'bold 14px sans-serif';
  ctx.textAlign = 'center';
  ctx.textBaseline = 'middle';
  ctx.fillText('Tx', s.x, s.y);
}

function draw() {
  ctx.fillStyle = BG;
  ctx.fillRect(0, 0, W, H);
  ctx.strokeStyle = 'rgba(235,91,32,0.10)';
  ctx.lineWidth = 1;
  ctx.beginPath();
  for (let x = 0; x < W; x += 40) {
    ctx.moveTo(x, 0);
    ctx.lineTo(x, H);
  }
  for (let y = 0; y < H; y += 40) {
    ctx.moveTo(0, y);
    ctx.lineTo(W, y);
  }
  ctx.stroke();

  if (dot) {
    // arrows first (behind everything)
    for (const s of senders) drawArrow(s.x, s.y, dot.x, dot.y);
    // while a receive flight is airborne the source slot's char rides on the
    // badge — never two instances of the same char visible at once
    const shown = inFlightReceives > 0 ? ch.items.slice(1) : ch.items;
    // collector actor (oval + interior) via the component — appears on "Spawn Actor"
    if (actor) {
      collector.draw(ctx, {
        x: dot.x,
        y: dot.y,
        scale: actor.scale,
        items: shown,
        received,
      });
    } else {
      // buffer region always exists — the channel is there even before the actor
      collector.drawBufferOnly(ctx, { x: dot.x, y: dot.y, items: shown });
    }
    // receiver dot on top of the oval's top point
    const dr = DOT_R * (dot.scale || 1);
    ctx.fillStyle = closed ? '#666' : '#000';
    ctx.beginPath();
    ctx.arc(dot.x, dot.y, dr, 0, TAU);
    ctx.fill();
    ctx.fillStyle = '#fff';
    ctx.font = 'bold 22px sans-serif';
    ctx.textAlign = 'center';
    ctx.textBaseline = 'middle';
    ctx.fillText(senders.length, dot.x, dot.y + 1);
  }

  for (const s of senders) drawSender(s);
  drawFlights();
}

// ---- input ----
function hitObject(mx, my) {
  if (actor && Math.hypot(mx - actor.x, my - actor.y) < collector.halfW + 5)
    return { type: 'actor' };
  if (dot && Math.hypot(mx - dot.x, my - dot.y) < DOT_R + 5) return { type: 'dot' };
  for (let i = 0; i < senders.length; i++) {
    if (Math.hypot(mx - senders[i].x, my - senders[i].y) < SEND_R + 5)
      return { type: 'sender', idx: i };
  }
  return null;
}

c.addEventListener('mousedown', (e) => {
  const mx = e.clientX,
    my = e.clientY;
  const obj = hitObject(mx, my);
  if (obj) {
    if (obj.type === 'actor') drag = { type: 'actor', offX: mx - actor.x, offY: my - actor.y };
    else if (obj.type === 'dot') drag = { type: 'dot', offX: mx - dot.x, offY: my - dot.y };
    else
      drag = {
        type: 'sender',
        idx: obj.idx,
        offX: mx - senders[obj.idx].x,
        offY: my - senders[obj.idx].y,
      };
  }
});

c.addEventListener('mousemove', (e) => {
  if (!drag) return;
  const mx = e.clientX,
    my = e.clientY;
  if (drag.type === 'actor') {
    actor.x = mx - drag.offX;
    actor.y = my - drag.offY;
    dot.x = actor.x;
    dot.y = actor.y - collector.halfH;
  } else if (drag.type === 'dot') {
    dot.x = mx - drag.offX;
    dot.y = my - drag.offY;
    if (actor) {
      actor.x = dot.x;
      actor.y = dot.y + collector.halfH;
    }
  } else {
    senders[drag.idx].x = mx - drag.offX;
    senders[drag.idx].y = my - drag.offY;
  }
  updateHudPositions();
});

c.addEventListener('mouseup', () => {
  drag = null;
});

// ---- init ----
updateButtons();
let lastT = performance.now();
function loop(now) {
  const dt = Math.min(0.05, (now - lastT) / 1000);
  lastT = now;
  anim.update(dt);
  draw();
  requestAnimationFrame(loop);
}
requestAnimationFrame(loop);
