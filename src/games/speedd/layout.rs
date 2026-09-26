//! Where each node of the speedd board sits, in percent of the board.
//!
//! Left to right as in speedd's `live-topology.drawio.png`: cameras, the
//! reporting inbox, the Collector (its subscription inbox below it), one
//! ticket queue per road, the dispatchers, and the clients off the right
//! edge where delivered tickets leave.

use super::model::Node;

/// Camera centres, grouped by road with a gap between roads.
const CAMERA_Y: [f64; 7] = [8.0, 19.5, 31.0, 46.0, 57.5, 72.5, 84.0];
/// Ticket queues, one per road, level with that road's cameras.
const QUEUE_Y: [f64; 3] = [19.5, 51.75, 78.25];
const DISPATCHER_Y: [f64; 5] = [9.0, 26.0, 43.0, 60.0, 77.0];

pub const CAMERA_X: f64 = 8.5;
pub const REPORTING: (f64, f64) = (26.0, 46.0);
pub const COLLECTOR: (f64, f64) = (45.0, 40.0);
pub const SUBSCRIPTION: (f64, f64) = (45.0, 83.0);
pub const QUEUE_X: f64 = 65.0;
pub const DISPATCHER_X: f64 = 86.5;

pub fn position(node: Node) -> (f64, f64) {
    match node {
        Node::Camera(i) => (CAMERA_X, CAMERA_Y[i]),
        Node::Reporting => REPORTING,
        Node::Collector => COLLECTOR,
        Node::Subscription => SUBSCRIPTION,
        Node::Queue(r) => (QUEUE_X, QUEUE_Y[r]),
        Node::Dispatcher(d) => (DISPATCHER_X, DISPATCHER_Y[d]),
        // just past the right edge: a ticket written to the socket leaves
        Node::Socket(d) => (103.0, DISPATCHER_Y[d]),
    }
}
