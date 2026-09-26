use dioxus::prelude::*;

use crate::games::broadcast::BroadcastGame;
use crate::games::button::ButtonGame;
use crate::games::call::CallGame;
use crate::games::cycle::CycleGame;
use crate::games::mpsc::MpscGame;
use crate::games::mutex::MutexGame;
use crate::games::oneshot::OneshotGame;
use crate::games::select::{LoopSelectGame, SelectGame};
use crate::games::timer::TimerGame;
use crate::games::unit_test::UnitTestGame;
use crate::games::watch::WatchGame;
use crate::{protocol::SLIDE_COUNT, AppCtx, GameMode};

#[component]
pub fn Title() -> Element {
    rsx! {
        div { class: "slide lead",
            h1 { "Actors with Tokio" }
            h2 { "Live minigames" }
            p { class: "dim", "You are the senders." }
        }
    }
}

/// CONCEPT.md §1: the hook. Shared state behind a lock, and the whole room
/// holding the `Arc`.
#[component]
pub fn MutexGameSlide() -> Element {
    rsx! {
        div { class: "slide",
            h2 { "Arc<Mutex<T>>" }
            p { class: "dim", "Safe: only the guard's holder can touch the value. That is all it buys. Nobody else can do anything but wait, for as long as the holder likes." }
            MutexGame {}
        }
    }
}

/// CONCEPT.md §2: the first future in the deck, and the first thing in the
/// talk that happens without anyone in the room doing it.
#[component]
pub fn TimerGameSlide() -> Element {
    rsx! {
        div { class: "slide",
            h2 { "A future that waits" }
            p { class: "dim", "Awaiting it yields nothing until the seconds reach a multiple of ten — then it yields how long it waited. Nobody in this room makes that happen." }
            TimerGame {}
        }
    }
}

/// CONCEPT.md §3: both futures, raced.
#[component]
pub fn SelectGameSlide() -> Element {
    rsx! {
        div { class: "slide",
            h2 { "select!" }
            p { class: "dim", "Two futures, awaited together. The first to complete wins and yields its value; the other is dropped mid-flight — never awaited, never resumed." }
            SelectGame {}
        }
    }
}

/// CONCEPT.md §4: the race, forever. The last slide before an actor appears,
/// and deliberately already shaped like one.
#[component]
pub fn LoopSelectGameSlide() -> Element {
    rsx! {
        div { class: "slide",
            h2 { "The loop around the select" }
            p { class: "dim", "Take the winner, go around again. A loop, a select, and state nothing outside the loop can reach — everything an actor is, except an inbox." }
            LoopSelectGame {}
        }
    }
}

#[component]
pub fn ButtonGameSlide() -> Element {
    rsx! {
        div { class: "slide",
            h2 { "The button future" }
            p { class: "dim", "This is not an actor — just a future. Activate it, then do real I/O: the future resolves with whichever button is pressed." }
            ButtonGame {}
        }
    }
}

#[component]
pub fn MpscGameSlide() -> Element {
    rsx! {
        div { class: "slide",
            h2 { "The mpsc channel" }
            p { class: "dim", "One bounded buffer, many senders. You are a sender: send on a full channel and you block until a receive frees a slot." }
            MpscGame {}
        }
    }
}

/// The cycle footgun: bounded channels in a loop.
#[component]
pub fn CycleGameSlide() -> Element {
    rsx! {
        div { class: "slide",
            h2 { "Two actors, one cycle" }
            p { class: "dim", "A sends to B, B sends to A, both inboxes bounded. Backpressure is what makes it seize: fill both, and each waits for the other to receive. Keep the topology a DAG." }
            CycleGame {}
        }
    }
}

#[component]
pub fn OneshotGameSlide() -> Element {
    rsx! {
        div { class: "slide",
            h2 { "The oneshot channel" }
            p { class: "dim", "One value, once. Every call takes its handle by value — send and await can each happen exactly once, and dropping either half is how the other finds out." }
            OneshotGame {}
        }
    }
}

#[component]
pub fn CallGameSlide() -> Element {
    rsx! {
        div { class: "slide",
            h2 { "Call and response" }
            p { class: "dim", "An mpsc message carrying a oneshot callback. You ask, then wait on your receiver. The presenter is the event loop — nobody gets an answer until it gets to them." }
            CallGame {}
        }
    }
}

/// CONCEPT.md §10, sketched: Alan Kay's definition of OOP, read as a
/// description of the actors the deck just built.
#[component]
pub fn KaySlide() -> Element {
    rsx! {
        div { class: "slide",
            h2 { "Original OOP" }
            blockquote { class: "kay-quote",
                "OOP to me means only messaging, local retention and protection and hiding of state-process, and extreme late-binding of all things."
                footer { class: "dim", "— Alan Kay" }
            }
            div { class: "kay-map",
                div { class: "kay-row",
                    span { class: "kay-term", "messaging" }
                    span { class: "kay-means", "values moving along channels, ownership and all" }
                }
                div { class: "kay-row",
                    span { class: "kay-term", "local retention" }
                    span { class: "kay-means", "state lives inside the running event loop; nothing outside can reach it" }
                }
                div { class: "kay-row",
                    span { class: "kay-term", "protection" }
                    span { class: "kay-means", "a task is a panic boundary, and a message holds no borrowed references" }
                }
                div { class: "kay-row",
                    span { class: "kay-term", "hiding" }
                    span { class: "kay-means", "the state and its handler are private" }
                }
                div { class: "kay-row",
                    span { class: "kay-term", "late binding" }
                    span { class: "kay-means", "a Sender<Message> is a vtable whose target is chosen at spawn time" }
                }
            }
        }
    }
}

#[component]
pub fn WatchGameSlide() -> Element {
    rsx! {
        div { class: "slide",
            h2 { "The watch channel" }
            p { class: "dim", "One value, many receivers. borrow() reveals it; changed() waits for the next different one." }
            WatchGame {}
        }
    }
}

#[component]
pub fn BroadcastGameSlide() -> Element {
    rsx! {
        div { class: "slide",
            h2 { "The broadcast channel" }
            p { class: "dim", "Sending never blocks: a full buffer evicts its oldest value. A new receiver starts at the tail — it never sees history. Fall behind and your next receive yields Lagged(n)." }
            BroadcastGame {}
        }
    }
}

#[component]
pub fn Recipe() -> Element {
    rsx! {
        div { class: "slide",
            h2 { "The actor recipe" }
            div { class: "recipe",
                ul { class: "recipe-rules",
                    li { strong { "The actor is its data." } " No channels, no sockets as fields: the loop owns the runtime resources." }
                    li { strong { "The loop consumes and returns " } code { "Self" } strong { "." } " Inspect the final state, or run it again." }
                    li { strong { "Spawning is the caller's call." } " Await it, spawn it, or put it in a " code { "FuturesUnordered" } "." }
                    li { strong { "No handle type." } " An associated function over the " code { "Sender" } " keeps " code { "closed()" } " and friends in reach — and hands back the receiver." }
                    li { strong { "Natural shutdown." } " Drop the last sender; " code { "recv()" } " drains, then yields " code { "None" } "." }
                }
            }
        }
    }
}

/// The recipe's payoff: the article's unit test, stepped line by line. Each
/// viewer drives their own copy — it is a debugger, not a crowd game.
#[component]
pub fn UnitTestSlide() -> Element {
    rsx! {
        div { class: "slide",
            h2 { "A test with nothing to race" }
            UnitTestGame {}
        }
    }
}

/// The presenter's navigation overlay, in the marp idiom: it fades in when
/// the mouse moves and fades back out when it rests.
///
/// A talk is mostly a still image, and a control bar parked over every
/// slide is one more thing on the projector. Keyboard navigation is the
/// primary interface — this is for when the presenter is holding a mouse.
#[component]
pub fn Chrome(slide: Signal<usize>, awake: Signal<bool>) -> Element {
    let ctx: AppCtx = use_context();
    let mode = use_context::<GameMode>();
    let current = slide() + 1;

    // Only the presenter drives the deck. The server refuses these commands
    // from anyone else, so this hides a control that would not have worked
    // rather than enforcing anything. No hooks run above this point, so the
    // early return cannot desynchronise them.
    if !ctx.may_present() {
        return rsx! {};
    }

    rsx! {
        div { class: "chrome-zone",
            div {
                class: if awake() { "chrome awake" } else { "chrome" },
                button {
                    class: "nav",
                    aria_label: "previous slide",
                    onclick: move |_| ctx.step_slide(mode, slide, false),
                    "←"
                }
                span { class: "counter", "{current} / {SLIDE_COUNT}" }
                button {
                    class: "nav",
                    aria_label: "next slide",
                    onclick: move |_| ctx.step_slide(mode, slide, true),
                    "→"
                }
            }
        }
    }
}
