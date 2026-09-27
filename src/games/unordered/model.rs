//! `FuturesUnordered`: many futures, one task, completion order.
//!
//! The task owns a set of futures. `push` adds one; `next().await` hands
//! back whichever has completed first — or, if none has, parks the task
//! until a waker fires. Nothing in the set runs by itself: it makes progress
//! only while its owner polls it. An empty set is not pending: `next()`
//! yields `None` at once, which ends a `while let` and is why
//! halres-downloader guards its branch with `if in_progress > 0`.
//!
//! `FuturesOrdered` is the same set with one rule changed: it yields in
//! push order, so a finished future waits behind an unfinished head.
//!
//! The network is the player: clicking a future wakes it.

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Mode {
    Unordered,
    Ordered,
}

/// The pages the task can download, as in halres-downloader's `urls.csv`.
pub const URLS: [&str; 5] = [
    "crates.io",
    "quickref.me",
    "meet.jit.si",
    "berline.rs",
    "lib.rs",
];

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Fut {
    pub url: usize,
    /// When its waker fired, in firing order; `None` while pending.
    pub woke: Option<u64>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Node {
    Task,
    Set,
    Out,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Hop {
    pub from: Node,
    pub to: Node,
    pub url: usize,
}

/// What one click did: the hops it made and what to say about it.
pub type Step = crate::games::step::Step<Hop>;

pub struct Set {
    pub mode: Mode,
    /// In push order.
    pub futs: Vec<Fut>,
    pub yielded: Vec<usize>,
    /// Whether each page has been pushed yet.
    pub pushed: [bool; URLS.len()],
    /// The last `next()` found nothing ready: the task is parked.
    pub parked: bool,
    wakes: u64,
}

impl Set {
    pub fn new(mode: Mode) -> Self {
        Self {
            mode,
            futs: Vec::new(),
            yielded: Vec::new(),
            pushed: [false; URLS.len()],
            parked: false,
            wakes: 0,
        }
    }

    pub fn name(&self) -> &'static str {
        match self.mode {
            Mode::Unordered => "FuturesUnordered",
            Mode::Ordered => "FuturesOrdered",
        }
    }

    /// `set.push(download(url))`: the future is created, not run.
    pub fn click_push(&mut self, url: usize) -> Step {
        if self.pushed[url] {
            return Step::note("Already pushed.");
        }
        self.pushed[url] = true;
        self.futs.push(Fut { url, woke: None });
        Step {
            hops: vec![Hop {
                from: Node::Task,
                to: Node::Set,
                url,
            }],
            note: format!(
                "push(download({})): the future goes into the set. Nothing has run yet — pushing is not polling.",
                URLS[url]
            ),
        }
    }

    /// The network answers `url`'s request: its waker fires.
    pub fn click_wake(&mut self, url: usize) -> Step {
        let Some(fut) = self
            .futs
            .iter_mut()
            .find(|f| f.url == url && f.woke.is_none())
        else {
            return Step::note("That one is already ready.");
        };
        self.wakes += 1;
        fut.woke = Some(self.wakes);
        let woke_task = std::mem::take(&mut self.parked);
        Step::note(if woke_task {
            format!(
                "{} answers. Its waker wakes the parked task — it can call next() again.",
                URLS[url]
            )
        } else {
            format!(
                "{} answers: the future is ready, waiting to be yielded.",
                URLS[url]
            )
        })
    }

    /// `set.next().await`.
    pub fn click_next(&mut self) -> Step {
        if self.futs.is_empty() {
            self.parked = false;
            return Step::note(
                "The set is empty, so next() yields None at once — not Pending. A `while let Some(..)` loop would end here, which is why the pipeline only polls its set `if in_progress > 0`.",
            );
        }
        let pick = match self.mode {
            Mode::Unordered => self
                .futs
                .iter()
                .enumerate()
                .filter_map(|(i, f)| f.woke.map(|at| (at, i)))
                .min()
                .map(|(_, i)| i),
            Mode::Ordered => self.futs[0].woke.map(|_| 0),
        };
        let Some(index) = pick else {
            self.parked = true;
            let ready_behind = self.futs.iter().skip(1).any(|f| f.woke.is_some());
            return Step::note(if self.mode == Mode::Ordered && ready_behind {
                format!(
                    "Pending: a later future is ready, but FuturesOrdered must yield {} first. Head-of-line blocking — the task parks.",
                    URLS[self.futs[0].url]
                )
            } else {
                "Pending: nothing is ready, so the task parks until a waker fires.".to_string()
            });
        };
        let fut = self.futs.remove(index);
        self.parked = false;
        self.yielded.push(fut.url);
        let overtook = index > 0;
        Step {
            hops: vec![Hop {
                from: Node::Set,
                to: Node::Out,
                url: fut.url,
            }],
            note: if overtook {
                format!(
                    "next() yields Some({}): the first to finish, ahead of {} older one(s).",
                    URLS[fut.url], index
                )
            } else {
                format!("next() yields Some({}).", URLS[fut.url])
            },
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn with_three(mode: Mode) -> Set {
        let mut set = Set::new(mode);
        for url in 0..3 {
            set.click_push(url);
        }
        set
    }

    #[test]
    fn unordered_yields_in_the_order_they_finish() {
        let mut set = with_three(Mode::Unordered);
        for url in [2, 0, 1] {
            set.click_wake(url);
        }
        for _ in 0..3 {
            set.click_next();
        }
        assert_eq!(set.yielded, [2, 0, 1]);
    }

    #[test]
    fn ordered_yields_in_push_order_whatever_finishes_first() {
        let mut set = with_three(Mode::Ordered);
        set.click_wake(2);
        let step = set.click_next();
        assert!(step.note.contains("Head-of-line"), "{}", step.note);
        assert!(set.parked);
        set.click_wake(0);
        set.click_next();
        assert_eq!(set.yielded, [0]);
    }

    #[test]
    fn nothing_ready_parks_the_task_and_a_wake_unparks_it() {
        let mut set = with_three(Mode::Unordered);
        set.click_next();
        assert!(set.parked);
        let step = set.click_wake(1);
        assert!(step.note.contains("wakes the parked task"));
        assert!(!set.parked);
    }

    #[test]
    fn an_empty_set_yields_none_rather_than_pending() {
        let mut set = Set::new(Mode::Unordered);
        let step = set.click_next();
        assert!(step.note.contains("None"));
        assert!(!set.parked);
    }

    #[test]
    fn a_page_is_pushed_once() {
        let mut set = Set::new(Mode::Unordered);
        set.click_push(0);
        set.click_push(0);
        assert_eq!(set.futs.len(), 1);
    }
}
