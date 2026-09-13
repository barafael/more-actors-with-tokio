/*
 * OneShot — live visualization of a tokio::sync::oneshot channel.
 * Green Tx (sender) has a text field: typing sends the single value (once).
 * After sending, the sender is consumed: it turns gray, and one second later
 * it is removed (animated). Orange Rx (receiver) stays up until it is awaited
 * (.await) and has yielded either a value or an error; the received value (or
 * 'error') is written to a label just right of the receiver, which then turns
 * gray and disappears a second later.
 * Both circles can be dragged around. After both are consumed the channel is
 * done — the label stays put until 'Create Channel' is clicked again.
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
const CONSUME_MS = 1000; // gray hold before animated removal

const TRAVEL_MS = 650;
const POP_MS = 450;
const SHRINK_MS = 400;

const TX_FILL = '#2e7d32';
const TX_BORDER = '#1b5e20';
const RX_FILL = '#f08c00';
const RX_BORDER = '#e8590c';
const GRAY = '#9e9e9e';

const anim = Anim.createAnimator();

let phase = 'idle'; // 'idle' | 'active' | 'done'
let tx = null; // {x, y, scale, spent} green sender circle
let rx = null; // {x, y, scale, consumed} orange receiver circle
let value = null; // yielded display value: a char string or 'error'
let awaiting = false; // rx is awaiting (rx held, sender may still send)
let lastTx = null; // last known tx position (error-flight origin)
const flights = []; // airborne badges

let txWrap = null,
  txInput = null,
  txDrop = null;
let rxWrap = null,
  rxDrop = null,
  awaitBtn = null;
let rxLabel = null; // div stating the received value, right of the rx

function usable() {
  return phase === 'active' && rx;
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
      if (obj.grayOut && obj.scale > 0.01) drawOnceClear(obj);
    },
    done,
  );
}

function drawOnceClear(obj) {
  /* no-op hook so shrinking objects clear behind them */ void obj;
}

// ---- consumption: gray now, animated removal one second later ----
function consumeNow(obj) {
  obj.consumed = true;
  updateButtons();
  if (obj === rx && rxLabel) rxLabel.style.opacity = '1';
  setTimeout(() => {
    if (obj === tx && tx && tx.consumed) {
      animateShrink(tx, () => {
        tx = null;
        if (txWrap) txWrap.remove();
        txWrap = txInput = txDrop = null;
        updateHudPositions();
        maybeDone();
      });
    } else if (obj === rx && rx && rx.consumed) {
      animateShrink(rx, () => {
        rx = null;
        if (rxWrap) rxWrap.remove();
        rxWrap = rxDrop = awaitBtn = null;
        updateHudPositions();
        maybeDone();
      });
    }
    updateButtons();
  }, CONSUME_MS);
}

function maybeDone() {
  if (phase !== 'active') return;
  if (!tx && !rx) {
    phase = 'done';
    updateButtons();
  }
}

// ---- lifecycle ----
function create() {
  if (phase === 'active') return;
  tx = { x: W * 0.3, y: H * 0.48, scale: 1, spent: false, consumed: false };
  rx = { x: W * 0.7, y: H * 0.48, scale: 1, consumed: false };
  value = null;
  awaiting = false;
  lastTx = null;
  if (rxLabel) {
    rxLabel.textContent = '';
    rxLabel.style.opacity = '0';
  }
  phase = 'active';
  createTxUI();
  createRxUI();
  popIn(tx, POP_MS);
  popIn(rx, POP_MS);
  updateButtons();
}

function doDespawn() {
  if (txWrap) txWrap.remove();
  if (rxWrap) rxWrap.remove();
  txWrap = txInput = txDrop = null;
  rxWrap = rxDrop = awaitBtn = null;
  tx = null;
  rx = null;
  value = null;
  awaiting = false;
  lastTx = null;
  flights.length = 0;
  anim.clear();
  phase = 'idle';
  updateButtons();
}

// Receiver has yielded (awaited and produced a value or an error): write the
// label, gray the receiver, and remove it one second later.
function yieldRx() {
  awaiting = false;
  if (rxLabel) rxLabel.textContent = value === 'error' ? 'error' : String(value);
  updateButtons();
  consumeNow(rx);
}

// ---- html controls ----
const createBtn = document.createElement('button');
createBtn.className = 'btn';
createBtn.textContent = 'Create Channel';
createBtn.onclick = () => create();
toolbar.appendChild(createBtn);

// received-value label: next-right of the receiver
rxLabel = document.createElement('div');
rxLabel.className = 'rx-label';
rxLabel.style.opacity = '0';
document.body.appendChild(rxLabel);

function createTxUI() {
  const wrap = document.createElement('div');
  wrap.className = 'side-btns';

  const input = document.createElement('input');
  input.className = 'field';
  input.maxLength = 1;
  input.title = 'type a char to send (once)';
  input.addEventListener('input', () => {
    if (input.disabled) return;
    const val = input.value;
    if (!val) return;
    input.value = '';
    sendValue(val);
  });

  const drop = document.createElement('button');
  drop.className = 'btn small drop';
  drop.textContent = 'Drop';
  drop.title = 'drop the sender without sending (rx.await then fails)';
  drop.onclick = () => dropTx();

  wrap.append(input, drop);
  sideOverlay.appendChild(wrap);
  txWrap = wrap;
  txInput = input;
  txDrop = drop;
}

function createRxUI() {
  const wrap = document.createElement('div');
  wrap.className = 'side-btns';

  const drop = document.createElement('button');
  drop.className = 'btn small drop';
  drop.textContent = 'Drop';
  drop.title = 'drop the receiver (disabled while awaiting)';
  drop.onclick = () => dropRx();

  const aw = document.createElement('button');
  aw.className = 'btn small await';
  aw.textContent = '.await';
  aw.title = 'await a value; resolves when a value or error arrives';
  aw.onclick = () => toggleAwait();

  wrap.append(drop, aw);
  sideOverlay.appendChild(wrap);
  rxWrap = wrap;
  rxDrop = drop;
  awaitBtn = aw;
}

// ---- behaviors ----
function toggleAwait() {
  if (!usable()) return;
  if (awaiting) {
    awaiting = false;
    updateButtons();
    return;
  } // cancel
  if (value !== null) {
    yieldRx();
    return;
  } // already yielded something
  awaiting = true;
  updateButtons();
  // if the tx is gone, an error is already (or about to be) on its way
  if (!tx && value === null) errorToRx(lastTx || { x: rx.x - 160, y: rx.y });
}

function sendValue(val) {
  if (phase !== 'active' || !tx || tx.spent) return;
  tx.spent = true; // sending consumes the sender
  if (txInput) txInput.disabled = true;
  consumeNow(tx); // gray, then animated removal after a second
  if (!rx) {
    // receiver already dropped -> send fails, red error drifts off the tx
    const origin = { x: tx.x, y: tx.y };
    const f = { text: 'error', x: origin.x, y: origin.y };
    flights.push(f);
    Anim.createFlight(anim, {
      fromX: f.x,
      fromY: f.y,
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
    updateButtons();
    return;
  }
  const f = { text: val, x: tx.x, y: tx.y };
  flights.push(f);
  const target = () => (rx && !rx.consumed ? { x: rx.x, y: rx.y } : { x: f.x, y: f.y });
  Anim.createFlight(anim, {
    fromX: tx.x,
    fromY: tx.y,
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
      if (!rx || rx.consumed) return;
      value = val;
      if (awaiting) yieldRx();
      updateButtons();
    },
  });
}

function errorToRx(origin) {
  if (!usable()) return;
  const f = { text: 'error', x: origin.x, y: origin.y, error: true };
  flights.push(f);
  const target = () => (rx && !rx.consumed ? { x: rx.x, y: rx.y } : { x: f.x, y: f.y });
  Anim.createFlight(anim, {
    fromX: origin.x,
    fromY: origin.y,
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
      if (!rx || rx.consumed) return;
      value = 'error';
      if (awaiting) yieldRx();
      updateButtons();
    },
  });
}

function dropTx() {
  if (phase !== 'active' || !tx) return;
  if (txInput) txInput.disabled = true;
  const wasSpent = tx.spent;
  const origin = { x: tx.x, y: tx.y };
  consumeNow(tx);
  lastTx = origin;
  if (!wasSpent && rx)
    errorToRx(origin); // sender dropped -> receiver yields an error
  else if (!rx) maybeDone();
  updateButtons();
}

function dropRx() {
  if (phase !== 'active' || !rx || awaiting) return;
  animateShrink(rx, () => {
    rx = null;
    if (rxWrap) rxWrap.remove();
    rxWrap = rxDrop = awaitBtn = null;
    updateHudPositions();
    updateButtons();
    if (!tx) maybeDone();
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
  place(rxWrap, rx, RX_R);
  if (rxLabel) {
    const obj = rx || { x: rxLabel._x, y: rxLabel._y };
    if (rx) {
      rxLabel._x = rx.x;
      rxLabel._y = rx.y;
    }
    const sw = rxLabel.offsetWidth || 40;
    const sh = rxLabel.offsetHeight || 28;
    rxLabel.style.left = obj.x + RX_R + 12 + 'px';
    rxLabel.style.top = obj.y - sh / 2 + 'px';
  }
}

function updateButtons() {
  createBtn.disabled = phase !== 'idle' && phase !== 'done';
  if (txInput) txInput.disabled = phase !== 'active' || !tx || tx.spent;
  if (txDrop) txDrop.disabled = phase !== 'active' || !tx;
  if (awaitBtn) {
    awaitBtn.disabled = phase !== 'active' || !rx || rx.consumed;
    awaitBtn.textContent = awaiting ? 'Cancel' : '.await';
  }
  if (rxDrop) rxDrop.disabled = phase !== 'active' || !rx || awaiting;
  updateHudPositions();
}

// ---- drag ----
let drag = null;
c.addEventListener('mousedown', (e) => {
  const mx = e.clientX,
    my = e.clientY;
  if (tx && !tx.consumed && Math.hypot(mx - tx.x, my - tx.y) < TX_R + 8) drag = { o: tx };
  else if (rx && !rx.consumed && Math.hypot(mx - rx.x, my - rx.y) < RX_R + 8) drag = { o: rx };
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

function drawArrow() {
  const dx = rx.x - tx.x,
    dy = rx.y - tx.y;
  const len = Math.hypot(dx, dy);
  if (len < 1) return;
  const ux = dx / len,
    uy = dy / len;
  const sx = tx.x + ux * TX_R;
  const sy = tx.y + uy * TX_R;
  const ex = rx.x - ux * RX_R;
  const ey = rx.y - uy * RX_R;
  ctx.strokeStyle = awaiting ? '#bbb' : '#333';
  ctx.lineWidth = 2;
  ctx.setLineDash(awaiting ? [6, 6] : []);
  ctx.beginPath();
  ctx.moveTo(sx, sy);
  ctx.lineTo(ex, ey);
  ctx.stroke();
  ctx.setLineDash([]);
  const as = 10;
  const a1x = ex - ux * as - uy * as * 0.6;
  const a1y = ey - uy * as + ux * as * 0.6;
  const a2x = ex - ux * as + uy * as * 0.6;
  const a2y = ey - uy * as - ux * as * 0.6;
  ctx.fillStyle = awaiting ? '#bbb' : '#333';
  ctx.beginPath();
  ctx.moveTo(ex, ey);
  ctx.lineTo(a1x, a1y);
  ctx.lineTo(a2x, a2y);
  ctx.closePath();
  ctx.fill();
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
    } else {
      ctx.fillStyle = '#fff';
      ctx.strokeStyle = '#333';
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

  if (tx && rx) drawArrow();
  if (tx) drawCircle(tx, TX_R, tx.spent ? GRAY : TX_FILL, TX_BORDER, 'Tx');
  if (rx) {
    const err = value === 'error';
    const label = awaiting ? '…' : rx.consumed ? '' : 'Rx';
    const border = err ? '#b71c1c' : awaiting ? '#555' : rx.consumed ? GRAY : RX_BORDER;
    drawCircle(
      rx,
      RX_R,
      rx.consumed ? GRAY : awaiting ? GRAY : RX_FILL,
      border,
      label,
      err ? '#b71c1c' : '#fff',
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
