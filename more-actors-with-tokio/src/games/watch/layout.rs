//! Geometry for the watch diagram: one fixed sender and a shared cell on the
//! left, a shrinking vertical list of receivers on the right (the mpsc sender
//! list rotated). Positions are % of diagram width/height.

use crate::protocol::RxInfo;

/// Left edge of the fixed sender box, its vertical center and width, in %.
pub const TX_LEFT: f64 = 8.0;
pub const TX_W: f64 = 13.0;
/// The presenter's send input sits right of the sender box.
pub const TX_CTL_LEFT: f64 = 24.0;

/// Shared channel cell center.
pub const CELL_LEFT: f64 = 36.0;
pub const CELL_W: f64 = 10.0;
pub const CELL_CY: f64 = 50.0;

/// Receivers: a vertical list anchored to the right edge.
pub const RX_RIGHT: f64 = 22.0;
pub const RX_W: f64 = 12.0;
pub const RX_LIST_TOP: f64 = 8.0;
pub const RX_LIST_SPAN: f64 = 78.0;
/// Every receiver's controls rest just left of the column.
pub const RX_CTL_RIGHT: f64 = 40.0;

/// `(top, height)` of the i-th receiver out of n, in % of diagram height.
pub fn rx_geometry(i: usize, n: usize) -> (f64, f64) {
    let row = RX_LIST_SPAN / n.max(1) as f64;
    let h = row * 0.74;
    let top = RX_LIST_TOP + i as f64 * row + (row - h) / 2.0;
    (top, h)
}

/// Vertical center of receiver `id`; falls back to the cell center when the
/// receiver is gone (a flight still in transit).
pub fn rx_cy(receivers: &[RxInfo], id: u64) -> f64 {
    match receivers.iter().position(|r| r.id == id) {
        Some(i) => {
            let (top, h) = rx_geometry(i, receivers.len());
            top + h / 2.0
        }
        None => CELL_CY,
    }
}

/// Horizontal center of the receiver column (constant by construction).
pub fn rx_center_x() -> f64 {
    100.0 - RX_RIGHT - RX_W / 2.0
}
