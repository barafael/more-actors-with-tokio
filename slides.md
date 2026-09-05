---
marp: true
theme: rhea
size: 16:9
---

<!-- _class: lead -->

# Collector Actor

## A minigame

---

<!-- header: 'Section' -->

# Normal Slide

Some content.

---

<!-- header: 'Game' -->

<iframe src="mpsc.html" style="width:100%;height:600px;border:none;"></iframe>

Two separate steps: first `Create Channel` (receiver dot + a sender), then `Spawn Actor` (the collector that processes received chars).

The sender has an input field for a single char — typing sends it into the channel immediately. The buffer (between dot and oval) has a capacity of 5; sending to a full channel grays the sender until space frees up.

The receive button is always present beside the dot; it is gray when the channel is empty, and yields one char into the collector once available.

The legend on the left explains the symbols.

---

<!-- header: 'Laws · mpsc' -->

# The mpsc channel

## One consumer, many producers

- **Bounded buffer** of capacity 5: sending on a **full** channel **blocks** the sender until a slot frees.
- A message occupies capacity **from the moment the send succeeds** — flying values already count against the buffer.
- The single consumer is a **task**: `receive()` blocks while the channel is empty and yields one value at a time.
- Delivery is **FIFO** — the receiver always gets the oldest buffered value.
- **Closing the receiver** keeps the buffered values, but no further sends are possible.
- Slow consumers **backpressure the producers**: the channel fills and only the senders pay.

---

<!-- header: 'Oneshot' -->

<iframe src="oneshot.html" style="width:100%;height:600px;border:none;"></iframe>

A oneshot channel contains at most one value and has no separate receiver task: you await it.

Tip the `Tx` field with a single char — the value flies across, the sender turns gray and is removed after a second. The receiver holds the value until it is `.await`ed.

On `.await`, the received value appears in a label right next to the receiver; receiver fades from the playfield one second later, and the channel stays done until you press `Create Channel`.

Drag `Tx`/`Rx` handles to rearrange. Click `Drop` on either side to tear down: dropping the sender without sending yields an `error` on the receiver. You can `.await` first and cancel it anytime.

---

<!-- header: 'Laws · oneshot' -->

# The oneshot channel

## One value, exactly once

- The channel carries **at most one value** — no buffer, no backlog.
- `send()` **never blocks**, but it **consumes the sender**: after one send the `Tx` is gone for good.
- `.await` **blocks until either a value or an error** arrives; if one is already there it completes immediately.
- Dropping the **sender without sending** lets the receiver's `.await` fail with `error` (closed).
- Dropping the **receiver** makes a later `send()` fail with `error`.
- Once both sides are consumed the channel is **done** — it cannot be reused.

---

<!-- header: 'Broadcast' -->

<iframe src="broadcast.html" style="width:100%;height:600px;border:none;"></iframe>

A broadcast channel keeps a bounded ring buffer that every receiver reads independently — receivers pull, the sender never pushes.

The channel starts as just a green `Tx`: typing writes the message into the ring buffer (a flight commits it on landing). Subscribing happens at the sender; a `Sub` button creates a new receiver that starts at the newest message. Sending never reaches a receiver by itself — each receiver's `Receive` button pulls the next unseen buffered message (and blocks, async, when there is none yet). `Re-subscribe` jumps a receiver back to the newest message.

The ring buffer holds up to 5 messages; each receiver shows its read index, which only advances when that receiver hits `Receive`. A slow receiver that misses the oldest buffered message discards it — its index skips forward and a red lag counter increments. The sender never blocks. Dropping the sender closes the channel: receivers stuck waiting on an empty buffer get an error.

---

<!-- header: 'Laws · broadcast' -->

# The broadcast channel

## Bounded ring, pull-only reads

- `send()` **pushes into a bounded ring buffer** of capacity 5 — the sender **never blocks**; when full, the oldest message is overwritten.
- A message is **not delivered automatically**: each receiver pulls its own copy with `recv()` and reads at its own pace.
- A receiver too slow to pull an overwritten message **lags**: its index skips and the value is lost (`Lagged` → red counter).
- A fresh subscriber **starts at the newest message** (`Sub`); `Re-subscribe` jumps back to it — older history is never replayed.
- A `Receive` with nothing new to read **blocks (async)** until the next send commits.
- **Dropping the sender closes** the channel: a receiver blocked on an empty buffer resolves with `error` (`Closed`).

---

<!-- header: 'Watch' -->

<iframe src="watch.html" style="width:100%;height:600px;border:none;"></iframe>

A watch channel keeps only the latest value — creating it takes an initial char (it fills the shared `state` cell right away). `subscribe` on the sender spawns another receiver that instantly sees the current state.

Typing a char updates the shared `state` cell and fans out to every receiver. Sending the same value again changes nothing (tokio's equal-values dedup — a `same` badge appears).

Each receiver can `.await` (`changed()`) to block until the value differs from what it has seen. Dropping the sender stops all further changes — awaiting receivers just stop, since watch has no error type.

---

<!-- header: 'Laws · watch' -->

# The watch channel

## One shared value, no history

- The channel is **created with an initial value** — a watch channel never exists empty.
- It holds only the **latest value**: there is no history and no buffer.
- A new subscriber **instantly sees the current value** and never misses it — no catch-up needed.
- Sending a value **equal** to the current one changes nothing (dedup) — nobody is woken.
- `changed()` blocks only until the value **differs from what this receiver has seen**; if it already changed, it resolves immediately.
- The sender **never blocks**; sending with **no receivers fails** with `error`.
- Dropping the sender only **stops future changes** — there is no error type; a pending `changed()` simply reports "no change".
