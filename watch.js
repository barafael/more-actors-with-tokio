/*
 * Watch — live visualization of a tokio::sync::watch channel.
 * The channel holds ONE shared cell of state (the latest value). Channel
 * creation takes an initial char (tokio watch::channel(init)) which fills the
 * state cell right away. The green Tx updates it by typing a char; sending a
 * value equal to the current one changes nothing (dedup — tokio's PartialEq
 * check).
 * Subscribing happens at the sender: a Sub button next to the input creates a
 * new receiver, which instantly starts at the current value — unlike broadcast,
 * a late subscriber never misses the current state. Every orange Rx holds the
 * latest value and can also .await (changed()) to block until the value differs
 * from what it has seen.
 * The sender never blocks (there is no bounded buffer), but sending with no
 * subscribers fails (Err(SendError)). Dropping the sender only stops future
 * changes — watch has no error type.
 * Reuses the Anim library and lib/live.css primitives.
 */
'use strict';

const c = document.getElementById('c');
const ctx = c.getContext('2d');
const toolbar = document.getElementById('toolbar');
const sideOverlay = document.getElementById('side-overlay');

const TAU = Math.PI * 2;
const TX_R = 38;
const RX_R = 38;

const TRAVEL_MS = 650;
const POP_MS = 450;
const SHRINK_MS = 400;

const MAX_RX = 6;

const TX_FILL = '#2e7d32';
const TX_BORDER = '#1b5e20';
const RX_FILL = '#f08c00';
const RX_BORDER = '#e8590c';
const GRAY = '#9e9e9e';

const STATE_HALFW = 64;
const STATE_HALFH = 40;

const anim = Anim.createAnimator();

let phase = 'idle'; // 'idle' | 'active'
let tx = null; // {x, y, scale} green sender
let state = null; // {x, y, scale, val, version} shared channel cell
let rxs = []; // [{x, y, scale, ch, version, awaiting, ui}]
const flights = []; // airborne badges

let txWrap = null,
  txInput = null,
  txDrop = null,
  txSub = null;

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

// ---- geometry ----
function txPos() {
  return { x: W * 0.13, y: H * 0.5 };
}
function statePos() {
  return { x: W * 0.4, y: H * 0.5 };
}
function rxPos(i) {
  return { x: W * 0.8, y: H * 0.22 + i * 120 };
}

// ---- pop-in / shrink ----
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

function animateShrink(obj, done) {
  anim.tween(
    SHRINK_MS,
    (t) => {
      obj.scale = 1 - Anim.easeInOut(t);
    },
    done,
  );
}

function pulseState() {
  const base = state.scale;
  anim.tween(
    160,
    (t) => {
      const p = t < 0.5 ? 2 * t : 2 * (1 - t);
      state.scale = base + p * 0.14;
    },
    () => {
      state.scale = base;
    },
  );
}

// ---- lifecycle ----
function create() {
  if (phase !== 'idle') return;
  const initVal = createInit.value || null;
  createInit.value = '';
  tx = { ...txPos(), scale: 1 };
  state = { ...statePos(), scale: 1, val: initVal, version: 0 };
  rxs = [{ ...rxPos(0), scale: 1, ch: initVal, version: 0, awaiting: false }];
  phase = 'active';
  createTxUI();
  createRxUI(rxs[0]);
  popIn(tx, POP_MS);
  popIn(state, POP_MS);
  popIn(rxs[0], POP_MS);
  if (initVal !== null) borrowToRx(rxs[0], initVal); // fresh receiver instantly sees it
  updateButtons();
}

function doDespawn() {
  if (txWrap) txWrap.remove();
  for (const r of rxs) if (r.ui) r.ui.wrap.remove();
  txWrap = txInput = txDrop = txSub = null;
  tx = null;
  state = null;
  rxs = [];
  flights.length = 0;
  anim.clear();
  phase = 'idle';
  updateButtons();
}

// ---- html controls ----
const createInit = document.createElement('input');
createInit.className = 'field';
createInit.maxLength = 1;
createInit.placeholder = 'ch';
createInit.title = 'initial value — a watch channel is created with one';
toolbar.appendChild(createInit);

const createBtn = document.createElement('button');
createBtn.className = 'btn';
createBtn.textContent = 'Create Channel';
createBtn.onclick = () => create();
toolbar.appendChild(createBtn);

function createTxUI() {
  const wrap = document.createElement('div');
  wrap.className = 'side-btns';

  const input = document.createElement('input');
  input.className = 'field';
  input.maxLength = 1;
  input.title = 'type a char to update the shared state';
  input.addEventListener('input', () => {
    if (input.disabled) return;
    const val = input.value;
    if (!val) return;
    input.value = '';
    send(val);
  });

  const drop = document.createElement('button');
  drop.className = 'btn small drop';
  drop.textContent = 'Drop';
  drop.title = 'drop the sender (no further changes; watch has no error)';
  drop.onclick = () => dropTx();

  const sub = document.createElement('button');
  sub.className = 'btn small sub';
  sub.textContent = 'Sub';
  sub.title = 'subscribe a new receiver at the sender — it instantly sees the current value';
  sub.onclick = () => subscribe();

  wrap.append(input, drop, sub);
  sideOverlay.appendChild(wrap);
  txWrap = wrap;
  txInput = input;
  txDrop = drop;
  txSub = sub;
}

function createRxUI(r) {
  const wrap = document.createElement('div');
  wrap.className = 'side-btns';

  const aw = document.createElement('button');
  aw.className = 'btn small await';
  aw.textContent = '.await';
  aw.title = 'wait for the value to change (changed())';
  aw.onclick = () => toggleAwait(r);

  const drop = document.createElement('button');
  drop.className = 'btn small drop';
  drop.textContent = 'Drop';
  drop.title = 'drop this receiver';
  drop.onclick = () => dropRx(r);

  wrap.append(aw, drop);
  sideOverlay.appendChild(wrap);
  r.ui = { wrap, aw, drop };
}

// ---- behaviors ----
// A fresh receiver instantly borrows the current value (a visual copy).
function borrowToRx(r, val) {
  const f = { text: String(val), x: state.x + STATE_HALFW, y: state.y, borrow: true };
  flights.push(f);
  Anim.createFlight(anim, {
    fromX: state.x + STATE_HALFW,
    fromY: state.y,
    dur: TRAVEL_MS,
    target: () => (rxs.includes(r) ? { x: r.x, y: r.y } : { x: f.x, y: f.y }),
    onUpdate: (x, y) => {
      f.x = x;
      f.y = y;
    },
    done: () => {
      const i = flights.indexOf(f);
      if (i >= 0) flights.splice(i, 1);
    },
  });
}

function subscribe() {
  if (phase !== 'active' || !tx || rxs.length >= MAX_RX) return;
  const idx = rxs.length;
  const nr = { ...rxPos(idx), scale: 0.02, ch: state.val, version: state.version, awaiting: false };
  rxs.push(nr);
  createRxUI(nr);
  popIn(nr, POP_MS);
  if (state.val !== null) borrowToRx(nr, state.val);
  updateButtons();
}

function send(val) {
  if (phase !== 'active' || !tx || !val) return;
  if (rxs.length === 0) {
    failSend(val); // tokio: SendError — no active receivers
    return;
  }
  if (state.val === val) {
    dupSend(val); // tokio: PartialEq check — no change, nobody wakes
    return;
  }
  state.val = val;
  state.version += 1;
  pulseState();
  updateButtons();
  // fan-out the change to every receiver (awaiting ones resolve)
  for (const r of rxs) {
    const f = { text: val, x: state.x, y: state.y };
    flights.push(f);
    Anim.createFlight(anim, {
      fromX: state.x,
      fromY: state.y,
      dur: TRAVEL_MS,
      target: () => (rxs.includes(r) ? { x: r.x, y: r.y } : { x: f.x, y: f.y }),
      onUpdate: (x, y) => {
        f.x = x;
        f.y = y;
      },
      done: () => {
        const i = flights.indexOf(f);
        if (i < 0) return;
        flights.splice(i, 1);
        if (!rxs.includes(r)) return;
        r.ch = val;
        r.version = state.version;
        if (r.awaiting) r.awaiting = false;
        updateButtons();
      },
    });
  }
}

function dupSend(val) {
  // same value as the current state: nothing enters the cell, nobody is woken
  const f = { text: 'same', x: state.x, y: state.y, same: true };
  flights.push(f);
  Anim.createFlight(anim, {
    fromX: state.x,
    fromY: state.y,
    dur: TRAVEL_MS,
    target: () => ({ x: f.x + 70, y: f.y - 24 }),
    onUpdate: (x, y) => {
      f.x = x;
      f.y = y;
    },
    done: () => {
      const i = flights.indexOf(f);
      if (i >= 0) flights.splice(i, 1);
    },
  });
}

function failSend(val) {
  const origin = { x: tx.x, y: tx.y };
  const f = { text: 'error', x: origin.x, y: origin.y, error: true };
  flights.push(f);
  Anim.createFlight(anim, {
    fromX: origin.x,
    fromY: origin.y,
    dur: TRAVEL_MS,
    target: () => ({ x: f.x - 70, y: f.y - 20 }),
    onUpdate: (x, y) => {
      f.x = x;
      f.y = y;
    },
    done: () => {
      const i = flights.indexOf(f);
      if (i >= 0) flights.splice(i, 1);
    },
  });
}

function toggleAwait(r) {
  if (phase !== 'active' || !rxs.includes(r)) return;
  if (r.awaiting) {
    r.awaiting = false;
    updateButtons();
    return;
  }
  if (!tx) return; // closed: no future changes, nothing to wait for
  if (r.version < state.version) {
    // changed() completes immediately: value changed already
    r.ch = state.val;
    r.version = state.version;
    updateButtons();
    return;
  }
  r.awaiting = true;
  updateButtons();
}

function dropRx(r) {
  if (phase !== 'active' || !rxs.includes(r)) return;
  animateShrink(r, () => {
    rxs = rxs.filter((x) => x !== r);
    if (r.ui) r.ui.wrap.remove();
    updateButtons();
    if (rxs.length === 0 && !tx) doDespawn();
  });
}

function dropTx() {
  if (phase !== 'active' || !tx) return;
  if (txInput) txInput.disabled = true;
  animateShrink(tx, () => {
    tx = null;
    if (txWrap) txWrap.remove();
    txWrap = txInput = txDrop = txSub = null;
    // no more changes: anything awaiting just stops (changed() -> Ok(false))
    for (const r of rxs) r.awaiting = false;
    updateButtons();
    if (rxs.length === 0) doDespawn();
  });
}

// ---- hud ----
function updateHudPositions() {
  const place = (wrap, obj, r) => {
    if (!wrap || !obj) return;
    const sw = wrap.offsetWidth || 160;
    const sh = wrap.offsetHeight || 26;
    wrap.style.left = obj.x - sw / 2 + 'px';
    wrap.style.top = obj.y - r - sh - 8 + 'px';
  };
  place(txWrap, tx, TX_R);
  for (const rx of rxs) if (rx.ui) place(rx.ui.wrap, rx, RX_R);
}

function updateButtons() {
  createBtn.disabled = phase !== 'idle';
  createInit.disabled = phase !== 'idle';
  if (txInput) txInput.disabled = phase !== 'active' || !tx;
  if (txDrop) txDrop.disabled = phase !== 'active' || !tx;
  if (txSub) txSub.disabled = phase !== 'active' || !tx || rxs.length >= MAX_RX;
  for (const rx of rxs) {
    if (!rx.ui) continue;
    rx.ui.aw.textContent = rx.awaiting ? 'Cancel' : '.await';
    rx.ui.aw.disabled = phase !== 'active' || !rx;
    rx.ui.drop.disabled = phase !== 'active';
  }
  updateHudPositions();
}

// ---- drag ----
let drag = null; // {o: tx|state|rx} current drag target
c.addEventListener('mousedown', (e) => {
  if (phase !== 'active') return;
  const mx = e.clientX,
    my = e.clientY;
  if (tx && Math.hypot(mx - tx.x, my - tx.y) < TX_R + 8) {
    drag = { o: tx };
  } else if (state) {
    const dx = (mx - state.x) / STATE_HALFW,
      dy = (my - state.y) / STATE_HALFH;
    if (dx * dx + dy * dy < 1.3) {
      drag = { o: state };
    } else {
      for (const rx of [...rxs].reverse()) {
        if (Math.hypot(mx - rx.x, my - rx.y) < RX_R + 8) {
          drag = { o: rx };
          break;
        }
      }
    }
  }
});
c.addEventListener('mousemove', (e) => {
  if (!drag) return;
  drag.o.x = e.clientX;
  drag.o.y = e.clientY;
  updateHudPositions();
});
c.addEventListener('mouseup', () => {
  drag = null;
});

// ---- draw ----
function drawCircle(obj, r, fill, border, label, textColor) {
  const rr = r * (obj.scale || 1);
  ctx.fillStyle = fill;
  ctx.beginPath();
  ctx.arc(obj.x, obj.y, rr, 0, TAU);
  ctx.fill();
  ctx.strokeStyle = border;
  ctx.lineWidth = 3;
  ctx.stroke();
  ctx.fillStyle = textColor || '#fff';
  ctx.font = 'bold 22px sans-serif';
  ctx.textAlign = 'center';
  ctx.textBaseline = 'middle';
  ctx.fillText(label, obj.x, obj.y + 1);
}

function drawState() {
  if (!state) return;
  const sx = state.x,
    sy = state.y;
  const hw = STATE_HALFW * (state.scale || 1);
  const hh = STATE_HALFH * (state.scale || 1);
  ctx.fillStyle = '#fffef7';
  ctx.beginPath();
  ctx.ellipse(sx, sy, hw, hh, 0, 0, TAU);
  ctx.fill();
  ctx.strokeStyle = '#333';
  ctx.lineWidth = 3;
  ctx.stroke();
  ctx.fillStyle = '#333';
  ctx.font = 'bold 20px sans-serif';
  ctx.textAlign = 'center';
  ctx.textBaseline = 'middle';
  ctx.fillText(state.val !== null ? String(state.val) : '…', sx, sy);
  ctx.font = '600 12px sans-serif';
  ctx.textBaseline = 'alphabetic';
  ctx.fillText('state (watch)', sx, sy + hh + 18);
}

function drawArrows() {
  if (!tx || rxs.length === 0) return;
  ctx.strokeStyle = 'rgba(51,51,51,0.45)';
  ctx.lineWidth = 2;
  for (const rx of rxs) {
    const dx = rx.x - state.x,
      dy = rx.y - state.y;
    const len = Math.hypot(dx, dy);
    if (len < 1) continue;
    const ux = dx / len,
      uy = dy / len;
    const sx = state.x + ux * (STATE_HALFW + 4);
    const sy = state.y + uy * (STATE_HALFH + 4);
    const ex = rx.x - ux * RX_R;
    const ey = rx.y - uy * RX_R;
    ctx.setLineDash([5, 6]);
    ctx.beginPath();
    ctx.moveTo(sx, sy);
    ctx.lineTo(ex, ey);
    ctx.stroke();
    ctx.setLineDash([]);
    const as = 8;
    const a1x = ex - ux * as - uy * as * 0.6;
    const a1y = ey - uy * as + ux * as * 0.6;
    const a2x = ex - ux * as + uy * as * 0.6;
    const a2y = ey - uy * as - ux * as * 0.6;
    ctx.fillStyle = 'rgba(51,51,51,0.45)';
    ctx.beginPath();
    ctx.moveTo(ex, ey);
    ctx.lineTo(a1x, a1y);
    ctx.lineTo(a2x, a2y);
    ctx.closePath();
    ctx.fill();
  }
}

function drawFlights() {
  for (const f of flights) {
    ctx.textAlign = 'center';
    ctx.textBaseline = 'middle';
    if (f.error) {
      const w = 56,
        h = 22;
      ctx.fillStyle = '#fff';
      ctx.strokeStyle = '#b71c1c';
      ctx.lineWidth = 2;
      ctx.beginPath();
      ctx.roundRect(f.x - w / 2, f.y - h / 2, w, h, 11);
      ctx.fill();
      ctx.stroke();
      ctx.fillStyle = '#b71c1c';
      ctx.font = 'bold 13px sans-serif';
      ctx.fillText('error', f.x, f.y + 1);
    } else if (f.same) {
      const w = 50,
        h = 20;
      ctx.fillStyle = '#fff';
      ctx.strokeStyle = GRAY;
      ctx.lineWidth = 2;
      ctx.beginPath();
      ctx.roundRect(f.x - w / 2, f.y - h / 2, w, h, 10);
      ctx.fill();
      ctx.stroke();
      ctx.fillStyle = '#555';
      ctx.font = 'bold 13px sans-serif';
      ctx.fillText('same', f.x, f.y + 1);
    } else {
      ctx.fillStyle = '#fff';
      ctx.strokeStyle = f.borrow ? GRAY : '#333';
      ctx.lineWidth = 2;
      ctx.beginPath();
      ctx.arc(f.x, f.y, 12, 0, TAU);
      ctx.fill();
      ctx.stroke();
      ctx.fillStyle = '#000';
      ctx.font = 'bold 14px sans-serif';
      ctx.fillText(f.text, f.x, f.y);
    }
  }
}

function draw() {
  ctx.clearRect(0, 0, W, H); // transparent canvas — the grid/background comes from live.css

  drawState();
  drawArrows();
  if (tx) drawCircle(tx, TX_R, TX_FILL, TX_BORDER, 'Tx');
  for (const rx of rxs) {
    const label = rx.awaiting ? '…' : rx.ch !== null ? String(rx.ch) : 'Rx';
    drawCircle(
      rx,
      RX_R,
      rx.awaiting ? GRAY : RX_FILL,
      rx.awaiting ? '#666' : RX_BORDER,
      label,
      rx.awaiting ? '#fff' : '#fff',
    );
  }
  drawFlights();
}

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
