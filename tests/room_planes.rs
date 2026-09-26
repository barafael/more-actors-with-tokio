//! End-to-end tests for the room games: the mutex and call-and-response.
//!
//! What only a full round trip shows is who a connection is and what
//! happens when it leaves: a phone that closes while holding the guard must
//! hand it on, and only the presenter may play the actor's loop.

#![cfg(feature = "server")]

mod common;

use common::{
    connect, connect_as_player, connect_as_presenter, env_guard, next_event, send, serve,
    take_ticket, Socket,
};
use more_actors_with_tokio::protocol::{
    CallEvent, CallPhase, CallSnapshot, CallWire, MutexEvent, MutexSnapshot, MutexWire, RoomEvent,
};
use serde::de::DeserializeOwned;

/// The connection id a room game's socket announces first.
async fn hello<S: DeserializeOwned>(socket: &mut Socket) -> u64 {
    next_event(socket, |event: RoomEvent<S>| match event {
        RoomEvent::Hello { conn } => Some(conn),
        RoomEvent::Snapshot { .. } => None,
    })
    .await
}

async fn mutex_until(
    socket: &mut Socket,
    mut want: impl FnMut(&MutexSnapshot) -> bool,
) -> MutexSnapshot {
    next_event(socket, |event: MutexEvent| match event {
        MutexEvent::Snapshot { state } if want(&state) => Some(state),
        _ => None,
    })
    .await
}

/// The hook's whole premise, over the wire: first come holds, the next one
/// parks, and a holder who walks away drops the guard.
#[tokio::test]
async fn a_departing_holder_hands_the_mutex_to_the_next_in_line() {
    let _guard = env_guard().await;
    let base = serve().await;

    let (_seat_a, ticket_a) = take_ticket(&base).await;
    let (_seat_b, ticket_b) = take_ticket(&base).await;
    let mut a = connect_as_player(&base, "/ws/game/mutex", &ticket_a).await;
    let a_conn = hello::<MutexSnapshot>(&mut a).await;
    let mut b = connect_as_player(&base, "/ws/game/mutex", &ticket_b).await;
    let b_conn = hello::<MutexSnapshot>(&mut b).await;

    send(&mut a, &MutexWire::Lock).await;
    mutex_until(&mut b, |s| s.holder.map(|h| h.task) == Some(a_conn)).await;
    send(&mut b, &MutexWire::Lock).await;
    send(&mut b, &MutexWire::Increment).await;
    let snap = mutex_until(&mut b, |s| !s.waiters.is_empty()).await;
    assert_eq!(snap.waiters[0].task, b_conn);
    assert_eq!(snap.value, 0, "a waiter cannot touch the value");

    a.close(None).await.expect("close");
    drop(a);
    let snap = mutex_until(&mut b, |s| s.holder.map(|h| h.task) == Some(b_conn)).await;
    assert!(snap.waiters.is_empty());
    assert!(
        snap.idle.iter().all(|t| t.task != a_conn),
        "the departed task is gone"
    );
}

#[tokio::test]
async fn a_spectator_watches_the_mutex_without_a_task() {
    let _guard = env_guard().await;
    let base = serve().await;

    let mut spectator = connect(&base, "/ws/game/mutex").await;
    hello::<MutexSnapshot>(&mut spectator).await;
    send(&mut spectator, &MutexWire::Lock).await;

    let (_seat, ticket) = take_ticket(&base).await;
    let mut player = connect_as_player(&base, "/ws/game/mutex", &ticket).await;
    let player_conn = hello::<MutexSnapshot>(&mut player).await;
    let snap = mutex_until(&mut spectator, |s| !s.idle.is_empty()).await;
    assert_eq!(snap.holder, None, "the spectator's lock went nowhere");
    assert_eq!(snap.idle.len(), 1);
    assert_eq!(snap.idle[0].task, player_conn);
}

/// Players ask; only the presenter answers.
#[tokio::test]
async fn only_the_presenter_runs_the_actor_loop() {
    let _guard = env_guard().await;
    let base = serve().await;

    let (_seat, ticket) = take_ticket(&base).await;
    let mut player = connect_as_player(&base, "/ws/game/call", &ticket).await;
    let me = hello::<CallSnapshot>(&mut player).await;
    let mut presenter = connect_as_presenter(&base, "/ws/game/call").await;
    hello::<CallSnapshot>(&mut presenter).await;

    send(&mut player, &CallWire::Request).await;
    // a player playing the actor is refused
    send(&mut player, &CallWire::Recv).await;
    send(&mut player, &CallWire::Reply).await;
    let snap = next_event(&mut presenter, |event: CallEvent| match event {
        CallEvent::Snapshot { state } if !state.queue.is_empty() => Some(state),
        _ => None,
    })
    .await;
    assert!(snap.in_hand.is_none());
    assert!(
        snap.tasks.iter().all(|t| t.task == me),
        "the presenter is the actor, not a requester"
    );

    send(&mut presenter, &CallWire::Recv).await;
    send(&mut presenter, &CallWire::Reply).await;
    let phase = next_event(&mut player, |event: CallEvent| match event {
        CallEvent::Snapshot { state } => state
            .tasks
            .iter()
            .find(|t| t.task == me)
            .map(|t| t.phase)
            .filter(|phase| *phase == CallPhase::Got(0)),
        CallEvent::Hello { .. } => None,
    })
    .await;
    assert_eq!(phase, CallPhase::Got(0));
}

/// A requester who leaves drops its receiver; the message it sent stays
/// behind, and answering it fails.
#[tokio::test]
async fn answering_a_departed_requester_fails() {
    let _guard = env_guard().await;
    let base = serve().await;

    let (_seat, ticket) = take_ticket(&base).await;
    let mut player = connect_as_player(&base, "/ws/game/call", &ticket).await;
    hello::<CallSnapshot>(&mut player).await;
    let mut presenter = connect_as_presenter(&base, "/ws/game/call").await;
    hello::<CallSnapshot>(&mut presenter).await;

    send(&mut player, &CallWire::Request).await;
    next_event(&mut presenter, |event: CallEvent| match event {
        CallEvent::Snapshot { state } if !state.queue.is_empty() => Some(()),
        _ => None,
    })
    .await;
    player.close(None).await.expect("close");
    drop(player);
    next_event(&mut presenter, |event: CallEvent| match event {
        CallEvent::Snapshot { state } if state.tasks.is_empty() => Some(()),
        _ => None,
    })
    .await;

    send(&mut presenter, &CallWire::Recv).await;
    send(&mut presenter, &CallWire::Reply).await;
    let last = next_event(&mut presenter, |event: CallEvent| match event {
        CallEvent::Snapshot { state } => state.last,
        CallEvent::Hello { .. } => None,
    })
    .await;
    assert!(
        matches!(
            last,
            more_actors_with_tokio::protocol::CallOutcome::Unheard { id: 0, .. }
        ),
        "got {last:?}"
    );
}
