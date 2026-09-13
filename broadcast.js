/*
 * Broadcast — live visualization of a tokio::sync::broadcast channel.
 * One green Tx (sender) with an input field, a "Sub" button (subscribing starts
 * a new receiver at the sender) and a "Drop" button. Sending does NOT fan out:
 * the message is committed to the ring buffer (capacity CAP) and only lands in
 * a receiver's circle when that receiver's "Receive" button is pressed — the
 * buffer read index only advances on a Receive. A Receive press with nothing
 * new to read simply blocks (async) until the next send commits.
 * Re-subscribe starts a fresh receiver at the newest message. If a slow
 * receiver falls behind, the ring discards the oldest message: lagged
 * receivers skip forward and a red lag counter increments.
 * During any flight the source character is gone from its origin — no two
 * instances of the same char are visible at once.
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

const CAP = 5;
const MAX_RX = 4;
const RX_DY = 160; // vertical spacing between stacked receivers

const SLOT_W = 20,
  SLOT_H = 22,
  SLOT_GAP = 5;
const BUFF_Y_RATIO = 0.18;

const TX_FILL = '#2e7d32';
const TX_BORDER = '#1b5e20';
const RX_FILL = '#f08c00';
const RX_BORDER = '#e8590c';
const GRAY = '#9e9e9e';

const anim = Anim.createAnimator();

let phase = 'idle'; // 'idle' | 'active'
let tx = null; // {x, y, scale}
let txGone = false; // sender dropped -> channel closed for new sends
let buffer = []; // [{seq, val}] ring-buffer contents, oldest first, length ≤ CAP
let nextSeq = 0;
let rxs = []; // [{x, y, scale, pos, val, lagged, pending, ui}]
const flights = []; // airborne badges
const transit = new Int32Array(CAP); // per-slot in-flight pull counter (source char hidden while >0)

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
  return { x: W * 0.18, y: H * 0.45 };
}
function rxPos(i) {
  // Spread the (up to MAX_RX) stacked receivers evenly down the available band:
  // generous fixed gap when the viewport is tall, shrinking only as needed.
  const top = H * 0.14;
  const avail = H - RX_R - 70 - top;
  const dy = Math.min(RX_DY, avail / (MAX_RX - 1));
  return { x: W * 0.8, y: top + i * dy };
}
function buffY() {
  return H * BUFF_Y_RATIO;
}
function slotTotal() {
  return CAP * SLOT_W + (CAP - 1) * SLOT_GAP;
}
function slotXY(i) {
  const x0 = W / 2 - slotTotal() / 2;
  return { x: x0 + i * (SLOT_W + SLOT_GAP) + SLOT_W / 2, y: buffY() };
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

// ---- lifecycle ----
function create() {
  if (phase !== 'idle') return;
  tx = { ...txPos(), scale: 1 };
  txGone = false;
  buffer = [];
  nextSeq = 0;
  rxs = [];
  phase = 'active';
  createTxUI();
  popIn(tx, POP_MS);
  updateButtons();
}

function doDespawn() {
  if (txWrap) txWrap.remove();
  for (const r of rxs) if (r.ui) r.ui.wrap.remove();
  txWrap = txInput = txDrop = txSub = null;
  tx = null;
  txGone = false;
  buffer = [];
  nextSeq = 0;
  rxs = [];
  flights.length = 0;
  transit.fill(0);
  anim.clear();
  phase = 'idle';
  updateButtons();
}

// ---- html controls ----
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
  input.title = 'type a char to add to the ring buffer (receivers pull it)';
  input.addEventListener('input', () => {
    if (input.disabled) return;
    const val = input.value;
    if (!val) return;
    input.value = '';
    send(val);
  });

  const sub = document.createElement('button');
  sub.className = 'btn small sub';
  sub.textContent = 'Sub';
  sub.title =
    'subscribe a new receiver at the sender (fresh receivers start at the newest message)';
  sub.onclick = () => subscribe();

  const drop = document.createElement('button');
  drop.className = 'btn small drop';
  drop.textContent = 'Drop';
  drop.title = 'drop the sender: receivers stuck on an empty buffer get a closed-channel error';
  drop.onclick = () => dropTx();

  wrap.append(input, sub, drop);
  sideOverlay.appendChild(wrap);
  txWrap = wrap;
  txInput = input;
  txSub = sub;
  txDrop = drop;
}

function createRxUI(r) {
  const wrap = document.createElement('div');
  wrap.className = 'side-btns rx-actions';

  const recv = document.createElement('button');
  recv.className = 'btn small rcv';
  recv.textContent = 'Receive';
  recv.title = 'pull the next unseen buffered message (blocks while there is none)';
  recv.onclick = () => doReceive(r);

  const resub = document.createElement('button');
  resub.className = 'btn small resub';
  resub.textContent = 'Re-subscribe';
  resub.title = 'start over at the newest message (tokio Receiver::resubscribe)';
  resub.onclick = () => resubscribeRx(r);

  const drop = document.createElement('button');
  drop.className = 'btn small drop';
  drop.textContent = 'Drop';
  drop.title = 'drop this receiver';
  drop.onclick = () => dropRx(r);

  wrap.append(recv, resub, drop);
  sideOverlay.appendChild(wrap);
  r.ui = { wrap, recv, resub, drop };
}

// ---- behaviors ----
// Subscribing happens at the sender — it creates a new receiver at the newest message.
function subscribe() {
  if (phase !== 'active' || !tx || rxs.length >= MAX_RX) return;
  const nr = {
    ...rxPos(rxs.length),
    scale: 0.02,
    pos: nextSeq,
    val: null,
    lagged: 0,
    pending: false,
  };
  rxs.push(nr);
  createRxUI(nr);
  popIn(nr, POP_MS);
  updateButtons();
}

// Re-subscribe: brand-new receiver at the current newest message.
function resubscribeRx(r) {
  if (phase !== 'active' || !rxs.includes(r)) return;
  r.pos = nextSeq;
  r.lagged = 0;
  r.val = null;
  r.pending = false;
  updateButtons();
}

// A send only writes into the ring buffer; receivers never see it until they Receive.
function send(val) {
  if (phase !== 'active' || !tx || txGone || !val) return;
  const seq = nextSeq;
  nextSeq += 1;
  const targetIdx = Math.min(buffer.length, CAP - 1);
  const f = { text: val, x: tx.x, y: tx.y };
  flights.push(f);
  Anim.createFlight(anim, {
    fromX: tx.x,
    fromY: tx.y,
    dur: TRAVEL_MS,
    target: () => slotXY(targetIdx),
    onUpdate: (x, y) => {
      f.x = x;
      f.y = y;
    },
    done: () => {
      const i = flights.indexOf(f);
      if (i < 0) return;
      flights.splice(i, 1);
      if (phase !== 'active') return;
      buffer.push({ seq, val });
      let discarded = null;
      if (buffer.length > CAP) discarded = buffer.shift();
      // lag handling: any receiver still needing the discarded message skips it
      if (discarded) {
        for (const r of rxs) {
          if (r.pos <= discarded.seq) r.lagged += 1;
        }
      }
      // wake receivers blocked waiting for data
      for (const r of [...rxs]) {
        if (r.pending && buffer.findIndex((m) => m.seq >= r.pos) !== -1) {
          r.pending = false;
          doReceive(r);
        }
      }
      updateButtons();
    },
  });
}

function doReceive(r) {
  if (phase !== 'active' || !rxs.includes(r) || r.pending) return;
  const k = buffer.findIndex((m) => m.seq >= r.pos);
  if (k === -1) {
    if (txGone && buffer.length === 0) {
      // channel closed and nothing left to read: recv() reports the closure
      r.val = 'error';
      updateButtons();
      return;
    }
    r.pending = true; // async block: resolves when a new message commits
    updateButtons();
    return;
  }
  launchReceiveFlight(r, buffer[k], k);
}

function launchReceiveFlight(r, msg, k) {
  r.pending = false;
  const src = slotXY(k);
  const f = { text: msg.val, x: src.x, y: src.y };
  flights.push(f);
  transit[k] += 1; // hide the source char until the pull lands
  Anim.createFlight(anim, {
    fromX: src.x,
    fromY: src.y,
    dur: TRAVEL_MS,
    target: () => (rxs.includes(r) ? { x: r.x, y: r.y } : { x: f.x, y: f.y }),
    onUpdate: (x, y) => {
      f.x = x;
      f.y = y;
    },
    done: () => {
      transit[k] = Math.max(0, transit[k] - 1);
      const i = flights.indexOf(f);
      if (i < 0) {
        updateButtons();
        return;
      }
      flights.splice(i, 1);
      if (!rxs.includes(r)) {
        updateButtons();
        return;
      }
      const skipped = Math.max(0, msg.seq - r.pos);
      r.pos = msg.seq + 1;
      r.lagged = Math.max(0, r.lagged - skipped);
      r.val = msg.val;
      updateButtons();
    },
  });
}

function dropRx(r) {
  if (phase !== 'active' || !rxs.includes(r)) return;
  r.pending = false;
  animateShrink(r, () => {
    rxs = rxs.filter((x) => x !== r);
    if (r.ui) r.ui.wrap.remove();
    updateButtons();
    if (rxs.length === 0 && !tx) doDespawn();
  });
}

function dropTx() {
  if (phase !== 'active' || !tx) return;
  txGone = true;
  if (txInput) txInput.disabled = true;
  animateShrink(tx, () => {
    tx = null;
    if (txWrap) txWrap.remove();
    txWrap = txInput = txDrop = txSub = null;
    // receivers blocked on an empty channel resolve with a closed-channel error
    for (const r of rxs) {
      if (r.pending) {
        r.pending = false;
        r.val = 'error';
      }
    }
    updateButtons();
    if (rxs.length === 0) doDespawn();
  });
}

// ---- hud ----
function updateHudPositions() {
  const place = (wrap, obj, r) => {
    if (!wrap || !obj) return;
    const sw = wrap.offsetWidth || 200;
    const sh = wrap.offsetHeight || 26;
    wrap.style.left = obj.x - sw / 2 + 'px';
    wrap.style.top = obj.y - r - sh - 8 + 'px';
  };
  place(txWrap, tx, TX_R);
  for (const rx of rxs) if (rx.ui) place(rx.ui.wrap, rx, RX_R);
}

function updateButtons() {
  createBtn.disabled = phase !== 'idle';
  if (txInput) txInput.disabled = phase !== 'active' || !tx;
  if (txDrop) txDrop.disabled = phase !== 'active' || !tx;
  if (txSub) txSub.disabled = phase !== 'active' || !tx || rxs.length >= MAX_RX;
  for (const rx of rxs) {
    if (!rx.ui) continue;
    rx.ui.recv.disabled = phase !== 'active';
    rx.ui.resub.disabled = phase !== 'active';
    rx.ui.drop.disabled = phase !== 'active';
    rx.ui.recv.classList.toggle('waiting', rx.pending);
  }
  updateHudPositions();
}

// ---- drag ----
let drag = null; // {o: tx|rx} current drag target
c.addEventListener('mousedown', (e) => {
  if (phase !== 'active') return;
  const mx = e.clientX,
    my = e.clientY;
  if (tx && Math.hypot(mx - tx.x, my - tx.y) < TX_R + 8) {
    drag = { o: tx };
  } else {
    for (const rx of [...rxs].reverse()) {
      if (Math.hypot(mx - rx.x, my - rx.y) < RX_R + 8) {
        drag = { o: rx };
        break;
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

function drawBuffer() {
  if (phase !== 'active') return;
  const y = buffY();
  for (let i = 0; i < CAP; i++) {
    const p = slotXY(i);
    const filled = i < buffer.length && transit[i] === 0;
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
      ctx.fillText(buffer[i].val, p.x, p.y);
    }
  }
  ctx.fillStyle = '#444';
  ctx.font = '600 12px sans-serif';
  ctx.textAlign = 'center';
  ctx.textBaseline = 'alphabetic';
  ctx.fillText('ring buffer, capacity ' + CAP, W / 2, y + SLOT_H + 16);
}

function drawReceiverBadges() {
  if (phase !== 'active') return;
  for (const rx of rxs) {
    const y = rx.y + RX_R + 10;
    ctx.textAlign = 'center';
    ctx.textBaseline = 'middle';
    // read-index badge (advances only when this receiver presses Receive)
    const idxTxt = '#' + rx.pos;
    ctx.font = '600 12px sans-serif';
    const idxW = ctx.measureText(idxTxt).width + 10;
    const idxX = rx.x - (rx.lagged > 0 ? (ctx.measureText('+' + rx.lagged).width + 10) / 2 + 2 : 0);
    ctx.fillStyle = 'rgba(255,255,255,0.85)';
    ctx.strokeStyle = '#333';
    ctx.lineWidth = 1.5;
    ctx.beginPath();
    ctx.roundRect(idxX - idxW / 2, y - 8, idxW, 16, 5);
    ctx.fill();
    ctx.stroke();
    ctx.fillStyle = '#222';
    ctx.fillText(idxTxt, idxX, y);
    if (rx.lagged > 0) {
      const lagTxt = '+' + rx.lagged;
      const lagW = ctx.measureText(lagTxt).width + 10;
      const lagX = idxX + idxW / 2 + lagW / 2 + 3;
      ctx.fillStyle = 'rgba(255,255,255,0.85)';
      ctx.strokeStyle = '#b71c1c';
      ctx.lineWidth = 1.5;
      ctx.beginPath();
      ctx.roundRect(lagX - lagW / 2, y - 8, lagW, 16, 5);
      ctx.fill();
      ctx.stroke();
      ctx.fillStyle = '#b71c1c';
      ctx.fillText(lagTxt, lagX, y);
    }
  }
}

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
    ctx.fillText(f.text, f.x, f.y);
  }
}

function draw() {
  ctx.clearRect(0, 0, W, H); // transparent canvas — the grid/background comes from live.css

  drawBuffer();
  if (tx) drawCircle(tx, TX_R, TX_FILL, TX_BORDER, 'Tx');
  for (const rx of rxs) {
    const err = rx.val === 'error';
    const label = err ? 'err' : rx.val !== null ? String(rx.val) : 'Rx';
    const border = err ? '#b71c1c' : RX_BORDER;
    drawCircle(rx, RX_R, RX_FILL, border, label, err ? '#b71c1c' : '#fff');
  }
  drawReceiverBadges();
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
