//! End-to-end tests over the clock-driven game planes.
//!
//! `src/sim_timer.rs` proves the sims resolve correctly when a number is
//! stepped forward. These prove the rest of the stack agrees: the generic
//! ticking actor, its supervisor, the socket handler, and the JSON the
//! browser actually speaks.
//!
//! They exist because the ticking actor is the one piece of plumbing here
//! that no other game exercises — it has a `select!` branch that sleeps on a
//! deadline the sim computes, and "the actor woke itself up" is only
//! observable through a full round trip.

#![cfg(feature = "server")]

mod common;

use std::time::Duration;

use common::{
    connect, connect_as_player, connect_as_presenter, env_guard, next_event, next_within, send,
    serve, take_ticket, Socket,
};
use futures_util::StreamExt;
use more_actors_with_tokio::protocol::{
    Color, LoopSelectEvent, LoopSelectSnapshot, LoopSelectWire, SelectEvent, SelectSnapshot,
    SelectWire, TimerEvent, TimerSnapshot, TimerWire, Won,
};
use tokio_tungstenite::tungstenite::Message;

async fn timer_snapshot(socket: &mut Socket) -> TimerSnapshot {
    next_event(socket, |event: TimerEvent| match event {
        TimerEvent::Snapshot { state } => Some(state),
        _ => None,
    })
    .await
}

async fn select_snapshot(socket: &mut Socket) -> SelectSnapshot {
    next_event(socket, |event: SelectEvent| match event {
        SelectEvent::Snapshot { state } => Some(state),
        _ => None,
    })
    .await
}

async fn loop_snapshot(socket: &mut Socket) -> LoopSelectSnapshot {
    next_event(socket, |event: LoopSelectEvent| match event {
        LoopSelectEvent::Snapshot { state } => Some(state),
        _ => None,
    })
    .await
}

#[tokio::test]
async fn a_joiner_gets_the_timer_state_immediately() {
    let _guard = env_guard().await;
    let base = serve().await;
    let (_app, ticket) = take_ticket(&base).await;
    let mut socket = connect_as_player(&base, "/ws/game/timer", &ticket).await;

    let snapshot = timer_snapshot(&mut socket).await;
    assert!(!snapshot.pending, "a fresh timer is idle");
    assert_eq!(snapshot.waited_s, None);
    assert!(
        (0.0..60_000.0).contains(&snapshot.wall_ms),
        "the dial is inside a minute: {}",
        snapshot.wall_ms,
    );
}

#[tokio::test]
async fn the_timer_actor_wakes_itself_and_resolves() {
    let _guard = env_guard().await;
    let base = serve().await;
    let (_app, ticket) = take_ticket(&base).await;
    let mut socket = connect_as_player(&base, "/ws/game/timer", &ticket).await;
    timer_snapshot(&mut socket).await;

    send(&mut socket, &TimerWire::Activate).await;
    let armed = next_event(&mut socket, |event: TimerEvent| match event {
        TimerEvent::Activated { waiting_ms } => Some(waiting_ms),
        _ => None,
    })
    .await;
    assert!(
        armed > 0.0 && armed <= 10_000.0,
        "the wait is at most one period: {armed}",
    );

    // Nothing else is sent on this socket, so a Resolved can only come from
    // the actor's own timer branch firing. Allow a full period plus slack.
    let waited = next_within(
        &mut socket,
        Duration::from_millis(12_000),
        |event: TimerEvent| match event {
            TimerEvent::Resolved { waited_s } => Some(waited_s),
            _ => None,
        },
    )
    .await;
    assert!(
        waited > 0.0 && waited <= 10.0,
        "it reports what it waited: {waited}",
    );

    let snapshot = timer_snapshot(&mut socket).await;
    assert!(!snapshot.pending, "the future is no longer pending");
    assert_eq!(snapshot.waited_s, Some(waited), "the snapshot agrees");
}

#[tokio::test]
async fn dropping_the_future_stops_the_actor_from_firing() {
    let _guard = env_guard().await;
    let base = serve().await;
    let (_app, ticket) = take_ticket(&base).await;
    let mut socket = connect_as_player(&base, "/ws/game/timer", &ticket).await;
    timer_snapshot(&mut socket).await;

    send(&mut socket, &TimerWire::Activate).await;
    next_event(&mut socket, |event: TimerEvent| match event {
        TimerEvent::Activated { .. } => Some(()),
        _ => None,
    })
    .await;

    send(&mut socket, &TimerWire::Cancel).await;
    // Every wire publishes its cosmetic events and then a snapshot, so the
    // `Activate` above left a `pending` snapshot on the wire. Sync on the
    // `Cancelled` cue first; the next snapshot is the one that reflects it.
    next_event(&mut socket, |event: TimerEvent| {
        matches!(event, TimerEvent::Cancelled).then_some(())
    })
    .await;
    let snapshot = timer_snapshot(&mut socket).await;
    assert!(!snapshot.pending, "the dropped future is gone");
    assert_eq!(snapshot.remaining_ms, None);

    // And it stays gone: a dropped future never fires. One period plus
    // slack is long enough that a live deadline would have resolved, so
    // reaching the deadline with nothing seen is the assertion.
    //
    // `next_within` panics on its own timeout, which is what every other
    // test here wants, so this reads the socket directly instead.
    let quiet = tokio::time::timeout(Duration::from_millis(11_500), async {
        while let Some(Ok(Message::Text(text))) = socket.next().await {
            if let Ok(TimerEvent::Resolved { waited_s }) = serde_json::from_str(&text) {
                return waited_s;
            }
        }
        panic!("socket closed while watching a dropped future");
    })
    .await;
    assert!(
        quiet.is_err(),
        "a dropped future resolved anyway: {quiet:?}"
    );
}

#[tokio::test]
async fn a_spectator_may_watch_the_timer_but_not_drive_it() {
    let _guard = env_guard().await;
    let base = serve().await;
    // no ticket, no key: the default role
    let mut socket = connect(&base, "/ws/game/timer").await;

    let snapshot = timer_snapshot(&mut socket).await;
    assert!(!snapshot.pending);

    send(&mut socket, &TimerWire::Activate).await;
    // The refusal is silent by design, so prove it by the absence of any
    // state change: a later joiner's socket would show `pending` if the
    // command had landed.
    tokio::time::sleep(Duration::from_millis(200)).await;
    let mut watcher = connect(&base, "/ws/game/timer").await;
    let seen = timer_snapshot(&mut watcher).await;
    assert!(!seen.pending, "a spectator's activate was refused");
}

#[tokio::test]
async fn the_button_branch_wins_the_select_and_drops_the_timer() {
    let _guard = env_guard().await;
    let base = serve().await;
    let (_app, ticket) = take_ticket(&base).await;
    let mut socket = connect_as_player(&base, "/ws/game/select", &ticket).await;
    select_snapshot(&mut socket).await;

    send(&mut socket, &SelectWire::Arm).await;
    let armed = select_snapshot(&mut socket).await;
    assert!(armed.armed, "both branches are live");

    send(&mut socket, &SelectWire::Press { color: Color::Red }).await;
    let won = next_event(&mut socket, |event: SelectEvent| match event {
        SelectEvent::Won { won } => Some(won),
        _ => None,
    })
    .await;
    assert_eq!(won, Won::Button { color: Color::Red });

    let settled = select_snapshot(&mut socket).await;
    assert!(!settled.armed, "the select was left");
    assert_eq!(settled.winner, Some(Won::Button { color: Color::Red }));
}

#[tokio::test]
async fn two_clients_see_the_same_select() {
    let _guard = env_guard().await;
    let base = serve().await;
    let (_app, ticket) = take_ticket(&base).await;
    let mut player = connect_as_player(&base, "/ws/game/select", &ticket).await;
    select_snapshot(&mut player).await;

    // a second pair of eyes on the same actor, as the room has
    let mut watcher = connect(&base, "/ws/game/select").await;
    select_snapshot(&mut watcher).await;

    send(&mut player, &SelectWire::Arm).await;
    send(&mut player, &SelectWire::Press { color: Color::Blue }).await;

    let seen = next_event(&mut watcher, |event: SelectEvent| match event {
        SelectEvent::Won { won } => Some(won),
        _ => None,
    })
    .await;
    assert_eq!(
        seen,
        Won::Button { color: Color::Blue },
        "the spectator saw the same race resolve",
    );
}

#[tokio::test]
async fn the_loop_records_a_round_and_arms_the_next() {
    let _guard = env_guard().await;
    let base = serve().await;
    let (_app, ticket) = take_ticket(&base).await;
    let mut socket = connect_as_player(&base, "/ws/game/loop-select", &ticket).await;
    loop_snapshot(&mut socket).await;

    send(&mut socket, &LoopSelectWire::Start).await;
    let running = loop_snapshot(&mut socket).await;
    assert!(running.running);
    assert!(running.select.armed, "the first race is live");

    send(
        &mut socket,
        &LoopSelectWire::Press {
            color: Color::Green,
        },
    )
    .await;
    let completed = next_event(&mut socket, |event: LoopSelectEvent| match event {
        LoopSelectEvent::Completed { winner } => Some(winner),
        _ => None,
    })
    .await;
    assert_eq!(completed.round, 1);
    assert_eq!(
        completed.won,
        Won::Button {
            color: Color::Green
        }
    );

    // The loop going around again is the beat this slide exists for.
    let next = next_event(&mut socket, |event: LoopSelectEvent| match event {
        LoopSelectEvent::Snapshot { state } if state.rounds == 1 && state.select.armed => {
            Some(state)
        }
        _ => None,
    })
    .await;
    assert!(next.running, "still looping");
    assert_eq!(next.history.len(), 1, "the tape kept the round");
}

#[tokio::test]
async fn breaking_the_loop_leaves_it_inert() {
    let _guard = env_guard().await;
    let base = serve().await;
    let (_app, ticket) = take_ticket(&base).await;
    let mut socket = connect_as_player(&base, "/ws/game/loop-select", &ticket).await;
    loop_snapshot(&mut socket).await;

    send(&mut socket, &LoopSelectWire::Start).await;
    loop_snapshot(&mut socket).await;
    send(&mut socket, &LoopSelectWire::Stop).await;

    let stopped = next_event(&mut socket, |event: LoopSelectEvent| match event {
        LoopSelectEvent::Snapshot { state } if !state.running => Some(state),
        _ => None,
    })
    .await;
    assert!(!stopped.select.armed, "the pending select was dropped");

    // A press after the break decides nothing: no further round is recorded.
    send(&mut socket, &LoopSelectWire::Press { color: Color::Red }).await;
    tokio::time::sleep(Duration::from_millis(200)).await;
    let mut watcher = connect(&base, "/ws/game/loop-select").await;
    let seen = loop_snapshot(&mut watcher).await;
    assert_eq!(seen.rounds, 0, "a stopped loop records nothing");
}

#[tokio::test]
async fn restarting_a_ticking_game_closes_sockets_with_4001() {
    let _guard = env_guard().await;
    let base = serve().await;
    let (_app, ticket) = take_ticket(&base).await;
    let mut socket = connect_as_player(&base, "/ws/game/loop-select", &ticket).await;
    loop_snapshot(&mut socket).await;
    send(&mut socket, &LoopSelectWire::Start).await;
    loop_snapshot(&mut socket).await;

    let mut presenter = connect_as_presenter(&base, "/ws/app").await;
    send(
        &mut presenter,
        &more_actors_with_tokio::protocol::AppUp::Restart {
            game: more_actors_with_tokio::protocol::Game::LoopSelect,
        },
    )
    .await;

    // The client contract from CONCEPT.md: a restart closes the game socket
    // with 4001 so clients clear local state and retry.
    let code = tokio::time::timeout(Duration::from_secs(5), async {
        while let Some(frame) = socket.next().await {
            if let Ok(Message::Close(Some(frame))) = frame {
                return u16::from(frame.code);
            }
        }
        panic!("socket ended without a close frame");
    })
    .await
    .expect("close before deadline");
    assert_eq!(code, 4001, "restart closes with the agreed code");

    // The replacement actor is a fresh one: the loop it was running is gone.
    let mut rejoined = connect(&base, "/ws/game/loop-select").await;
    let fresh = loop_snapshot(&mut rejoined).await;
    assert!(!fresh.running, "the respawned actor starts stopped");
    assert_eq!(fresh.rounds, 0);
}
