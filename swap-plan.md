Swap Plan: app sims → sim-channels cores
Locked decisions
- Broadcast subscribe: tokio is the benchmark → subscribe() starts at the tail (new receiver only sees future values). No subscribe_at op; the broadcast game's "click through history" beat is removed and its tests/UI text adjusted.
- mpsc flights: woken sends get an InFlight event too (value lands in the buffer and flies simultaneously — accepted duplication).
Strategy stays: driver adapters. Each Sim keeps its exact public surface (handle(&wire, conn) -> Vec<Event>, snapshot(), sync_now) so src/games/*, src/server/*, src/protocol.rs compile unchanged. Channel truth lives in the core; connection/owner registries, cosmetics, and protocol mapping stay driver-side.
Part A — Crate additions (crates/sim-channels)
A1. # Invariants doc sections (docs only, no behavior) in each module:
- oneshot: one value moves exactly once; send consumes the sender and never blocks (succeeds iff live receiver + no value sent); receiver drop before taking → every send fails with the value back, is_abandoned; sender drop → RecvError::Closed, sent-but-unreceived value stays receivable; try_recv → Empty only while sender alive and value absent.
- mpsc: occupancy len() <= capacity always (parked sends hold values outside the buffer); FIFO everywhere — delivery order == accept order, parked sends wake in park order, a freed slot goes to the queue head before any new send; each accepted value received exactly once; close drains buffer then Disconnected, parked/new sends fail with value back, last-sender-drop never discards buffered values; try_send never queues.
- broadcast: dense seqs from 0, +1 per successful send, failed sends consume none; ring keeps newest capacity, oldest first; per-receiver no gaps, cursor advances via Value/Lagged-resync only; Lagged(skipped) exactly once then resume at oldest; Empty while senders alive / Closed when gone (buffered values stay receivable after senders go); clone inherits source cursor, subscribe() starts at tail.
- watch: one cell/version, version 0 + +1 per accepted change (not on send_if_modified/try_modify → unchanged); only-latest observable (receiver catch-up jumps); changed() Immediate iff seen < current else parks until change or sender-out; borrow guards park sends in FIFO queue, commits in order when last guard drops, Failed/value-back if receivers vanish; send rejected at 0 receivers (no bump), send_replace/send_modify/mark_changed never fail; clone inherits seen-version, never awaiting.
A2. Read-only introspection + unit tests (drivers need these to build snapshots):
- MpscCore::buffer() -> impl Iterator<Item=&T> (accept order)
- MpscCore::blocked() -> impl Iterator<Item=(WaiterId, &T)> (queue/wake order)
- MpscCore::awaiting_recv() -> Option<WaiterId> (for waiting_receive)
- BroadcastCore::ring() -> impl Iterator<Item=(u64, &T)> (oldest→newest, seq+value)
- BroadcastCore::receiver_cursor(rx) -> Option<u64> + areceiver waiter accessor (poll result .Empty(waiter) stored driver-side)
- WatchCore::receiver_ids() -> impl Iterator<Item=u64> and pending_values() -> impl Iterator<Item=&T>
- Each pinned by a test; all existing 64+4 tests/clippy/wasm check stay green.
Part B — App drivers (only src/sim.rs internals rewritten)
B1. ButtonSim → oneshot::OneshotCore<Color>
ButtonSim { pending: Option<OneshotCore<Color>>, state }. Activate = drop old core, new(). Press = core.send(color) → Ok → Resolved{color}, state Ready. state stays as a derived field for ButtonState consumers. Events unchanged.
B2. MpscSim → MpscCore<BufferChar> + registry
T = BufferChar so conn travels with the value. Driver: senders: Vec<SenderInfo>, parked: HashMap<WaiterId,conn>, waiting: Option<WaiterId>, flights (unchanged clock), last_received. Drop rng/with_seed.
- Send: registry auth → offer_send: Accepted → InFlight + flight; Blocked → mark blocked, parked.insert, no event; Rejected unreachable.
- Receive: poll_recv → Value → last_received + settle; Empty → waiting; Disconnected unreachable.
- settle() (after every mutation, FIFO): if waiting, poll_recv_wait → Value → last_received; then for each parked waiter in queue order, poll_send → Accepted → clear blocked + emit InFlight + schedule flight (fly on wake too) → Consumed at landing; Cancelled/Failed → cleanup.
- poll_due: unchanged. Sender add/remove/drop_connection: registry + cancel_send for removed conn's parked values.
- Snapshot: core.buffer(), core.blocked() (order now IS wake order — update the protocol doc line "order is not wake order" to "FIFO"), awaiting_recv, flights.len, last_received.
B3. WatchSim → WatchCore<Option<char>> (lazy) + registry
T = Option<char> matches the wire (Create{init: Option<char>}). created = core.is_some(). Immediately drop_receiver(1) after new() (game starts with zero receivers). Driver: presenter, owners: HashMap<rx,conn>, borrowing: HashSet<rx>.
- Send (presenter-only): (1) if pending_values().next().is_some() → ignore (preserves "one pending" beat); (2) if *core.value() == Some(ch) → SameValue; (3) if borrowing non-empty → core.send(Some(ch)) → Blocked → SendBlocked, pending=Some(ch); (4) else send_if_modified → Modified → Changed{version,ch}, Unchanged → SameValue, Refused → SendRefused.
- LookInside: add to borrowing → begin_borrow → BorrowFlight; remove → end_borrow → Released(no event)/Resolved→Changed/Refused→SendRefused.
- AwaitChange: is_awaiting(rx) → cancel_changed else poll_changed → Immediate (catch-up, no event) / Blocked / Closed. NewReceiver: subscribe() → ReceiverAdded. CloneReceiver: auth then clone_receiver. DropReceiver: if borrowing, end_borrow first, emit [ReceiverRemoved, Changed] (order preserved from tests), then drop_receiver + cleanup.
- Snapshot: value(), version(), pending_values(), receiver_ids() + per-rx receiver_version/is_awaiting + owner/borrowing maps.
B4. BroadcastSim → BroadcastCore<BufferChar> + registry
Driver: senders (host seed conn 0), presenter, owners, rx_state: HashMap<rx, (lagged_total, last, error, waiting-waiter)>, next_sender for clone ids.
- Send: auth → capture ring-front seq → send → Ok(count) → InFlight; if oldest seq advanced → Evicted{oldest_was}; settle: for each waiting rx poll_recv_wait → Value → Received (+ last/error update) / Lagged → event + lagged_total / Closed → error=Closed; empty → keep waiting.
- Receive: poll_recv → Value/Lagged/Closed/Empty(waiter, mark waiting) → events + rx_state.
- Subscribe tail-start: core.subscribe(), one-receiver-per-conn driver check → ReceiverAdded. next in snapshot = core cursor + 1 (core is 0-based, sim was 1-based); tail = core.next_seq() (same count), buffer = ring().map(|(_,v)| v).
- Unsubscribe/ClaimPresenter/drop_connection: registry + drop_receiver/drop_sender; Closed detected naturally when sender count hits 0.
B5. Game/protocol touch-ups
- Broadcaster game: src/games/broadcast/state.rs (+ labels in mod.rs) — replace "new receiver starts at the oldest buffered value / click through history" with "new receivers only see values sent after they subscribe".
- src/protocol.rs: mpsc Snapshot.blocked_sends doc — "order is not wake order" becomes "FIFO wake order"; broadcast RxState.next doc note becomes 0-based↔tail mapping.
- mpsc games/mpsc/*: no code change (client already animates InFlight landings; woken sends now get one too).
B6. Tests + verification
- sim.rs test ports: mpsc unblock_wake_order_is_deterministic_per_seed → unblock_wake_order_is_fifo ('x' wakes before 'y'); broadcast subscribe_mid_stream_starts_at_oldest → subscribe_mid_stream_starts_at_tail; recv_yields_lagged… reworked so lag arises from tail-subscribe-then-fall-behind; watch send_while_pending_is_ignored keeps pre-check; everything else unchanged (public surface is stable).
- Verify: cargo test --workspace (all sim+crate tests), cargo clippy --workspace --all-targets, cargo check -p sim-channels --target wasm32-unknown-unknown, then dx serve local-mode smoke test of button/mpsc/watch/broadcast.
Order & hazard note
Execute A1 → A2 → B1→B4 → B5 → B6. src/sim.rs is being actively modified by the other agent (broadcast landed recently) — implementation should run in a quiet window or after their work lands to avoid conflicting edits; that's why we planned first.
