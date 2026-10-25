# Talk Concept: Actors with Tokio

**Format**: 40–50 min.
Playable minigames for specific concepts, driven from the deck itself.

## Talk structure

1. **Hook — shared synchronized state**: `Arc<Mutex<T>>` buys safety but not backpressure, lifecycle, panic isolation, or deterministic tests. A mutex is an unstructured actor.
  - **Built** (`games/mutex/`): a fair `Arc<Mutex<u64>>` and three tasks each holding a clone; waiters count up the seconds they have been parked.
  - Mutex is the first minigame. Somebody will lock it. On unlock, somebody waiting on the lock is selected and gets it. Nobody else can do anything because they get blocked on acquire. So this is not ideal.
2. Let's start with futures though. **Built.**
  - Timer minigame. Not interactive. When activated, the future blocks until the next time the seconds are a multiple of 10. Then it yields the number of seconds waited. For comfort, a seconds-of-the-minute timer is displayed.
  - Button minigame. This is real I/O. When activated, the future blocks until either of three colored buttons is pressed, then it yields the button which was clicked. This is a composite future already - it waits for 3 things.
3. Select minigame. Both Futures from before next to each other. Whichever wins, gets displayed at the bottom (seconds or button). **Built.**
4. Loop-select minigame. Both Futures from before next to each other. Whichever wins, gets displayed at the bottom (seconds or button). Then the loop repeats. **Built**, with a tape of the last few rounds.

  These three share the **tick contract** (`src/clock.rs`): `sync_now` / `next_delay_ms` / `poll_due`, so a sim reports *when* it next wants polling and the component decides how to wait — a frame loop in the browser (`src/games/ticker.rs`). The sims never sleep, so they are tested by stepping a number forward. `SelectSim` composes the real `TimerSim` and `ButtonSim` rather than re-modelling them, which is what makes "the losing branch is dropped" one line rather than a special case.

5. Because loop-select needs to run somewhere, let's put it in an actor. Make a page minigame where the watchdog actor from https://github.com/barafael/watchdog is visualized.
  - **Built** (`src/games/watchdog/`): the crate's event loop, live and single-player. You spawn its `event_loop()`; `send(Reset)`, `send(Stop)` and `drop(reset_tx)` fly into its `select!`, drawn with the select slides' two branches — `reset_rx.recv()` and `sleep, if active` with a draining countdown. Left alone the sleep wins, `Expired` flies out on the oneshot, and the actor is gone: sends fail until a fresh one is spawned. A reset still in flight at the deadline arrives too late.
6. **The actor recipe** (from the blog): actor = plain data; event loop as consuming method returning `Self`; no handle types; natural shutdown by dropping; deterministic unit tests.
   This is the meat chapter. It contains a bunch of code examples. Especially the ones in the end with the deterministic testing. Include buttons here which open vscode at the correct file/line/col/ in the example projects.
  - **Built**: the recipe slide lists its five rules (the blog's `UniqueIdService` code is off the slide for now).
  - Unit-test stepper, **built** (`src/games/unit_test/`). The blog's `should_increment_unique_id`, stepped line by line like a debugger: step into `get_unique_id` and `event_loop`, step over, step back. Panes show the test, the call in progress, and the world after each line: the actor's state and where it lives, the mpsc buffer, and each oneshot pair coloured end to end. Two broken variants, one per precondition: `forget drop(tx)` parks `recv()` forever, and `channel(2)` parks the third send before the loop ever runs. The whole run is precomputed as a trace, so the game is just an index into it and every frame is unit-tested.
7. How do we connect actors? How do they talk? Channel minigames. The viewer is an actor.
  **Built**: mpsc and oneshot close the prerequisites; call-and-response sits in the pattern; the cycle, broadcast and watch in the architecture chapter.
  - `mpsc` (inbox + backpressure + deadlock-cycle footgun), the footgun as its own single-player game: A and B forward to each other over bounded inboxes (`MpscCore`), seize at six messages, drain once the cycle is cut,
  - `oneshot` (transfer single value, an exercise in your understanding of ownership), single-player: every call takes its handle by value, and each result (including `Err(v)` handed back and `RecvError`) is written down,
  - `mpsc` with `oneshot` (call-and-response): requesters call `get_unique_id`, the viewer is the event loop and answers or drops each callback by hand; waiting requesters count up,
  - `broadcast` (fan-out, honest lag),
  - `watch` (latest-value config).
8. Channels determine architecture. Build whiteboard app in steps. *Deferred.*
  - **Built**: budget chat (Protohackers 3) without the lock (`src/games/chat/`). The original's `Arc<Mutex<HashMap>>` member list becomes a room actor that owns it; connections join, speak and leave through its bounded inbox, a join is answered over a oneshot, and the room holds the only broadcast sender. Click-stepped like speedd; a connection left unclicked lags and loses lines. No `Arc<Mutex<_>>` anywhere in the deck's answers — the mutex appears only as the hook's problem.
  - **Built**: FuturesUnordered on its own, just before the pipeline (`src/games/unordered/`). The player is the task and the network: push downloads into the set, wake them in any order, `next().await` yields the first to finish — `Pending` parks the task, an empty set yields `None`. A toggle to FuturesOrdered shows head-of-line blocking, which is why each pipeline stage uses the unordered one.
  - **Built**: the pipeline from `barafael/halres-downloader` (`src/games/pipeline/`), and with it natural shutdown across a DAG. Reader → downloader → processor → collector over bounded channels; each stage is the crate's `select! { biased; … }` over a bounded `FuturesUnordered`, whose jobs the player completes by clicking — so output order is completion order, and a finished job overtakes an older one. A full downstream parks a stage; a failed download is `warn!`ed and dropped. `drop(pages_tx)` at the end of the file and each stage breaks once its input is drained and its work empty, dropping its own sender, until the collector's `recv()` sees `None`.
  - **Built instead, first**: speedd (Protohackers 6) as a board you step by hand (`src/games/speedd/`). Seven cameras on three roads, the Collector, one mpmc ticket queue per road, five dispatchers `[7] [7] [7,8] [8,9] [9]` — the topology of speedd's own `live-topology.drawio.png`, widened to three roads. Single-player. Nothing advances on its own: clicking an actor runs one iteration of its loop, and what it sent flies along the graph. Every channel shows its fill, so backpressure is watched propagating: a full road queue parks the Collector, then `reporting` fills, then cameras park. Dispatchers start unsubscribed, so the `(road, oneshot)` handshake and tickets queuing with no dispatcher are both part of play. The ticket logic is ported from speedd's `collector.rs`.
10. **Garnish — OOOP** (*sketched*: the quote and the five-way mapping, no game): Alan Kay; message passing was the point; `Sender<Message>` as a late-bound vtable.

## The Slide App

- **Dioxus 0.7.10**, rhea style (cream `#fff8e1`, orange `#eb5b20`, Bitter/Fira Mono). Runs in one browser; served as static files.
- **One machine, all controls**: keyboard nav, per-game restart. The deck owns its own slide state and its own games; there is no server and nothing to keep out.
- **Slides are rsx components**, highlighted with **syntect**.
- **Code examples derive from example projects** by snipped extraction, and have a button to jump to file:line:column in vscode, using vscode url handler. Snippet extraction happens with a start/end-snippet-marker comment. Similar to https://github.com/barafael/markdown-tools/tree/main/snippet-extractor
- **"Print" = SSR batch export**: a binary rendering every slide to static HTML via dioxus-ssr, shipped as a static *site* plus the wasm bundle: on load, slides hydrate and games run **in the browser**.
  - The sim is shared Rust: each game is a model over the `sim-channels` cores, running in the browser. Feasibility spike: **verified** — `cargo run --features export --bin export` renders all slides via dioxus-ssr (syntect highlighting, byte-deterministic).
  - On-stage fallback is the live `dx serve` deck on localhost.
- **All games run in place**, each slide owning its game's model.
- **The slide controls and displays its model.**
- **Restart** (per game): the slide rebuilds the model.
- **Whiteboard**: command replay after accept (event sourcing, not periodic snapshots).
- live-topology view of the whiteboard app's real actor graph (games, messages flowing in).
- possibly other architecture realtime views (protohackers exercise number 6) — *built as the speedd board, stepped rather than realtime*

**Porting note**: the existing JS demos have a clean sim/render split — `channel.js` ports to Rust ~1:1 (it's already a state machine); the render layer is rewritten as rsx + signals + rAF, styled from the rhea/live.css tokens. Slide demos stay local-iframed in the repo deck during development, but the final product has none of that.
