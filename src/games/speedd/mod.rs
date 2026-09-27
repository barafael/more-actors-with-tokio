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

use crate::games::board::{at_style, edge, use_stepper, ChannelBox, Chip, Flight, FlightLayer};
use crate::games::palette;
use layout::position;
use model::{
    road_index, Cargo, Collector, Dispatcher, Hop, Node, Speedd, Ticket, CAMERAS, CARS,
    DISPATCHERS, QUEUE_CAPACITY, REPORTING_CAPACITY, ROADS, SUBSCRIPTION_CAPACITY,
};

/// The colour a car's plates and tickets carry through the board.
fn car_hex(car: usize) -> String {
    palette::sender_hex(car as u64 + 1)
}

fn flight(hop: &Hop) -> Flight {
    let (color, control) = match hop.cargo {
        Cargo::Plate(car) | Cargo::Ticket(car) => (car_hex(car), false),
        Cargo::Subscription => ("var(--color-blue)".to_string(), true),
        Cargo::Reply => ("var(--color-green)".to_string(), true),
    };
    Flight {
        from: position(hop.from),
        to: position(hop.to),
        label: hop.label.clone(),
        color,
        control,
        leg: hop.leg,
    }
}

const START: &str =
    "Click an actor to run one step of its loop. Start with a camera: pick a car it sees.";

#[component]
pub fn SpeeddGame() -> Element {
    let board = use_stepper(Speedd::new, START, flight);
    let sim = board.state.read();

    rsx! {
        div { class: "diagram board-game speedd-game",
            div { class: "game-header", "speedd" }
            div { class: "status", "single-player · click an actor to step it" }

            div { class: "bd-board",
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
                    chips: sim.reporting().iter().map(|r| plate_chip(r.car)).collect::<Vec<_>>(),
                    parked: sim.reporting_parked().iter().map(|r| plate_chip(r.car)).collect::<Vec<_>>(),
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
                    chips: sim.subscriptions().iter().map(|s| sub_chip(s.road, s.dispatcher)).collect::<Vec<_>>(),
                    parked: sim.subscriptions_parked().iter().map(|s| sub_chip(s.road, s.dispatcher)).collect::<Vec<_>>(),
                }

                for (r, (road, limit)) in ROADS.iter().enumerate() {
                    match sim.queue(*road) {
                        Some(tickets) => rsx! {
                            ChannelBox {
                                key: "q{r}",
                                at: position(Node::Queue(r)),
                                title: format!("mpmc · ticket · road {road} ({limit} mph)"),
                                capacity: QUEUE_CAPACITY,
                                chips: tickets.iter().map(ticket_chip).collect::<Vec<_>>(),
                                parked: sim.queue_parked(*road).iter().map(ticket_chip).collect::<Vec<_>>(),
                            }
                        },
                        None => rsx! {
                            div {
                                key: "q{r}",
                                class: "bd-channel bd-uncreated",
                                style: at_style(position(Node::Queue(r))),
                                span { class: "bd-channel-title", "road {road}: no queue yet" }
                                span { class: "bd-hint", "created by the first ticket or subscription" }
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

                FlightLayer { flights: board.flights }
            }

            div { class: "bd-note",
                span { class: "note-pill", "{board.note}" }
            }

            div { class: "game-footer",
                span { class: "dim", "the Listener and heartbeat tasks are left out · subscriptions win the Collector's select!" }
                button { class: "btn", onclick: move |_| board.reset(Speedd::new()), "reset" }
            }
        }
    }
}

fn plate_chip(car: usize) -> Chip {
    Chip {
        label: CARS[car].plate.to_string(),
        color: car_hex(car),
    }
}

fn ticket_chip(ticket: &Ticket) -> Chip {
    Chip {
        label: format!("{} {}", CARS[ticket.car].plate, ticket.speed / 100),
        color: car_hex(ticket.car),
    }
}

fn sub_chip(road: u16, dispatcher: usize) -> Chip {
    Chip {
        label: format!("{road}? · d{}", dispatcher + 1),
        color: "var(--color-blue)".to_string(),
    }
}

/// The edges of the graph, drawn once under everything.
#[component]
fn Edges() -> Element {
    rsx! {
        svg { class: "arrow-layer bd-edges", view_box: "0 0 100 100", preserve_aspect_ratio: "none",
            for i in 0..CAMERAS.len() {
                {edge(position(Node::Camera(i)), layout::REPORTING, false)}
            }
            {edge(layout::REPORTING, layout::COLLECTOR, false)}
            {edge(layout::SUBSCRIPTION, layout::COLLECTOR, true)}
            for r in 0..ROADS.len() {
                {edge(layout::COLLECTOR, position(Node::Queue(r)), false)}
            }
            for (d, roads) in DISPATCHERS.iter().enumerate() {
                for road in roads.iter() {
                    {edge(position(Node::Queue(road_index(*road))), position(Node::Dispatcher(d)), false)}
                }
                {edge(position(Node::Dispatcher(d)), layout::SUBSCRIPTION, true)}
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
            class: if stuck { "bd-actor sd-camera parked" } else { "bd-actor sd-camera" },
            style: at_style(position(Node::Camera(index))),
            div { class: "bd-head", title: "camera: road {road}, mile {mile}, limit {limit} mph", "road {road} · mile {mile}" }
            div { class: "sd-cars",
                for (car, spec) in CARS.iter().enumerate() {
                    button {
                        class: "bd-chip",
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
            "bd-actor sd-collector parked",
            format!("parked: send → road {road}"),
        ),
        Collector::Sending { road, .. } => (
            "bd-actor sd-collector held",
            format!("send → road {road} done · click"),
        ),
        Collector::Ready => ("bd-actor sd-collector", "in select!".to_string()),
    };
    rsx! {
        button {
            class,
            style: at_style(layout::COLLECTOR),
            onclick: move |e| onstep.call(e),
            div { class: "bd-head", "Collector" }
            div { class: "bd-doing", "{doing}" }
            div { class: "sd-records",
                if summary.is_empty() {
                    span { class: "bd-hint", "no sightings yet" }
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
            class: "bd-actor sd-dispatcher{class}",
            style: at_style(position(Node::Dispatcher(index))),
            onclick: move |e| onstep.call(e),
            div { class: "bd-head", "dispatcher [{names.join(\", \")}]" }
            div { class: "bd-doing", "{doing}" }
        }
    }
}
