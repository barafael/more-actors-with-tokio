//! The futures chapter: CONCEPT.md §2–4. A future that resolves on its own
//! (the timer), one that resolves from real I/O (the button), the two raced
//! in a `select!`, that select spawned as a task, and the select in a loop.

mod button;
pub mod model;
mod select;
mod timer;

pub use button::ButtonGame;
pub use select::{LoopSelectGame, SelectGame, SpawnGame};
pub use timer::TimerGame;
