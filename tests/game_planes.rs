//! End-to-end tests over the real websocket planes.
//!
//! The unit tests in `src/sim.rs` prove the drivers agree with the channel
//! cores. These prove the rest of the stack agrees with the drivers: axum
//! state, the supervisor, the socket handlers, and the JSON protocol the
//! browser actually speaks.
//!
//! They exist because the two beats this release changed — a broadcast
//! subscriber starting at the tail, and buffered values surviving closure —
//! are only observable through a full round trip.

#![cfg(feature = "server")]

mod common;

use std::time::Duration;

use common::{
    connect, connect_as_player, connect_as_presenter, env_guard, next_event, send, serve,
    take_ticket, Socket,
};
use futures_util::{SinkExt, StreamExt};
use more_actors_with_tokio::protocol::{
    AppDown, BroadcastError, BroadcastEvent, BroadcastWire, MpscEvent, MpscWire, Role, KEEPALIVE,
    PRESENTER_PARAM,
};
use more_actors_with_tokio::server::auth::PRESENTER_KEY_ENV;
use tokio::net::TcpListener;
use tokio_tungstenite::tungstenite::Message;

async fn mpsc_hello(socket: &mut Socket) -> u64 {
    next_event(socket, |event: MpscEvent| match event {
        MpscEvent::Hello { conn } => Some(conn),
        _ => None,
    })
    .await
}

async fn hello(socket: &mut Socket) -> u64 {
    next_event(socket, |event: BroadcastEvent| match event {
        BroadcastEvent::Hello { conn } => Some(conn),
        _ => None,
    })
    .await
}

/// A receiver that subscribes mid-stream must not be handed history.
#[tokio::test]
async fn broadcast_subscriber_starts_at_the_tail_over_the_wire() {
    let _guard = env_guard().await;
    let base = serve().await;

    let mut presenter = connect_as_presenter(&base, "/ws/game/broadcast").await;
    hello(&mut presenter).await;
    send(&mut presenter, &BroadcastWire::ClaimPresenter).await;

    // Someone has to be listening: like tokio's, this send fails with no
    // receivers attached. This early subscriber is the one that keeps the
    // channel alive while the ring fills.
    let (_early_seat, early_ticket) = take_ticket(&base).await;
    let mut early = connect_as_player(&base, "/ws/game/broadcast", &early_ticket).await;
    hello(&mut early).await;
    send(&mut early, &BroadcastWire::Subscribe).await;
    next_event(&mut early, |event| match event {
        BroadcastEvent::Snapshot { state } if !state.receivers.is_empty() => Some(()),
        _ => None,
    })
    .await;

    for ch in ['a', 'b', 'c'] {
        send(&mut presenter, &BroadcastWire::Send { conn: 0, ch }).await;
    }
    let snap = next_event(&mut presenter, |event| match event {
        BroadcastEvent::Snapshot { state } if state.buffer.len() == 3 => Some(state),
        _ => None,
    })
    .await;
    assert_eq!(snap.tail, 3, "three values were accepted");

    // a phone joins now
    let (_late_seat, late_ticket) = take_ticket(&base).await;
    let mut late = connect_as_player(&base, "/ws/game/broadcast", &late_ticket).await;
    let late_conn = hello(&mut late).await;
    send(&mut late, &BroadcastWire::Subscribe).await;

    let snap = next_event(&mut late, |event| match event {
        BroadcastEvent::Snapshot { state }
            if state.receivers.iter().any(|r| r.owner == late_conn) =>
        {
            Some(state)
        }
        _ => None,
    })
    .await;
    let rx = snap
        .receivers
        .iter()
        .find(|r| r.owner == late_conn)
        .expect("our receiver");
    assert_eq!(
        rx.next,
        snap.tail + 1,
        "a fresh subscriber is positioned past every buffered value"
    );
    assert!(
        snap.buffer.len() == 3,
        "the history is still in the ring, just not owed to this receiver"
    );
}

/// Values still in the ring stay receivable after the last sender leaves;
/// only once they are drained does the receiver see Closed.
#[tokio::test]
async fn buffered_values_outlive_the_last_sender_over_the_wire() {
    let _guard = env_guard().await;
    let base = serve().await;

    let mut presenter = connect_as_presenter(&base, "/ws/game/broadcast").await;
    hello(&mut presenter).await;
    send(&mut presenter, &BroadcastWire::ClaimPresenter).await;

    let (_listener_seat, listener_ticket) = take_ticket(&base).await;
    let mut listener = connect_as_player(&base, "/ws/game/broadcast", &listener_ticket).await;
    let listener_conn = hello(&mut listener).await;
    send(&mut listener, &BroadcastWire::Subscribe).await;
    let snap = next_event(&mut listener, |event| match event {
        BroadcastEvent::Snapshot { state }
            if state.receivers.iter().any(|r| r.owner == listener_conn) =>
        {
            Some(state)
        }
        _ => None,
    })
    .await;
    let receiver = snap
        .receivers
        .iter()
        .find(|r| r.owner == listener_conn)
        .expect("our receiver")
        .receiver;

    for ch in ['x', 'y'] {
        send(&mut presenter, &BroadcastWire::Send { conn: 0, ch }).await;
    }
    next_event(&mut listener, |event| match event {
        BroadcastEvent::Snapshot { state } if state.buffer.len() == 2 => Some(()),
        _ => None,
    })
    .await;

    // the presenter walks out, taking every sender handle with it
    drop(presenter);
    next_event(&mut listener, |event| match event {
        BroadcastEvent::Snapshot { state } if state.senders.is_empty() => Some(()),
        _ => None,
    })
    .await;

    // both values are still owed to us
    send(&mut listener, &BroadcastWire::Receive { receiver }).await;
    let ch = next_event(&mut listener, |event| match event {
        BroadcastEvent::Received { ch, .. } => Some(ch),
        _ => None,
    })
    .await;
    assert_eq!(ch, 'x');

    send(&mut listener, &BroadcastWire::Receive { receiver }).await;
    let ch = next_event(&mut listener, |event| match event {
        BroadcastEvent::Received { ch, .. } => Some(ch),
        _ => None,
    })
    .await;
    assert_eq!(ch, 'y', "no value was lost when the channel closed");

    // only now is it closed
    send(&mut listener, &BroadcastWire::Receive { receiver }).await;
    let snap = next_event(&mut listener, |event| match event {
        BroadcastEvent::Snapshot { state }
            if state
                .receivers
                .iter()
                .any(|r| r.error == Some(BroadcastError::Closed)) =>
        {
            Some(state)
        }
        _ => None,
    })
    .await;
    assert!(snap.senders.is_empty());
}

/// A spectator — anyone who opened the URL without scanning — may watch but
/// must not be able to act.
///
/// mpsc is the honest game to prove this in: every connection is registered
/// as its own sender, so without the role gate a spectator's send would be
/// accepted by the sim as perfectly legitimate. (In broadcast the sim
/// already rejects it for lacking a handle, which would make this test pass
/// for the wrong reason.)
#[tokio::test]
async fn a_spectator_cannot_send() {
    let _guard = env_guard().await;
    let base = serve().await;

    let (_seat, ticket) = take_ticket(&base).await;
    let mut player = connect_as_player(&base, "/ws/game/mpsc", &ticket).await;
    let player_conn = mpsc_hello(&mut player).await;

    // no ticket, no key
    let mut spectator = connect(&base, "/ws/game/mpsc").await;
    let spectator_conn = mpsc_hello(&mut spectator).await;
    send(
        &mut spectator,
        &MpscWire::Send {
            conn: spectator_conn,
            ch: 'x',
        },
    )
    .await;

    // The player's send is the barrier, but only once it has *landed*:
    // mpsc values spend MPSC_FLIGHT_MS in the air, so a snapshot taken
    // while either value is still in flight proves nothing.
    send(
        &mut player,
        &MpscWire::Send {
            conn: player_conn,
            ch: 'p',
        },
    )
    .await;

    let snap = next_event(&mut player, |event| match event {
        MpscEvent::Snapshot { state } if state.in_flight == 0 && !state.buffer.is_empty() => {
            Some(state)
        }
        _ => None,
    })
    .await;

    let sent: Vec<char> = snap.buffer.iter().map(|value| value.ch).collect();
    assert_eq!(
        sent,
        vec!['p'],
        "only the player's value should have landed",
    );
}

/// The presenter slot is the server's to grant. A player asking for it is
/// refused, however convincing their client-side `is_desktop()` looks.
#[tokio::test]
async fn a_player_cannot_seize_the_presenter_slot() {
    let _guard = env_guard().await;
    let base = serve().await;

    let mut presenter = connect_as_presenter(&base, "/ws/game/broadcast").await;
    let presenter_conn = hello(&mut presenter).await;
    send(&mut presenter, &BroadcastWire::ClaimPresenter).await;
    next_event(&mut presenter, |event| match event {
        BroadcastEvent::Snapshot { state } if state.presenter == Some(presenter_conn) => Some(()),
        _ => None,
    })
    .await;

    let (_seat, ticket) = take_ticket(&base).await;
    let mut player = connect_as_player(&base, "/ws/game/broadcast", &ticket).await;
    let player_conn = hello(&mut player).await;
    send(&mut player, &BroadcastWire::ClaimPresenter).await;

    // Subscribe is allowed for a player, so its effect is the barrier: once
    // the receiver shows up, the claim ahead of it has been processed.
    send(&mut player, &BroadcastWire::Subscribe).await;
    let snap = next_event(&mut player, |event| match event {
        BroadcastEvent::Snapshot { state } if !state.receivers.is_empty() => Some(state),
        _ => None,
    })
    .await;

    assert_eq!(
        snap.presenter,
        Some(presenter_conn),
        "the slot stayed with the real presenter, not {player_conn}",
    );
}

/// The pool is finite, and running out is a normal thing for a full room.
#[tokio::test]
async fn the_room_runs_out_of_tickets() {
    let _guard = env_guard().await;
    use more_actors_with_tokio::protocol::PLAYER_TICKETS;

    let base = serve().await;

    // hold every seat: the sockets must stay alive to keep the holds
    let mut held = Vec::new();
    for _ in 0..PLAYER_TICKETS {
        held.push(take_ticket(&base).await);
    }

    let mut latecomer = connect(&base, "/ws/app").await;
    let frame = tokio::time::timeout(Duration::from_secs(5), latecomer.next())
        .await
        .expect("app payload before deadline")
        .expect("stream open")
        .expect("frame");
    let Message::Text(text) = frame else {
        panic!("expected a text frame, got {frame:?}");
    };
    let down: AppDown = serde_json::from_str(&text).expect("decode AppDown");

    assert_eq!(down.role, Role::Spectator, "every seat was taken");
    assert_eq!(down.granted_ticket, None);
    assert_eq!(down.players_present, PLAYER_TICKETS);
    assert_eq!(down.players_capacity, PLAYER_TICKETS);
}

/// The keep-alive must not look like a malformed command.
///
/// Every client sends one every 3 seconds, so if the server treated it as
/// an undecodable message the logs would fill with decode errors and bury
/// anything real. The socket must also stay open.
#[tokio::test]
async fn the_keepalive_is_not_a_protocol_error() {
    let _guard = env_guard().await;
    let base = serve().await;

    let mut app = connect_as_presenter(&base, "/ws/app").await;
    let first = tokio::time::timeout(Duration::from_secs(5), app.next())
        .await
        .expect("app payload before deadline")
        .expect("stream open")
        .expect("frame");
    let Message::Text(text) = first else {
        panic!("expected a text frame, got {first:?}");
    };
    let before: AppDown = serde_json::from_str(&text).expect("decode AppDown");

    for _ in 0..3 {
        app.send(Message::Text(KEEPALIVE.into()))
            .await
            .expect("send keepalive");
    }

    // The socket is still usable: a real command after the pings works, and
    // its reply proves the pings neither closed nor desynchronised it.
    app.send(Message::Text(
        serde_json::to_string("AdvanceSlide")
            .expect("encode")
            .into(),
    ))
    .await
    .expect("send command");

    let frame = tokio::time::timeout(Duration::from_secs(5), app.next())
        .await
        .expect("payload before deadline")
        .expect("stream open")
        .expect("frame");
    let Message::Text(text) = frame else {
        panic!("expected a text frame, got {frame:?}");
    };
    let after: AppDown = serde_json::from_str(&text).expect("decode AppDown");
    assert_eq!(after.slide, before.slide + 1, "the command still landed");
}

/// A generated presenter key must actually work.
///
/// `presenter_key()` caches on first read, so an announcement that reads
/// before it writes pins `None` and nobody can ever present — a failure
/// that is invisible until someone tries to advance a slide on stage.
#[tokio::test]
async fn a_generated_presenter_key_can_present() {
    // Held for the whole test: clearing the variable would otherwise strip
    // the key from every other test's server while they are mid-handshake.
    let _guard = env_guard().await;

    // no PRESENTER_KEY in the environment: the server must generate one
    std::env::remove_var(PRESENTER_KEY_ENV);
    more_actors_with_tokio::server::announce_presenter_key();

    let key = std::env::var(PRESENTER_KEY_ENV).expect("a key was generated");
    assert!(!key.is_empty());

    let state = more_actors_with_tokio::server::AppState::spawn();
    let app = more_actors_with_tokio::server::game_routes().with_state(state);
    let listener = TcpListener::bind("127.0.0.1:0").await.expect("bind");
    let addr = listener.local_addr().expect("addr");
    tokio::spawn(async move {
        axum::serve(listener, app).await.expect("serve");
    });
    let base = format!("ws://{addr}");

    let mut app = connect(&base, &format!("/ws/app?{PRESENTER_PARAM}={key}")).await;
    let frame = tokio::time::timeout(Duration::from_secs(5), app.next())
        .await
        .expect("payload before deadline")
        .expect("stream open")
        .expect("frame");
    let Message::Text(text) = frame else {
        panic!("expected a text frame, got {frame:?}");
    };
    let down: AppDown = serde_json::from_str(&text).expect("decode AppDown");
    assert_eq!(down.role, Role::Presenter, "the generated key must present");
}

/// Someone at the machine running the server presents without a key.
///
/// They already have the terminal, so a secret adds nothing and costs the
/// common case: `dx serve`, open the page, and find a deck whose arrow
/// keys do nothing. The audience is unaffected — they arrive by hostname,
/// which is what every other test here models.
#[tokio::test]
async fn loopback_presents_without_a_key() {
    let _guard = env_guard().await;
    let base = serve().await;

    // no `?k=`, and the default Host that tungstenite derives from the URL
    let (socket, _) = tokio_tungstenite::connect_async(format!("{base}/ws/app"))
        .await
        .expect("connect");
    let mut socket = socket;

    let frame = tokio::time::timeout(Duration::from_secs(5), socket.next())
        .await
        .expect("payload before deadline")
        .expect("stream open")
        .expect("frame");
    let Message::Text(text) = frame else {
        panic!("expected a text frame, got {frame:?}");
    };
    let down: AppDown = serde_json::from_str(&text).expect("decode AppDown");
    assert_eq!(down.role, Role::Presenter, "localhost presents");
}
