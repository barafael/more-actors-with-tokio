Port Plan: Dioxus games → standalone-stage look, then marp removal

The standalone canvas games (mpsc.js, oneshot.js, watch.js, broadcast.js +
lib/anim.js, lib/channel.js, lib/live.css) are the reference for look and
feel. The Dioxus games keep everything below the presentation layer — sims,
cores, wires, snapshots, actors, sockets — and replace only the stage. Slide
text is not carried over; the deck keeps its own copy.

Locked decisions
- The stage is canvas + DOM overlay, exactly the standalone's split: one
  transparent <canvas> draws nodes, arrows, buffer slots and flight glyphs;
  rsx overlay positions inputs/buttons at node coordinates. No full-DOM
  rebuild of the diagram.
- Node positions are per-viewer cosmetics. They are never sent over the
  wire and never enter snapshots. Flights stay keyed off sim events.
- channel.js is not ported: sim-channels cores (MpscCore & co.) already are
  that state machine, with one deliberate divergence kept — blocked sends
  wake FIFO, not random (swap-plan locked this).
- Remote and local modes share the stage unchanged; use_game_connection,
  ChannelState.apply, badges, restart supervision all stay.
- SSR/export first paint must stay byte-deterministic (CONCEPT.md): the
  server renders the static default-layout diagram as DOM; the canvas takes
  over on mount.

What the stage gains from the standalone (the "nicer" delta)
1. Full-bleed stage: grid background from live.css tokens, transparent
   canvas, one visual language (green Tx #2e7d32/#1b5e20, orange Rx
   #f08c00/#e8590c, black receiver dot, gray blocked #9e9e9e, white
   flight badges, rounded slot strips).
2. Free layout: every node draggable; arrows, buffer strip and HUD all
   anchored to node positions and redrawn per frame.
3. Motion: rAF tween engine — easeInOut flights that re-target every frame
   (they track dragged nodes), easeOutBack pop-in, shrink-out, despawn,
   watch-cell pulse.
4. Lifecycle: idle → Create Channel → active → despawn when the last sender
   drops; legend panel ("what means what").
5. Per-game beats the standalone does better:
   - mpsc: Receive always pressable, visibly parks when the buffer is empty.
   - watch: version pulse on change, gray dedup flight on SameValue, error
     flight to the Tx on SendRefused, Sub button at the sender.
   - broadcast: seq-numbered ring, per-slot transit hiding (a char whose
     pull flight is airborne is not drawn twice), lagged badges.
   - button: await toggle, error flight (standalone oneshot.js).

Part A — Stage infrastructure (new src/stage/)
A1. src/stage/anim.rs — port of lib/anim.js: lerp, easeInOut, easeOutBack,
    TweenAnimator (tween/update(dt)/clear), anchored create_flight with
    per-frame target() re-evaluation. dt clamped (≤50 ms) so throttled
    tabs never jump. Unit tests on easing/animator, no DOM deps.
A2. src/stage/mod.rs — Stage component: <canvas> sized to the diagram box
    (dpr-aware, like the standalone resize()), overlay div, and a
    use_stage_loop hook: gloo-render::request_animation_frame on wasm,
    compiled out server-side. Owns a Layout signal (node id → px position,
    scale); exposes drag handling via pointer events (pointerdown/move/up —
    the standalone is mouse-only; our audience is on phones, so touch is a
    requirement, not a nicety).
A3. assets/main.css — add a `.stage` section: grid background, palette
    constants, legend panel styles, `.recv-label`, ported from live.css.
    The old per-game percentage-layout CSS is deleted per game in Part B.
A4. SSR path: Stage renders the static DOM diagram (default positions)
    during server rendering; the canvas mount swaps it in after hydration.

Part B — Game ports (order matters: pilot first, timer/select last)
B1. mpsc (pilot). Canvas: receiver dot with sender-count, buffer strip,
    Tx circles, arrows, flight glyphs. Overlay: char input + Clone/Drop per
    sender, Receive + last-received label, legend. Type→send launches a
    flight; full buffer marks the sender blocked (snapshot.blocked_sends);
    Receive parks on empty (snapshot.waiting_receive) and auto-completes —
    the sim already queues receives. Restart = despawn/recreate (local).
    MpscSim/sim-channels untouched.
B2. watch. Canvas: green Tx, dark state cell with version pulse (Changed
    event), orange Rx cards as canvas nodes. Gray dedup flight on SameValue,
    error flight on SendRefused. Sub button at the Tx → NewReceiver.
    AwaitChange/LookInside stay per-Rx toggles (dioxus-only beats).
B3. broadcast. Canvas: seq-numbered ring (snapshot.tail), host+clone
    senders (SenderJoined/Left), stacked receivers with lagged badges
    (Lagged event, lagged_total). Transit hiding keyed off Received events:
    hide a source char while its pull flight is airborne.
B4. button. Port oneshot.js visuals onto ButtonSim events (Activated/
    Resolved): await toggle, spent/respawn lifecycle, error flight.
B5. timer/select/loop-select. Re-skin onto Stage primitives for a coherent
    deck; keep the sim_timer machinery and the recently fixed frame/hydration
    beats exactly as they are. These land last on purpose.
B6. Per game: delete its bespoke absolute-% CSS and layout modules once the
    stage version ships (games/*/layout.rs shrink to constants the stage
    uses, or disappear).

Part C — Marp removal (gated on B1–B4 parity, one commit)
C1. Delete: slides.md, slides.html, slides.standalone.html, bundle.mjs,
    package.json, package-lock.json, .prettierrc.json, mpsc.html,
    oneshot.html, broadcast.html, watch.html, mpsc.js, oneshot.js,
    broadcast.js, watch.js, lib/ (anim.js, channel.js, live.css).
C2. Submodule: git submodule deinit marp-theme-rhea && git rm marp-theme-rhea,
    delete .gitmodules. (node_modules/ is already untracked.)
C3. Purge references: CONCEPT.md porting note (line ~48) — rewrite to point
    at the stage port; src/slides.rs:135 "marp idiom" comment;
    src/lib.rs:167 "marp-style" comment; assets/main.css:1 "marp rhea/gaia"
    header. Re-grep case-insensitively for marp/iframe/standalone before
    committing.
C4. Keep: export/ (dioxus-ssr output, regenerated by
    cargo run --features export --bin export), all of src/, tests/.

Order & hazards
Execute A1→A4 → B1 → B2 → B3 → B4 → C1–C4 → B5. B5 before C is fine too,
but C is gated on B1–B4 only (the standalone quartet is what the marp deck
iframes). Hazards:
- Hydration: never start the rAF loop during SSR; the static diagram is the
  first paint (export byte-determinism, JS-disabled readability).
- Remote mode: stage cosmetics must derive only from events the server
  already broadcasts; no new wire messages for positions.
- Phones: pointer events + no hover-only affordances; test in a phone
  viewport via the QR join path.
- Do not disturb the sim/actor layer; all Part B diffs belong under
  src/games/*, src/stage/*, assets/main.css.

Verification per phase
- cargo test --features server (all planes), cargo clippy --all-targets.
- cargo check -p sim-channels --target wasm32-unknown-unknown.
- dx serve: local mode smoke per ported game; remote smoke presenter+phone
  (two browsers) over the QR path.
- cargo run --features export --bin export: regenerates byte-deterministic
  slides; diff shows only intended changes.
