# Build and run the fullstack talk app.
#
# Layout matters at runtime: dioxus-server resolves its static assets as
# `current_exe().parent()/public`, so the server binary and the `public`
# directory must stay siblings in the final image.

FROM rust:1-bookworm AS build

# wasm target for the client half of the fullstack build
RUN rustup target add wasm32-unknown-unknown

# Pin the CLI to the dioxus version in Cargo.toml: `dx` and the `dioxus`
# crate share a build protocol and drift between them breaks the bundle.
RUN cargo install dioxus-cli --version 0.7.10 --locked

WORKDIR /src

# Warm the dependency cache on manifests alone, so editing src/ does not
# refetch the whole tree on every deploy. `cargo fetch` only resolves the
# graph, so stub sources are enough — no target needs to compile here.
COPY Cargo.toml Cargo.lock ./
COPY crates/sim-channels/Cargo.toml crates/sim-channels/
RUN mkdir -p src crates/sim-channels/src \
    && echo 'fn main() {}' > src/main.rs \
    && echo '' > src/lib.rs \
    && echo '' > crates/sim-channels/src/lib.rs \
    && cargo fetch --locked \
    && rm -rf src crates/sim-channels/src

COPY . .

# `--out-dir` pins the output path; without it the server binary carries a
# content hash in its name and the COPY below could not name it. The binary
# and `public/` land at the out-dir root, already siblings.
RUN dx bundle \
    --platform web \
    --fullstack \
    --release \
    --out-dir /out

# Fail here rather than in production. The crate default-builds the wasm
# client, so a build that picks the wrong features yields a native binary
# that panics inside wasm-bindgen on startup. `dx bundle` selects the
# server feature itself; this asserts that it actually did.
RUN test -x /out/server || { echo "no server binary in bundle" >&2; exit 1; }
RUN test -f /out/public/index.html || { echo "no client assets in bundle" >&2; exit 1; }

FROM debian:bookworm-slim AS runtime

# The deck loads webfonts from Google Fonts in the client, and any future
# outbound TLS from the server needs a trust store. A non-root user is
# ordinary hygiene for a public-facing process.
RUN apt-get update \
    && apt-get install -y --no-install-recommends ca-certificates \
    && rm -rf /var/lib/apt/lists/* \
    && useradd --create-home --uid 10001 app

WORKDIR /app

COPY --from=build /out/server /app/server
COPY --from=build /out/public /app/public

USER app

# dioxus-server reads IP and PORT (dioxus-cli-config: SERVER_IP_ENV /
# SERVER_PORT_ENV) and defaults to 127.0.0.1:8080, which fly cannot reach.
ENV IP=0.0.0.0
ENV PORT=8080
EXPOSE 8080

CMD ["/app/server"]
