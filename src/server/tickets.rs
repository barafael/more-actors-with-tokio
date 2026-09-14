//! The player-ticket pool: a fixed number of seats in the room.
//!
//! CONCEPT.md gives the audience 24 handles, so "no capacity left" is
//! something the room runs into rather than a number quoted on a slide.
//! That makes the pool itself a channel story, and it is modelled as one:
//! taking a ticket is a request to an actor, and the free seats are its
//! state. There is no lock and no shared counter.
//!
//! A ticket outlives the socket that claimed it. The audience keeps a URL
//! in a phone browser that will background itself, drop wifi and come back;
//! reconnecting with the same ticket must return the same seat, or a person
//! loses their handle by locking their screen. Seats are therefore reclaimed
//! on expiry, not on disconnect.

use std::collections::{BTreeSet, HashMap};
use std::time::{Duration, Instant};

use tokio::sync::{mpsc, oneshot};

use crate::protocol::{Role, PLAYER_TICKETS};

/// How long a ticket survives with nobody holding it open.
///
/// Longer than `DEAD_PEER_TIMEOUT` on purpose: losing the socket is normal
/// for a phone, losing the seat is not. A ticket is only recycled once its
/// holder has plausibly left the room.
const TICKET_TTL: Duration = Duration::from_secs(15 * 60);

/// A ticket's secret. Opaque to everyone but the holder; possession is the
/// whole proof, so it must be unguessable rather than sequential.
pub type Ticket = String;

#[derive(Debug)]
struct Seat {
    /// Seat number, 1-based: this is what the audience sees, so it must be
    /// stable for the ticket's life.
    number: usize,
    /// Live sockets currently holding this ticket open. A phone with the
    /// deck and a game slide open counts twice.
    holders: usize,
    /// When the last holder let go. `None` while anyone is connected.
    idle_since: Option<Instant>,
}

enum Request {
    /// Present a ticket, and optionally ask for a fresh seat if it does not
    /// name a live one.
    Claim {
        ticket: Option<Ticket>,
        issue: Issue,
        reply: oneshot::Sender<Claim>,
    },
    /// A socket holding `ticket` closed.
    Release { ticket: Ticket },
    /// How many seats are taken, for the presenter's headcount.
    Count { reply: oneshot::Sender<Headcount> },
}

/// Whether a connection may be given a seat it did not arrive with.
///
/// Only the page the QR code points at issues seats. If any socket could,
/// the pool would not be a pool: a spectator would be promoted to player
/// simply by opening a game, and the 24 handles would never run out.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Issue {
    /// Grant a free seat when the presented ticket is absent or stale.
    IfAvailable,
    /// Honour an existing ticket, but never mint one.
    Never,
}

/// The outcome of presenting credentials.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Claim {
    pub role: Role,
    /// The ticket to keep using. Echoed back so a client that arrived
    /// without one can store the seat it was granted.
    pub ticket: Option<Ticket>,
    /// 1-based seat number, for display.
    pub seat: Option<usize>,
}

impl Claim {
    /// Nobody is turned away — they simply cannot act.
    fn spectator() -> Self {
        Self {
            role: Role::Spectator,
            ticket: None,
            seat: None,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Headcount {
    /// Seats claimed, whether or not their holder is connected right now.
    pub claimed: usize,
    /// Seats with at least one live socket.
    pub present: usize,
    pub capacity: usize,
}

/// Handle to the ticket actor.
#[derive(Clone)]
pub struct Tickets(mpsc::UnboundedSender<Request>);

impl Tickets {
    /// Spawn the pool actor and return its handle.
    pub fn spawn() -> Self {
        let (tx, rx) = mpsc::unbounded_channel();
        tokio::spawn(run(rx));
        Self(tx)
    }

    /// Rule on the credentials a connecting socket presented.
    ///
    /// `presenter_ok` is decided by the caller, which compares the secret,
    /// so the pool never sees the key. A presenter also takes a seat,
    /// because the presenter plays too.
    pub async fn claim(&self, ticket: Option<Ticket>, issue: Issue, presenter_ok: bool) -> Claim {
        let (reply, response) = oneshot::channel();
        if self
            .0
            .send(Request::Claim {
                ticket,
                issue,
                reply,
            })
            .is_err()
        {
            tracing::error!("ticket pool is gone");
            return Claim::spectator();
        }
        let mut claim = response.await.unwrap_or_else(|_| Claim::spectator());
        if presenter_ok {
            // The key outranks the pool: a presenter who arrives after the
            // seats are gone still presents.
            claim.role = Role::Presenter;
        }
        claim
    }

    /// Report that a socket holding this ticket has closed.
    pub fn release(&self, ticket: Ticket) {
        self.0
            .send(Request::Release { ticket })
            .inspect_err(|error| tracing::warn!(%error, "ticket pool is gone"))
            .ok();
    }

    pub async fn headcount(&self) -> Headcount {
        let (reply, response) = oneshot::channel();
        let empty = Headcount {
            claimed: 0,
            present: 0,
            capacity: PLAYER_TICKETS,
        };
        if self.0.send(Request::Count { reply }).is_err() {
            return empty;
        }
        response.await.unwrap_or(empty)
    }
}

async fn run(mut rx: mpsc::UnboundedReceiver<Request>) {
    let mut pool = Pool::new();
    while let Some(request) = rx.recv().await {
        match request {
            Request::Claim {
                ticket,
                issue,
                reply,
            } => {
                let claim = pool.claim(ticket, issue);
                // the caller may have given up waiting; that is not an error
                reply.send(claim).ok();
            }
            Request::Release { ticket } => pool.release(&ticket),
            Request::Count { reply } => {
                reply.send(pool.headcount()).ok();
            }
        }
    }
}

/// The pool's state machine, kept free of tokio so it can be tested
/// directly — the same split the channel sims use.
struct Pool {
    seats: HashMap<Ticket, Seat>,
    /// Seat numbers nobody holds, smallest first, so the room fills up from
    /// seat 1 and the numbers on stage stay tidy.
    free: BTreeSet<usize>,
    minted: u64,
}

impl Pool {
    fn new() -> Self {
        Self {
            seats: HashMap::new(),
            free: (1..=PLAYER_TICKETS).collect(),
            minted: 0,
        }
    }

    fn claim(&mut self, ticket: Option<Ticket>, issue: Issue) -> Claim {
        self.expire();

        // A returning holder keeps their seat, even past the point where
        // the pool is otherwise full.
        if let Some(ticket) = ticket {
            if let Some(seat) = self.seats.get_mut(&ticket) {
                seat.holders += 1;
                seat.idle_since = None;
                return Claim {
                    role: Role::Player,
                    seat: Some(seat.number),
                    ticket: Some(ticket),
                };
            }
            // An unknown ticket is a stale one (restarted server, expired
            // seat). Fall through and issue a fresh seat if any remain.
        }

        if issue == Issue::Never {
            return Claim::spectator();
        }

        let Some(number) = self.free.iter().next().copied() else {
            return Claim::spectator();
        };
        self.free.remove(&number);

        let ticket = self.mint();
        self.seats.insert(
            ticket.clone(),
            Seat {
                number,
                holders: 1,
                idle_since: None,
            },
        );
        Claim {
            role: Role::Player,
            seat: Some(number),
            ticket: Some(ticket),
        }
    }

    fn release(&mut self, ticket: &str) {
        if let Some(seat) = self.seats.get_mut(ticket) {
            seat.holders = seat.holders.saturating_sub(1);
            if seat.holders == 0 {
                seat.idle_since = Some(Instant::now());
            }
        }
    }

    /// Recycle seats whose holder has been gone longer than the TTL.
    fn expire(&mut self) {
        let now = Instant::now();
        let dead: Vec<Ticket> = self
            .seats
            .iter()
            .filter(|(_, seat)| {
                seat.holders == 0
                    && seat
                        .idle_since
                        .is_some_and(|idle| now.duration_since(idle) >= TICKET_TTL)
            })
            .map(|(ticket, _)| ticket.clone())
            .collect();
        for ticket in dead {
            if let Some(seat) = self.seats.remove(&ticket) {
                self.free.insert(seat.number);
            }
        }
    }

    fn headcount(&self) -> Headcount {
        Headcount {
            claimed: self.seats.len(),
            present: self.seats.values().filter(|seat| seat.holders > 0).count(),
            capacity: PLAYER_TICKETS,
        }
    }

    /// Mint an unguessable ticket.
    ///
    /// Seat numbers are public and sequential; this is not. It mixes a
    /// counter with a per-call randomly seeded hasher and the clock, which
    /// is enough to stop an attendee guessing a neighbour's ticket over the
    /// length of a talk. It is not a cryptographic token and does not need
    /// to be — the worst case is one audience member playing as another.
    fn mint(&mut self) -> Ticket {
        use std::hash::{BuildHasher, Hasher};
        self.minted += 1;
        let mut hasher = std::collections::hash_map::RandomState::new().build_hasher();
        hasher.write_u64(self.minted);
        hasher.write_u128(
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|elapsed| elapsed.as_nanos())
                .unwrap_or_default(),
        );
        let high = hasher.finish();
        hasher.write_u64(high);
        format!("{high:016x}{:016x}", hasher.finish())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_fresh_claim_takes_the_lowest_free_seat() {
        let mut pool = Pool::new();
        assert_eq!(pool.claim(None, Issue::IfAvailable).seat, Some(1));
        assert_eq!(pool.claim(None, Issue::IfAvailable).seat, Some(2));
    }

    #[test]
    fn presenting_a_ticket_returns_the_same_seat() {
        let mut pool = Pool::new();
        let first = pool.claim(None, Issue::IfAvailable);
        let again = pool.claim(first.ticket.clone(), Issue::IfAvailable);
        assert_eq!(again.seat, first.seat);
        assert_eq!(again.ticket, first.ticket);
    }

    #[test]
    fn tickets_do_not_share_a_prefix() {
        // sequential secrets would collide on everything but the last bytes
        let mut pool = Pool::new();
        let first = pool.claim(None, Issue::IfAvailable).ticket.expect("seat");
        let second = pool.claim(None, Issue::IfAvailable).ticket.expect("seat");
        assert_ne!(first, second);
        assert_ne!(first[..8], second[..8]);
    }

    #[test]
    fn the_pool_runs_out_and_seats_spectators() {
        let mut pool = Pool::new();
        for _ in 0..PLAYER_TICKETS {
            assert_eq!(pool.claim(None, Issue::IfAvailable).role, Role::Player);
        }
        let turned_away = pool.claim(None, Issue::IfAvailable);
        assert_eq!(turned_away.role, Role::Spectator);
        assert_eq!(turned_away.seat, None);
        assert_eq!(turned_away.ticket, None);
    }

    #[test]
    fn a_disconnect_does_not_free_the_seat() {
        // a phone locking its screen must not lose the person's handle
        let mut pool = Pool::new();
        let claim = pool.claim(None, Issue::IfAvailable);
        let ticket = claim.ticket.expect("seat");
        pool.release(&ticket);
        assert_eq!(pool.headcount().claimed, 1);
        assert_eq!(pool.headcount().present, 0);
        assert_eq!(
            pool.claim(Some(ticket), Issue::IfAvailable).seat,
            claim.seat
        );
    }

    #[test]
    fn two_sockets_share_one_ticket() {
        // the deck socket and a game socket both hold the same ticket open
        let mut pool = Pool::new();
        let ticket = pool.claim(None, Issue::IfAvailable).ticket.expect("seat");
        pool.claim(Some(ticket.clone()), Issue::IfAvailable);
        pool.release(&ticket);
        assert_eq!(pool.headcount().present, 1, "one socket is still open");
        pool.release(&ticket);
        assert_eq!(pool.headcount().present, 0);
    }

    #[test]
    fn an_expired_seat_is_recycled_in_order() {
        let mut pool = Pool::new();
        let ticket = pool.claim(None, Issue::IfAvailable).ticket.expect("seat");
        pool.claim(None, Issue::IfAvailable);
        pool.release(&ticket);
        // force the seat past its TTL rather than waiting out the clock
        pool.seats.get_mut(&ticket).expect("seat").idle_since =
            Some(Instant::now() - TICKET_TTL - Duration::from_secs(1));

        let recycled = pool.claim(None, Issue::IfAvailable);
        assert_eq!(recycled.seat, Some(1), "seat 1 came free and is lowest");
        assert_ne!(recycled.ticket.as_deref(), Some(ticket.as_str()));
    }

    #[test]
    fn a_held_seat_never_expires() {
        let mut pool = Pool::new();
        let ticket = pool.claim(None, Issue::IfAvailable).ticket.expect("seat");
        // no release: the socket is still open, so idle_since stays None
        pool.expire();
        assert_eq!(pool.headcount().claimed, 1);
        assert_eq!(pool.claim(Some(ticket), Issue::IfAvailable).seat, Some(1));
    }

    #[test]
    fn a_stale_ticket_is_issued_a_new_seat() {
        // e.g. the server restarted; the phone still has the old URL
        let mut pool = Pool::new();
        let claim = pool.claim(Some("not-a-real-ticket".to_string()), Issue::IfAvailable);
        assert_eq!(claim.role, Role::Player);
        assert_eq!(claim.seat, Some(1));
        assert_ne!(claim.ticket.as_deref(), Some("not-a-real-ticket"));
    }

    #[test]
    fn a_stale_ticket_at_capacity_becomes_a_spectator() {
        let mut pool = Pool::new();
        for _ in 0..PLAYER_TICKETS {
            pool.claim(None, Issue::IfAvailable);
        }
        assert_eq!(
            pool.claim(Some("stale".to_string()), Issue::IfAvailable)
                .role,
            Role::Spectator
        );
    }

    #[test]
    fn a_game_socket_never_mints_a_seat() {
        // opening a game without a ticket must not promote a spectator:
        // scanning the code is what grants a handle
        let mut pool = Pool::new();
        let claim = pool.claim(None, Issue::Never);
        assert_eq!(claim.role, Role::Spectator);
        assert_eq!(claim.ticket, None);
        assert_eq!(pool.headcount().claimed, 0, "no seat was taken");
    }

    #[test]
    fn a_game_socket_still_honours_a_real_ticket() {
        let mut pool = Pool::new();
        let ticket = pool.claim(None, Issue::IfAvailable).ticket.expect("seat");
        let claim = pool.claim(Some(ticket), Issue::Never);
        assert_eq!(claim.role, Role::Player);
        assert_eq!(claim.seat, Some(1));
    }
}
