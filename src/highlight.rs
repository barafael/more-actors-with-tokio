use std::fmt::Write as _;
use std::sync::OnceLock;

use syntect::easy::HighlightLines;
use syntect::highlighting::{FontStyle, Style, Theme, ThemeSet};
use syntect::parsing::SyntaxSet;
use syntect::util::LinesWithEndings;

static SYNTAX_SET: OnceLock<SyntaxSet> = OnceLock::new();
static THEME: OnceLock<Theme> = OnceLock::new();

fn syntax_set() -> &'static SyntaxSet {
    SYNTAX_SET.get_or_init(SyntaxSet::load_defaults_newlines)
}

fn theme() -> &'static Theme {
    THEME.get_or_init(|| {
        ThemeSet::load_defaults()
            .themes
            .remove("InspiredGitHub")
            .expect("bundled InspiredGitHub theme")
    })
}

/// Highlight `code` as `token` (e.g. "rust") into HTML with inline styles.
/// Deterministic across targets, so SSR and the hydrated client agree.
pub fn highlight(code: &str, token: &str) -> String {
    let Some(syntax) = syntax_set().find_syntax_by_token(token) else {
        return html_escape(code);
    };
    let mut highlighter = HighlightLines::new(syntax, theme());
    let mut html = String::new();
    for line in LinesWithEndings::from(code) {
        let Ok(regions) = highlighter.highlight_line(line, syntax_set()) else {
            html.push_str(&html_escape(line));
            continue;
        };
        for (style, text) in regions {
            push_region(&mut html, style, text);
        }
    }
    html
}

fn push_region(html: &mut String, style: Style, text: &str) {
    if text.is_empty() {
        return;
    }
    let fg = style.foreground;
    let mut css = format!("color:#{:02x}{:02x}{:02x}", fg.r, fg.g, fg.b);
    if style.font_style.contains(FontStyle::BOLD) {
        css.push_str(";font-weight:bold");
    }
    if style.font_style.contains(FontStyle::ITALIC) {
        css.push_str(";font-style:italic");
    }
    if style.font_style.contains(FontStyle::UNDERLINE) {
        css.push_str(";text-decoration:underline");
    }
    let _ = write!(html, "<span style=\"{css}\">{}</span>", html_escape(text));
}

fn html_escape(text: &str) -> String {
    text.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
}
