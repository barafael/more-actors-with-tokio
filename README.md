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
                plus qr.rs, the join code the audience scans
  server/       one actor per game, supervised, behind axum State
                auth.rs decides roles; tickets.rs is the seat pool
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
PRESENTER_KEY=stagekey JOIN_URL=http://127.0.0.1:8080   dx serve --platform web --fullstack
```

Open <http://127.0.0.1:8080/?k=stagekey> to present, and plain
<http://127.0.0.1:8080> for an audience phone. The server also logs the
presenter URL on startup, so you can copy it out of the terminal.

Presenter keys:

| Key | Does |
|---|---|
| `→` `space` `PageDown` | next slide |
| `←` `PageUp` | previous slide |
| `q` | the fullscreen join code |
| `Esc` | close the join code |

The navigation bar hides itself until the mouse moves.

Leave `PRESENTER_KEY` unset and the server generates one per run and logs
it — safe by default, but it changes on every restart.

Without the `--fullstack` flag you get the client alone, which runs every
game single-player in the browser against the same sims.

## Who may do what

Three roles, decided by the server at connect time and carried by the
connection. The client renders what its role allows; the server refuses
everything else, so hiding a control is courtesy rather than enforcement.

| Role | How you get it | May |
|---|---|---|
| Spectator | open the URL | watch |
| Player | scan the QR (one of 24 tickets) | play every game |
| Presenter | `?k=<PRESENTER_KEY>` | play, drive slides, restart actors |

Tickets live in the phone's URL and survive a reload; a seat is only
recycled 2 minutes after its last socket closes, so locking a screen does
not cost someone their handle. See [DEPLOY.md](DEPLOY.md) for the
operational detail.

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

## Known gaps

Tickets are unguessable but not cryptographic, and they are visible in the
URL — someone reading a neighbour's screen could take their seat. The worst
case is one audience member playing as another, which is the right amount
of security for a conference game.

The chapters before the channels (mutex hook, timer, select, loop-select,
the watchdog actor) and after them (whiteboard, OOOP closer) are described
in [CONCEPT.md](CONCEPT.md) but not built yet.
