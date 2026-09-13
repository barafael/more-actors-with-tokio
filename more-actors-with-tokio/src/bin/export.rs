//! SSR batch export: renders every slide to a static HTML page ("print" mode
//! from CONCEPT.md). Pages ship with the wasm client and a game-mode=local
//! marker: on load, the client boots the same `Deck` tree that was
//! server-rendered, and the games run single-player in the browser — same
//! components, same sims, no server.
use dioxus::prelude::*;

use more_actors_with_tokio::protocol::SLIDE_COUNT;
use more_actors_with_tokio::{Deck, DeckProps, GameMode};

fn render_deck(index: usize) -> String {
    let mut dom = VirtualDom::new_with_props(
        Deck,
        DeckProps {
            mode: GameMode::Local,
            initial_slide: index,
        },
    );
    dom.rebuild_in_place();
    // pre_render emits the hydration markers the wasm client expects
    dioxus_ssr::pre_render(&dom)
}

fn page(title: &str, body: &str) -> String {
    // The client's hydrate feature is compiled in, so the exported pages must
    // carry root hydration data like the fullstack server does. Our app has no
    // server functions; this is the byte-for-byte root payload the live server
    // sends (capture again if use_server_futures are ever introduced).
    let hydration_data = "hoEY9vb2gRj1gRj1gRj1";
    format!(
        "<!doctype html>\n<html>\n<head>\n<meta charset=\"utf-8\">\n<title>{title}</title>\n\
         <meta name=\"game-mode\" content=\"local\">\n\
         <link rel=\"stylesheet\" href=\"https://fonts.googleapis.com/css2?family=Bitter:ital@0;1&amp;family=Fira+Mono&amp;display=swap\">\n\
         <link rel=\"stylesheet\" href=\"main.css\">\n\
         <script>{STREAMING_INIT}</script>\n\
         </head>\n<body>\n<div id=\"main\">\n{body}\n</div>\n\
         <script>window.initial_dioxus_hydration_data=\"{hydration_data}\";window.initial_dioxus_hydration_debug_types=[];window.initial_dioxus_hydration_debug_locations=[];</script>\n\
         <script type=\"module\" src=\"./wasm/more-actors-with-tokio.js\"></script>\n</body>\n</html>\n",
    )
}

/// Mirrors dioxus-interpreter-js's initialize_streaming.js: the hydration
/// machinery expects a streaming queue to exist before the client boots.
const STREAMING_INIT: &str = "window.hydrate_queue=[];window.dx_hydrate=(id,data,debug_types,debug_locations)=>{let decoded=atob(data),bytes=Uint8Array.from(decoded,(c)=>c.charCodeAt(0));if(window.hydration_callback)window.hydration_callback(id,bytes,debug_types,debug_locations);else window.hydrate_queue.push([id,bytes,debug_types,debug_locations])};";

const SLIDE_NAMES: [&str; SLIDE_COUNT] = [
    "title",
    "button-future",
    "mpsc",
    "watch",
    "recipe",
    "broadcast",
];

fn main() {
    let out_dir = std::path::Path::new("export");
    std::fs::create_dir_all(out_dir).expect("create export dir");

    let mut index_links = String::new();
    for (i, name) in SLIDE_NAMES.iter().enumerate() {
        let body = render_deck(i);

        // determinism: a second render must produce identical bytes
        let again = render_deck(i);
        assert_eq!(body, again, "non-deterministic render on slide {i}");

        // the recipe slide carries pre-highlighted code from syntect
        let highlighted = body.contains("<span style=\"color:#");
        println!(
            "slide-{i}-{name}.html ({} bytes, syntect spans: {highlighted})",
            body.len()
        );

        let file = format!("slide-{i}-{name}.html");
        std::fs::write(out_dir.join(&file), page(name, &body))
            .unwrap_or_else(|e| panic!("write {file}: {e}"));
        index_links.push_str(&format!("<li><a href=\"{file}\">{name}</a></li>\n"));
    }

    let index_body = render_deck(0);
    std::fs::write(out_dir.join("index.html"), page("index", &index_body)).expect("write index");

    std::fs::copy(
        std::path::Path::new("assets/main.css"),
        out_dir.join("main.css"),
    )
    .expect("copy main.css");
    let _ = copy_dir(std::path::Path::new("assets"), &out_dir.join("assets"));

    let wasm_src = std::path::Path::new("target/dx/more-actors-with-tokio/debug/web/public/wasm");
    match copy_dir(wasm_src, &out_dir.join("wasm")) {
        Ok(()) => println!("wasm bundle copied"),
        Err(e) => eprintln!(
            "warning: no wasm bundle copied from {} ({e}); run `dx build --platform web` first — pages will stay static",
            wasm_src.display()
        ),
    }
    println!("export complete: {}/", out_dir.display());
}

fn copy_dir(src: &std::path::Path, dst: &std::path::Path) -> std::io::Result<()> {
    std::fs::create_dir_all(dst)?;
    for entry in std::fs::read_dir(src)? {
        let entry = entry?;
        if entry.file_type()?.is_dir() {
            copy_dir(&entry.path(), &dst.join(entry.file_name()))?;
        } else {
            std::fs::copy(entry.path(), dst.join(entry.file_name()))?;
        }
    }
    Ok(())
}
