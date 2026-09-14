Written for: whoever deploys and runs this talk — likely you, possibly six months from now.

# Deploying the talk app to fly.io

The deck is a fullstack Dioxus app: a server that renders the slides and
hosts the minigame actors, plus a wasm client the audience loads on their
phones. `fly deploy` builds both from [Dockerfile](Dockerfile).

## Deploy

```sh
fly deploy
```

The crate sits at the repo root, next to [fly.toml](fly.toml) and
[Dockerfile](Dockerfile), so there is only one place to run this from and
only one config for fly to find. It was not always so — see Troubleshooting.

## First deploy

```sh
fly launch --no-deploy     # only to create the app; keep the fly.toml here
fly deploy
```

`fly launch` will offer to overwrite [fly.toml](fly.toml). Decline — the
committed one carries settings this app depends on (see below).

Afterwards:

```sh
fly open          # the deck
fly logs          # RUST_LOG is info, with debug for this crate
fly status
```

## What the config does, and why

**`IP=0.0.0.0`.** `dioxus-server` reads `IP` and `PORT` and defaults to
`127.0.0.1:8080`. On the default, fly's proxy cannot reach the process and
every request times out. This single line is the difference between a
working deploy and a silent one.

**The binary and `public/` are siblings in `/app`.** At runtime the server
resolves static assets as `current_exe().parent()/public`. Separate them and
the app boots, serves a blank fallback page, and logs a warning you have to
be looking for.

**One machine — and fly.toml cannot enforce it.** Every minigame is a server
actor whose state lives only in that machine's memory; nothing is persisted
and nothing is shared. Two machines means two unrelated sets of games, with
audience members split between them at random. Machine count is a scale
setting, not an `[http_service]` key, so set it explicitly and never raise
it:

```sh
fly scale count 1
```

**Connection-based concurrency.** Each client holds a websocket for the app
plane plus one per game slide it visits. Request-based limits would measure
the wrong thing.

## Before you go on stage

The app is configured to **suspend when idle** to save money between
rehearsals. A suspended machine wakes with fresh actors: every websocket
drops and all four games reset. Clients do reconnect on their own (1–2–4 s
backoff) and a respawn is sub-second, so it is survivable — but you do not
want to discover it during the talk.

Warm it before presenting:

```sh
fly machine start
curl -sS https://<app>.fly.dev/ > /dev/null
```

For the talk itself, pin it awake and redeploy:

```toml
# fly.toml, under [http_service]
auto_stop_machines = false
min_machines_running = 1
```

```sh
fly deploy
```

Then set it back afterwards if you care about the idle cost.

## Fallbacks

Ranked by how much they preserve:

1. **Run it on the laptop.** `dx serve --platform web --fullstack` gives you
   everything except audience phones reaching it over the internet.
2. **Serve the static export.** `cargo run --features export --bin export`
   writes `export/` — the slides as static HTML that hydrate and run every
   game single-player in the browser. No server, no audience participation,
   but the deck still works and still demos.

## The two secrets

Both are environment variables. Neither has a safe default that is also
usable, so set them explicitly.

**`PRESENTER_KEY`** — whoever presents it may drive slides, claim the
presenter slot in each game, and restart actors. Everyone else is refused
by the server, not merely hidden from the button.

```sh
fly secrets set PRESENTER_KEY="$(openssl rand -hex 16)"
```

Present at `https://<app>.fly.dev/?k=<key>`. Open that once on the laptop
before the talk; the key stays in that tab's URL and every socket it opens
carries it. The server logs the whole URL on startup (`fly logs`), built
from `JOIN_URL`, so you do not have to assemble it by hand.

If the variable is unset the server **generates one per run and logs it at
warn level**. That keeps the default safe for a public URL while still
letting you in — but it changes on every restart, and a suspended machine
restarts. Set it.

**`JOIN_URL`** — the address the QR code encodes. The server cannot work
this out for itself: behind fly's proxy it only sees a private bind address.

```sh
fly secrets set JOIN_URL="https://<app>.fly.dev"
```

Unset, the presenter simply gets no QR code — the deck still works.

## Audience handles

The room shares a fixed pool of **24 player tickets** (`PLAYER_TICKETS`).
Scanning the QR opens the deck, which takes one seat and writes the ticket
into that phone's URL, so a reload or a dropped connection keeps the same
seat.

- A seat is released **2 minutes after its last socket closes**, not on
  disconnect — a phone that locks its screen must not lose its handle. The
  dead-peer window is 25s, so a backgrounded phone keeps its seat with room
  to spare, while a closed tab frees one before the next chapter.
- Once every seat is taken, later arrivals are seated as **spectators**:
  they see every game but cannot act. Their badge says so.
- Only the page the QR points at hands out seats. Opening a game socket
  directly never mints one.

Press **`q`** on the presenter's deck for the fullscreen join code; it also
shows how many handles are still free.

## Troubleshooting

### `cannot call wasm-bindgen imported functions on non-wasm targets`

The machine is running the **web client** compiled for Linux, not the
server. It boots, panics immediately, exits 101, and fly restarts it in a
loop.

The cause is the crate's default feature:

```toml
[features]
default = ["web"]          # dioxus/web — the wasm client
server = ["dioxus/server"] # the axum server
```

A plain `cargo build` or `cargo install` — which is what fly's Rust
buildpack runs when it cannot find a Dockerfile — therefore builds the
client and names it `more-actors-with-tokio`. Two tells in the log:

- the binary is `/usr/local/bin/more-actors-with-tokio`, not `/app/server`,
  so the image did not come from this Dockerfile;
- the validated config path was `more-actors-with-tokio/fly.toml` rather
  than the repo-root one.

This happened because the crate used to live in a `more-actors-with-tokio/`
subdirectory. `fly launch`, run from in there, wrote its own Dockerfile and
fly.toml beside the crate where the real ones at the repo root were not
visible — and its Dockerfile ran a bare `cargo build --bin
more-actors-with-tokio`, which is the client.

Two changes make it hard to repeat: the crate is now at the repo root, so
there is one obvious place for config; and `src/main.rs` fails with an
explanation instead of a wasm panic if a client binary is ever run
natively again.

### `flyctl deploy --image ...` deploys the wrong thing

Passing `--image` skips the build entirely and ships whatever that tag
points at, including an image built earlier by the buildpack. To rebuild
from the Dockerfile, deploy without `--image`:

```sh
fly deploy                 # builds from Dockerfile
fly deploy --no-cache      # if you suspect a stale layer
```

### `failed to add ip to app: org_slug is only supported with private_v6 type`

This surfaced alongside the crash loop and is an IP-allocation error, not an
app fault. Once the app boots, allocate addresses explicitly:

```sh
fly ips list
fly ips allocate-v4 --shared
fly ips allocate-v6
```
