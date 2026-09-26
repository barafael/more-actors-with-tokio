//! The speedd minigame: a whole actor architecture, stepped by hand.
//!
//! CONCEPT.md §8, "channels determine architecture", on the Protohackers
//! Speed Daemon. Seven cameras, one Collector, three roads' ticket queues,
//! five dispatchers. Nothing moves on its own: clicking an actor runs one
//! iteration of its loop, and the messages it sent fly along the graph.
//! Every channel shows how full it is, so backpressure can be watched
//! propagating — fill a road's queue and the Collector stops, then the
//! reporting inbox fills, then the cameras park.
//!
//! Single-player in the talk as in the export: it is a board to poke at.

mod layout;
mod model;

use dioxus::prelude::*;

use crate::games::palette;
use layout::position;
use model::{
    road_index, Cargo, Collector, Dispatcher, Hop, Node, Speedd, Step, Ticket, CAMERAS, CARS,
    DISPATCHERS, QUEUE_CAPACITY, REPORTING_CAPACITY, ROADS, SUBSCRIPTION_CAPACITY,
};

/// How long one leg of a hop flies, in seconds; the next leg starts as it
/// lands. Matches `.sd-flight` in main.css.
const LEG_S: f64 = 0.6;

/// The colour a car's plates and tickets carry through the board.
fn car_hex(car: usize) -> String {
    palette::sender_hex(car as u64 + 1)
}

/// A message in the air between two nodes.
#[derive(Clone, PartialEq)]
struct Flight {
    key: u64,
    from: (f64, f64),
    to: (f64, f64),
    label: String,
    kind: &'static str,
    color: String,
    delay: f64,
}

impl Flight {
    fn new(key: u64, hop: &Hop) -> Self {
        let (kind, color) = match hop.cargo {
            Cargo::Plate(car) => ("plate", car_hex(car)),
            Cargo::Ticket(car) => ("ticket", car_hex(car)),
            Cargo::Subscription => ("sub", "var(--color-blue)".to_string()),
            Cargo::Reply => ("reply", "var(--color-green)".to_string()),
        };
        Self {
            key,
            from: position(hop.from),
            to: position(hop.to),
            label: hop.label.clone(),
            kind,
            color,
            delay: f64::from(hop.leg) * LEG_S,
        }
    }
}

#[derive(Clone, Copy)]
struct Board {
    sim: Signal<Speedd>,
    flights: Signal<Vec<Flight>>,
    next_key: Signal<u64>,
    note: Signal<String>,
}

impl Board {
    /// Run one actor step and launch what it sent.
    fn step(mut self, run: impl FnOnce(&mut Speedd) -> Step) {
        let step = self.sim.with_mut(run);
        self.note.set(step.note);
        for hop in &step.hops {
            let key = (self.next_key)();
            self.next_key.set(key + 1);
            self.flights.with_mut(|f| f.push(Flight::new(key, hop)));
            // never let a missed animationend strand a flight on the board
            #[cfg(target_arch = "wasm32")]
            {
                let mut flights = self.flights;
                let lifetime = ((f64::from(hop.leg) + 1.0) * LEG_S * 1000.0) as u32 + 900;
                spawn(async move {
                    gloo_timers::future::TimeoutFuture::new(lifetime).await;
                    flights.with_mut(|f| f.retain(|flight| flight.key != key));
                });
            }
        }
    }

    fn land(mut self, key: u64) {
        self.flights
            .with_mut(|f| f.retain(|flight| flight.key != key));
    }

    fn reset(mut self) {
        self.sim.set(Speedd::new());
        self.flights.set(Vec::new());
        self.note.set(START.to_string());
    }
}

const START: &str =
    "Click an actor to run one step of its loop. Start with a camera: pick a car it sees.";

#[component]
pub fn SpeeddGame() -> Element {
    let board = Board {
        sim: use_signal(Speedd::new),
        flights: use_signal(Vec::new),
        next_key: use_signal(|| 0u64),
        note: use_signal(|| START.to_string()),
    };
    let sim = board.sim.read();

    rsx! {
        div { class: "diagram speedd-game",
            div { class: "game-header", "speedd" }
            div { class: "status", "single-player · click an actor to step it" }

            div { class: "sd-board",
                Edges {}

                for (i, spec) in CAMERAS.iter().enumerate() {
                    CameraNode {
                        key: "{i}",
                        index: i,
                        road: spec.road,
                        mile: spec.mile,
                        limit: spec.limit,
                        stuck: sim.camera_stuck(i),
                        onpick: move |car| board.step(|s| s.click_camera(i, car)),
                    }
                }

                ChannelBox {
                    at: layout::REPORTING,
                    title: "mpsc · reporting",
                    capacity: REPORTING_CAPACITY,
                    chips: sim.reporting().iter().map(|r| Chip::car(r.car, CARS[r.car].plate)).collect::<Vec<_>>(),
                    parked: sim.reporting_parked().iter().map(|r| Chip::car(r.car, CARS[r.car].plate)).collect::<Vec<_>>(),
                }

                CollectorNode {
                    state: sim.collector(),
                    stuck: sim.collector_stuck(),
                    summary: collector_summary(&sim),
                    onstep: move |_| board.step(|s| s.click_collector()),
                }

                ChannelBox {
                    at: layout::SUBSCRIPTION,
                    title: "mpsc · dispatcher subscription",
                    capacity: SUBSCRIPTION_CAPACITY,
                    chips: sim.subscriptions().iter().map(|s| Chip::sub(s.road, s.dispatcher)).collect::<Vec<_>>(),
                    parked: sim.subscriptions_parked().iter().map(|s| Chip::sub(s.road, s.dispatcher)).collect::<Vec<_>>(),
                }

                for (r, (road, limit)) in ROADS.iter().enumerate() {
                    match sim.queue(*road) {
                        Some(tickets) => rsx! {
                            ChannelBox {
                                key: "q{r}",
                                at: position(Node::Queue(r)),
                                title: format!("mpmc · ticket · road {road} ({limit} mph)"),
                                capacity: QUEUE_CAPACITY,
                                chips: tickets.iter().map(Chip::ticket).collect::<Vec<_>>(),
                                parked: sim.queue_parked(*road).iter().map(Chip::ticket).collect::<Vec<_>>(),
                            }
                        },
                        None => rsx! {
                            div {
                                key: "q{r}",
                                class: "sd-channel sd-uncreated",
                                style: at_style(position(Node::Queue(r))),
                                span { class: "sd-channel-title", "road {road}: no queue yet" }
                                span { class: "sd-hint", "created by the first ticket or subscription" }
                            }
                        },
                    }
                }

                for (d, roads) in DISPATCHERS.iter().enumerate() {
                    DispatcherNode {
                        key: "d{d}",
                        index: d,
                        roads: roads.to_vec(),
                        state: sim.dispatcher(d),
                        stuck: sim.dispatcher_stuck(d),
                        reply_ready: sim.reply_waiting(d),
                        delivered: sim.delivered[d].len(),
                        onstep: move |_| board.step(|s| s.click_dispatcher(d)),
                    }
                }

                for flight in board.flights.read().iter().cloned() {
                    span {
                        key: "{flight.key}",
                        class: "sd-flight {flight.kind}",
                        style: "--fx: {flight.from.0}%; --fy: {flight.from.1}%; --tx: {flight.to.0}%; --ty: {flight.to.1}%; --delay: {flight.delay}s; --c: {flight.color};",
                        onanimationend: move |_| board.land(flight.key),
                        "{flight.label}"
                    }
                }
            }

            div { class: "sd-note",
                span { class: "note-pill", "{board.note}" }
            }

            div { class: "game-footer",
                span { class: "dim", "the Listener and heartbeat tasks are left out · subscriptions win the Collector's select!" }
                button { class: "btn", onclick: move |_| board.reset(), "reset" }
            }
        }
    }
}

fn at_style((x, y): (f64, f64)) -> String {
    format!("left: {x}%; top: {y}%;")
}

/// One message drawn in a channel slot.
#[derive(Clone, PartialEq)]
struct Chip {
    label: String,
    color: String,
}

impl Chip {
    fn car(car: usize, label: &str) -> Self {
        Self {
            label: label.to_string(),
            color: car_hex(car),
        }
    }

    fn ticket(ticket: &Ticket) -> Self {
        Self {
            label: format!("{} {}", CARS[ticket.car].plate, ticket.speed / 100),
            color: car_hex(ticket.car),
        }
    }

    fn sub(road: u16, dispatcher: usize) -> Self {
        Self {
            label: format!("{road}? · d{}", dispatcher + 1),
            color: "var(--color-blue)".to_string(),
        }
    }
}

/// The edges of the graph, drawn once under everything.
#[component]
fn Edges() -> Element {
    let line = |from: (f64, f64), to: (f64, f64), dashed: bool| {
        rsx! {
            line {
                class: if dashed { "sd-edge dashed" } else { "sd-edge" },
                x1: "{from.0}", y1: "{from.1}", x2: "{to.0}", y2: "{to.1}",
            }
        }
    };
    rsx! {
        svg { class: "arrow-layer sd-edges", view_box: "0 0 100 100", preserve_aspect_ratio: "none",
            for i in 0..CAMERAS.len() {
                {line(position(Node::Camera(i)), layout::REPORTING, false)}
            }
            {line(layout::REPORTING, layout::COLLECTOR, false)}
            {line(layout::SUBSCRIPTION, layout::COLLECTOR, true)}
            for r in 0..ROADS.len() {
                {line(layout::COLLECTOR, position(Node::Queue(r)), false)}
            }
            for (d, roads) in DISPATCHERS.iter().enumerate() {
                for road in roads.iter() {
                    {line(position(Node::Queue(road_index(*road))), position(Node::Dispatcher(d)), false)}
                }
                {line(position(Node::Dispatcher(d)), layout::SUBSCRIPTION, true)}
            }
        }
    }
}

#[component]
fn CameraNode(
    index: usize,
    road: u16,
    mile: u16,
    limit: u16,
    stuck: bool,
    onpick: EventHandler<usize>,
) -> Element {
    rsx! {
        div {
            class: if stuck { "sd-actor sd-camera parked" } else { "sd-actor sd-camera" },
            style: at_style(position(Node::Camera(index))),
            div { class: "sd-actor-head", title: "camera: road {road}, mile {mile}, limit {limit} mph", "road {road} · mile {mile}" }
            div { class: "sd-cars",
                for (car, spec) in CARS.iter().enumerate() {
                    button {
                        class: "sd-car",
                        style: "--c: {car_hex(car)};",
                        disabled: stuck,
                        title: "{spec.plate} at {spec.mph} mph passes this camera",
                        onclick: move |_| onpick.call(car),
                        "{spec.plate}"
                    }
                }
            }
        }
    }
}

#[component]
fn ChannelBox(
    at: (f64, f64),
    title: String,
    capacity: usize,
    chips: Vec<Chip>,
    parked: Vec<Chip>,
) -> Element {
    let full = chips.len() >= capacity;
    rsx! {
        div {
            class: if full { "sd-channel full" } else { "sd-channel" },
            style: at_style(at),
            span { class: "sd-channel-title", "{title}" }
            div { class: "sd-slots",
                for slot in 0..capacity {
                    match chips.get(slot) {
                        Some(chip) => rsx! {
                            span { class: "sd-slot filled", style: "--c: {chip.color};", "{chip.label}" }
                        },
                        None => rsx! { span { class: "sd-slot" } },
                    }
                }
                span { class: "sd-fill", "{chips.len()}/{capacity}" }
            }
            if !parked.is_empty() {
                div { class: "sd-parked",
                    span { class: "sd-hint", "parked in send()" }
                    for chip in parked.iter() {
                        span { class: "sd-slot parked", style: "--c: {chip.color};", "{chip.label}" }
                    }
                }
            }
        }
    }
}

/// The Collector's state, one line per car it has seen.
fn collector_summary(sim: &Speedd) -> Vec<String> {
    CARS.iter()
        .enumerate()
        .filter_map(|(car, spec)| {
            let sightings: Vec<String> = sim
                .records()
                .iter()
                .filter(|((c, _), _)| *c == car)
                .map(|((_, road), seen)| format!("r{road}×{}", seen.len()))
                .collect();
            if sightings.is_empty() {
                return None;
            }
            let days = sim.ticketed_days().get(&car).map_or(0, |d| d.len());
            let ticketed = if days > 0 {
                format!(" · ticketed {days}d")
            } else {
                String::new()
            };
            Some(format!("{} {}{ticketed}", spec.plate, sightings.join(" ")))
        })
        .collect()
}

#[component]
fn CollectorNode(
    state: Collector,
    stuck: bool,
    summary: Vec<String>,
    onstep: EventHandler<MouseEvent>,
) -> Element {
    let (class, doing) = match state {
        Collector::Sending { road, .. } if stuck => (
            "sd-actor sd-collector parked",
            format!("parked: send → road {road}"),
        ),
        Collector::Sending { road, .. } => (
            "sd-actor sd-collector held",
            format!("send → road {road} done · click"),
        ),
        Collector::Ready => ("sd-actor sd-collector", "in select!".to_string()),
    };
    rsx! {
        button {
            class,
            style: at_style(layout::COLLECTOR),
            onclick: move |e| onstep.call(e),
            div { class: "sd-actor-head", "Collector" }
            div { class: "sd-doing", "{doing}" }
            div { class: "sd-records",
                if summary.is_empty() {
                    span { class: "sd-hint", "no sightings yet" }
                }
                for line in summary.iter() {
                    span { "{line}" }
                }
            }
        }
    }
}

#[component]
fn DispatcherNode(
    index: usize,
    roads: Vec<u16>,
    state: Dispatcher,
    stuck: bool,
    reply_ready: bool,
    delivered: usize,
    onstep: EventHandler<MouseEvent>,
) -> Element {
    let names: Vec<String> = roads.iter().map(|r| r.to_string()).collect();
    let (class, doing) = match state {
        Dispatcher::Connecting { next } => ("", format!("subscribe to {}", roads[next])),
        Dispatcher::Subscribing { next, .. } if stuck => {
            (" parked", format!("parked: subscribe {}", roads[next]))
        }
        Dispatcher::Subscribing { next, .. } => (" held", format!("sent {} · click", roads[next])),
        Dispatcher::AwaitingReply { next } if reply_ready => {
            (" held", format!("reply for {} ready", roads[next]))
        }
        Dispatcher::AwaitingReply { next } => ("", format!("awaiting reply for {}", roads[next])),
        Dispatcher::Running => (" running", format!("{delivered} delivered")),
    };
    rsx! {
        button {
            class: "sd-actor sd-dispatcher{class}",
            style: at_style(position(Node::Dispatcher(index))),
            onclick: move |e| onstep.call(e),
            div { class: "sd-actor-head", "dispatcher [{names.join(\", \")}]" }
            div { class: "sd-doing", "{doing}" }
        }
    }
}
