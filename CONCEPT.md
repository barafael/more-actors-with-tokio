# Talk Concept: Actors with Tokio

**Format**: 40–50 min.
Audience-playable minigames for specific concepts.

## Talk structure

1. **Hook — shared synchronized state**: `Arc<Mutex<T>>` buys safety but not backpressure, lifecycle, panic isolation, or deterministic tests. A mutex is an unstructured actor.
  - **Built** (`sim_mutex.rs`, `games/mutex.rs`): a fair `Arc<Mutex<u64>>`; every phone is a task, waiters count up the seconds they have been parked, a phone that leaves drops its guard. In the export the lone player runs three tasks.
  - Mutex is the first shared minigame. Display QR code. Somebody will lock it. On unlock, somebody is selected who is waiting on the lock who gets unlocked. Nobody else can do anything because they get blocked on acquire. So this is not ideal.
2. Let's start with futures though. **Built.**
  - Timer minigame. Not interactive. When activated, the future blocks until the next time the seconds are a multiple of 10. Then it yields the number of seconds waited. For comfort, a seconds-of-the-minute timer is displayed.
  - Button minigame. This is real I/O. When activated, the future blocks until either of three colored buttons is pressed, then it yields the button which was clicked. This is a composite future already - it waits for 3 things.
3. Select minigame. Both Futures from before next to each other. Whichever wins, gets displayed at the bottom (seconds or button). **Built.**
4. Loop-select minigame. Both Futures from before next to each other. Whichever wins, gets displayed at the bottom (seconds or button). Then the loop repeats. **Built**, with a tape of the last few rounds.

  These three share the **tick contract** (`src/clock.rs`): `sync_now` / `next_delay_ms` / `poll_due`, so a sim reports *when* it next wants polling and the driver decides how to wait — a tokio `select!` branch on the server (`src/server/ticking.rs`, one generic actor body for all three) and a frame loop in the browser (`src/games/ticker.rs`). The sims never sleep, so they are tested by stepping a number forward. `SelectSim` composes the real `TimerSim` and `ButtonSim` rather than re-modelling them, which is what makes "the losing branch is dropped" one line rather than a special case.

5. Because loop-select needs to run somewhere, let's put it in an actor. Make a page minigame where the watchdog actor from https://github.com/barafael/watchdog is visualized.
6. **The actor recipe** (from the blog): actor = plain data; event loop as consuming method returning `Self`; no handle types; natural shutdown by dropping; deterministic unit tests.
   This is the meat chapter. It contains a bunch of code examples. Especially the ones in the end with the deterministic testing. Include buttons here which open vscode at the correct file/line/col/ in the example projects.
  - **Built**: the recipe slide shows the blog's `UniqueIdService` beside its five rules.
  - Unit-test stepper, **built** (`src/games/unit_test/`). The blog's `should_increment_unique_id`, stepped line by line like a debugger: step into `get_unique_id` and `event_loop`, step over, step back. Panes show the test, the call in progress, and the world after each line: the actor's state and where it lives, the mpsc buffer, and each oneshot pair coloured end to end. Two broken variants, one per precondition: `forget drop(tx)` parks `recv()` forever, and `channel(2)` parks the third send before the loop ever runs. **Not collab**: every viewer steps their own copy, even in the talk, and there is no server actor. The whole run is precomputed as a trace, so the game is just an index into it and every frame is unit-tested.
7. How do we connect actors? How do they talk? Channel minigames. The audience member is an actor.
  **Built, in this order**: mpsc, the cycle, oneshot, call-and-response, broadcast, watch.
  - `mpsc` (inbox + backpressure + deadlock-cycle footgun), the footgun as its own single-player game: A and B forward to each other over bounded inboxes (`MpscCore`), seize at six messages, drain once the cycle is cut,
  - `oneshot` (transfer single value, an exercise in your understanding of ownership), single-player: every call takes its handle by value, and each result (including `Err(v)` handed back and `RecvError`) is written down,
  - `mpsc` with `oneshot` (call-and-response): phones call `get_unique_id`, the presenter is the event loop and answers or drops each callback by hand; waiting phones count up,
  - `broadcast` (fan-out, honest lag),
  - `watch` (latest-value config).
8. Channels determine architecture. Build whiteboard app in steps. *Deferred.*
  - **Built instead, first**: speedd (Protohackers 6) as a board you step by hand (`src/games/speedd/`). Seven cameras on three roads, the Collector, one mpmc ticket queue per road, five dispatchers `[7] [7] [7,8] [8,9] [9]` — the topology of speedd's own `live-topology.drawio.png`, widened to three roads. Single-player. Nothing advances on its own: clicking an actor runs one iteration of its loop, and what it sent flies along the graph. Every channel shows its fill, so backpressure is watched propagating: a full road queue parks the Collector, then `reporting` fills, then cameras park. Dispatchers start unsubscribed, so the `(road, oneshot)` handshake and tickets queuing with no dispatcher are both part of play. The ticket logic is ported from speedd's `collector.rs`.
10. **Garnish — OOOP** (*sketched*: the quote and the five-way mapping, no game): Alan Kay; message passing was the point; `Sender<Message>` as a late-bound vtable.

## The Slide App

- **Dioxus 0.7.10 fullstack**, rhea style (cream `#fff8e1`, orange `#eb5b20`, Bitter/Fira Mono). Hosted on fly.io or own VPS.
- **Desktop = presenter**: keyboard nav, `q` = fullscreen QR (top-right) for joining minigames, per-game restart, reset-all. Connects as a normal client with more capabilities; last desktop connected wins the presenter slot; server state survives presenter leave; presenter join restores from server state.
- **Web = client-only**: same views, less UI. QR login acquires one of **24 configurable player tickets**. After that, passive viewer.
- **Slides are rsx components**, highlighted with **syntect**.
- **Code examples derive from example projects** by snipped extraction, and have a button to jump to file:line:column in vscode, using vscode url handler. Snippet extraction happens with a start/end-snippet-marker comment. Similar to https://github.com/barafael/markdown-tools/tree/main/snippet-extractor
- **"Print" = SSR batch export**: a binary rendering every slide to static HTML via dioxus-ssr, shipped as a static *site* plus the wasm bundle: on load, slides hydrate and games run **single-player, purely client-side**
  - The sim is shared Rust; single-player is a loopback implementation of the actor boundary — engine runs as a browser loop instead of a server actor. Only ws-dependent chrome (QR) stays a disabled placeholder. Feasibility spike: **verified** — `cargo run --features export --bin export` renders all slides via dioxus-ssr (server-side hooks, syntect highlighting, byte-deterministic); fullstack SSR first paint works with JS disabled, hydration resumes via `/ws/app`.
  - On-stage fallback is running the fullstack app on localhost (everything except audience participation).
- **Two ws planes**: `/ws/app` (presence, presenter slot, slide index with `watch` semantics, `advance-slide`, restart commands) and `/ws/game/<name>` — one endpoint, one actor, one dedicated enum protocol per game.
- **All games run concurrently from session start, permanently**, all shared/server-authoritative, state persists across slide switches (roughly in order of increasing complexity):
- **The slide controls and displays its actor.** Navigating away keeps the game alive; returning makes the slide an ordinary late joiner (snapshot/replay catch-up, same as any phone).
- **Restart** (per game, presenter-only): token cancels → `select!` branch breaks → cleanup → game-ws closes with **4001** → the backend respawns a fresh actor → clients clear local state, retry at 1–2–4 s (games are permanent; restarts are sub-second, so no cap, no lobby).
- **Whiteboard**: command replay after accept (event sourcing, not periodic snapshots). Other minigames: snapshot after accept.
- live-topology view of the whiteboard app's real actor graph (games, messages flowing in).
- possibly other architecture realtime views (protohackers exercise number 6) — *built as the speedd board, stepped rather than realtime*

**Porting note**: the existing JS demos have a clean sim/render split — `channel.js` ports to Rust ~1:1 (it's already a state machine); the render layer is rewritten as rsx + signals + rAF, styled from the rhea/live.css tokens. Slide demos stay local-iframed in the repo deck during development, but the final product has none of that.
