//! speedd, the Protohackers "Speed Daemon" server, as a board you step
//! through by hand.
//!
//! The actors and channels are the ones in `barafael/protohackers`'
//! `speedd` crate, drawn in its `live-topology.drawio.png`: cameras report
//! plates into one bounded `mpsc` to the Collector; the Collector turns
//! sightings into tickets and sends each into its road's `mpmc` queue;
//! dispatchers subscribe to roads through a second `mpsc` that carries a
//! `oneshot` for the reply, then pull tickets off their roads' queues.
//!
//! Nothing here runs on its own. Every public `click_*` is one iteration of
//! one actor's loop — one message processed — and returns the hops that
//! iteration made, for the view to animate. Channels are the
//! `sim-channels` cores, so a full queue parks its sender and a receive
//! frees the longest-parked send, by the cores' rules.
//!
//! The ticket logic (`insert_record`, `is_violation`, `dispatch_tickets`,
//! `days`) is ported from `speedd/src/collector.rs`, integer days aside.
//! Left out: the Listener, heartbeat tasks, and `limits`, which speedd
//! writes and never reads.

use std::collections::{BTreeMap, BTreeSet};

use sim_channels::mpsc::{MpscCore, SendOffer, SendPoll};
use sim_channels::oneshot::OneshotCore;
use sim_channels::WaiterId;

/// Room in the Collector's `reporting` inbox (speedd: 256).
pub const REPORTING_CAPACITY: usize = 4;
/// Room in the `dispatcher_subscription` inbox (speedd: 16).
pub const SUBSCRIPTION_CAPACITY: usize = 2;
/// Room in each road's ticket queue (speedd: 1024).
pub const QUEUE_CAPACITY: usize = 3;

const SECONDS_PER_DAY: u32 = 86_400;

pub type Road = u16;

/// The three roads, in drawing order, with their limits in mph.
pub const ROADS: [(Road, u16); 3] = [(7, 60), (8, 50), (9, 70)];

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct CameraSpec {
    pub road: Road,
    pub mile: u16,
    pub limit: u16,
}

pub const CAMERAS: [CameraSpec; 7] = [
    CameraSpec {
        road: 7,
        mile: 0,
        limit: 60,
    },
    CameraSpec {
        road: 7,
        mile: 10,
        limit: 60,
    },
    CameraSpec {
        road: 7,
        mile: 25,
        limit: 60,
    },
    CameraSpec {
        road: 8,
        mile: 0,
        limit: 50,
    },
    CameraSpec {
        road: 8,
        mile: 8,
        limit: 50,
    },
    CameraSpec {
        road: 9,
        mile: 0,
        limit: 70,
    },
    CameraSpec {
        road: 9,
        mile: 15,
        limit: 70,
    },
];

/// The dispatchers and the roads each serves, as in the drawio diagram
/// widened to three roads: two share road 7, two straddle roads.
pub const DISPATCHERS: [&[Road]; 5] = [&[7], &[7], &[7, 8], &[8, 9], &[9]];

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Car {
    pub plate: &'static str,
    pub mph: u32,
}

/// The plates the cameras can see. A slow car never earns a ticket, a
/// middling one only where the limit is low, a fast one everywhere.
pub const CARS: [Car; 3] = [
    Car {
        plate: "SNAIL",
        mph: 45,
    },
    Car {
        plate: "SEDAN",
        mph: 65,
    },
    Car {
        plate: "ROCKET",
        mph: 100,
    },
];

/// A camera's report: `(PlateRecord, Camera)` in speedd.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Report {
    pub car: usize,
    pub timestamp: u32,
    pub camera: usize,
}

/// A `TicketRecord`. `speed` is in hundredths of a mph, like speedd's.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Ticket {
    pub car: usize,
    pub road: Road,
    pub mile1: u16,
    pub timestamp1: u32,
    pub mile2: u16,
    pub timestamp2: u32,
    pub speed: u16,
}

/// `(Road, oneshot::Sender<mpmc::Receiver<Ticket>>)`: the dispatcher index
/// stands for the oneshot sender, whose channel lives in
/// [`Speedd::replies`].
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Subscribe {
    pub road: Road,
    pub dispatcher: usize,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Collector {
    /// In its `select!`, ready for the next message.
    Ready,
    /// Parked in `tx.send(ticket).await` on a full road queue.
    Sending { road: Road, waiter: WaiterId },
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Dispatcher {
    /// `Dispatcher::new` has not sent the subscription for `roads[next]`.
    Connecting { next: usize },
    /// Parked sending that subscription into the full inbox.
    Subscribing { next: usize, waiter: WaiterId },
    /// Sent it; awaiting the oneshot with the road's receiver.
    AwaitingReply { next: usize },
    /// Holding every road's receiver, merged as in `select_all`.
    Running,
}

/// Where a hop starts or lands, for the view's layout.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Node {
    Camera(usize),
    Reporting,
    Collector,
    Subscription,
    /// A road's ticket queue, by index into [`ROADS`].
    Queue(usize),
    Dispatcher(usize),
    /// The TCP client behind a dispatcher, where tickets leave the system.
    Socket(usize),
}

/// What a hop carries, which decides its colour.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Cargo {
    Plate(usize),
    Ticket(usize),
    Subscription,
    Reply,
}

/// One message moving along one edge.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Hop {
    pub from: Node,
    pub to: Node,
    pub label: String,
    pub cargo: Cargo,
    /// Which leg of the step it is; the view staggers legs.
    pub leg: u8,
}

/// What one click did: the hops it made and what to say about it.
pub type Step = crate::games::step::Step<Hop>;

/// Index of `road` in [`ROADS`].
pub fn road_index(road: Road) -> usize {
    ROADS.iter().position(|(r, _)| *r == road).unwrap_or(0)
}

pub struct Speedd {
    reporting: MpscCore<Report>,
    subscription: MpscCore<Subscribe>,
    /// One ticket queue per road, created on first use like speedd's
    /// `dispatchers.entry(road).or_insert_with(|| mpmc::bounded(..))`.
    queues: BTreeMap<Road, MpscCore<Ticket>>,
    /// The oneshot each subscribing dispatcher awaits.
    replies: BTreeMap<usize, OneshotCore<Road>>,
    /// Sightings per plate and road: timestamp → mile.
    records: BTreeMap<(usize, Road), BTreeMap<u32, u16>>,
    ticketed_days: BTreeMap<usize, BTreeSet<u32>>,
    collector: Collector,
    /// Each camera's send parked on the full `reporting` inbox.
    cameras_parked: [Option<WaiterId>; CAMERAS.len()],
    /// How often each camera has seen each car: each pass is a new day.
    passes: [[u32; CARS.len()]; CAMERAS.len()],
    dispatchers: [Dispatcher; DISPATCHERS.len()],
    /// Which of its roads a dispatcher tries first next time.
    rotation: [usize; DISPATCHERS.len()],
    pub delivered: [Vec<Ticket>; DISPATCHERS.len()],
}

impl Default for Speedd {
    fn default() -> Self {
        Self::new()
    }
}

impl Speedd {
    pub fn new() -> Self {
        let mut reporting = MpscCore::new(REPORTING_CAPACITY);
        let mut subscription = MpscCore::new(SUBSCRIPTION_CAPACITY);
        // every camera and dispatcher holds a sender clone
        for _ in 1..CAMERAS.len() {
            reporting.add_sender();
        }
        for _ in 1..DISPATCHERS.len() {
            subscription.add_sender();
        }
        Self {
            reporting,
            subscription,
            queues: BTreeMap::new(),
            replies: BTreeMap::new(),
            records: BTreeMap::new(),
            ticketed_days: BTreeMap::new(),
            collector: Collector::Ready,
            cameras_parked: [None; CAMERAS.len()],
            passes: [[0; CARS.len()]; CAMERAS.len()],
            dispatchers: [Dispatcher::Connecting { next: 0 }; DISPATCHERS.len()],
            rotation: [0; DISPATCHERS.len()],
            delivered: Default::default(),
        }
    }

    // ---- reading the board ----

    pub fn reporting(&self) -> Vec<Report> {
        self.reporting.buffer().copied().collect()
    }

    pub fn reporting_parked(&self) -> Vec<Report> {
        self.reporting.blocked().map(|(_, r)| *r).collect()
    }

    pub fn subscriptions(&self) -> Vec<Subscribe> {
        self.subscription.buffer().copied().collect()
    }

    pub fn subscriptions_parked(&self) -> Vec<Subscribe> {
        self.subscription.blocked().map(|(_, s)| *s).collect()
    }

    /// A road's queued tickets, or `None` if nobody has created it yet.
    pub fn queue(&self, road: Road) -> Option<Vec<Ticket>> {
        self.queues
            .get(&road)
            .map(|q| q.buffer().copied().collect())
    }

    /// The ticket the Collector is parked trying to send into `road`.
    pub fn queue_parked(&self, road: Road) -> Option<Ticket> {
        self.queues
            .get(&road)
            .and_then(|q| q.blocked().map(|(_, t)| *t).next())
    }

    pub fn collector(&self) -> Collector {
        self.collector
    }

    /// Parked in a send no slot has taken yet.
    pub fn collector_stuck(&self) -> bool {
        match self.collector {
            Collector::Sending { road, waiter } => self.still_queued_ticket(road, waiter),
            Collector::Ready => false,
        }
    }

    pub fn camera_stuck(&self, camera: usize) -> bool {
        self.cameras_parked[camera]
            .is_some_and(|waiter| self.reporting.blocked().any(|(w, _)| w == waiter))
    }

    pub fn dispatcher(&self, index: usize) -> Dispatcher {
        self.dispatchers[index]
    }

    pub fn dispatcher_stuck(&self, index: usize) -> bool {
        match self.dispatchers[index] {
            Dispatcher::Subscribing { waiter, .. } => {
                self.subscription.blocked().any(|(w, _)| w == waiter)
            }
            _ => false,
        }
    }

    /// Whether this dispatcher's oneshot holds an answer it has not read.
    pub fn reply_waiting(&self, index: usize) -> bool {
        self.replies.get(&index).is_some_and(|r| r.has_value())
    }

    /// Sightings per plate, for the Collector's state panel.
    pub fn records(&self) -> &BTreeMap<(usize, Road), BTreeMap<u32, u16>> {
        &self.records
    }

    pub fn ticketed_days(&self) -> &BTreeMap<usize, BTreeSet<u32>> {
        &self.ticketed_days
    }

    fn still_queued_ticket(&self, road: Road, waiter: WaiterId) -> bool {
        self.queues
            .get(&road)
            .is_some_and(|q| q.blocked().any(|(w, _)| w == waiter))
    }

    // ---- cameras ----

    /// Camera `camera` reads `car`'s plate off its socket and sends it on.
    pub fn click_camera(&mut self, camera: usize, car: usize) -> Step {
        if self.camera_stuck(camera) {
            return Step::note(
                "This camera is still parked in reporting.send().await — the Collector has to receive first.",
            );
        }
        self.cameras_parked[camera] = None;
        let spec = CAMERAS[camera];
        let pass = self.passes[camera][car];
        self.passes[camera][car] += 1;
        let timestamp = pass * SECONDS_PER_DAY
            + (f64::from(spec.mile) * 3600.0 / f64::from(CARS[car].mph)).round() as u32;
        let report = Report {
            car,
            timestamp,
            camera,
        };
        let plate = CARS[car].plate;
        let hop = Hop {
            from: Node::Camera(camera),
            to: Node::Reporting,
            label: plate.to_string(),
            cargo: Cargo::Plate(car),
            leg: 0,
        };
        let note = match self.reporting.offer_send(report) {
            SendOffer::Blocked { waiter } => {
                self.cameras_parked[camera] = Some(waiter);
                format!("{plate} at road {} mile {}: reporting is full, so the camera parks in send().await.", spec.road, spec.mile)
            }
            SendOffer::Accepted | SendOffer::Rejected(_) => format!(
                "{plate} passes road {} mile {} at t={timestamp}: the camera sends (plate, camera) to the Collector.",
                spec.road, spec.mile
            ),
        };
        Step {
            hops: vec![hop],
            note,
        }
    }

    // ---- the Collector ----

    /// One iteration of the Collector's `select!`: subscriptions first
    /// (tokio picks at random; a fixed order keeps the board predictable).
    pub fn click_collector(&mut self) -> Step {
        if let Collector::Sending { road, waiter } = self.collector {
            let queue = self
                .queues
                .get_mut(&road)
                .expect("parked on a queue that exists");
            return match queue.poll_send(waiter) {
                SendPoll::Pending => Step::note(format!(
                    "The Collector is parked sending into road {road}'s full queue. Nothing reaches it until a road-{road} dispatcher takes a ticket."
                )),
                _ => {
                    self.collector = Collector::Ready;
                    Step::note(format!(
                        "A dispatcher made room: the send into road {road} returns, and the Collector is back in its select!."
                    ))
                }
            };
        }
        if let Ok(subscribe) = self.subscription.try_recv() {
            return self.subscribe(subscribe);
        }
        if let Ok(report) = self.reporting.try_recv() {
            return self.report(report);
        }
        Step::note(
            "Both inboxes are empty: the Collector would park in select! until something arrives.",
        )
    }

    fn subscribe(&mut self, Subscribe { road, dispatcher }: Subscribe) -> Step {
        self.queues
            .entry(road)
            .or_insert_with(|| MpscCore::new(QUEUE_CAPACITY));
        if let Some(reply) = self.replies.get_mut(&dispatcher) {
            reply.send(road).ok();
        }
        let queued = self.queues.get(&road).map_or(0, |q| q.len());
        Step {
            hops: vec![
                Hop {
                    from: Node::Subscription,
                    to: Node::Collector,
                    label: format!("{road}?"),
                    cargo: Cargo::Subscription,
                    leg: 0,
                },
                Hop {
                    from: Node::Collector,
                    to: Node::Dispatcher(dispatcher),
                    label: format!("rx {road}"),
                    cargo: Cargo::Reply,
                    leg: 1,
                },
            ],
            note: format!(
                "A subscription for road {road}: the Collector answers on its oneshot with a clone of road {road}'s receiver ({queued} ticket(s) already waiting)."
            ),
        }
    }

    fn report(&mut self, report: Report) -> Step {
        let plate = CARS[report.car].plate;
        let spec = CAMERAS[report.camera];
        let mut hops = vec![Hop {
            from: Node::Reporting,
            to: Node::Collector,
            label: plate.to_string(),
            cargo: Cargo::Plate(report.car),
            leg: 0,
        }];
        let tickets = self.insert_record(report);
        let Some(ticket) = self.dispatch_tickets(&tickets) else {
            let note = match tickets.first() {
                Some(t) => format!(
                    "{plate} did {} mph on road {}, but already has a ticket for that day: ignored.",
                    t.speed / 100,
                    t.road
                ),
                None => format!(
                    "{plate} recorded at road {} mile {}. No neighbouring sighting shows it speeding.",
                    spec.road, spec.mile
                ),
            };
            return Step { hops, note };
        };
        let road = ticket.road;
        let queue = self
            .queues
            .entry(road)
            .or_insert_with(|| MpscCore::new(QUEUE_CAPACITY));
        hops.push(Hop {
            from: Node::Collector,
            to: Node::Queue(road_index(road)),
            label: format!("{plate} {}", ticket.speed / 100),
            cargo: Cargo::Ticket(ticket.car),
            leg: 1,
        });
        let note = match queue.offer_send(ticket) {
            SendOffer::Blocked { waiter } => {
                self.collector = Collector::Sending { road, waiter };
                format!(
                    "{plate}: {} mph on road {road} — a ticket, but road {road}'s queue is full. The Collector parks, and so does everything behind it.",
                    ticket.speed / 100
                )
            }
            SendOffer::Accepted | SendOffer::Rejected(_) => format!(
                "{plate}: {} mph on road {road} (limit {}) — ticket! It waits in road {road}'s queue for any dispatcher of that road.",
                ticket.speed / 100,
                ROADS[road_index(road)].1
            ),
        };
        Step { hops, note }
    }

    /// Ported from speedd's `Collector::insert_record`: store the sighting
    /// and check it against its nearest neighbours in time.
    fn insert_record(&mut self, report: Report) -> Vec<Ticket> {
        let CameraSpec { road, mile, limit } = CAMERAS[report.camera];
        let limit = limit.saturating_mul(100);
        let timestamp = report.timestamp;
        let map = self.records.entry((report.car, road)).or_default();

        let prev = map.range(..timestamp).next_back().map(|(t, m)| (*t, *m));
        let next = map.range(timestamp..).next().map(|(t, m)| (*t, *m));
        *map.entry(timestamp).or_default() = mile;

        let mut tickets = Vec::new();
        if let Some((earlier, previous_mile)) = prev {
            if let Some(speed) = is_violation(limit, earlier, timestamp, previous_mile, mile) {
                tickets.push(Ticket {
                    car: report.car,
                    road,
                    mile1: previous_mile,
                    timestamp1: earlier,
                    mile2: mile,
                    timestamp2: timestamp,
                    speed,
                });
            }
        }
        if let Some((later, next_mile)) = next {
            if let Some(speed) = is_violation(limit, timestamp, later, mile, next_mile) {
                tickets.push(Ticket {
                    car: report.car,
                    road,
                    mile1: mile,
                    timestamp1: timestamp,
                    mile2: next_mile,
                    timestamp2: later,
                    speed,
                });
            }
        }
        tickets
    }

    /// Ported from speedd's `dispatch_tickets`: the first candidate on no
    /// already-ticketed day marks its days and is sent; the rest are not.
    /// Returns the ticket to send.
    fn dispatch_tickets(&mut self, tickets: &[Ticket]) -> Option<Ticket> {
        let ticketed = self.ticketed_days.entry(tickets.first()?.car).or_default();
        for ticket in tickets {
            if days(ticket.timestamp1, ticket.timestamp2).any(|day| ticketed.contains(&day)) {
                continue;
            }
            ticketed.extend(days(ticket.timestamp1, ticket.timestamp2));
            return Some(*ticket);
        }
        None
    }

    // ---- dispatchers ----

    /// One step of a dispatcher: its next subscription while connecting,
    /// one ticket once running.
    pub fn click_dispatcher(&mut self, index: usize) -> Step {
        let roads = DISPATCHERS[index];
        match self.dispatchers[index] {
            Dispatcher::Connecting { next } => self.send_subscription(index, next),
            Dispatcher::Subscribing { next, waiter } => match self.subscription.poll_send(waiter) {
                SendPoll::Pending => Step::note(
                    "Still parked: the subscription inbox is full until the Collector takes one.",
                ),
                _ => {
                    self.dispatchers[index] = Dispatcher::AwaitingReply { next };
                    Step::note(format!(
                        "The subscription for road {} is in: now it awaits the oneshot reply.",
                        roads[next]
                    ))
                }
            },
            Dispatcher::AwaitingReply { next } => {
                let road = roads[next];
                let reply = self.replies.get_mut(&index).and_then(|r| r.try_recv().ok());
                if reply.is_none() {
                    return Step::note(format!(
                        "rx.await: the Collector has not answered the road {road} subscription yet."
                    ));
                }
                self.replies.remove(&index);
                if next + 1 < roads.len() {
                    let mut step = self.send_subscription(index, next + 1);
                    step.note = format!(
                        "Got road {road}'s receiver; Dispatcher::new moves on to road {}. {}",
                        roads[next + 1],
                        step.note
                    );
                    return step;
                }
                self.dispatchers[index] = Dispatcher::Running;
                Step::note(format!(
                    "Got road {road}'s receiver: the dispatcher holds all its roads and starts pulling tickets."
                ))
            }
            Dispatcher::Running => self.take_ticket(index),
        }
    }

    fn send_subscription(&mut self, index: usize, next: usize) -> Step {
        let road = DISPATCHERS[index][next];
        self.replies.insert(index, OneshotCore::new());
        let hop = Hop {
            from: Node::Dispatcher(index),
            to: Node::Subscription,
            label: format!("{road}?"),
            cargo: Cargo::Subscription,
            leg: 0,
        };
        let note = match self.subscription.offer_send(Subscribe {
            road,
            dispatcher: index,
        }) {
            SendOffer::Blocked { waiter } => {
                self.dispatchers[index] = Dispatcher::Subscribing { next, waiter };
                format!("Subscribing to road {road}: the subscription inbox is full, so it parks.")
            }
            SendOffer::Accepted | SendOffer::Rejected(_) => {
                self.dispatchers[index] = Dispatcher::AwaitingReply { next };
                format!(
                    "It sends (road {road}, a oneshot sender) to the Collector, and awaits the oneshot."
                )
            }
        };
        Step {
            hops: vec![hop],
            note,
        }
    }

    /// `select_all` over its roads: try them in turn, starting after the
    /// last one that delivered.
    fn take_ticket(&mut self, index: usize) -> Step {
        let roads = DISPATCHERS[index];
        for offset in 0..roads.len() {
            let pick = (self.rotation[index] + offset) % roads.len();
            let road = roads[pick];
            let Some(ticket) = self.queues.get_mut(&road).and_then(|q| q.try_recv().ok()) else {
                continue;
            };
            self.rotation[index] = (pick + 1) % roads.len();
            self.delivered[index].push(ticket);
            let plate = CARS[ticket.car].plate;
            let label = format!("{plate} {}", ticket.speed / 100);
            return Step {
                hops: vec![
                    Hop {
                        from: Node::Queue(road_index(road)),
                        to: Node::Dispatcher(index),
                        label: label.clone(),
                        cargo: Cargo::Ticket(ticket.car),
                        leg: 0,
                    },
                    Hop {
                        from: Node::Dispatcher(index),
                        to: Node::Socket(index),
                        label,
                        cargo: Cargo::Ticket(ticket.car),
                        leg: 1,
                    },
                ],
                note: format!(
                    "The dispatcher takes {plate}'s road {road} ticket — no other dispatcher will see it — and writes it to its client."
                ),
            };
        }
        Step::note("None of its roads has a ticket: the dispatcher would park in tickets.next().")
    }
}

/// Ported from speedd: speed over the stretch, rounded to whole mph, then
/// in hundredths; a violation if above the (hundredths) limit.
fn is_violation(limit: u16, ts1: u32, ts2: u32, mile1: u16, mile2: u16) -> Option<u16> {
    let delta_t = ts1.abs_diff(ts2);
    let delta_m = mile1.abs_diff(mile2);
    let speed = (delta_m as f32 / delta_t as f32) * 60.0 * 60.0;
    let speed = (speed.round() as u16).saturating_mul(100);
    (speed > limit).then_some(speed)
}

/// The days a stretch covers: speedd's `(t1..t2).map(day).unique()`, as
/// the equivalent range rather than one step per second.
fn days(timestamp1: u32, timestamp2: u32) -> impl Iterator<Item = u32> {
    let span = (timestamp1 < timestamp2)
        .then(|| timestamp1 / SECONDS_PER_DAY..=(timestamp2 - 1) / SECONDS_PER_DAY);
    span.into_iter().flatten()
}

#[cfg(test)]
mod tests {
    use super::*;

    const SNAIL: usize = 0;
    const SEDAN: usize = 1;
    const ROCKET: usize = 2;

    fn run_all_dispatchers(sim: &mut Speedd) {
        // enough clicks for every dispatcher to subscribe to every road
        for _ in 0..12 {
            for d in 0..DISPATCHERS.len() {
                if sim.dispatcher(d) != Dispatcher::Running {
                    sim.click_dispatcher(d);
                }
            }
            sim.click_collector();
        }
    }

    #[test]
    fn speedds_own_example_is_a_violation_at_379_mph() {
        assert_eq!(is_violation(1000, 1, 20, 2, 4), Some(37900));
    }

    #[test]
    fn speed_is_rounded_to_whole_mph_before_comparing() {
        // 60.4 mph rounds to 60, which is not above a 60 limit
        let t = (10.0f32 / 60.4 * 3600.0).round() as u32;
        assert_eq!(is_violation(6000, 0, t, 0, 10), None);
        assert_eq!(is_violation(6000, 0, 500, 0, 10), Some(7200));
    }

    #[test]
    fn days_are_half_open_like_speedds() {
        assert_eq!(days(0, 1).collect::<Vec<_>>(), [0]);
        assert_eq!(
            days(10, 86_400).collect::<Vec<_>>(),
            [0],
            "ts2 on the boundary is excluded"
        );
        assert_eq!(days(86_399, 86_401).collect::<Vec<_>>(), [0, 1]);
        assert_eq!(days(5, 5).count(), 0);
    }

    #[test]
    fn a_fast_car_between_two_cameras_is_ticketed_into_its_roads_queue() {
        let mut sim = Speedd::new();
        sim.click_camera(0, ROCKET);
        sim.click_camera(1, ROCKET);
        sim.click_collector();
        assert!(sim.queue(7).is_none(), "no ticket yet: one sighting");
        sim.click_collector();
        let queue = sim.queue(7).expect("created by the first ticket");
        assert_eq!(queue.len(), 1);
        assert_eq!(queue[0].speed, 10_000);
    }

    #[test]
    fn a_slow_car_is_never_ticketed() {
        let mut sim = Speedd::new();
        sim.click_camera(3, SNAIL);
        sim.click_camera(4, SNAIL);
        sim.click_collector();
        sim.click_collector();
        assert!(sim.queue(8).is_none());
    }

    #[test]
    fn the_middling_car_is_ticketed_only_where_the_limit_is_low() {
        let mut sim = Speedd::new();
        for camera in [3, 4, 5, 6] {
            sim.click_camera(camera, SEDAN);
            sim.click_collector();
        }
        assert_eq!(sim.queue(8).map(|q| q.len()), Some(1), "65 on a 50 road");
        assert!(sim.queue(9).is_none(), "65 on a 70 road is fine");
    }

    #[test]
    fn three_cameras_on_one_road_still_make_one_ticket_per_day() {
        let mut sim = Speedd::new();
        for camera in [0, 1, 2] {
            sim.click_camera(camera, ROCKET);
            sim.click_collector();
        }
        assert_eq!(sim.queue(7).map(|q| q.len()), Some(1));
    }

    #[test]
    fn the_next_pass_is_the_next_day_and_earns_another_ticket() {
        let mut sim = Speedd::new();
        for _ in 0..2 {
            for camera in [0, 1] {
                sim.click_camera(camera, ROCKET);
                sim.click_collector();
            }
        }
        assert_eq!(sim.queue(7).map(|q| q.len()), Some(2));
    }

    #[test]
    fn a_full_reporting_inbox_parks_the_next_camera_until_the_collector_receives() {
        let mut sim = Speedd::new();
        for camera in 0..REPORTING_CAPACITY {
            sim.click_camera(camera, SNAIL);
        }
        sim.click_camera(5, SNAIL);
        assert!(sim.camera_stuck(5));
        assert_eq!(sim.reporting_parked().len(), 1);
        sim.click_camera(5, SEDAN);
        assert_eq!(
            sim.reporting().len(),
            REPORTING_CAPACITY,
            "a parked camera sends nothing"
        );
        sim.click_collector();
        assert!(!sim.camera_stuck(5), "the freed slot took its report");
        assert_eq!(sim.reporting().len(), REPORTING_CAPACITY);
    }

    #[test]
    fn tickets_queue_before_any_dispatcher_subscribes() {
        let mut sim = Speedd::new();
        for _ in 0..2 {
            for camera in [0, 1] {
                sim.click_camera(camera, ROCKET);
                sim.click_collector();
            }
        }
        assert_eq!(sim.queue(7).map(|q| q.len()), Some(2));
        assert!((0..5).all(|d| sim.dispatcher(d) == Dispatcher::Connecting { next: 0 }));
    }

    #[test]
    fn the_subscription_is_a_oneshot_handshake() {
        let mut sim = Speedd::new();
        sim.click_dispatcher(0);
        assert_eq!(sim.dispatcher(0), Dispatcher::AwaitingReply { next: 0 });
        assert_eq!(sim.subscriptions().len(), 1);

        let step = sim.click_dispatcher(0);
        assert!(step.note.contains("not answered"));
        let step = sim.click_collector();
        assert_eq!(step.hops[1].to, Node::Dispatcher(0), "the reply flies back");
        assert!(sim.reply_waiting(0));
        assert!(sim.queue(7).is_some(), "subscribing creates the queue");

        sim.click_dispatcher(0);
        assert_eq!(sim.dispatcher(0), Dispatcher::Running);
    }

    #[test]
    fn a_straddling_dispatcher_subscribes_road_by_road_then_alternates() {
        let mut sim = Speedd::new();
        sim.click_dispatcher(2);
        sim.click_collector();
        let step = sim.click_dispatcher(2);
        assert_eq!(step.hops[0].label, "8?", "moves straight on to road 8");
        assert_eq!(sim.dispatcher(2), Dispatcher::AwaitingReply { next: 1 });
        sim.click_collector();
        sim.click_dispatcher(2);
        assert_eq!(sim.dispatcher(2), Dispatcher::Running);

        // two road-7 tickets, one road-8 ticket. Another car on road 8:
        // the rule is one ticket per car per day across *all* roads, so a
        // second ROCKET ticket on day 0 would be dropped.
        for camera in [0, 1] {
            sim.click_camera(camera, ROCKET);
            sim.click_collector();
        }
        for camera in [3, 4] {
            sim.click_camera(camera, SEDAN);
            sim.click_collector();
        }
        for camera in [0, 1] {
            sim.click_camera(camera, ROCKET);
            sim.click_collector();
        }
        let roads: Vec<Road> = (0..3)
            .map(|_| {
                sim.click_dispatcher(2);
                sim.delivered[2].last().unwrap().road
            })
            .collect();
        assert_eq!(
            roads,
            [7, 8, 7],
            "select_all takes turns while both have tickets"
        );
    }

    #[test]
    fn one_ticket_per_car_per_day_holds_across_roads() {
        let mut sim = Speedd::new();
        for camera in [0, 1, 3, 4] {
            sim.click_camera(camera, ROCKET);
            sim.click_collector();
        }
        assert_eq!(sim.queue(7).map(|q| q.len()), Some(1));
        assert!(sim.queue(8).is_none(), "day 0 is already ticketed");
    }

    #[test]
    fn any_dispatcher_of_a_road_can_take_its_ticket_but_only_one_does() {
        let mut sim = Speedd::new();
        run_all_dispatchers(&mut sim);
        for camera in [0, 1] {
            sim.click_camera(camera, ROCKET);
            sim.click_collector();
        }
        sim.click_dispatcher(1);
        assert_eq!(sim.delivered[1].len(), 1);
        sim.click_dispatcher(0);
        assert!(sim.delivered[0].is_empty(), "work stealing: it was taken");
    }

    #[test]
    fn a_full_ticket_queue_parks_the_collector_until_a_dispatcher_takes_one() {
        let mut sim = Speedd::new();
        for _ in 0..=QUEUE_CAPACITY {
            for camera in [0, 1] {
                sim.click_camera(camera, ROCKET);
                sim.click_collector();
            }
        }
        assert!(sim.collector_stuck());
        assert!(sim.queue_parked(7).is_some());

        // parked, it receives nothing: reports pile up behind it
        sim.click_camera(3, SNAIL);
        sim.click_collector();
        assert_eq!(sim.reporting().len(), 1);

        run_all_dispatchers(&mut sim);
        assert!(
            sim.collector_stuck(),
            "subscriptions queue behind the stuck Collector too"
        );
    }

    #[test]
    fn the_collector_resumes_once_a_dispatcher_makes_room() {
        let mut sim = Speedd::new();
        // subscribe dispatcher 0 before filling road 7
        sim.click_dispatcher(0);
        sim.click_collector();
        sim.click_dispatcher(0);
        for _ in 0..=QUEUE_CAPACITY {
            for camera in [0, 1] {
                sim.click_camera(camera, ROCKET);
                sim.click_collector();
            }
        }
        assert!(sim.collector_stuck());
        sim.click_dispatcher(0);
        assert!(
            !sim.collector_stuck(),
            "the freed slot took the parked ticket"
        );
        assert_eq!(sim.queue(7).map(|q| q.len()), Some(QUEUE_CAPACITY));
        let step = sim.click_collector();
        assert!(step.note.contains("returns"));
        assert_eq!(sim.collector(), Collector::Ready);
    }
}
