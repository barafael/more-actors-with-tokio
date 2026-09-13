/*
 * mpsc — live visualization of a tokio::sync::mpsc bounded channel.
 * One dark receiver dot owns a bounded buffer of capacity CAP right below it.
 * Green senders type chars that fly into the buffer; a send while the buffer is
 * full blocks that sender until the receiver takes a char. Receive() always
 * works: pressed with an empty buffer it simply blocks until a char lands.
 * Each successful receive pulls the oldest char out of the buffer onto the
 * receiver dot, remembers it as the last received value and
 * unblocks a blocked sender.
 * The receiver dot and every sender can be dragged around.
 * Reuses the Anim and Channel libraries.
 * Top-level bindings (dot, senders, ch, ...) stay inspectable so the minigame
 * can be driven and asserted from the outside.
 */
'use strict';

const c = document.getElementById('c');
const ctx = c.getContext('2d');
const toolbar = document.getElementById('toolbar');
const senderOverlay = document.getElementById('sender-overlay');

const TAU = Math.PI * 2;
const DOT_R = 30;
const SEND_R = 55;
const CAP = 5;

const SLOT_W = 20,
  SLOT_H = 22,
  SLOT_GAP = 5;
const BUFF_DY = 70; // buffer strip center, below the receiver dot

const TRAVEL_MS = 650;
const POP_MS = 450;
const SHRINK_MS = 400;
const DESPAWN_MS = 500;

const anim = Anim.createAnimator();
const ch = Channel.create({ cap: CAP });

let dot = null; // {x, y, scale} receiver dot holding the buffer
let senders = []; // [{x, y, scale, blocked?, pendingChar?}]
let received = []; // chars consumed by the receiver, oldest first
let phase = 'idle'; // 'idle' | 'active'
const senderButtonEls = new Map(); // sender -> {wrap, input, clone, drop}
let receiveBtn = null; // receiver's Receive button
let lastReceivedEl = null; // label left of Receive showing the last received char
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
  const dpr = Math.min(window.devicePixelRatio || 1, 2); // render crisp on scaled displays
  W = window.innerWidth;
  H = window.innerHeight;
  c.width = Math.round(W * dpr);
  c.height = Math.round(H * dpr);
  ctx.setTransform(dpr, 0, 0, dpr, 0, 0); // keep drawing in CSS-pixel units
  updateHudPositions();
}
window.addEventListener('resize', resize);
resize();

// ---- drag ----
let drag = null; // {type:'sender'|'dot', idx, offX, offY}

// ---- geometry ----
function slotTotal() {
  return CAP * SLOT_W + (CAP - 1) * SLOT_GAP;
}
function slotXY(i) {
  const x0 = dot.x - slotTotal() / 2;
  return { x: x0 + i * (SLOT_W + SLOT_GAP) + SLOT_W / 2, y: dot.y + BUFF_DY };
}

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
const createBtn = document.createElement('button');
createBtn.className = 'btn';
createBtn.textContent = 'Create Channel';
createBtn.onclick = () => {
  dot = { x: W / 2, y: clamp((H * 34) / 100, 160, (H * 5) / 10) };
  senders = [{ x: clamp(dot.x + 190, 90, W - 90), y: clamp(H * 0.3, 90, (H * 2) / 3) }];
  phase = 'active';
  pendingReceive = false;
  ensureReceiveBtn();
  ensureLegend();
  createSenderUI(senders[0]);
  popIn(dot, POP_MS);
  popInSender(senders[0]);
  updateButtons();
};

toolbar.appendChild(createBtn);

// ---- receive button (always present while active, left of the dot) ----
function ensureReceiveBtn() {
  if (receiveBtn) return;
  receiveBtn = document.createElement('button');
  receiveBtn.className = 'btn rcv';
  receiveBtn.textContent = 'Receive';
  receiveBtn.title = 'receive the next char from the buffer (blocks while empty)';
  receiveBtn.onclick = () => doReceive();
  senderOverlay.appendChild(receiveBtn);

  lastReceivedEl = document.createElement('span');
  lastReceivedEl.className = 'recv-label';
  lastReceivedEl.textContent = '';
  senderOverlay.appendChild(lastReceivedEl);
}

function setLastReceived() {
  if (lastReceivedEl)
    lastReceivedEl.textContent = received.length ? received[received.length - 1] : '';
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
  const target = () => (usable() ? slotXY(idx) : { x: f.x, y: f.y });
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
  const target = () => (usable() ? slotXY(idx) : { x: f.x, y: f.y });
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
      if (!usable() || f.cancelled) {
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
// empty (or while another receive is in flight) only queues a pending receive
// that auto-delivers as soon as a char lands.
function doReceive() {
  if (flights.length > 0) return; // ignore clicks during any animation
  if (ch.length === 0) {
    pendingReceive = true;
    updateButtons();
    return;
  }
  launchReceiveFlight();
}

function launchReceiveFlight() {
  inFlightReceives++;
  pendingReceive = false;
  const start = slotXY(0);
  const f = { ch: ch.first, x: start.x, y: start.y };
  flights.push(f);
  const target = () => (usable() ? { x: dot.x, y: dot.y } : { x: f.x, y: f.y });
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
      setLastReceived();
      tryDeliverPending();
      if (pendingReceive && ch.length > 0) launchReceiveFlight();
      updateButtons();
    },
  });
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
  el.appendChild(row('30px', '#9e9e9e', 'Sender blocked — channel full, cannot send'));
  el.appendChild(
    row('16px', '#000000', 'Receiver — receive() takes the oldest char out of the buffer'),
  );
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
    receiveBtn.style.left = dot.x - DOT_R - bw - 8 + 'px';
    receiveBtn.style.top = dot.y - bh / 2 + 'px';
    if (lastReceivedEl) {
      const lw = lastReceivedEl.offsetWidth || 32;
      lastReceivedEl.style.height = bh + 'px';
      lastReceivedEl.style.left = dot.x - DOT_R - bw - 8 - lw - 6 + 'px';
      lastReceivedEl.style.top = dot.y - bh / 2 + 'px';
    }
  }
}

function updateButtons() {
  createBtn.disabled = phase !== 'idle';
  if (receiveBtn) {
    receiveBtn.disabled = false;
    receiveBtn.classList.toggle('waiting', pendingReceive);
  }
  updateHudPositions();
}

// ---- life cycle ----
function animateDespawn() {
  flights.length = 0;
  inFlightReceives = 0;
  if (!dot) {
    doDespawn();
    return;
  }
  anim.tween(
    DESPAWN_MS,
    (t) => {
      const q = Anim.easeInOut(t);
      dot.scale = Math.max(0, 1 - q);
      if (receiveBtn) receiveBtn.style.opacity = String(1 - q);
      if (lastReceivedEl) lastReceivedEl.style.opacity = String(1 - q);
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
  if (lastReceivedEl) {
    lastReceivedEl.style.opacity = '1';
    lastReceivedEl.remove();
    lastReceivedEl = null;
  }
  if (legendEl) {
    legendEl.remove();
    legendEl = null;
  }
  dot = null;
  senders = [];
  received = [];
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

// buffer strip pinned below the receiver dot
function drawBuffer() {
  // while a receive flight is airborne the source slot's char rides on the
  // badge — never two instances of the same char visible at once
  const shown = inFlightReceives > 0 ? ch.items.slice(1) : ch.items;
  const y = dot.y + BUFF_DY;
  for (let i = 0; i < CAP; i++) {
    const p = slotXY(i);
    const filled = i < shown.length;
    ctx.strokeStyle = filled ? '#333' : '#bbb';
    ctx.lineWidth = filled ? 2.5 : 1.5;
    ctx.fillStyle = 'rgba(255,255,255,0.75)';
    ctx.beginPath();
    ctx.roundRect(p.x - SLOT_W / 2, y - SLOT_H / 2, SLOT_W, SLOT_H, 4);
    ctx.fill();
    ctx.stroke();
    if (filled) {
      ctx.fillStyle = '#000';
      ctx.font = 'bold 14px sans-serif';
      ctx.textAlign = 'center';
      ctx.textBaseline = 'middle';
      ctx.fillText(shown[i], p.x, p.y);
    }
  }
  ctx.fillStyle = '#444';
  ctx.font = '600 12px sans-serif';
  ctx.textAlign = 'center';
  ctx.textBaseline = 'alphabetic';
  ctx.fillText('buffer, capacity ' + CAP, dot.x, y + SLOT_H + 14);
}

function draw() {
  ctx.clearRect(0, 0, W, H); // transparent canvas — the grid/background comes from live.css

  if (dot) {
    // arrows first (behind everything)
    for (const s of senders) drawArrow(s.x, s.y, dot.x, dot.y);
    drawBuffer();
    // receiver dot on top
    const dr = DOT_R * (dot.scale || 1);
    ctx.fillStyle = '#000';
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
    if (obj.type === 'dot') drag = { type: 'dot', offX: mx - dot.x, offY: my - dot.y };
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
  if (drag.type === 'dot') {
    dot.x = mx - drag.offX;
    dot.y = my - drag.offY;
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
