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

use std::time::Duration;

use futures_util::{SinkExt, StreamExt};
use more_actors_with_tokio::protocol::{
    BroadcastError, BroadcastEvent, BroadcastWire,
};
use tokio::net::TcpListener;
use tokio_tungstenite::tungstenite::Message;

type Socket = tokio_tungstenite::WebSocketStream<tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>>;

/// Boot the game planes on an ephemeral port and return its address.
async fn serve() -> String {
    let state = more_actors_with_tokio::server::AppState::spawn();
    let app = more_actors_with_tokio::server::game_routes().with_state(state);
    let listener = TcpListener::bind("127.0.0.1:0").await.expect("bind");
    let addr = listener.local_addr().expect("addr");
    tokio::spawn(async move {
        axum::serve(listener, app).await.expect("serve");
    });
    format!("ws://{addr}")
}

async fn connect(base: &str, path: &str) -> Socket {
    let (socket, _) = tokio_tungstenite::connect_async(format!("{base}{path}"))
        .await
        .expect("connect");
    socket
}

async fn send(socket: &mut Socket, wire: &BroadcastWire) {
    let json = serde_json::to_string(wire).expect("encode");
    socket.send(Message::Text(json.into())).await.expect("send");
}

/// Read events until one decodes to something `pick` accepts.
async fn next_matching<T>(socket: &mut Socket, mut pick: impl FnMut(BroadcastEvent) -> Option<T>) -> T {
    let deadline = tokio::time::Instant::now() + Duration::from_secs(5);
    loop {
        let frame = tokio::time::timeout_at(deadline, socket.next())
            .await
            .expect("event before deadline")
            .expect("stream open")
            .expect("frame");
        let Message::Text(text) = frame else { continue };
        let Ok(event) = serde_json::from_str::<BroadcastEvent>(&text) else {
            continue;
        };
        if let Some(found) = pick(event) {
            return found;
        }
    }
}

async fn hello(socket: &mut Socket) -> u64 {
    next_matching(socket, |event| match event {
        BroadcastEvent::Hello { conn } => Some(conn),
        _ => None,
    })
    .await
}

/// A receiver that subscribes mid-stream must not be handed history.
#[tokio::test]
async fn broadcast_subscriber_starts_at_the_tail_over_the_wire() {
    let base = serve().await;

    let mut presenter = connect(&base, "/ws/game/broadcast").await;
    hello(&mut presenter).await;
    send(&mut presenter, &BroadcastWire::ClaimPresenter).await;

    // Someone has to be listening: like tokio's, this send fails with no
    // receivers attached. This early subscriber is the one that keeps the
    // channel alive while the ring fills.
    let mut early = connect(&base, "/ws/game/broadcast").await;
    hello(&mut early).await;
    send(&mut early, &BroadcastWire::Subscribe).await;
    next_matching(&mut early, |event| match event {
        BroadcastEvent::Snapshot { state } if !state.receivers.is_empty() => Some(()),
        _ => None,
    })
    .await;

    for ch in ['a', 'b', 'c'] {
        send(&mut presenter, &BroadcastWire::Send { conn: 0, ch }).await;
    }
    let snap = next_matching(&mut presenter, |event| match event {
        BroadcastEvent::Snapshot { state } if state.buffer.len() == 3 => Some(state),
        _ => None,
    })
    .await;
    assert_eq!(snap.tail, 3, "three values were accepted");

    // a phone joins now
    let mut late = connect(&base, "/ws/game/broadcast").await;
    let late_conn = hello(&mut late).await;
    send(&mut late, &BroadcastWire::Subscribe).await;

    let snap = next_matching(&mut late, |event| match event {
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
    let base = serve().await;

    let mut presenter = connect(&base, "/ws/game/broadcast").await;
    hello(&mut presenter).await;
    send(&mut presenter, &BroadcastWire::ClaimPresenter).await;

    let mut listener = connect(&base, "/ws/game/broadcast").await;
    let listener_conn = hello(&mut listener).await;
    send(&mut listener, &BroadcastWire::Subscribe).await;
    let snap = next_matching(&mut listener, |event| match event {
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
    next_matching(&mut listener, |event| match event {
        BroadcastEvent::Snapshot { state } if state.buffer.len() == 2 => Some(()),
        _ => None,
    })
    .await;

    // the presenter walks out, taking every sender handle with it
    drop(presenter);
    next_matching(&mut listener, |event| match event {
        BroadcastEvent::Snapshot { state } if state.senders.is_empty() => Some(()),
        _ => None,
    })
    .await;

    // both values are still owed to us
    send(&mut listener, &BroadcastWire::Receive { receiver }).await;
    let ch = next_matching(&mut listener, |event| match event {
        BroadcastEvent::Received { ch, .. } => Some(ch),
        _ => None,
    })
    .await;
    assert_eq!(ch, 'x');

    send(&mut listener, &BroadcastWire::Receive { receiver }).await;
    let ch = next_matching(&mut listener, |event| match event {
        BroadcastEvent::Received { ch, .. } => Some(ch),
        _ => None,
    })
    .await;
    assert_eq!(ch, 'y', "no value was lost when the channel closed");

    // only now is it closed
    send(&mut listener, &BroadcastWire::Receive { receiver }).await;
    let snap = next_matching(&mut listener, |event| match event {
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
