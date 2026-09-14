//! Who may do what, decided server-side.
//!
//! Every socket presents its credentials in the query string, because that
//! is what a QR code can carry and what a phone browser will keep across a
//! reconnect. The server rules on them once, at connect time, and the
//! resulting [`Role`] travels with the connection.
//!
//! This replaces a client-side `(pointer: coarse)` media query, which was
//! the only thing standing between an attendee with devtools and the slide
//! controls.

use crate::protocol::{Credentials, Role, PRESENTER_PARAM, TICKET_PARAM};
use crate::server::tickets::Issue;

/// Environment variable holding the presenter secret.
pub const PRESENTER_KEY_ENV: &str = "PRESENTER_KEY";

/// The presenter secret for this process, read once.
///
/// Unset means nobody can present remotely. That is the safe default for a
/// public URL, but it would also lock the presenter out of their own talk,
/// so `serve_app` generates one at startup and logs it when the variable is
/// missing.
pub fn presenter_key() -> Option<&'static str> {
    static KEY: std::sync::OnceLock<Option<String>> = std::sync::OnceLock::new();
    KEY.get_or_init(|| {
        std::env::var(PRESENTER_KEY_ENV)
            .ok()
            .filter(|k| !k.is_empty())
    })
    .as_deref()
}

/// Whether a presented key matches the configured secret.
///
/// Compared in constant time: the key is short and an attendee can open a
/// great many websockets over the length of a talk.
pub fn presenter_key_ok(presented: Option<&str>) -> bool {
    let (Some(expected), Some(presented)) = (presenter_key(), presented) else {
        return false;
    };
    constant_time_eq(expected.as_bytes(), presented.as_bytes())
}

fn constant_time_eq(left: &[u8], right: &[u8]) -> bool {
    if left.len() != right.len() {
        return false;
    }
    left.iter()
        .zip(right)
        .fold(0u8, |acc, (l, r)| acc | (l ^ r))
        == 0
}

/// Pull credentials out of a raw query string (the part after `?`).
///
/// Hand-rolled rather than pulled in as a dependency: two known keys, and
/// anything unrecognised is ignored.
pub fn credentials_from_query(query: Option<&str>) -> Credentials {
    let mut credentials = Credentials::default();
    let Some(query) = query else {
        return credentials;
    };
    for pair in query.split('&') {
        let Some((key, value)) = pair.split_once('=') else {
            continue;
        };
        let value = percent_decode(value);
        if value.is_empty() {
            continue;
        }
        match key {
            TICKET_PARAM => credentials.ticket = Some(value),
            PRESENTER_PARAM => credentials.presenter_key = Some(value),
            _ => {}
        }
    }
    credentials
}

/// Minimal percent-decoding, enough for the opaque hex tokens we mint and
/// for a hand-typed presenter key. Invalid escapes are left as written.
fn percent_decode(value: &str) -> String {
    let mut out = String::with_capacity(value.len());
    let mut bytes = value.as_bytes().iter().copied().enumerate();
    while let Some((index, byte)) = bytes.next() {
        match byte {
            b'+' => out.push(' '),
            b'%' => {
                let hex = value
                    .get(index + 1..index + 3)
                    .and_then(|hex| u8::from_str_radix(hex, 16).ok().filter(|b| b.is_ascii()));
                match hex {
                    Some(decoded) => {
                        out.push(decoded as char);
                        bytes.next();
                        bytes.next();
                    }
                    None => out.push('%'),
                }
            }
            other => out.push(other as char),
        }
    }
    out
}

/// A connection's decided identity, carried alongside its `conn` id.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Identity {
    pub role: Role,
    pub seat: Option<usize>,
    /// The ticket this socket is holding open, released when it closes.
    pub ticket: Option<String>,
}

impl Identity {
    pub fn may_play(&self) -> bool {
        self.role.may_play()
    }

    pub fn may_present(&self) -> bool {
        self.role.may_present()
    }
}

/// Resolves a connecting socket's identity from its query string.
///
/// An axum extractor so every socket handler gets this for free and none of
/// them can forget: the five `*_socket` functions differ only in which game
/// they talk to, and authorization must not be one of their differences.
///
/// Game sockets never mint a seat — see [`Issue`]. Only the app plane does,
/// because that is the socket the scanned page opens.
pub struct Connecting {
    pub identity: Identity,
    pub conn: u64,
}

/// The app plane's extractor: the one place a seat can be handed out.
pub struct Joining(pub Connecting);

async fn resolve(
    parts: &axum::http::request::Parts,
    state: &crate::server::AppState,
    issue: Issue,
) -> Connecting {
    let credentials = credentials_from_query(parts.uri.query());
    let presenter_ok = presenter_key_ok(credentials.presenter_key.as_deref());
    let claim = state
        .tickets
        .claim(credentials.ticket, issue, presenter_ok)
        .await;
    Connecting {
        identity: Identity {
            role: claim.role,
            seat: claim.seat,
            ticket: claim.ticket,
        },
        conn: crate::server::next_conn(),
    }
}

impl axum::extract::FromRequestParts<crate::server::AppState> for Connecting {
    type Rejection = std::convert::Infallible;

    async fn from_request_parts(
        parts: &mut axum::http::request::Parts,
        state: &crate::server::AppState,
    ) -> Result<Self, Self::Rejection> {
        Ok(resolve(parts, state, Issue::Never).await)
    }
}

impl axum::extract::FromRequestParts<crate::server::AppState> for Joining {
    type Rejection = std::convert::Infallible;

    async fn from_request_parts(
        parts: &mut axum::http::request::Parts,
        state: &crate::server::AppState,
    ) -> Result<Self, Self::Rejection> {
        Ok(Self(resolve(parts, state, Issue::IfAvailable).await))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_absent_query_yields_no_credentials() {
        assert_eq!(credentials_from_query(None), Credentials::default());
        assert_eq!(credentials_from_query(Some("")), Credentials::default());
    }

    #[test]
    fn both_parameters_are_read() {
        let credentials = credentials_from_query(Some("t=abc123&k=hunter2"));
        assert_eq!(credentials.ticket.as_deref(), Some("abc123"));
        assert_eq!(credentials.presenter_key.as_deref(), Some("hunter2"));
    }

    #[test]
    fn unknown_and_malformed_pairs_are_ignored() {
        let credentials = credentials_from_query(Some("slide=3&novalue&t=abc"));
        assert_eq!(credentials.ticket.as_deref(), Some("abc"));
        assert_eq!(credentials.presenter_key, None);
    }

    #[test]
    fn an_empty_value_is_not_a_claim() {
        // `?t=` must not count as presenting a ticket
        assert_eq!(
            credentials_from_query(Some("t=&k=")),
            Credentials::default()
        );
    }

    #[test]
    fn percent_escapes_are_decoded() {
        let credentials = credentials_from_query(Some("k=a%20b%2Bc"));
        assert_eq!(credentials.presenter_key.as_deref(), Some("a b+c"));
    }

    #[test]
    fn a_trailing_percent_is_left_alone() {
        let credentials = credentials_from_query(Some("k=abc%"));
        assert_eq!(credentials.presenter_key.as_deref(), Some("abc%"));
    }

    #[test]
    fn no_configured_key_refuses_every_presenter_claim() {
        // presenter_key() reads the process environment once, so this test
        // asserts the branch that does not depend on it
        assert!(!presenter_key_ok(None));
    }

    #[test]
    fn constant_time_eq_matches_ordinary_equality() {
        assert!(constant_time_eq(b"secret", b"secret"));
        assert!(!constant_time_eq(b"secret", b"secrey"));
        assert!(!constant_time_eq(b"secret", b"secretly"));
        assert!(constant_time_eq(b"", b""));
    }
}
