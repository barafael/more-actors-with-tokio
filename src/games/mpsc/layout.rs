//! Canvas geometry for the mpsc diagram: senders in a vertical list that
//! shrinks as it grows, arrows fanning right into the receiver.

pub const SENDER_LIST_TOP: f64 = 6.0;
pub const SENDER_LIST_SPAN: f64 = 82.0;
/// Right edge of the sender nodes, in % of diagram width.
pub const ARROW_X0: f64 = 31.5;
/// Left edge of the receiver box, where the arrows end. The stylesheet
/// pins the box there (through `--rx`), so the arrowheads touch it at any
/// diagram size rather than missing it for the buffer below.
pub const ARROW_X1: f64 = 60.0;
/// Vertical center of the receiver box, where arrows and flights converge.
/// Pinned in the stylesheet through `--ry`, like [`ARROW_X1`].
pub const RECEIVER_CY: f64 = 36.0;
/// Right edge of the per-sender controls, just left of the nodes.
pub const CONTROLS_RIGHT: f64 = 82.5;

/// `(top, height)` of the i-th sender out of n, in % of diagram height.
pub fn sender_geometry(i: usize, n: usize) -> (f64, f64) {
    let row = SENDER_LIST_SPAN / n.max(1) as f64;
    // cap the height: with one sender the row is the whole column, and a
    // single handle drawn as a full-height slab reads as a wall, not a node
    let h = (row * 0.72).min(26.0);
    let top = SENDER_LIST_TOP + i as f64 * row + (row - h) / 2.0;
    (top, h)
}

/// Vertical center of `handle`'s node, where its arrow and flights start.
pub fn sender_cy(handles: &[u64], handle: u64) -> f64 {
    match handles.iter().position(|h| *h == handle) {
        Some(i) => {
            let (top, h) = sender_geometry(i, handles.len());
            top + h / 2.0
        }
        None => RECEIVER_CY,
    }
}
