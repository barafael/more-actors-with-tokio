#[cfg(feature = "server")]
fn main() {
    more_actors_with_tokio::server::serve_app();
}

// The client half targets wasm. Building it for a native target compiles
// fine and then panics deep inside wasm-bindgen on first use, which is a
// baffling way to find out you built the wrong thing — and exactly what
// happens when a host's Rust buildpack runs a plain `cargo build`, since
// the default feature is `web`.
//
// Deploys must build the server: `dx bundle --platform web --fullstack`
// (see Dockerfile), or `--no-default-features --features server`.
#[cfg(all(not(feature = "server"), not(target_arch = "wasm32")))]
fn main() {
    eprintln!(
        "{}",
        [
            "this is the web client, built for a native target, so it cannot run.",
            "build the server instead:",
            "",
            "    dx bundle --platform web --fullstack --release",
            "",
            "or, without the CLI:",
            "",
            "    cargo build --release --no-default-features --features server",
        ]
        .join("
")
    );
    std::process::exit(2);
}

#[cfg(all(not(feature = "server"), target_arch = "wasm32"))]
fn main() {
    dioxus::launch(more_actors_with_tokio::App);
}
