# Actors with Tokio

A conference talk about actors and channels in Tokio, where the slides *are*
the running system they describe. Every minigame runs inside the deck itself,
live, on the machine driving the projector.

See [CONCEPT.md](CONCEPT.md) for the talk structure.

## Layout

```text
src/
  lib.rs        the deck: routing, slide state, keyboard navigation
  slides.rs     each slide as an rsx component
  games/        one module per minigame, each a component over its own
                model.rs: mutex, futures (timer, button, select, spawn,
                loop-select), watchdog, unit_test (stepper), mpsc, cycle
                (deadlock), oneshot, call (mpsc + oneshot), chat,
                unordered, pipeline, speedd (actor architectures stepped by
                hand; board.rs is what the boards share), broadcast, watch
  clock.rs      the monotonic clock and the tick contract clock-driven
                models share
crates/
  sim-channels/ tokio's four channels as pure state machines (no tokio, wasm-safe)
tests/          css sanity checks against the stylesheet
assets/         css and images
export/         generated static deck (see "Static export")
```

The interesting split: **`crates/sim-channels` is the source of truth** for
what the channels do. It models `oneshot`, `mpsc`, `broadcast` and `watch`
as plain state machines with explicit FIFO waiter queues — no async, no
runtime, no clock — so wake order is deterministic and assertable. Each
channel game's `model.rs` is a thin adapter over it that adds handle ids and
animation state. Where the two disagree with tokio, tokio wins.

There is no server. Every game keeps its model in a signal, calls its methods
on a click and renders straight from it — the same models the tests
exercise, so what is on the slide is what the test suite asserts.

## Running it

```sh
dx serve --platform web
```

Open <http://127.0.0.1:8080> and drive the deck. Everything is local to that
browser: the games, the slide state, the clock.

Deck keys:

| Key | Does |
|---|---|
| `→` `space` `PageDown` | next slide |
| `←` `PageUp` | previous slide |

The navigation bar hides itself until the mouse moves.

## Tests

```sh
cargo test --workspace
cargo clippy --workspace --all-targets
cargo check --target wasm32-unknown-unknown
```

`sim-channels` has its own suite whose test names read as tokio's spec.
The mpsc, watch and broadcast models add differential tests that run the model
and the bare core through the same operations and assert they agree — that is
what keeps the two from drifting apart again.

## Static export

```sh
dx build --platform web --features hydrate
cargo run --features export --bin export
```

The first line builds the wasm bundle the pages ship, with hydration on so
the client adopts the rendered page instead of drawing a second deck under
it (`dx serve` has no page to adopt, so the live deck builds without it).
The second renders every slide to `export/` as static HTML that hydrates in the browser
and runs each game in place. The export asserts its own byte-determinism.
This is the artifact to share after the talk, and the fallback if the live
run misbehaves.

## Known gaps

Deferred, per [CONCEPT.md](CONCEPT.md): the whiteboard
app and the recipe tooling (snippet
extraction, jump-to-VS-Code). The OOOP closer is a sketch. Code on slides is
removed for now; only the unit-test stepper shows any.
