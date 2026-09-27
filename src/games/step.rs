//! What one click on a stepped board does, shared by every board's model.
//!
//! Plain data with no view in it, so models stay free of dioxus: a model
//! returns the hops its step made and a sentence saying what happened,
//! and the board (`super::board`) turns hops into flights.

/// How long one leg of a flight takes, in milliseconds. A model whose
/// messages arrive when their flight lands (the watchdog) times arrivals
/// with this; the board animates with it.
pub const LEG_MS: f64 = 600.0;

/// One click's worth of motion and narration. `H` is the board's own hop
/// type: what moved, from where to where.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Step<H> {
    pub hops: Vec<H>,
    /// What happened, for the narration line. Empty keeps the last one.
    pub note: String,
}

impl<H> Default for Step<H> {
    fn default() -> Self {
        Self {
            hops: Vec::new(),
            note: String::new(),
        }
    }
}

impl<H> Step<H> {
    /// A step that moved nothing and only says so.
    pub fn note(note: impl Into<String>) -> Self {
        Self {
            hops: Vec::new(),
            note: note.into(),
        }
    }
}
