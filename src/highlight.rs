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

/// Highlight `code` as `token` (e.g. "rust") into one HTML fragment per
/// source line, newlines dropped, with inline styles. Highlighter state
/// carries across lines, so a string or comment spanning several stays
/// coloured; deterministic across targets, so SSR and the hydrated client
/// agree.
pub fn highlight_lines(code: &str, token: &str) -> Vec<String> {
    let Some(syntax) = syntax_set().find_syntax_by_token(token) else {
        return code.lines().map(html_escape).collect();
    };
    let mut highlighter = HighlightLines::new(syntax, theme());
    LinesWithEndings::from(code)
        .map(|line| {
            let Ok(regions) = highlighter.highlight_line(line, syntax_set()) else {
                return html_escape(line.trim_end_matches('\n'));
            };
            let mut html = String::new();
            for (style, text) in regions {
                push_region(&mut html, style, text.trim_end_matches('\n'));
            }
            html
        })
        .collect()
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
