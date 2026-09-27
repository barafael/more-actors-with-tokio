//! The pipeline minigame: `halres-downloader`, stepped by hand.
//!
//! Reader → downloader → processor → collector, three bounded channels
//! between them, and in each stage a `FuturesUnordered` of jobs you
//! complete yourself — so the output order is whatever order you finish
//! them in. Nothing runs on its own: click the reader to read a line, a
//! stage to run one turn of its `select!`, a job to let the network answer,
//! the collector to collect. Read past the end of the file and watch
//! shutdown walk down the pipeline without anyone being told to stop.
//!
//! Single-player, in the talk as in the export.

mod model;

use dioxus::prelude::*;

use crate::games::board::{at_style, edge, use_stepper, ChannelBox, Chip, Flight, FlightLayer};
use crate::games::palette;
use model::{
    Hop, Node, Pipeline, Stage, StageId, CHANNEL_SIZE, DOWNLOAD_LIMIT, PROCESS_LIMIT, RECORDS,
};

const Y: f64 = 42.0;

fn position(node: Node) -> (f64, f64) {
    match node {
        Node::Reader => (8.0, Y),
        Node::Pages => (21.5, Y),
        Node::Downloader => (36.0, Y),
        Node::Responses => (50.5, Y),
        Node::Processor => (64.0, Y),
        Node::Resources => (77.5, Y),
        Node::Collector => (91.5, Y),
        Node::Dropped => (36.0, 84.0),
    }
}

fn record_hex(id: usize) -> String {
    palette::sender_hex(id as u64 + 1)
}

fn chip(id: usize) -> Chip {
    Chip {
        label: RECORDS[id].short.to_string(),
        color: record_hex(id),
    }
}

fn flight(hop: &Hop) -> Flight {
    Flight {
        from: position(hop.from),
        to: position(hop.to),
        label: if hop.failed {
            format!("{} ✗", RECORDS[hop.id].short)
        } else {
            RECORDS[hop.id].short.to_string()
        },
        color: if hop.failed {
            "var(--color-red)".to_string()
        } else {
            record_hex(hop.id)
        },
        control: hop.failed,
        leg: hop.leg,
    }
}

const START: &str =
    "Click the reader to read a line of urls.csv, then a stage to run one turn of its select!.";

#[component]
pub fn PipelineGame() -> Element {
    let board = use_stepper(Pipeline::new, START, flight);
    let pipe = board.state.read();
    let channel = |node: Node, name: &str| {
        let (queued, parked, closed) = pipe.channel(node);
        let title = if closed {
            format!("{name} · closed")
        } else {
            format!("mpsc · {name}")
        };
        (
            title,
            queued.into_iter().map(chip).collect::<Vec<_>>(),
            parked.into_iter().map(chip).collect::<Vec<_>>(),
        )
    };
    let (pages_title, pages, pages_parked) = channel(Node::Pages, "pages");
    let (responses_title, responses, responses_parked) = channel(Node::Responses, "responses");
    let (resources_title, resources, resources_parked) = channel(Node::Resources, "resources");

    rsx! {
        div { class: "diagram board-game pipeline-game",
            div { class: "game-header", "halres-downloader" }
            div { class: "status", "single-player · click an actor to step it" }

            div { class: "bd-board",
                svg { class: "arrow-layer bd-edges", view_box: "0 0 100 100", preserve_aspect_ratio: "none",
                    {edge(position(Node::Reader), position(Node::Collector), false)}
                    {edge(position(Node::Downloader), position(Node::Dropped), true)}
                }

                button {
                    class: if pipe.reader_done { "bd-actor pl-reader gone" } else if pipe.reader_stuck() { "bd-actor pl-reader parked" } else { "bd-actor pl-reader" },
                    style: at_style(position(Node::Reader)),
                    onclick: move |_| board.step(|p| p.click_reader()),
                    div { class: "bd-head", "reader · urls.csv" }
                    div { class: "pl-records",
                        for id in pipe.remaining_records() {
                            span { class: "bd-record", style: "--c: {record_hex(id)};", "{RECORDS[id].host}" }
                        }
                        if pipe.remaining_records().is_empty() && !pipe.reader_done {
                            span { class: "bd-hint", "end of file: next, drop(pages_tx)" }
                        }
                    }
                    div { class: "bd-doing",
                        if pipe.reader_done { "sender dropped" } else if pipe.reader_stuck() { "parked: pages full" } else { "for record in csv" }
                    }
                }

                ChannelBox { at: position(Node::Pages), title: pages_title, capacity: CHANNEL_SIZE, chips: pages, parked: pages_parked }

                StageNode {
                    id: StageId::Download,
                    stage: pipe.stage(StageId::Download).clone(),
                    stuck: pipe.stage_stuck(StageId::Download),
                    onstep: move |_| board.step(|p| p.click_stage(StageId::Download)),
                    onjob: move |id| board.step(|p| p.click_job(StageId::Download, id)),
                }

                ChannelBox { at: position(Node::Responses), title: responses_title, capacity: CHANNEL_SIZE, chips: responses, parked: responses_parked }

                StageNode {
                    id: StageId::Process,
                    stage: pipe.stage(StageId::Process).clone(),
                    stuck: pipe.stage_stuck(StageId::Process),
                    onstep: move |_| board.step(|p| p.click_stage(StageId::Process)),
                    onjob: move |id| board.step(|p| p.click_job(StageId::Process, id)),
                }

                ChannelBox { at: position(Node::Resources), title: resources_title, capacity: CHANNEL_SIZE, chips: resources, parked: resources_parked }

                button {
                    class: if pipe.collector_done { "bd-actor pl-collector gone" } else { "bd-actor pl-collector" },
                    style: at_style(position(Node::Collector)),
                    onclick: move |_| board.step(|p| p.click_collector()),
                    div { class: "bd-head", "collector" }
                    div { class: "pl-records",
                        if pipe.collected.is_empty() {
                            span { class: "bd-hint", "Vec::new()" }
                        }
                        for (n, id) in pipe.collected.iter().enumerate() {
                            span { class: "bd-record", style: "--c: {record_hex(*id)};",
                                "{n + 1}. {RECORDS[*id].title.unwrap_or_default()}"
                                if RECORDS[*id].title == Some("") { "(no <title>: {RECORDS[*id].host})" }
                            }
                        }
                    }
                    div { class: "bd-doing",
                        if pipe.collector_done { "returned → resources.json" } else { "while let Some(..)" }
                    }
                }

                div { class: "bd-channel pl-dropped", style: at_style(position(Node::Dropped)),
                    span { class: "bd-channel-title", "warn!: failed, dropped" }
                    div { class: "bd-slots",
                        if pipe.dropped.is_empty() {
                            span { class: "bd-hint", "nothing yet" }
                        }
                        for id in pipe.dropped.iter() {
                            span { class: "bd-slot parked", style: "--c: var(--color-red);", "{RECORDS[*id].short} ✗" }
                        }
                    }
                }

                FlightLayer { flights: board.flights }
            }

            div { class: "bd-note",
                span { class: "note-pill", "{board.note}" }
            }

            div { class: "game-footer",
                span { class: "dim", "channel size {CHANNEL_SIZE}, limits {DOWNLOAD_LIMIT} and {PROCESS_LIMIT} (the crate: 64 and 64) · titles from the crate's own resources.json · the timeout is simulated" }
                button { class: "btn", onclick: move |_| board.reset(Pipeline::new()), "reset" }
            }
        }
    }
}

#[component]
fn StageNode(
    id: StageId,
    stage: Stage,
    stuck: bool,
    onstep: EventHandler<MouseEvent>,
    onjob: EventHandler<usize>,
) -> Element {
    let class = if stage.finished {
        "bd-actor pl-stage gone"
    } else if stuck {
        "bd-actor pl-stage parked"
    } else if !stage.work.is_empty() {
        "bd-actor pl-stage held"
    } else {
        "bd-actor pl-stage"
    };
    let (name, job, limit) = (id.name(), id.job(), id.limit());
    let at = position(id.node());
    rsx! {
        div { class, style: at_style(at),
            button { class: "pl-step", onclick: move |e| onstep.call(e),
                div { class: "bd-head", "{name}" }
                div { class: "bd-doing",
                    if stage.finished { "returned" } else if stuck { "parked: forward full" } else { "step select!" }
                }
            }
            div { class: "pl-work",
                span { class: "bd-hint", "FuturesUnordered · {stage.work.len()}/{limit}" }
                for j in stage.work.iter().copied() {
                    button {
                        key: "{j.id}",
                        class: if j.done.is_some() { "bd-chip done" } else { "bd-chip pending" },
                        style: "--c: {record_hex(j.id)};",
                        disabled: j.done.is_some(),
                        title: "{job}({RECORDS[j.id].host})",
                        onclick: move |_| onjob.call(j.id),
                        "{RECORDS[j.id].short}"
                        span { class: "pl-job-state",
                            match j.done {
                                None => " · pending",
                                Some(_) if id.fails(j.id) => " · Err",
                                Some(_) => " · ready",
                            }
                        }
                    }
                }
            }
        }
    }
}
