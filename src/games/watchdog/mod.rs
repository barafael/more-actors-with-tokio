//! The watchdog minigame: the loop-select, given an actor to live in.
//!
//! CONCEPT.md §5. Real time, for the presenter to drive live; not shared,
//! so every screen runs its own. `run()` spawns the actor; `Reset` and
//! `Stop` fly down its `mpsc` and land in its `select!`; left alone, the
//! sleep branch wins, `Expired` flies out on the oneshot, and the actor is
//! gone — sends fail from then on, until you `run()` a fresh one.

mod model;

use dioxus::prelude::*;

use crate::clock::Ticking;
use crate::games::board::{at_style, edge, use_flights, Flight, FlightLayer, Flights};
use crate::games::ticker::{subscribe, use_frame_clock, use_local_tick};
use crate::sim::now_ms;
// the crate calls its messages `Signal`, which dioxus already means
use model::{Fired, Phase, Signal as Pet, WatchdogSim, CAPACITY, TIMEOUT_MS};

const OWNER: (f64, f64) = (13.0, 50.0);
const ACTOR: (f64, f64) = (50.0, 50.0);
const EXPIRY: (f64, f64) = (88.0, 50.0);
/// Where the channel's label sits on the edge.
const CHANNEL: (f64, f64) = (25.0, 42.0);

/// How long a fired branch stays lit.
const FLASH_MS: f64 = 900.0;

const START: &str = "Watchdog::with_timeout(5s).run() spawns the actor. Then keep it fed.";

fn signal_flight(signal: Pet) -> Flight {
    Flight {
        from: OWNER,
        to: ACTOR,
        label: format!("{signal:?}"),
        color: match signal {
            Pet::Reset => "var(--color-green)".to_string(),
            Pet::Stop => "var(--color-blue)".to_string(),
        },
        control: false,
        leg: 0,
    }
}

fn expired_flight() -> Flight {
    Flight {
        from: ACTOR,
        to: EXPIRY,
        label: "Expired".to_string(),
        color: "var(--color-red)".to_string(),
        control: false,
        leg: 0,
    }
}

#[derive(Clone, Copy)]
struct Dog {
    sim: Signal<WatchdogSim>,
    flights: Flights,
    note: Signal<String>,
}

impl Dog {
    fn run(mut self) {
        self.sim.with_mut(|s| s.run(now_ms()));
        self.flights.clear();
        self.note.set(format!(
            "A fresh watchdog: its sleep started now, and it expires in {}s unless reset.",
            TIMEOUT_MS / 1000.0
        ));
    }

    fn send(mut self, signal: Pet) {
        let sent = self.sim.with_mut(|s| {
            s.sync_now(now_ms());
            s.send(signal)
        });
        if sent {
            self.flights.launch([signal_flight(signal)]);
            self.note.set(format!(
                "reset_tx.send({signal:?}) — on its way into the actor's inbox."
            ));
        } else {
            self.note.set(
                "The send fails: the watchdog has exited and its receiver is gone. Call run() for a new one."
                    .to_string(),
            );
        }
    }

    fn drop_sender(mut self) {
        self.sim.with_mut(|s| s.drop_sender());
        self.note.set(
            "drop(reset_tx): the last sender is gone. Once the inbox drains, recv() returns None and the loop exits."
                .to_string(),
        );
    }

    fn fired(mut self, fired: Fired) {
        let note = match fired {
            Fired::Recv(Pet::Reset) => {
                "recv() won: Reset — active again, and the sleep starts over."
            }
            Fired::Recv(Pet::Stop) => {
                "recv() won: Stop — active is false, so the sleep branch is switched off."
            }
            Fired::Closed => {
                "recv() returned None: no sender left. The loop breaks; the actor is done."
            }
            Fired::Sleep => {
                self.flights.launch([expired_flight()]);
                "The sleep won: Expired goes out on the oneshot, and the loop breaks."
            }
        };
        self.note.set(note.to_string());
    }
}

#[component]
pub fn WatchdogGame() -> Element {
    let dog = Dog {
        sim: use_signal(WatchdogSim::new),
        flights: use_flights(),
        note: use_signal(|| START.to_string()),
    };
    let (clock, _live) = use_frame_clock();
    use_local_tick(dog.sim, clock, move |fired| dog.fired(fired));

    let sim = dog.sim.read();
    // Redraw every frame only while something on screen moves: the
    // countdown, or a branch still lit. Once both settle the subscription
    // lapses, and an expired or stopped watchdog costs nothing.
    let flashing = sim.last.is_some_and(|(_, at)| now_ms() - at < FLASH_MS);
    if sim.remaining_ms().is_some() || flashing {
        subscribe(clock);
    }
    let lit = |branch: fn(Fired) -> bool| {
        sim.last
            .is_some_and(|(fired, at)| branch(fired) && now_ms() - at < FLASH_MS)
    };
    let recv_lit = lit(|f| matches!(f, Fired::Recv(_) | Fired::Closed));
    let sleep_lit = lit(|f| f == Fired::Sleep);
    let running = sim.phase == Phase::Running;
    let remaining = sim.remaining_ms();

    rsx! {
        div { class: "diagram board-game watchdog-game",
            div { class: "game-header", "watchdog" }
            div { class: "status", "live · your own watchdog" }

            div { class: "bd-board",
                svg { class: "arrow-layer bd-edges", view_box: "0 0 100 100", preserve_aspect_ratio: "none",
                    {edge(OWNER, ACTOR, false)}
                    {edge(ACTOR, EXPIRY, true)}
                }

                div { class: "bd-actor wd-owner", style: at_style(OWNER),
                    div { class: "bd-head", "you: the owner" }
                    if !running {
                        button { class: "btn await", onclick: move |_| dog.run(), "run()" }
                    }
                    if running {
                        button { class: "btn rcv", disabled: sim.sender_dropped(), onclick: move |_| dog.send(Pet::Reset), "send(Reset)" }
                        button { class: "btn", disabled: sim.sender_dropped(), onclick: move |_| dog.send(Pet::Stop), "send(Stop)" }
                        button { class: "btn", disabled: sim.sender_dropped(), onclick: move |_| dog.drop_sender(), "drop(reset_tx)" }
                    }
                }

                div { class: "bd-channel wd-channel", style: at_style(CHANNEL),
                    div { "mpsc({CAPACITY})" }
                    div { class: "bd-hint",
                        if sim.sender_dropped() { "sender dropped" } else { "{sim.in_channel()} queued" }
                    }
                }

                div {
                    class: match sim.phase {
                        Phase::Running => "bd-actor wd-actor running",
                        Phase::Idle => "bd-actor wd-actor idle",
                        _ => "bd-actor wd-actor gone",
                    },
                    style: at_style(ACTOR),
                    div { class: "bd-head", "Watchdog {{ duration: {TIMEOUT_MS / 1000.0}s }}" }
                    div { class: "wd-loop", "loop {{ select! {{" }
                    div { class: "race",
                        div { class: if recv_lit { "race-branch won" } else if running { "race-branch live" } else { "race-branch" },
                            div { class: "branch-head", "reset_rx.recv()" }
                            div { class: "branch-body wd-body",
                                match sim.last {
                                    Some((Fired::Recv(signal), _)) => rsx! { span { class: "wd-signal", "Some({signal:?})" } },
                                    Some((Fired::Closed, _)) => rsx! { span { class: "wd-signal", "None → break" } },
                                    _ if running => rsx! { span { class: "bd-hint", "waiting for a signal" } },
                                    _ => rsx! { span { class: "bd-hint", "—" } },
                                }
                            }
                        }
                        div { class: if sleep_lit { "race-branch won" } else if remaining.is_some() { "race-branch live" } else { "race-branch" },
                            div { class: "branch-head", "sleep, if active" }
                            div { class: "branch-body wd-body",
                                match remaining {
                                    Some(ms) => rsx! {
                                        span { class: "countdown", "{ms / 1000.0:.1}s" }
                                        div { class: "wd-bar", style: "--left: {ms / TIMEOUT_MS * 100.0}%;" }
                                    },
                                    None if running => rsx! { span { class: "bd-hint", "active == false: branch off" } },
                                    None if sim.phase == Phase::Expired => rsx! { span { class: "countdown done", "fired → break" } },
                                    None => rsx! { span { class: "bd-hint", "—" } },
                                }
                            }
                        }
                    }
                    div { class: "wd-loop", "}} }}" }
                    div { class: "bd-doing",
                        match sim.phase {
                            Phase::Idle => "not spawned",
                            Phase::Running => "running",
                            Phase::Expired => "returned: expired",
                            Phase::Exited => "returned: no senders",
                        }
                        if sim.resets > 0 { " · reset ×{sim.resets}" }
                    }
                }

                div {
                    class: if sim.phase == Phase::Expired { "bd-actor wd-expiry fired" } else { "bd-actor wd-expiry" },
                    style: at_style(EXPIRY),
                    div { class: "bd-head", "expire_rx" }
                    div { class: "bd-hint", "oneshot::Receiver<Expired>" }
                    div { class: "wd-verdict",
                        match sim.phase {
                            Phase::Expired => "Expired!",
                            Phase::Running => "pending",
                            Phase::Exited => "RecvError: sender dropped",
                            Phase::Idle => "—",
                        }
                    }
                }

                FlightLayer { flights: dog.flights }
            }

            div { class: "bd-note",
                span { class: "note-pill", "{dog.note}" }
            }
        }
    }
}
