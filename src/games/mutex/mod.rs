//! The mutex minigame: the hook. `Arc<Mutex<Rgb>>`, a shared colour, and
//! three tasks each holding a clone.
//!
//! CONCEPT.md §1. Somebody will lock it. Everyone who tries next parks in
//! `lock().await` and can do nothing else — the only control a waiter has
//! is a clock counting how long it has been stuck. When the holder drops
//! the guard, the longest waiter gets it. Safety, yes; backpressure,
//! lifecycle, a say in any of it — no.

mod model;

use dioxus::prelude::*;

use crate::clock::now_ms;
use crate::games::ticker::use_clock;
use crate::games::{palette, TaskChip, Waited};
use model::{MutexSim, Standing, PALETTE, TASKS};

/// A pointy-top hexagon's corners at radius 50, clockwise from the top.
/// Written out rather than computed with sin and cos, so the server and the
/// wasm client print the very same points.
const HEX_CORNERS: [(f64, f64); 6] = [
    (0.0, -50.0),
    (43.3, -25.0),
    (43.3, 25.0),
    (0.0, 50.0),
    (-43.3, 25.0),
    (-43.3, -25.0),
];

/// The centre hexagon, showing the current colour, at this share of the
/// full radius. The wedges stop at its edge.
const INNER: f64 = 0.4;

/// Corner `k` at `scale` of the full radius, as an SVG point.
fn corner(k: usize, scale: f64) -> String {
    let (x, y) = HEX_CORNERS[k % 6];
    format!("{:.2},{:.2}", x * scale, y * scale)
}

/// The corners at `scale` of the full radius, as an SVG points list.
fn hexagon_points(scale: f64) -> String {
    (0..6).map(|k| corner(k, scale)).collect::<Vec<_>>().join(" ")
}

/// Wedge `k`: the trapezoid between corners `k` and `k + 1`, from the rim
/// in to the centre hexagon.
fn wedge_points(k: usize) -> String {
    format!(
        "{} {} {} {}",
        corner(k, 1.0),
        corner(k + 1, 1.0),
        corner(k + 1, INNER),
        corner(k, INNER)
    )
}

#[component]
pub fn MutexGame() -> Element {
    let mut sim = use_signal(MutexSim::new);
    let mut act = move |act: &dyn Fn(&mut MutexSim)| {
        sim.with_mut(|s| {
            s.sync_now(now_ms());
            act(s);
        });
    };

    // Read by the count-up badges alone, so only they redraw each frame.
    let clock = use_clock();
    let mutex = sim.read();
    let value = mutex.value();
    let holder = mutex.holder();
    let waiters: Vec<(u64, f64)> = mutex.waiters().collect();
    let standings = TASKS.map(|task| (task, mutex.standing(task)));
    drop(mutex);

    rsx! {
        div { class: "diagram mutex-game",
            div { class: "game-header", "Arc<Mutex<Rgb>>" }

            div { class: "mx-stage",
                div { class: "mx-queue",
                    div { class: "mx-chips",
                        if waiters.is_empty() {
                            span { class: "mx-empty", "nobody waiting" }
                        }
                        for (place, (task, since)) in waiters.iter().copied().enumerate() {
                            TaskChip { key: "{task}", task, class: "waiting",
                                Waited {
                                    since,
                                    clock,
                                    label: format!("{}. ", place + 1),
                                }
                            }
                        }
                    }
                }

                div { class: if holder.is_some() { "mx-lock held" } else { "mx-lock" },
                    div { class: "mx-lock-head",
                        if holder.is_some() {
                            "locked"
                        } else {
                            "unlocked"
                        }
                    }
                    // The chooser lives in the lock, round the value it sets:
                    // six wedges, live only while someone holds the guard,
                    // and every pick goes through that holder.
                    div { class: "mx-value",
                        svg {
                            class: if holder.is_some() { "mx-hex" } else { "mx-hex shut" },
                            view_box: "-54 -54 108 108",
                            g { class: "mx-wedges",
                                for (k, colour) in PALETTE.into_iter().enumerate() {
                                    polygon {
                                        key: "{k}",
                                        class: "mx-wedge",
                                        points: wedge_points(k),
                                        fill: colour.hex(),
                                        // its own colour as stroke closes the
                                        // antialiased seam between neighbours
                                        stroke: colour.hex(),
                                        onclick: move |_| {
                                            if let Some((task, _)) = holder {
                                                act(&|s| s.paint(task, colour));
                                            }
                                        },
                                    }
                                }
                            }
                            // the colour the guard last wrote, outlined on
                            // top of every wedge so no neighbour covers it
                            if let Some(k) = PALETTE.iter().position(|c| *c == value) {
                                polygon {
                                    class: "mx-picked",
                                    points: wedge_points(k),
                                }
                            }
                            polygon {
                                class: "mx-hex-centre",
                                points: hexagon_points(INNER),
                                fill: value.hex(),
                            }
                        }
                        span { class: "mx-rgb", "Rgb {{ r: {value.r}, g: {value.g}, b: {value.b} }}" }
                    }
                    div { class: "mx-holder",
                        match holder {
                            Some((task, since)) => rsx! {
                                TaskChip { task, class: "holding",
                                    Waited { since, clock, label: "guard · " }
                                }
                            },
                            None => rsx! {
                                span { class: "mx-empty", "nobody holds the guard" }
                            },
                        }
                    }
                }

                div { class: "mx-idle",
                    for (task, _) in standings.iter().filter(|(_, s)| *s == Standing::Idle) {
                        TaskChip { key: "{task}", task: *task, class: "idle" }
                    }
                }
            }

            div { class: "mx-controls",
                for (task, standing) in standings {
                    div {
                        key: "{task}",
                        class: "mx-row",
                        style: "--c: {palette::sender_hex(task)};",
                        span { class: "mx-row-name", "task {task}" }
                        match standing {
                            Standing::Idle => rsx! {
                                button { class: "btn await", onclick: move |_| act(&|s| s.lock(task)), "lock().await" }
                            },
                            Standing::Waiting { place, since } => rsx! {
                                button { class: "btn", disabled: true,
                                    Waited {
                                        since,
                                        clock,
                                        label: format!("parked · #{place} in line · "),
                                        class: "",
                                    }
                                }
                            },
                            Standing::Holding { .. } => rsx! {
                                button { class: "btn rcv", onclick: move |_| act(&|s| s.unlock(task)), "drop(guard)" }
                            },
                        }
                    }
                }
            }

            div { class: "game-footer",
                if !waiters.is_empty() {
                    span { class: "note-pill blocked",
                        "{waiters.len()} parked — nothing to do but wait"
                    }
                } else {
                    span {}
                }
                button { class: "btn", onclick: move |_| sim.set(MutexSim::new()), "restart" }
            }
        }
    }
}
