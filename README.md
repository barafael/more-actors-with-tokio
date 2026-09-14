# Actors with Tokio

A conference talk about actors and channels in Tokio, where the slides *are*
the running system they describe. The audience joins on their phones and
each becomes a `Sender<T>`, so backpressure is something you feel rather
than something you are told about.

See [CONCEPT.md](CONCEPT.md) for the talk structure.

## Layout

```text
src/
  lib.rs        the deck: slides, routing, the two websocket planes
  slides.rs     each slide as an rsx component
  games/        one module per minigame (button, mpsc, watch, broadcast)
  server/       one actor per game, supervised, behind axum State
  sim.rs        game drivers: registries and cosmetics over the channel cores
  protocol.rs   the wire types shared by client and server
crates/
  sim-channels/ tokio's four channels as pure state machines (no tokio, wasm-safe)
tests/          end-to-end tests over the real websocket planes
assets/         css and images
export/         generated static deck (see "Static export")
```

The interesting split: **`crates/sim-channels` is the source of truth** for
what the channels do. It models `oneshot`, `mpsc`, `broadcast` and `watch`
as plain state machines with explicit FIFO waiter queues — no async, no
runtime, no clock — so wake order is deterministic and assertable. `sim.rs`
is a thin driver over it that adds connection registries and animation
state. Where the two disagree with tokio, tokio wins.

## Running it

```sh
dx serve --platform web --fullstack
```

Then open <http://127.0.0.1:8080>. The desktop client claims the presenter
slot; other browsers and phones join as players.

Without the `--fullstack` flag you get the client alone, which runs every
game single-player in the browser against the same sims.

## Tests

```sh
cargo test --workspace --features server
cargo clippy --workspace --all-targets --features server
cargo check --target wasm32-unknown-unknown
```

`sim-channels` has its own suite whose test names read as tokio's spec.
`src/sim.rs` adds differential tests that run the drivers and the bare cores
through the same operations and assert they agree — that is what keeps the
two from drifting apart again.

## Static export

```sh
cargo run --features export --bin export
```

Renders every slide to `export/` as static HTML that hydrates in the browser
and runs each game single-player, with no server. The export asserts its own
byte-determinism. This is the artifact to share after the talk, and the
fallback if the network fails during it.

## Deploying

See [DEPLOY.md](DEPLOY.md). Short version: `fly deploy` from this directory.

## Known gap

The presenter slot is not authenticated — any connection may claim it or
advance slides. Fine on localhost; think before putting a public URL in
front of a room.
