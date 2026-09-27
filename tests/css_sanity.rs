//! Structural checks on the deck's stylesheet.
//!
//! CSS fails silently: a selector whose `{` goes missing turns everything up
//! to the next `{` into one invalid rule, and the browser drops it without a
//! word. That once swallowed the `@keyframes` every board's flights use, and
//! nothing noticed until a screenshot looked wrong. These tests notice.

const CSS: &str = include_str!("../assets/main.css");

/// The stylesheet with its comments blanked out, keeping line breaks so
/// line numbers still point at the right place.
fn without_comments(css: &str) -> String {
    let mut out = String::with_capacity(css.len());
    let mut rest = css;
    while let Some(start) = rest.find("/*") {
        out.push_str(&rest[..start]);
        let end = rest[start..]
            .find("*/")
            .map_or(rest.len(), |e| start + e + 2);
        out.extend(rest[start..end].chars().filter(|c| *c == '\n'));
        rest = &rest[end..];
    }
    out.push_str(rest);
    out
}

#[test]
fn braces_balance() {
    let css = without_comments(CSS);
    let mut depth = 0i32;
    for (number, line) in css.lines().enumerate() {
        for c in line.chars() {
            match c {
                '{' => depth += 1,
                '}' => depth -= 1,
                _ => {}
            }
            assert!(depth >= 0, "unmatched `}}` on line {}", number + 1);
        }
    }
    assert_eq!(depth, 0, "a `{{` is never closed");
}

#[test]
fn no_selector_runs_into_declarations_without_a_brace() {
    let css = without_comments(CSS);
    let lines: Vec<&str> = css.lines().collect();
    for (i, pair) in lines.windows(2).enumerate() {
        let (selector, next) = (pair[0], pair[1]);
        let is_selector_line = !selector.starts_with(char::is_whitespace)
            && selector.trim_end().ends_with(',')
            && !selector.contains('{');
        let next_is_declaration = next.starts_with(char::is_whitespace)
            && next.trim().split_once(':').is_some_and(|(property, _)| {
                property.chars().all(|c| c.is_ascii_lowercase() || c == '-')
            });
        assert!(
            !(is_selector_line && next_is_declaration),
            "line {}: `{}` is followed by a declaration, not a selector or `{{`",
            i + 1,
            selector.trim()
        );
    }
}

#[test]
fn every_animation_names_keyframes_that_exist() {
    let css = without_comments(CSS);
    let defined: Vec<&str> = css
        .split("@keyframes")
        .skip(1)
        .filter_map(|rest| rest.split_whitespace().next())
        .collect();
    assert!(
        defined.contains(&"bd-fly"),
        "the boards' flight keyframes are gone"
    );
    for (number, line) in css.lines().enumerate() {
        let Some(value) = line.trim().strip_prefix("animation:") else {
            continue;
        };
        let name = value.split_whitespace().next().unwrap_or_default();
        if name == "none" {
            continue;
        }
        assert!(
            defined.contains(&name),
            "line {}: animation `{name}` has no @keyframes",
            number + 1
        );
    }
}
