# Talk Concept: Actors with Tokio

**Format**: 40–50 min.
Audience-playable minigames for specific concepts.

## Talk structure

1. **Hook — shared synchronized state**: `Arc<Mutex<T>>` buys safety but not backpressure, lifecycle, panic isolation, or deterministic tests. A mutex is an unstructured actor.
  - Mutex is the first shared minigame. Display QR code. Somebody will lock it. On unlock, somebody is selected who is waiting on the lock who gets unlocked. Nobody else can do anything because they get blocked on acquire. So this is not ideal.
2. Let's start with futures though.
  - Timer minigame. Not interactive. When activated, the future blocks until the next time the seconds are a multiple of 10. Then it yields the number of seconds waited. For comfort, a seconds-of-the-minute timer is displayed.
  - Button minigame. This is real I/O. When activated, the future blocks until either of three colored buttons is pressed, then it yields the button which was clicked. This is a composite future already - it waits for 3 things.
3. Select minigame. Both Futures from before next to each other. Whichever wins, gets displayed at the bottom (seconds or button).
4. Loop-select minigame. Both Futures from before next to each other. Whichever wins, gets displayed at the bottom (seconds or button). Then the loop repeats.

5. Because loop-select needs to run somewhere, let's put it in an actor. Make a page minigame where the watchdog actor from https://github.com/barafael/watchdog is visualized.
6. **The actor recipe** (from the blog): actor = plain data; event loop as consuming method returning `Self`; no handle types; natural shutdown by dropping; deterministic unit tests.
   This is the meat chapter. It contains a bunch of code examples. Especially the ones in the end with the deterministic testing. Include buttons here which open vscode at the correct file/line/col/ in the example projects.
7. How do we connect actors? How do they talk? Channel minigames. The audience member is an actor.
  - `mpsc` (inbox + backpressure + deadlock-cycle footgun),
  - `oneshot` (transfer single value, an exercise in your understanding of ownership),
  - `mpsc` with `oneshot` (call-and-response),
  - `broadcast` (fan-out, honest lag),
  - `watch` (latest-value config).
8. Channels determine architecture. Build whiteboard app in steps.
10. **Garnish — OOOP**: Alan Kay; message passing was the point; `Sender<Message>` as a late-bound vtable.

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
- possibly other architecture realtime views (protohackers exercise number 6)

**Porting note**: the existing JS demos have a clean sim/render split — `channel.js` ports to Rust ~1:1 (it's already a state machine); the render layer is rewritten as rsx + signals + rAF, styled from the rhea/live.css tokens. Slide demos stay local-iframed in the repo deck during development, but the final product has none of that.
