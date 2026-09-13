//! Canvas geometry for the mpsc diagram: senders in a vertical list that
//! shrinks as it grows, arrows fanning right into the receiver.

use crate::protocol::SenderInfo;

pub const SENDER_LIST_TOP: f64 = 6.0;
pub const SENDER_LIST_SPAN: f64 = 82.0;
/// Right edge of the sender nodes, in % of diagram width.
pub const ARROW_X0: f64 = 31.5;
/// Arrow end, hidden under the receiver box so the exact box edge never
/// matters.
pub const ARROW_X1: f64 = 78.0;
/// Vertical center of the receiver box, where arrows and flights converge.
pub const RECEIVER_CY: f64 = 36.0;
/// Right edge of the per-sender controls, just left of the nodes.
pub const CONTROLS_RIGHT: f64 = 82.5;

/// `(top, height)` of the i-th sender out of n, in % of diagram height.
pub fn sender_geometry(i: usize, n: usize) -> (f64, f64) {
    let row = SENDER_LIST_SPAN / n.max(1) as f64;
    let h = row * 0.72;
    let top = SENDER_LIST_TOP + i as f64 * row + (row - h) / 2.0;
    (top, h)
}

pub fn sender_cy(senders: &[SenderInfo], conn: u64) -> f64 {
    match senders.iter().position(|s| s.conn == conn) {
        Some(i) => {
            let (top, h) = sender_geometry(i, senders.len());
            top + h / 2.0
        }
        None => RECEIVER_CY,
    }
}
