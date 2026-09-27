//! The pieces every architecture board shares: nodes at percent positions,
//! channels drawn as rows of slots, edges under everything, and messages
//! flying from one node to another.
//!
//! A board's model decides what moves; this only draws it. Positions are
//! percent of the `.bd-board`, and a node is centred on its point.

use dioxus::prelude::*;

use super::step::{Step, LEG_MS};

/// How long one leg of a flight takes, in seconds; a second leg starts as
/// the first lands. Matches `.bd-hop` in main.css.
pub const LEG_S: f64 = LEG_MS / 1000.0;

/// A message to launch from one point to another.
#[derive(Clone, Debug, PartialEq)]
pub struct Flight {
    pub from: (f64, f64),
    pub to: (f64, f64),
    pub label: String,
    /// Any CSS colour, including `var(..)`.
    pub color: String,
    /// Drawn as a pill rather than a tag: control traffic, not payload.
    pub control: bool,
    /// Which leg of a step this is; later legs wait for earlier ones.
    pub leg: u8,
}

#[derive(Clone, PartialEq)]
struct Keyed {
    key: u64,
    flight: Flight,
}

/// The flights currently in the air on one board.
#[derive(Clone, Copy, PartialEq)]
pub struct Flights {
    list: Signal<Vec<Keyed>>,
    next: Signal<u64>,
}

pub fn use_flights() -> Flights {
    Flights {
        list: use_signal(Vec::new),
        next: use_signal(|| 0),
    }
}

impl Flights {
    pub fn launch(mut self, flights: impl IntoIterator<Item = Flight>) {
        for flight in flights {
            let key = (self.next)();
            self.next.set(key + 1);
            // never let a missed animationend strand a flight on the board
            #[cfg(target_arch = "wasm32")]
            {
                let mut list = self.list;
                let lifetime = ((f64::from(flight.leg) + 1.0) * LEG_S * 1000.0) as u32 + 900;
                spawn(async move {
                    gloo_timers::future::TimeoutFuture::new(lifetime).await;
                    // usually it has landed already; only then is there
                    // nothing to do, and nothing to re-render
                    if list.peek().iter().any(|k| k.key == key) {
                        list.with_mut(|l| l.retain(|k| k.key != key));
                    }
                });
            }
            self.list.with_mut(|l| l.push(Keyed { key, flight }));
        }
    }

    pub fn clear(mut self) {
        self.list.set(Vec::new());
    }

    fn land(mut self, key: u64) {
        self.list.with_mut(|l| l.retain(|k| k.key != key));
    }
}

/// A stepped board's state: its model, the flights in the air, and the
/// narration line. Every click-stepped board holds one; `H` is its model's
/// hop type, and `flight` how a hop is drawn.
pub struct Stepper<S: 'static, H: 'static> {
    pub state: Signal<S>,
    pub flights: Flights,
    pub note: Signal<String>,
    start: &'static str,
    flight: fn(&H) -> Flight,
}

impl<S, H> Clone for Stepper<S, H> {
    fn clone(&self) -> Self {
        *self
    }
}

impl<S, H> Copy for Stepper<S, H> {}

/// Mount a stepped board. `start` is the narration before the first click.
pub fn use_stepper<S, H>(
    init: impl FnOnce() -> S,
    start: &'static str,
    flight: fn(&H) -> Flight,
) -> Stepper<S, H> {
    Stepper {
        state: use_signal(init),
        flights: use_flights(),
        note: use_signal(|| start.to_string()),
        start,
        flight,
    }
}

impl<S, H> Stepper<S, H> {
    /// Run one actor step on the model and launch what it sent.
    pub fn step(mut self, run: impl FnOnce(&mut S) -> Step<H>) {
        let step = self.state.with_mut(run);
        if !step.note.is_empty() {
            self.note.set(step.note);
        }
        self.flights.launch(step.hops.iter().map(self.flight));
    }

    /// Start over with `fresh`.
    pub fn reset(mut self, fresh: S) {
        self.state.set(fresh);
        self.flights.clear();
        self.note.set(self.start.to_string());
    }
}

#[component]
pub fn FlightLayer(flights: Flights) -> Element {
    rsx! {
        for Keyed { key, flight } in flights.list.read().iter().cloned() {
            span {
                key: "{key}",
                class: if flight.control { "bd-hop control" } else { "bd-hop" },
                style: "--fx: {flight.from.0}%; --fy: {flight.from.1}%; --tx: {flight.to.0}%; --ty: {flight.to.1}%; --delay: {f64::from(flight.leg) * LEG_S}s; --c: {flight.color};",
                onanimationend: move |_| flights.land(key),
                "{flight.label}"
            }
        }
    }
}

/// Centre a node on its point.
pub fn at_style((x, y): (f64, f64)) -> String {
    format!("left: {x}%; top: {y}%;")
}

/// One edge of the graph, for the board's `svg.bd-edges`.
pub fn edge(from: (f64, f64), to: (f64, f64), dashed: bool) -> Element {
    rsx! {
        line {
            class: if dashed { "bd-edge dashed" } else { "bd-edge" },
            x1: "{from.0}", y1: "{from.1}", x2: "{to.0}", y2: "{to.1}",
        }
    }
}

/// One message drawn in a channel slot.
#[derive(Clone, Debug, PartialEq)]
pub struct Chip {
    pub label: String,
    pub color: String,
}

/// A bounded channel: its name, one slot per unit of capacity, how full it
/// is, and the sends parked on it.
#[component]
pub fn ChannelBox(
    at: (f64, f64),
    title: String,
    capacity: usize,
    chips: Vec<Chip>,
    parked: Vec<Chip>,
) -> Element {
    let full = chips.len() >= capacity;
    rsx! {
        div {
            class: if full { "bd-channel full" } else { "bd-channel" },
            style: at_style(at),
            span { class: "bd-channel-title", "{title}" }
            div { class: "bd-slots",
                for slot in 0..capacity {
                    match chips.get(slot) {
                        Some(chip) => rsx! {
                            span { class: "bd-slot filled", style: "--c: {chip.color};", "{chip.label}" }
                        },
                        None => rsx! { span { class: "bd-slot" } },
                    }
                }
                span { class: "bd-fill", "{chips.len()}/{capacity}" }
            }
            if !parked.is_empty() {
                div { class: "bd-parked",
                    span { class: "bd-hint", "parked in send()" }
                    for chip in parked.iter() {
                        span { class: "bd-slot parked", style: "--c: {chip.color};", "{chip.label}" }
                    }
                }
            }
        }
    }
}
