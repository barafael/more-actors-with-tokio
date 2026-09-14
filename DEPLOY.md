Written for: whoever deploys and runs this talk — likely you, possibly six months from now.

# Deploying the talk app to fly.io

The deck is a fullstack Dioxus app: a server that renders the slides and
hosts the minigame actors, plus a wasm client the audience loads on their
phones. `fly deploy` builds both from [Dockerfile](Dockerfile).

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

## Known gap

The presenter slot is **not authenticated**. `ClaimPresenter` and
`AdvanceSlide` are accepted from any connection; the only gate is a
client-side `(pointer: coarse)` media query that anyone can bypass with
devtools. On a public URL, an attendee can take the presenter slot or flip
your slides. Fine on localhost; think about it before putting the fly URL on
a QR code in front of a room.
