//! Shared scaffolding for the websocket-plane integration tests.
//!
//! Both test binaries stand up the real game planes on an ephemeral port and
//! talk to them over real websockets, so the connect/ticket/read helpers are
//! the same. They live here rather than being copied because one of them —
//! [`env_guard`] — guards a *process-wide* resource, and a per-file copy of a
//! mutex documents an invariant it cannot enforce.

#![allow(dead_code)] // each test binary uses a subset

use std::time::Duration;

use futures_util::{SinkExt, StreamExt};
use more_actors_with_tokio::protocol::{AppDown, Role, PRESENTER_PARAM, TICKET_PARAM};
use more_actors_with_tokio::server::auth::PRESENTER_KEY_ENV;
use serde::de::DeserializeOwned;
use serde::Serialize;
use tokio::net::TcpListener;
use tokio_tungstenite::tungstenite::Message;

pub type Socket =
    tokio_tungstenite::WebSocketStream<tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>>;

/// The presenter secret these tests present.
pub const TEST_KEY: &str = "test-presenter-key";

/// The `Host` these tests present.
///
/// Loopback connections are trusted as presenters without a key (see
/// `auth::is_loopback_host`), which is right for a laptop but would make
/// every socket here a presenter and collapse the roles these tests exist to
/// check. An audience phone reaches the deck by its public hostname, so that
/// is what is modelled.
pub const AUDIENCE_HOST: &str = "deck.example.test";

/// Serialises every test against the process-wide `PRESENTER_KEY`.
///
/// The variable is global but cargo runs tests in threads, and one of them
/// must clear it to prove the server generates its own. Scoping the lock to
/// just the write is not enough — the clear would still land while another
/// test's server is mid-handshake — so every test holds this for its whole
/// body and they run one at a time.
///
/// A tokio mutex rather than a std one: the guard is held across every await
/// in a test body, which is exactly what a blocking mutex must not do. It
/// also does not poison, so one failing test cannot cascade.
static ENV: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

pub async fn env_guard() -> tokio::sync::MutexGuard<'static, ()> {
    ENV.lock().await
}

/// Boot the game planes on an ephemeral port and return its `ws://` base.
pub async fn serve() -> String {
    std::env::set_var(PRESENTER_KEY_ENV, TEST_KEY);
    let state = more_actors_with_tokio::server::AppState::spawn();
    let app = more_actors_with_tokio::server::game_routes().with_state(state);
    let listener = TcpListener::bind("127.0.0.1:0").await.expect("bind");
    let addr = listener.local_addr().expect("addr");
    tokio::spawn(async move {
        axum::serve(listener, app).await.expect("serve");
    });
    format!("ws://{addr}")
}

pub async fn connect(base: &str, path: &str) -> Socket {
    use tokio_tungstenite::tungstenite::client::IntoClientRequest;

    let mut request = format!("{base}{path}")
        .into_client_request()
        .expect("build request");
    request
        .headers_mut()
        .insert("host", AUDIENCE_HOST.parse().expect("host header is valid"));
    let (socket, _) = tokio_tungstenite::connect_async(request)
        .await
        .expect("connect");
    socket
}

/// Take a player ticket from the pool, the way a phone that scanned the QR
/// code does: open the app plane and keep the socket for the seat's life.
///
/// The returned socket must stay alive — dropping it releases the hold.
pub async fn take_ticket(base: &str) -> (Socket, String) {
    let mut app = connect(base, "/ws/app").await;
    let frame = tokio::time::timeout(Duration::from_secs(5), app.next())
        .await
        .expect("app payload before deadline")
        .expect("stream open")
        .expect("frame");
    let Message::Text(text) = frame else {
        panic!("expected a text frame, got {frame:?}");
    };
    let down: AppDown = serde_json::from_str(&text).expect("decode AppDown");
    assert_eq!(down.role, Role::Player, "the pool should have seats free");
    let ticket = down.granted_ticket.expect("a seat was granted");
    (app, ticket)
}

/// Connect to a game socket as a ticket-holding player.
pub async fn connect_as_player(base: &str, path: &str, ticket: &str) -> Socket {
    connect(base, &format!("{path}?{TICKET_PARAM}={ticket}")).await
}

/// Connect to a game socket with the presenter key.
pub async fn connect_as_presenter(base: &str, path: &str) -> Socket {
    connect(base, &format!("{path}?{PRESENTER_PARAM}={TEST_KEY}")).await
}

/// Send any wire type as JSON.
pub async fn send<W: Serialize>(socket: &mut Socket, wire: &W) {
    let json = serde_json::to_string(wire).expect("encode");
    socket.send(Message::Text(json.into())).await.expect("send");
}

/// Read events until one decodes to something `pick` accepts, or `timeout`
/// elapses (which panics).
pub async fn next_within<E: DeserializeOwned, T>(
    socket: &mut Socket,
    timeout: Duration,
    mut pick: impl FnMut(E) -> Option<T>,
) -> T {
    let deadline = tokio::time::Instant::now() + timeout;
    loop {
        let frame = tokio::time::timeout_at(deadline, socket.next())
            .await
            .expect("event before deadline")
            .expect("stream open")
            .expect("frame");
        let Message::Text(text) = frame else { continue };
        let Ok(event) = serde_json::from_str::<E>(&text) else {
            continue;
        };
        if let Some(found) = pick(event) {
            return found;
        }
    }
}

/// [`next_within`] with the default five-second budget.
pub async fn next_event<E: DeserializeOwned, T>(
    socket: &mut Socket,
    pick: impl FnMut(E) -> Option<T>,
) -> T {
    next_within(socket, Duration::from_secs(5), pick).await
}
