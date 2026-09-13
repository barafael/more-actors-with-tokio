#[cfg(feature = "server")]
fn main() {
    more_actors_with_tokio::server::serve_app();
}

#[cfg(not(feature = "server"))]
fn main() {
    dioxus::launch(more_actors_with_tokio::App);
}
