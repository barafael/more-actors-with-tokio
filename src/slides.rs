use dioxus::prelude::*;

use crate::games::broadcast::BroadcastGame;
use crate::games::call::CallGame;
use crate::games::chat::ChatGame;
use crate::games::cycle::CycleGame;
use crate::games::futures::{ButtonGame, LoopSelectGame, SelectGame, SpawnGame, TimerGame};
use crate::games::mpsc::MpscGame;
use crate::games::mutex::MutexGame;
use crate::games::oneshot::OneshotGame;
use crate::games::pipeline::PipelineGame;
use crate::games::speedd::SpeeddGame;
use crate::games::unit_test::{UnitTestGame, EVENT_LOOP, GET_UNIQUE_ID};
use crate::games::unordered::UnorderedGame;
use crate::games::watch::WatchGame;
use crate::games::watchdog::WatchdogGame;

/// Declares the deck: each slide's export name and what it shows, in order.
/// Expands to the names list and the index-to-slide function, so adding or
/// moving a slide is one line here — the deck, the slide count and the
/// export all follow.
///
/// A slide is a component, or `section "Title"` for a chapter divider, or
/// `topic "Title"` for a heading still waiting for its content, or
/// `code "Title", CONST` for a heading over a short code example.
macro_rules! deck {
    ($(($name:literal, $($slide:tt)+)),* $(,)?) => {
        /// Every slide's name, in deck order: the export's file names.
        pub const SLIDE_NAMES: &[&str] = &[$($name),*];

        /// How many slides the deck has.
        pub const SLIDE_COUNT: usize = SLIDE_NAMES.len();

        /// The slide at `index` in the deck.
        pub fn slide(index: usize) -> Element {
            let mut position = 0;
            $(
                if index == position {
                    return slide!($($slide)+);
                }
                position += 1;
            )*
            let _ = position;
            rsx! {}
        }
    };
}

/// One `deck!` entry's slide.
macro_rules! slide {
    (section $title:literal) => {
        rsx! { Section { title: $title } }
    };
    (topic $title:literal) => {
        rsx! { Topic { title: $title } }
    };
    (code $title:literal, $code:ident) => {
        rsx! { Snippet { title: $title, code: $code } }
    };
    ($component:ident) => {
        rsx! { $component {} }
    };
}

// schedule.md's chapters: the hook (2 min), the prerequisites (10 min:
// futures, combining them, spawning, channels), the pattern (12 min, the
// blog post), its consequences for architecture (12 min), and the closing
// (4 min: Kay, ownership as responsibility, credit to Alice Ryhl, and a
// call to report problems with the pattern on the blog repo). Then a
// divider for whatever is left over.
deck! {
    // the hook
    ("title", Title),
    ("prelude", PreludeSlide),
    ("mutex", MutexGameSlide),

    ("prerequisites", section "Prerequisites"),
    // futures
    ("timer", TimerGameSlide),
    ("button-future", ButtonGameSlide),
    // combining futures
    ("select", SelectGameSlide),
    ("loop-select", LoopSelectGameSlide),
    // spawning
    ("spawn", SpawnGameSlide),
    // channels
    ("mpsc", MpscGameSlide),
    ("oneshot", OneshotGameSlide),
    // all of it together: the loop-select, spawned, with an mpsc inbox and
    // a oneshot out
    ("watchdog", WatchdogSlide),
    // its pitfalls, now that a select over a channel has been seen
    ("select-pitfalls", SelectPitfallsSlide),

    ("the-pattern", section "The Pattern"),
    ("recipe", Recipe),
    ("message-enums", code "Message enums", MESSAGE_ENUM_CODE),
    ("event-loop", code "The event loop as consuming method", EVENT_LOOP),
    ("spawning", code "Leave the spawning to the user", SPAWNING_CODE),
    ("no-handles", code "No actor handles", GET_UNIQUE_ID),
    ("unit-test", UnitTestSlide),
    ("call-and-response", CallGameSlide),
    // opinions
    ("no-framework", topic "There is no Rust actor framework, and it's called tokio"),
    ("no-heartbeat", topic "Don't make a Heartbeat actor"),
    ("no-actor", topic "The only thing better than an actor: no actor"),

    ("architecture", section "Architecture Consequences"),
    // cancellation cascade
    ("futures-unordered", UnorderedSlide),
    ("pipeline", PipelineSlide),
    ("explicit-shutdown", topic "Explicit shutdown"),
    ("panic-safety", topic "Panic safety"),
    // deadlocks: keep the topology a DAG
    ("cycle", CycleGameSlide),
    // other channels
    ("broadcast", BroadcastGameSlide),
    ("watch", WatchGameSlide),
    // in practice
    ("budget-chat", ChatSlide),
    ("speedd", SpeeddSlide),

    ("closing", section "Closing"),
    ("original-oop", KaySlide),
    ("ownership", OwnershipSlide),
    ("thanks", topic "Thanks, Alice Ryhl"),
    ("report-issues", topic "Found a problem with the pattern? Tell me"),

    ("bonus", BonusSlide),
    ("transports", TransportsSlide),
}

/// A chapter divider.
#[component]
fn Section(title: String) -> Element {
    rsx! {
        div { class: "slide lead",
            h1 { "{title}" }
        }
    }
}

/// A topic's heading, before it has content.
#[component]
fn Topic(title: String) -> Element {
    rsx! {
        div { class: "slide",
            h2 { "{title}" }
        }
    }
}

/// A topic's heading over a short code example.
#[component]
fn Snippet(title: String, code: &'static str) -> Element {
    rsx! {
        div { class: "slide",
            h2 { "{title}" }
            Code { code }
        }
    }
}

/// A short block of highlighted Rust.
#[component]
fn Code(code: &'static str) -> Element {
    let lines = use_hook(|| crate::highlight::highlight_lines(code, "rust"));
    rsx! {
        div { class: "snippet",
            for (line, html) in lines.iter().enumerate() {
                div { key: "{line}", class: "snippet-line", dangerous_inner_html: "{html}" }
            }
        }
    }
}

const SPAWNING_CODE: &str = r#"// await it right here
let service = actor.event_loop(rx).await;

// or spawn it, and keep the handle
let handle = tokio::spawn(actor.event_loop(rx));

// or poll it alongside others
actors.push(actor.event_loop(rx)); // FuturesUnordered"#;

/// The stepper's `Message`, and the shape most actors take instead.
const MESSAGE_ENUM_CODE: &str = r#"/// UniqueIdService's one message: call and response.
pub enum Message {
    GetUniqueId { callback: oneshot::Sender<u32> },
}

/// Most actors take commands: plain data, nobody waits.
pub enum Command {
    Increment { by: u32 },
    Reset,
}"#;

#[component]
pub fn Title() -> Element {
    rsx! {
        div { class: "slide lead title-slide",
            h1 { "More Actors with Tokio" }
            h3 { "Or: how not to " code { "Arc<Mutex<T>>" } }
            h4 { "(and " i { "why" } ")" }
        }
    }
}

/// The prelude: the chain of reasoning the talk stands on, each clause
/// picking up the word the last one ended on, down to ownership.
#[component]
pub fn PreludeSlide() -> Element {
    rsx! {
        div { class: "slide lead",
            div { class: "prelude",
                p { span { class: "prelude-term", "Communication" } " implies " span { class: "prelude-term", "trust" } ";" }
                p { span { class: "prelude-term", "trust" } " requires " span { class: "prelude-term", "responsibility" } ";" }
                p { span { class: "prelude-term", "responsibility" } " means " strong { "ownership" } "." }
            }
        }
    }
}

/// CONCEPT.md §1: the hook. Shared state behind a lock, and every task
/// holding the `Arc`.
#[component]
pub fn MutexGameSlide() -> Element {
    rsx! {
        div { class: "slide",
            h3 { code { "Arc<Mutex<Resource>>" }}
            p { class: "dim", "Shared Ownership (Arc) with Synchronized Mutability (Mutex)." }
            MutexGame {}
        }
    }
}

/// "Responsibility is ownership", from the blog: back where the prelude
/// ended, now that the pattern has shown it.
#[component]
pub fn OwnershipSlide() -> Element {
    rsx! {
        div { class: "slide",
            h2 { "Ownership is Responsibility" }
            div { class: "recipe",
                ul { class: "recipe-rules",
                    li { strong { "Whoever owns a value is responsible for it." } " Everyone else asks." }
                    li { strong { "Architecture distributes responsibility." } " So it distributes ownership." }
                    li { strong { "Channels transfer ownership." } " A message is a value handed over, not lent." }
                    li { strong { "Borrowing means time-coupling." } " A shared reference ties every holder to every other." }
                }
            }
        }
    }
}

/// CONCEPT.md §2: the first future in the deck, and the first thing that
/// happens without anyone doing it.
#[component]
pub fn TimerGameSlide() -> Element {
    rsx! {
        div { class: "slide",
            h2 { "Waiting for Time to Pass" }
            p { class: "dim", "Sleeps for the seconds you give it, from 0 to 10, then yields them." }
            TimerGame {}
        }
    }
}

/// CONCEPT.md §3: both futures, raced.
#[component]
pub fn SelectGameSlide() -> Element {
    rsx! {
        div { class: "slide",
            h2 { "Waiting for Multiple Futures" }
            p { class: "dim", "Two futures, awaited together. The first to complete wins; the other is dropped mid-flight, never resumed." }
            SelectGame {}
        }
    }
}

/// The race from §3, handed to `tokio::spawn`: the same future, now a task
/// the caller holds only a handle to.
#[component]
pub fn SpawnGameSlide() -> Element {
    rsx! {
        div { class: "slide",
            h2 { "Spawning the select" }
            p { class: "dim", "The loop-select, moved into a task. tokio::spawn starts it once and it runs on its own, forever: the JoinHandle it hands back never resolves." }
            SpawnGame {}
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
            p { class: "dim", "Take the winner, go around again. A loop, a select, and state nothing outside can reach: an actor, minus the inbox." }
            LoopSelectGame {}
        }
    }
}

/// The loop-select's pitfalls, named and left to the post that covers them.
#[component]
pub fn SelectPitfallsSlide() -> Element {
    rsx! {
        div { class: "slide",
            h2 { "Loop-select pitfalls" }
            div { class: "recipe",
                ul { class: "recipe-rules",
                    li { strong { "Cancellation safety." } " The losing branch is dropped mid-flight; whatever it already did stays done." }
                    li { strong { "The panic." } " Every branch disabled and no " code { "else" } ": " code { "select!" } " panics." }
                    li { strong { "The busy loop." } " Once every channel is closed, " code { "None" } " matches at once, every time, forever." }
                    li { strong { "The macro." } " rustfmt and rust-analyzer give up inside " code { "select!" } ". Keep it small." }
                }
                p { class: "dim pitfalls-link",
                    "All four, in detail: "
                    a { href: "https://barafael.github.io/posts/stop-worrying-and-learn-to-loop-select/",
                        "Stop Worrying and Learn to Loop-Select"
                    }
                }
            }
        }
    }
}

#[component]
pub fn ButtonGameSlide() -> Element {
    rsx! {
        div { class: "slide",
            h2 { "Waiting for User Input" }
            p { class: "dim", "This is not an actor — just a future. Await it, then do real I/O: the future resolves with whichever button is pressed." }
            ButtonGame {}
        }
    }
}

#[component]
pub fn MpscGameSlide() -> Element {
    rsx! {
        div { class: "slide",
            h2 { "The mpsc channel" }
            p { class: "dim", "One bounded buffer, many senders. send().await on a full channel parks the sender until a receive frees a slot." }
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
            p { class: "dim", "A sends to B, B sends to A, both inboxes bounded. Fill both and each waits for the other to receive. Keep the topology a DAG. And what does this mean for shutdown?" }
            CycleGame {}
        }
    }
}

#[component]
pub fn OneshotGameSlide() -> Element {
    rsx! {
        div { class: "slide",
            h2 { "The oneshot channel" }
            p { class: "dim", "One value, once. Every call takes its handle by value, and dropping either half is how the other finds out." }
            OneshotGame {}
        }
    }
}

#[component]
pub fn CallGameSlide() -> Element {
    rsx! {
        div { class: "slide",
            h2 { "Call and response" }
            p { class: "dim", "An mpsc message carrying a oneshot callback. A task asks, then waits on its receiver; you play the actor's event loop that answers." }
            CallGame {}
        }
    }
}

/// CONCEPT.md §5: the loop-select needs somewhere to run. The watchdog
/// crate's actor is exactly that loop, with an inbox and a way out.
#[component]
pub fn WatchdogSlide() -> Element {
    rsx! {
        div { class: "slide",
            h2 { "The watchdog actor" }
            p { class: "dim", "A watchdog: the same select!, now owning an inbox and a oneshot. Feed it, or it fires." }
            WatchdogGame {}
        }
    }
}

/// Budget Chat without the lock: a room actor owns the member list that
/// the original kept in an Arc<Mutex<HashMap>>.
#[component]
pub fn ChatSlide() -> Element {
    rsx! {
        div { class: "slide",
            h2 { "A chat room, without the lock" }
            p { class: "dim", "Budget Chat, where a room actor owns the member list instead of a lock." }
            ChatGame {}
        }
    }
}

/// FuturesUnordered on its own, before the pipeline puts one in each stage.
#[component]
pub fn UnorderedSlide() -> Element {
    rsx! {
        div { class: "slide",
            h2 { "FuturesUnordered" }
            p { class: "dim", "FuturesUnordered: push futures in, and next() hands back whichever finishes first." }
            UnorderedGame {}
        }
    }
}

/// A pipeline: halres-downloader's stages, and the shutdown that walks down
/// them once the reader drops its sender.
#[component]
pub fn PipelineSlide() -> Element {
    rsx! {
        div { class: "slide",
            h2 { "A pipeline" }
            p { class: "dim", "halres-downloader: each stage takes work while it has room and passes on whatever finishes first." }
            PipelineGame {}
        }
    }
}

/// CONCEPT.md §8: channels determine architecture — the Protohackers Speed
/// Daemon, stepped one actor at a time.
#[component]
pub fn SpeeddSlide() -> Element {
    rsx! {
        div { class: "slide",
            h2 { "An architecture, by hand" }
            p { class: "dim", "speedd, the Protohackers speed daemon: click an actor to run one step of its loop." }
            SpeeddGame {}
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

/// The divider after Kay, for extra material if there are questions or time
/// left over.
#[component]
pub fn BonusSlide() -> Element {
    rsx! {
        div { class: "slide lead",
            h1 { "Bonus Material" }
        }
    }
}

/// Generic over `Stream`/`Sink`, the actor never learns which wire a
/// message came in on: every transport is an adapter into the same inbox.
#[component]
pub fn TransportsSlide() -> Element {
    const TRANSPORTS: [(&str, &str); 4] = [
        ("TCP", "JSON"),
        ("TCP", "protobuf"),
        ("WebSocket", "JSON"),
        ("WebTransport", "postcard"),
    ];
    rsx! {
        div { class: "slide",
            h2 { "One actor, any wire" }
            p { class: "dim", "Inject resources by type: Stream, Sink, AsyncRead. Each transport and codec becomes an adapter, and they all send to the same actor." }
            div { class: "transports",
                div { class: "transport-list",
                    for (wire, codec) in TRANSPORTS {
                        div { class: "transport",
                            span { class: "transport-wire", "{wire}" }
                            span { class: "transport-codec", "{codec}" }
                        }
                    }
                }
                div { class: "transport-arrows", "→" }
                div { class: "transport-adapter", code { "impl Stream<Item = Message>" } }
                div { class: "transport-arrows", "→" }
                div { class: "transport-actor", code { "mpsc::Sender<Message>" } span { "one actor" } }
            }
            Code { code: TRANSPORT_CODE }
        }
    }
}

const TRANSPORT_CODE: &str = r#"async fn forward(mut frames: impl Stream<Item = Message> + Unpin, actor: Sender<Message>) {
    while let Some(message) = frames.next().await {
        if actor.send(message).await.is_err() { break; }
    }
}"#;

#[component]
pub fn WatchGameSlide() -> Element {
    rsx! {
        div { class: "slide",
            h2 { "The watch channel" }
            p { class: "dim", "One value, many receivers. borrow() reveals it; changed() waits for the next send." }
            WatchGame {}
        }
    }
}

#[component]
pub fn BroadcastGameSlide() -> Element {
    rsx! {
        div { class: "slide",
            h2 { "The broadcast channel" }
            p { class: "dim", "Sending never blocks: a full buffer evicts its oldest value. Fall behind and your next receive yields Lagged(n)." }
            BroadcastGame {}
        }
    }
}

#[component]
pub fn Recipe() -> Element {
    rsx! {
        div { class: "slide",
            h2 { "The actor recipe" }
            div { class: "recipe with-code",
                ul { class: "recipe-rules",
                    li { strong { "The actor is its data." } " No channels, no sockets as fields: the loop owns the runtime resources." }
                    li { strong { "The loop consumes and returns " } code { "Self" } strong { "." } " Inspect the final state, or run it again." }
                    li { strong { "Spawning is the caller's call." } " Await it, spawn it, or put it in a " code { "FuturesUnordered" } "." }
                    li { strong { "No handle type." } " An associated function over the " code { "Sender" } " keeps " code { "closed()" } " and friends in reach — and hands back the receiver." }
                    li { strong { "Natural shutdown." } " Drop the last sender; " code { "recv()" } " drains, then yields " code { "None" } "." }
                }
            }
            Code { code: ACTOR_DATA_CODE }
        }
    }
}

const ACTOR_DATA_CODE: &str = r#"pub struct UniqueIdService {
    next_id: u32,
}"#;

/// The recipe's payoff: the article's unit test, stepped line by line like
/// a debugger.
#[component]
pub fn UnitTestSlide() -> Element {
    rsx! {
        div { class: "slide",
            h2 { "A test with nothing to race" }
            UnitTestGame {}
        }
    }
}

/// The viewer's navigation overlay, in the marp idiom: it fades in when
/// the mouse moves and fades back out when it rests.
///
/// A talk is mostly a still image, and a control bar parked over every
/// slide is one more thing on the projector. Keyboard navigation is the
/// primary interface — this is for when you are holding a mouse.
#[component]
pub fn Chrome(slide: Signal<usize>, awake: Signal<bool>) -> Element {
    let current = slide() + 1;

    rsx! {
        div { class: "chrome-zone",
            div {
                class: if awake() { "chrome awake" } else { "chrome" },
                button {
                    class: "nav",
                    aria_label: "previous slide",
                    onclick: move |_| crate::step_slide(slide, false),
                    "←"
                }
                span { class: "counter", "{current} / {SLIDE_COUNT}" }
                button {
                    class: "nav",
                    aria_label: "next slide",
                    onclick: move |_| crate::step_slide(slide, true),
                    "→"
                }
            }
        }
    }
}
