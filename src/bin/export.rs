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

/// Static, wasm-free navigation for a slide page: `prev`/`next` file names
/// (None disables that side) and the "n / N" counter.
struct Nav {
    prev: Option<&'static str>,
    next: Option<&'static str>,
    counter: String,
}

fn page(title: &str, body: &str, nav: Option<&Nav>) -> String {
    // The client's hydrate feature is compiled in, so the exported pages must
    // carry root hydration data like the fullstack server does. Our app has no
    // server functions; this is the byte-for-byte root payload the live server
    // sends (capture again if use_server_futures are ever introduced).
    let hydration_data = "hoEY9vb2gRj1gRj1gRj1";
    let (nav_style, nav_html) = match nav {
        None => (String::new(), String::new()),
        Some(nav) => {
            let (prev, next) = (
                nav.prev
                    .map(|f| format!("<a href=\"{f}\" rel=\"prev\">&#8249; prev</a>"))
                    .unwrap_or_else(|| "<span class=\"off\">&#8249; prev</span>".to_string()),
                nav.next
                    .map(|f| format!("<a href=\"{f}\" rel=\"next\">next &#8250;</a>"))
                    .unwrap_or_else(|| "<span class=\"off\">next &#8250;</span>".to_string()),
            );
            let style = "<style>\n.export-nav{position:fixed;bottom:8px;left:50%;transform:translateX(-50%);z-index:60;display:flex;gap:10px;align-items:center;font:700 12px 'Space Mono',monospace;color:#77705f;background:rgba(250,245,232,.9);border:1px solid rgba(59,56,47,.35);padding:4px 12px;border-radius:0;user-select:none}\n.export-nav a{color:#eb5b20;text-decoration:none}\n.export-nav a:hover{text-decoration:underline}\n.export-nav .off{opacity:.35}\n</style>\n".to_string();
            let bar = format!(
                "<nav class=\"export-nav\">{prev}<span class=\"count\">{}</span>{next}</nav>\n",
                nav.counter
            );
            (style, bar)
        }
    };
    let key_nav = match nav {
        None => String::new(),
        Some(nav) => {
            let prev_js = nav
                .prev
                .map(|f| format!("\"{f}\""))
                .unwrap_or_else(|| "null".into());
            let next_js = nav
                .next
                .map(|f| format!("\"{f}\""))
                .unwrap_or_else(|| "null".into());
            // Arrow/space paging for the page as opened from disk, where the
            // wasm client cannot load (module scripts fail on file://). It
            // stands down the moment the real bundle boots (`__dx_booted`,
            // set by the module script's onload): the hydrated deck binds the
            // same keys, and both firing would reload the page mid-deck.
            format!(
                "<script>(function(){{var PREV={prev_js},NEXT={next_js};document.addEventListener(\"keydown\",function(e){{if(window.__dx_booted===true||e.defaultPrevented||e.altKey||e.ctrlKey||e.metaKey)return;var t=e.target;if(t&&(t.tagName===\"INPUT\"||t.tagName===\"TEXTAREA\"||t.tagName===\"BUTTON\"||t.isContentEditable))return;var k=e.key,go=null;if(k===\"ArrowRight\"||k===\"PageDown\"||k===\" \")go=NEXT;else if(k===\"ArrowLeft\"||k===\"PageUp\")go=PREV;if(go){{e.preventDefault();location.href=go;}}}});}})();</script>\n"
            )
        }
    };
    format!(
        "<!doctype html>\n<html>\n<head>\n<meta charset=\"utf-8\">\n<title>{title}</title>\n\
         <meta name=\"game-mode\" content=\"local\">\n\
         <link rel=\"stylesheet\" href=\"https://fonts.googleapis.com/css2?family=Bitter:ital@0;1&amp;family=Fira+Mono&amp;family=Patrick+Hand&amp;family=Space+Mono:wght@400;700&amp;display=swap\">\n\
         <link rel=\"stylesheet\" href=\"main.css\">\n\
         <script>{STREAMING_INIT}</script>\n\
         {nav_style}\
         </head>\n<body>\n<div id=\"main\">\n{body}\n</div>\n\
         {nav_html}\
         <script>window.initial_dioxus_hydration_data=\"{hydration_data}\";window.initial_dioxus_hydration_debug_types=[];window.initial_dioxus_hydration_debug_locations=[];</script>\n\
         {key_nav}\
         <script type=\"module\" src=\"./wasm/more-actors-with-tokio.js\" onload=\"window.__dx_booted=true\"></script>\n</body>\n</html>\n",
    )
}

/// Mirrors dioxus-interpreter-js's initialize_streaming.js: the hydration
/// machinery expects a streaming queue to exist before the client boots.
const STREAMING_INIT: &str = "window.hydrate_queue=[];window.dx_hydrate=(id,data,debug_types,debug_locations)=>{let decoded=atob(data),bytes=Uint8Array.from(decoded,(c)=>c.charCodeAt(0));if(window.hydration_callback)window.hydration_callback(id,bytes,debug_types,debug_locations);else window.hydrate_queue.push([id,bytes,debug_types,debug_locations])};";

/// One name per slide, in deck order. The array is sized by `SLIDE_COUNT`,
/// so adding a slide without naming it fails to compile rather than
/// exporting a page called `slide-7-`.
const SLIDE_NAMES: [&str; SLIDE_COUNT] = [
    "title",
    "timer",
    "button-future",
    "select",
    "loop-select",
    "recipe",
    "mpsc",
    "watch",
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
        let slide_file = |j: usize| format!("slide-{j}-{}.html", SLIDE_NAMES[j]);
        std::fs::write(
            out_dir.join(&file),
            page(
                name,
                &body,
                Some(&Nav {
                    prev: if i > 0 {
                        Some(slide_file(i - 1).leak())
                    } else {
                        None
                    },
                    next: if i + 1 < SLIDE_NAMES.len() {
                        Some(slide_file(i + 1).leak())
                    } else {
                        None
                    },
                    counter: format!("{} / {}", i + 1, SLIDE_NAMES.len()),
                }),
            ),
        )
        .unwrap_or_else(|e| panic!("write {file}: {e}"));
        index_links.push_str(&format!("<li><a href=\"{file}\">{name}</a></li>\n"));
    }

    let index_body = render_deck(0);
    std::fs::write(out_dir.join("index.html"), page("index", &index_body, None))
        .expect("write index");

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
