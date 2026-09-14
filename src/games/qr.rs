//! The join code: how the room gets its hands on a `Sender<T>`.
//!
//! Rendered client-side as an inline SVG so it costs no round trip and
//! works in the static export. The `qrcode` crate is pure Rust with no
//! dependencies, so this compiles to wasm unchanged.

use dioxus::prelude::*;

/// Render a QR code for `url` as an inline SVG.
///
/// Returns `None` when the payload cannot be encoded, which in practice
/// means a URL long past anything a phone camera would resolve anyway.
pub fn qr_svg(url: &str) -> Option<String> {
    use qrcode::render::svg;
    use qrcode::{EcLevel, QrCode};

    // Medium correction: the code is on a projector, not a coffee cup. It
    // survives a head in the way without inflating the module count.
    let code = QrCode::with_error_correction_level(url, EcLevel::M)
        .inspect_err(|error| tracing::warn!(%error, "cannot encode the join url"))
        .ok()?;

    Some(
        code.render::<svg::Color>()
            // The deck is cream on orange; a QR must stay high-contrast to
            // scan, so this is the one place that uses plain black.
            .light_color(svg::Color("#ffffff"))
            .dark_color(svg::Color("#000000"))
            .quiet_zone(true)
            .min_dimensions(0, 0)
            .build(),
    )
}

/// The fullscreen join overlay: the QR code, the URL under it, and how many
/// of the room's handles are still free.
///
/// Presenter-only, because `join_url` only reaches the presenter.
#[component]
pub fn JoinOverlay(
    url: Option<String>,
    players_present: usize,
    players_capacity: usize,
    on_close: EventHandler<()>,
) -> Element {
    let svg = use_memo(use_reactive!(|url| url.as_deref().and_then(qr_svg)));
    let free = players_capacity.saturating_sub(players_present);

    rsx! {
        div {
            class: "join-overlay",
            onclick: move |_| on_close.call(()),
            div { class: "join-card",
                match (url.clone(), svg()) {
                    (Some(url), Some(svg)) => rsx! {
                        div { class: "join-qr", dangerous_inner_html: "{svg}" }
                        p { class: "join-url", "{url}" }
                        p { class: "join-count",
                            if free == 0 {
                                "all {players_capacity} handles taken"
                            } else {
                                "{free} of {players_capacity} handles free"
                            }
                        }
                    },
                    // Say why rather than showing an empty box: without
                    // JOIN_URL the server cannot know its own public
                    // address, and a presenter pressing `q` to nothing has
                    // no way to guess that.
                    (None, _) => rsx! {
                        p { class: "join-missing", "No join code" }
                        p { class: "dim",
                            "Set JOIN_URL to the address the audience should scan, "
                            "then restart the server."
                        }
                    },
                    (Some(url), None) => rsx! {
                        p { class: "join-missing", "Cannot encode a code for this url" }
                        p { class: "join-url", "{url}" }
                    },
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_url_renders_to_an_svg() {
        let svg = qr_svg("https://example.test/?t=abc").expect("encodable");
        assert!(svg.starts_with("<?xml") || svg.starts_with("<svg"));
        assert!(svg.contains("</svg>"));
    }

    #[test]
    fn distinct_urls_render_differently() {
        let first = qr_svg("https://example.test/a").expect("encodable");
        let second = qr_svg("https://example.test/b").expect("encodable");
        assert_ne!(first, second);
    }

    #[test]
    fn an_unencodable_payload_is_reported_rather_than_panicking() {
        // past the version-40 capacity for any error-correction level
        let huge = "x".repeat(8000);
        assert_eq!(qr_svg(&huge), None);
    }
}
