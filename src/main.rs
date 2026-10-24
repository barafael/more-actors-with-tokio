#[cfg(target_arch = "wasm32")]
fn main() {
    dioxus::launch(more_actors_with_tokio::App);
}

// The deck is a browser client, so a native build has nothing to do but
// explain itself — a plain `cargo run` lands here rather than panicking
// somewhere deep inside wasm-bindgen on first use.
#[cfg(not(target_arch = "wasm32"))]
fn main() {
    eprintln!("the deck is a web client and has no native target to run.");
    eprintln!("serve it in a browser instead:");
    eprintln!();
    eprintln!("    dx serve --platform web");
    eprintln!();
    eprintln!("or render the static export: `cargo run --features export --bin export`.");
    std::process::exit(2);
}
