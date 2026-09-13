//! Sterile, synchronous ("blocking") channel primitives mirroring the four
//! tokio channel types: [`oneshot`], [`mpsc`], [`broadcast`] and [`watch`].
//!
//! This crate exists for teaching and for the minigames: it models what the
//! tokio channels do, expressed with plain methods and outcome enums instead
//! of async futures. There is no tokio, no runtime and no time: every
//! operation either completes immediately or parks a thread (native only).
//!
//! # Layers
//!
//! Each channel module exposes two layers:
//!
//! * **Core** (e.g. [`mpsc::MpscCore`]): a pure state machine, free of locks,
//!   threads and clocks. Operations return outcome enums and blocked
//!   operations are modeled as explicit [`WaiterId`]s in FIFO queues, so wake
//!   order is deterministic and assertable without threads. This layer
//!   compiles to `wasm32-unknown-unknown` and is what the minigame drivers
//!   use.
//! * **Blocking handles** (e.g. `mpsc::Sender` / `mpsc::Receiver`): the
//!   tokio-shaped API (`channel()`, `send`, `recv`, `try_send`, `try_recv`,
//!   the usual error enums). Blocking methods park the calling thread via a
//!   `Condvar` and are only available on native targets; on wasm the
//!   non-blocking `try_*` methods and the core remain available.
//!
//! # Errors
//!
//! Error and outcome enums mirror tokio: [`oneshot::SendError`],
//! [`mpsc::TrySendError`], [`broadcast::RecvError::Lagged`],
//! [`watch::SendError`], and so on.
//!
//! # Configuration
//!
//! Constructors match tokio: `oneshot::channel()`, `mpsc::channel(capacity)`
//! (and `mpsc::unbounded_channel()`), `broadcast::channel(capacity)` and
//! `watch::channel(initial_value)`.

mod waiter;

pub mod broadcast;
pub mod mpsc;
pub mod oneshot;
pub mod watch;

pub use waiter::WaiterId;
