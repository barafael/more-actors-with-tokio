//! `barafael/halres-downloader` as a pipeline you step through by hand.
//!
//! A reader feeds records into a bounded `mpsc`; a downloader stage turns
//! records into responses; a processor stage turns responses into
//! resources; a collector gathers them. Each stage is the same loop:
//!
//! ```text
//! loop {
//!     let in_progress = work.len();
//!     select! {
//!         biased;
//!         Some(item) = input.recv(), if in_progress < limit => work.push(job(item)),
//!         Some(done) = work.next(), if in_progress > 0 => match done {
//!             Ok(out) => forward.send(out).await,   // parks when downstream is full
//!             Err(e)  => warn!(%e),                 // dropped, not forwarded
//!         },
//!         else => break,                            // input closed, nothing in flight
//!     }
//! }
//! ```
//!
//! `work` is a `FuturesUnordered`: jobs finish in whatever order the network
//! decides, and here the network is you — click a job to let it complete.
//! When the reader drops its sender, each stage breaks once its input is
//! drained and its work is done, dropping its own sender: shutdown runs down
//! the pipeline by itself.

use sim_channels::mpsc::{MpscCore, SendOffer, SendPoll, TryRecvError};
use sim_channels::WaiterId;

/// The pipeline's channels (the crate defaults to 64).
pub const CHANNEL_SIZE: usize = 2;
/// Downloads in flight at once (the crate: `concurrency_limit`, 64).
pub const DOWNLOAD_LIMIT: usize = 3;
/// Pages being processed at once.
pub const PROCESS_LIMIT: usize = 2;

/// One line of `urls.csv`, and what fetching it yields. Titles are the
/// ones the crate's own run wrote to `resources.json`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Record {
    pub host: &'static str,
    /// What a chip has room for.
    pub short: &'static str,
    /// `None`: the download fails (simulated: a timeout).
    pub title: Option<&'static str>,
}

pub const RECORDS: [Record; 7] = [
    Record {
        host: "crates.io",
        short: "crates",
        title: Some(""),
    },
    Record {
        host: "quickref.me",
        short: "quickref",
        title: Some("Rust Cheat Sheet & Quick Reference"),
    },
    Record {
        host: "meet.jit.si",
        short: "jitsi",
        title: Some("Jitsi Meet"),
    },
    Record {
        host: "berline.rs",
        short: "berline",
        title: Some("Berline.rs"),
    },
    Record {
        host: "this-week-in-rust.org",
        short: "TWiR",
        title: Some("This Week in Rust"),
    },
    Record {
        host: "rustacean-station.org",
        short: "station",
        title: None,
    },
    Record {
        host: "lib.rs",
        short: "lib.rs",
        title: Some("Lib.rs — home for Rust crates"),
    },
];

/// Which stage. Stage `i` reads `CHANNELS[i]` and writes `CHANNELS[i + 1]`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum StageId {
    Download,
    Process,
}

/// The pipeline's channels, in order.
const CHANNELS: [Node; 3] = [Node::Pages, Node::Responses, Node::Resources];

impl StageId {
    fn index(self) -> usize {
        self as usize
    }

    pub fn name(self) -> &'static str {
        ["downloader", "processor"][self.index()]
    }

    /// The async fn each of this stage's futures runs, as the crate names it.
    pub fn job(self) -> &'static str {
        ["download_page", "page_details"][self.index()]
    }

    pub fn limit(self) -> usize {
        [DOWNLOAD_LIMIT, PROCESS_LIMIT][self.index()]
    }

    /// Whether this stage's job for record `id` fails.
    pub fn fails(self, id: usize) -> bool {
        self == StageId::Download && RECORDS[id].title.is_none()
    }

    pub fn node(self) -> Node {
        [Node::Downloader, Node::Processor][self.index()]
    }
}

/// One future in a stage's `FuturesUnordered`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Job {
    pub id: usize,
    /// When it completed, in completion order; `None` while pending.
    pub done: Option<u64>,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Stage {
    pub work: Vec<Job>,
    /// Parked in `forward.send(id).await`.
    pub parked: Option<(usize, WaiterId)>,
    /// Broke out of its loop and dropped its sender.
    pub finished: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Node {
    Reader,
    Pages,
    Downloader,
    Responses,
    Processor,
    Resources,
    Collector,
    /// Where `warn!` drops a failed job.
    Dropped,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Hop {
    pub from: Node,
    pub to: Node,
    pub id: usize,
    pub failed: bool,
    pub leg: u8,
}

/// What one click did: the hops it made and what to say about it.
pub type Step = crate::games::step::Step<Hop>;

fn hop(from: Node, to: Node, id: usize, leg: u8) -> Hop {
    Hop {
        from,
        to,
        id,
        failed: false,
        leg,
    }
}

pub struct Pipeline {
    /// The next line of `urls.csv` to read.
    next_record: usize,
    reader_parked: Option<WaiterId>,
    /// `drop(pages_tx)` has happened.
    pub reader_done: bool,
    /// `CHANNELS`, in order: pages, responses, resources.
    chans: [MpscCore<usize>; 3],
    stages: [Stage; 2],
    /// Resources in the order they arrived.
    pub collected: Vec<usize>,
    /// The collector's `recv()` saw `None` and returned its Vec.
    pub collector_done: bool,
    /// Jobs `warn!` dropped.
    pub dropped: Vec<usize>,
    /// Completion counter, so `work.next()` yields in completion order.
    completions: u64,
}

impl Default for Pipeline {
    fn default() -> Self {
        Self::new()
    }
}

impl Pipeline {
    pub fn new() -> Self {
        Self {
            next_record: 0,
            reader_parked: None,
            reader_done: false,
            chans: std::array::from_fn(|_| MpscCore::new(CHANNEL_SIZE)),
            stages: Default::default(),
            collected: Vec::new(),
            collector_done: false,
            dropped: Vec::new(),
            completions: 0,
        }
    }

    // ---- reading the board ----

    pub fn remaining_records(&self) -> std::ops::Range<usize> {
        self.next_record..RECORDS.len()
    }

    pub fn channel(&self, node: Node) -> (Vec<usize>, Vec<usize>, bool) {
        let core = &self.chans[CHANNELS.iter().position(|c| *c == node).unwrap_or(2)];
        (
            core.buffer().copied().collect(),
            core.blocked().map(|(_, id)| *id).collect(),
            core.sender_count() == 0,
        )
    }

    pub fn stage(&self, id: StageId) -> &Stage {
        &self.stages[id.index()]
    }

    pub fn reader_stuck(&self) -> bool {
        self.reader_parked
            .is_some_and(|w| self.chans[0].blocked().any(|(b, _)| b == w))
    }

    pub fn stage_stuck(&self, id: StageId) -> bool {
        let output = &self.chans[id.index() + 1];
        self.stages[id.index()]
            .parked
            .is_some_and(|(_, w)| output.blocked().any(|(b, _)| b == w))
    }

    // ---- the reader: `for record in reader.deserialize()` ----

    pub fn click_reader(&mut self) -> Step {
        if self.reader_done {
            return Step::note("The reader is done: its sender is dropped.");
        }
        if self.reader_stuck() {
            return Step::note(
                "The reader is parked in pages_tx.send().await — the pages channel is full.",
            );
        }
        self.reader_parked = None;
        if self.next_record == RECORDS.len() {
            self.reader_done = true;
            self.chans[0].drop_sender();
            return Step::note(
                "End of urls.csv: drop(pages_tx). No stage is told to stop — each will notice on its own.",
            );
        }
        let id = self.next_record;
        self.next_record += 1;
        let hops = vec![hop(Node::Reader, Node::Pages, id, 0)];
        let note = match self.chans[0].offer_send(id) {
            SendOffer::Blocked { waiter } => {
                self.reader_parked = Some(waiter);
                format!(
                    "{} is read, but the pages channel is full: the reader parks.",
                    RECORDS[id].host
                )
            }
            _ => format!(
                "The reader parses {} from urls.csv and sends it on.",
                RECORDS[id].host
            ),
        };
        Step { hops, note }
    }

    // ---- a stage's network: the user completes a job ----

    /// Let job `id` in `stage` complete, as the network would.
    pub fn click_job(&mut self, stage: StageId, id: usize) -> Step {
        let completions = &mut self.completions;
        let work = &mut self.stages[stage.index()].work;
        let Some(job) = work.iter_mut().find(|j| j.id == id && j.done.is_none()) else {
            return Step::note("That job has already completed.");
        };
        *completions += 1;
        job.done = Some(*completions);
        let host = RECORDS[id].host;
        Step::note(match (stage, stage.fails(id)) {
            (StageId::Download, true) => {
                format!("{host} times out: the future is ready — with an error.")
            }
            (StageId::Download, false) => format!(
                "{host} answers: the download future is ready. The stage still has to poll it."
            ),
            (StageId::Process, _) => {
                format!("{host}'s page is parsed (spawn_blocking): ready for the stage to pick up.")
            }
        })
    }

    // ---- a stage: one turn of its `select! { biased; ... }` ----

    pub fn click_stage(&mut self, id: StageId) -> Step {
        let i = id.index();
        let stage = &mut self.stages[i];
        let (upstream, downstream) = self.chans.split_at_mut(i + 1);
        let (input, output) = (&mut upstream[i], &mut downstream[0]);
        let name = id.name();
        if stage.finished {
            return Step::note(format!("The {name} has returned."));
        }
        if let Some((item, waiter)) = stage.parked {
            return match output.poll_send(waiter) {
                SendPoll::Pending => Step::note(format!(
                    "The {name} is parked in forward.send().await: downstream is full, so it takes no new work either."
                )),
                _ => {
                    stage.parked = None;
                    Step::note(format!(
                        "Room downstream: {}'s send returns and the {name} is back in its loop.",
                        RECORDS[item].host
                    ))
                }
            };
        }

        let in_progress = stage.work.len();
        // biased: the first branch is tried first, every time
        let mut input_closed = false;
        if in_progress < id.limit() {
            match input.try_recv() {
                Ok(item) => {
                    stage.work.push(Job {
                        id: item,
                        done: None,
                    });
                    return Step {
                        hops: vec![hop(CHANNELS[i], id.node(), item, 0)],
                        note: format!(
                            "biased: recv() first. {} joins the FuturesUnordered ({}/{} in flight).",
                            RECORDS[item].host,
                            in_progress + 1,
                            id.limit()
                        ),
                    };
                }
                Err(TryRecvError::Disconnected) => input_closed = true,
                Err(TryRecvError::Empty) => {}
            }
        }

        // second branch: whichever job completed first
        let ready = stage
            .work
            .iter()
            .enumerate()
            .filter_map(|(i, j)| j.done.map(|at| (at, i)))
            .min();
        if let Some((_, index)) = ready {
            let job = stage.work.remove(index);
            let host = RECORDS[job.id].host;
            if id.fails(job.id) {
                self.dropped.push(job.id);
                return Step {
                    hops: vec![Hop {
                        failed: true,
                        ..hop(id.node(), Node::Dropped, job.id, 0)
                    }],
                    note: format!("{host} failed: warn!(\"Failed to download page\") — logged and dropped, never forwarded."),
                };
            }
            let hops = vec![hop(id.node(), CHANNELS[i + 1], job.id, 0)];
            let note = match output.offer_send(job.id) {
                SendOffer::Blocked { waiter } => {
                    stage.parked = Some((job.id, waiter));
                    format!("{host} is done, but downstream is full: the {name} parks in forward.send().await.")
                }
                _ => format!(
                    "work.next() yields {host} — not necessarily the oldest job, the first to finish — and it goes downstream."
                ),
            };
            return Step { hops, note };
        }

        if stage.work.is_empty() && input_closed {
            // `else => break`: input closed and drained, nothing in flight
            stage.finished = true;
            output.drop_sender();
            return Step::note(format!(
                "Input closed and drained, nothing in flight: else => break. The {name} returns and drops its sender — the next stage will notice."
            ));
        }
        let why = if in_progress >= id.limit() {
            format!("at its limit of {} and none finished", id.limit())
        } else if input_closed {
            "its input is closed, but jobs are still in flight".to_string()
        } else {
            "nothing to receive, nothing finished".to_string()
        };
        Step::note(format!("select!: {why} — the {name} would wait here."))
    }

    // ---- the collector: `while let Some(resource) = rx.recv().await` ----

    pub fn click_collector(&mut self) -> Step {
        if self.collector_done {
            return Step::note("The collector has returned its Vec: resources.json is written.");
        }
        match self.chans[2].try_recv() {
            Ok(id) => {
                self.collected.push(id);
                Step {
                    hops: vec![hop(Node::Resources, Node::Collector, id, 0)],
                    note: format!("The collector pushes {}.", RECORDS[id].host),
                }
            }
            Err(TryRecvError::Disconnected) => {
                self.collector_done = true;
                Step::note(format!(
                    "recv() returned None: every stage upstream is gone. The collector returns {} resources — in the order they finished, not the order they were read.",
                    self.collected.len()
                ))
            }
            Err(TryRecvError::Empty) => Step::note("Nothing to collect yet: recv() would wait."),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Click everything until the collector returns, finishing each stage's
    /// newest pending job (`pick_last`) or its oldest.
    fn drain(p: &mut Pipeline, pick_last: bool) {
        for _ in 0..500 {
            if p.collector_done {
                return;
            }
            p.click_reader();
            for stage in [StageId::Download, StageId::Process] {
                let pending: Vec<usize> = p
                    .stage(stage)
                    .work
                    .iter()
                    .filter(|j| j.done.is_none())
                    .map(|j| j.id)
                    .collect();
                let pick = if pick_last {
                    pending.last()
                } else {
                    pending.first()
                };
                if let Some(&id) = pick {
                    p.click_job(stage, id);
                }
                p.click_stage(stage);
            }
            p.click_collector();
        }
        panic!("the pipeline never finished");
    }

    #[test]
    fn everything_but_the_failure_arrives_and_everything_shuts_down() {
        let mut p = Pipeline::new();
        drain(&mut p, false);
        let mut got = p.collected.clone();
        got.sort_unstable();
        assert_eq!(got, [0, 1, 2, 3, 4, 6], "all but the timeout");
        assert_eq!(p.dropped, [5]);
        assert!(p.stage(StageId::Download).finished && p.stage(StageId::Process).finished);
    }

    #[test]
    fn jobs_leave_in_the_order_they_finish_not_the_order_they_arrived() {
        let mut p = Pipeline::new();
        for _ in 0..DOWNLOAD_LIMIT {
            p.click_reader();
            p.click_stage(StageId::Download);
        }
        for id in (0..DOWNLOAD_LIMIT).rev() {
            p.click_job(StageId::Download, id);
        }
        p.click_stage(StageId::Download);
        p.click_stage(StageId::Download);
        assert_eq!(p.channel(Node::Responses).0, [2, 1], "last read, first out");
    }

    #[test]
    fn a_stage_never_holds_more_than_its_limit() {
        let mut p = Pipeline::new();
        for _ in 0..10 {
            p.click_reader();
            p.click_stage(StageId::Download);
        }
        assert_eq!(p.stage(StageId::Download).work.len(), DOWNLOAD_LIMIT);
    }

    #[test]
    fn biased_select_prefers_new_work_over_finished_work() {
        let mut p = Pipeline::new();
        p.click_reader();
        p.click_stage(StageId::Download);
        p.click_job(StageId::Download, 0);
        p.click_reader();
        let step = p.click_stage(StageId::Download);
        assert!(step.note.starts_with("biased"), "{}", step.note);
        assert_eq!(p.stage(StageId::Download).work.len(), 2);
    }

    #[test]
    fn a_full_downstream_parks_the_stage_and_it_takes_no_new_work() {
        let mut p = Pipeline::new();
        // fill the downloader and let its jobs finish
        for _ in 0..DOWNLOAD_LIMIT {
            p.click_reader();
            p.click_stage(StageId::Download);
        }
        for id in 0..DOWNLOAD_LIMIT {
            p.click_job(StageId::Download, id);
        }
        // two forwards fill the responses channel; the third parks
        for _ in 0..DOWNLOAD_LIMIT {
            p.click_stage(StageId::Download);
        }
        assert!(p.stage_stuck(StageId::Download));
        p.click_reader();
        let step = p.click_stage(StageId::Download);
        assert!(step.note.contains("parked"));
        // the processor takes one: the parked send completes
        p.click_stage(StageId::Process);
        assert!(!p.stage_stuck(StageId::Download));
    }

    #[test]
    fn a_failed_download_is_dropped_not_forwarded() {
        let mut p = Pipeline::new();
        // start the reader at rustacean-station.org, which times out
        p.next_record = 5;
        p.click_reader();
        p.click_stage(StageId::Download);
        p.click_job(StageId::Download, 5);
        let step = p.click_stage(StageId::Download);
        assert!(step.hops[0].failed);
        assert_eq!(p.dropped, [5]);
        assert!(p.channel(Node::Responses).0.is_empty());
        assert!(p.stage(StageId::Download).work.is_empty());
    }

    #[test]
    fn a_stage_does_not_break_while_work_is_in_flight() {
        let mut p = Pipeline::new();
        p.click_reader();
        p.click_stage(StageId::Download);
        p.next_record = RECORDS.len();
        p.click_reader(); // drop(pages_tx)
        let step = p.click_stage(StageId::Download);
        assert!(!p.stage(StageId::Download).finished, "{}", step.note);
        p.click_job(StageId::Download, 0);
        p.click_stage(StageId::Download); // forwards
        p.click_stage(StageId::Download); // now: else => break
        assert!(p.stage(StageId::Download).finished);
        assert!(p.channel(Node::Responses).2, "its sender is dropped");
    }

    #[test]
    fn the_collector_only_returns_once_everything_upstream_is_gone() {
        let mut p = Pipeline::new();
        p.next_record = RECORDS.len();
        p.click_reader();
        p.click_collector();
        assert!(!p.collector_done, "the stages still hold their senders");
        p.click_stage(StageId::Download);
        p.click_collector();
        assert!(!p.collector_done);
        p.click_stage(StageId::Process);
        p.click_collector();
        assert!(p.collector_done);
    }
}
