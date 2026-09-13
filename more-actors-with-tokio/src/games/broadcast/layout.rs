//! Canvas geometry for the broadcast diagram: senders on the left, the ring
//! buffer in the middle, receivers on the right.

pub const SENDERS_LEFT: f64 = 3.0;
pub const SENDERS_WIDTH: f64 = 13.0;
pub const SENDERS_TOP: f64 = 8.0;
pub const SENDERS_SPAN: f64 = 78.0;

/// The buffer row: five slots centered vertically between the columns.
pub const BUFFER_LEFT: f64 = 39.0;
pub const BUFFER_SLOT_W: f64 = 4.6;
pub const BUFFER_GAP: f64 = 1.2;
pub const BUFFER_CY: f64 = 46.0;

/// Receivers: a vertical list on the right.
pub const RECEIVERS_RIGHT: f64 = 4.0;
pub const RECEIVERS_WIDTH: f64 = 13.0;
pub const RECEIVERS_TOP: f64 = 6.0;
pub const RECEIVERS_SPAN: f64 = 70.0;

/// Left x of buffer slot `i`, in %.
pub fn slot_left(i: usize) -> f64 {
    BUFFER_LEFT + i as f64 * (BUFFER_SLOT_W + BUFFER_GAP)
}

/// Center y of the i-th receiver out of n, in %.
pub fn receiver_cy(i: usize, n: usize) -> f64 {
    let row = RECEIVERS_SPAN / n.max(1) as f64;
    RECEIVERS_TOP + i as f64 * row + row / 2.0
}
