//! The stage view of the mpsc game: a transparent canvas draws the receiver
//! dot, buffer strip, sender circles, arrows and flight glyphs; an rsx
//! overlay anchors the controls (char input, send, receive) to the draggable
//! nodes. A port of the standalone `mpsc.js` + `lib/anim.js`.
//!
//! Presentation only: node positions are per-viewer fractions of the canvas
//! (0..1), so drags and resizes never touch the sim, the wire, or the
//! snapshot. Flights are cosmetic tweens; the buffer and counts always come
//! from the snapshot.

use std::cell::{Cell, RefCell};
use std::collections::{BTreeMap, BTreeSet};
use std::rc::Rc;

use dioxus::prelude::*;

use super::state::Flight;
use crate::games::palette;
use crate::protocol::{MpscSnapshot, MPSC_CAPACITY, MPSC_FLIGHT_MS};
use crate::stage::anim::Animator;
use crate::stage::{clamp, start_frame_loop, CanvasHost, SharedCanvasHost, Stop};

const DOT_R: f64 = 30.0;
const SEND_R: f64 = 40.0;
// Slot geometry is only drawn on wasm; the SSR build never paints.
#[cfg_attr(not(target_arch = "wasm32"), allow(dead_code))]
const SLOT_W: f64 = 22.0;
#[cfg_attr(not(target_arch = "wasm32"), allow(dead_code))]
const SLOT_H: f64 = 24.0;
#[cfg_attr(not(target_arch = "wasm32"), allow(dead_code))]
const SLOT_GAP: f64 = 5.0;
/// Distance from the receiver dot center to the buffer strip center (px).
const BUFF_DY: f64 = 76.0;
/// A send glyph flies exactly as long as the sim's flight window, so glyph
/// and buffer update together without talking to the sim.
const SEND_MS: f64 = MPSC_FLIGHT_MS as f64;
const PULL_MS: f64 = 450.0;
/// Keep dragged nodes this far inside their edges (px).
const DRAG_MARGIN_X: f64 = 60.0;
const DRAG_MARGIN_Y: f64 = 70.0;

/// Node positions are fractions of the canvas (0..1), so a resize needs no
/// work; each use converts to pixels.
type Frac = (f64, f64);

#[derive(Default)]
struct Layout {
    dot: Option<Frac>,
    senders: BTreeMap<u64, Frac>,
}

/// A char in the air. The tween writes px into the cells each frame; the
/// draw pass reads them. `tracked` glyphs mirror a `Flight` and are dropped
/// when that flight lands; pull glyphs are purely local cosmetics.
#[cfg_attr(not(target_arch = "wasm32"), allow(dead_code))]
struct Glyph {
    key: u64,
    ch: char,
    color: String,
    x: Rc<Cell<f64>>,
    y: Rc<Cell<f64>>,
    tracked: bool,
}

struct Drag {
    dot: bool,
    conn: u64,
    off_x: f64,
    off_y: f64,
}

/// Shared stage state. Deliberately outside signals: the frame loop touches
/// it 60×/s, and none of it may re-render rsx. HUD handlers bump a `hud`
/// signal explicitly when node positions change.
pub struct Stage {
    canvas: SharedCanvasHost,
    anim: Rc<RefCell<Animator>>,
    layout: Rc<RefCell<Layout>>,
    glyphs: Rc<RefCell<Vec<Glyph>>>,
    drag: RefCell<Option<Drag>>,
    next_key: Rc<Cell<u64>>,
    stop: RefCell<Option<Stop>>,
}

impl Stage {
    /// Build the stage and start its frame loop. On the server the loop is
    /// a no-op and the static diagram is shown instead.
    pub fn new(snap: Signal<MpscSnapshot>, flights: Signal<Vec<Flight>>) -> Self {
        let canvas = CanvasHost::new();
        let anim = Rc::new(RefCell::new(Animator::new()));
        let layout = Rc::new(RefCell::new(Layout::default()));
        let glyphs = Rc::new(RefCell::new(Vec::new()));
        let draw = {
            let (c, a, l, g) = (canvas.clone(), anim.clone(), layout.clone(), glyphs.clone());
            move |dt: f64, w: f64, h: f64| frame(&c, &a, &l, &g, snap, flights, dt, w, h)
        };
        let stop = start_frame_loop(&canvas, draw);
        Self {
            canvas,
            anim,
            layout,
            glyphs,
            drag: RefCell::new(None),
            next_key: Rc::new(Cell::new(0)),
            stop: RefCell::new(Some(stop)),
        }
    }

    pub fn stop(&self) {
        if let Some(stop) = self.stop.borrow_mut().take() {
            stop.stop();
        }
    }

    pub fn canvas(&self) -> SharedCanvasHost {
        self.canvas.clone()
    }

    /// Node fractions for the HUD. Inserts defaults for senders that have
    /// not been drawn yet (first frame, or a joiner), so the overlay never
    /// waits on the canvas.
    pub fn sender_fracs(&self, snap: &MpscSnapshot) -> Vec<(u64, Frac)> {
        resolve(&mut self.layout.borrow_mut(), snap);
        self.layout
            .borrow()
            .senders
            .iter()
            .map(|(conn, pos)| (*conn, *pos))
            .collect()
    }

    pub fn dot_frac(&self, snap: &MpscSnapshot) -> Frac {
        resolve(&mut self.layout.borrow_mut(), snap);
        self.layout.borrow().dot.unwrap_or((0.62, 0.40))
    }

    /// Grab the node under `(x, y)` (canvas px). Returns true when a node
    /// was picked up.
    pub fn pointer_down(&self, x: f64, y: f64) -> bool {
        let (w, h) = self.canvas.size();
        let layout = self.layout.borrow();
        let grab = |dot: bool, conn: u64, pos: Frac| {
            let (px, py) = frac_px(pos, w, h);
            *self.drag.borrow_mut() = Some(Drag {
                dot,
                conn,
                off_x: px - x,
                off_y: py - y,
            });
        };
        let near = |pos: Frac, r: f64| {
            let (px, py) = frac_px(pos, w, h);
            (px - x).hypot(py - y) <= r
        };
        if let Some(dot) = layout.dot {
            if near(dot, DOT_R + 6.0) {
                grab(true, 0, dot);
                return true;
            }
        }
        for (conn, pos) in layout.senders.iter().rev() {
            if near(*pos, SEND_R + 6.0) {
                grab(false, *conn, *pos);
                return true;
            }
        }
        false
    }

    /// Move the grabbed node with the pointer. Returns true when a node
    /// actually moved (so the HUD can re-render).
    pub fn pointer_move(&self, x: f64, y: f64) -> bool {
        let Some((dot, conn, off_x, off_y)) = self
            .drag
            .borrow()
            .as_ref()
            .map(|d| (d.dot, d.conn, d.off_x, d.off_y))
        else {
            return false;
        };
        let (w, h) = self.canvas.size();
        if w <= 0.0 || h <= 0.0 {
            return false;
        }
        let fx = clamp((x + off_x) / w, DRAG_MARGIN_X / w, 1.0 - DRAG_MARGIN_X / w);
        let fy = clamp((y + off_y) / h, DRAG_MARGIN_Y / h, 1.0 - DRAG_MARGIN_Y / h);
        let mut layout = self.layout.borrow_mut();
        if dot {
            if let Some(pos) = layout.dot.as_mut() {
                *pos = (fx, fy);
            }
        } else if let Some(pos) = layout.senders.get_mut(&conn) {
            *pos = (fx, fy);
        }
        true
    }

    pub fn pointer_up(&self) {
        *self.drag.borrow_mut() = None;
    }

    /// The receiver pulls the oldest buffered char onto the dot. Cosmetic:
    /// the snapshot loses the char at once, the dot badge catches up when
    /// the glyph arrives.
    pub fn on_receive_pull(&self, ch: char, conn: u64) {
        let (w, h) = self.canvas.size();
        let Some(dot) = self.layout.borrow().dot else {
            return;
        };
        let from = slot_xy(frac_px(dot, w, h), 0);
        let key = u64::MAX - self.next_key.get();
        self.next_key.set(self.next_key.get() + 1);
        let glyph = new_glyph(key, ch, conn, from, false);
        push_tween(
            &self.anim,
            &self.canvas,
            &self.layout,
            &self.glyphs,
            glyph,
            usize::MAX,
            PULL_MS,
        );
    }
}

fn new_glyph(key: u64, ch: char, conn: u64, from: (f64, f64), tracked: bool) -> Glyph {
    Glyph {
        key,
        ch,
        color: palette::sender_hex(conn),
        x: Rc::new(Cell::new(from.0)),
        y: Rc::new(Cell::new(from.1)),
        tracked,
    }
}

/// Register a flight tween for `glyph` and hand the glyph to the draw list.
/// A target of `usize::MAX` means the receiver dot; otherwise it is a buffer
/// slot. The target is re-read every step, so a glyph chases a node that is
/// being dragged.
fn push_tween(
    anim: &Rc<RefCell<Animator>>,
    canvas: &SharedCanvasHost,
    layout: &Rc<RefCell<Layout>>,
    glyphs: &Rc<RefCell<Vec<Glyph>>>,
    glyph: Glyph,
    target: usize,
    dur_ms: f64,
) {
    let key = glyph.key;
    let from = (glyph.x.get(), glyph.y.get());
    let (x, y) = (glyph.x.clone(), glyph.y.clone());
    let (canvas, layout) = (canvas.clone(), layout.clone());
    anim.borrow_mut().fly(
        from,
        dur_ms,
        move || match layout.borrow().dot {
            Some(dot) => {
                let (w, h) = canvas.size();
                let d = frac_px(dot, w, h);
                if target == usize::MAX {
                    d
                } else {
                    slot_xy(d, target)
                }
            }
            None => from,
        },
        move |nx, ny| {
            x.set(nx);
            y.set(ny);
        },
        Box::new({
            let glyphs = glyphs.clone();
            move || glyphs.borrow_mut().retain(|g| g.key != key)
        }),
    );
    glyphs.borrow_mut().push(glyph);
}

/// Give every snapshot sender a node and drop the ones that left. Defaults
/// fan the senders down the left; a drag overrides its node for good.
fn resolve(layout: &mut Layout, snap: &MpscSnapshot) {
    layout.dot.get_or_insert((0.62, 0.40));
    let n = snap.senders.len();
    for (i, sender) in snap.senders.iter().enumerate() {
        layout
            .senders
            .entry(sender.conn)
            .or_insert_with(|| default_sender_frac(i, n));
    }
    let live: BTreeSet<u64> = snap.senders.iter().map(|s| s.conn).collect();
    layout.senders.retain(|conn, _| live.contains(conn));
}

fn default_sender_frac(i: usize, n: usize) -> Frac {
    if n <= 1 {
        (0.22, 0.5)
    } else {
        (0.22, 0.16 + 0.68 * i as f64 / (n as f64 - 1.0))
    }
}

fn frac_px(pos: Frac, w: f64, h: f64) -> (f64, f64) {
    (pos.0 * w, pos.1 * h)
}

fn slot_total() -> f64 {
    MPSC_CAPACITY as f64 * SLOT_W + (MPSC_CAPACITY as f64 - 1.0) * SLOT_GAP
}

/// Center of buffer slot `i`, in px, relative to the dot center.
fn slot_xy(dot: (f64, f64), i: usize) -> (f64, f64) {
    let x0 = dot.0 - slot_total() / 2.0;
    (
        x0 + i as f64 * (SLOT_W + SLOT_GAP) + SLOT_W / 2.0,
        dot.1 + BUFF_DY,
    )
}

/// One frame: refresh geometry, spawn/prune glyphs from the live flight
/// list, advance tweens, paint.
#[allow(clippy::too_many_arguments)]
fn frame(
    canvas: &SharedCanvasHost,
    anim: &Rc<RefCell<Animator>>,
    layout: &Rc<RefCell<Layout>>,
    glyphs: &Rc<RefCell<Vec<Glyph>>>,
    snap: Signal<MpscSnapshot>,
    flights: Signal<Vec<Flight>>,
    dt: f64,
    w: f64,
    h: f64,
) {
    let snapshot = snap.read().clone();
    resolve(&mut layout.borrow_mut(), &snapshot);
    let live = flights.read();
    sync_glyphs(canvas, anim, layout, glyphs, live.as_slice(), &snapshot);
    anim.borrow_mut().update(dt);
    draw(canvas, layout, glyphs, &snapshot, w, h);
}

/// Mirrors `chan.flights` into glyphs: new flights get a glyph aimed at the
/// next free buffer slot, landed flights drop theirs. Runs every frame, so
/// it works identically in local and remote mode without event plumbing.
fn sync_glyphs(
    canvas: &SharedCanvasHost,
    anim: &Rc<RefCell<Animator>>,
    layout: &Rc<RefCell<Layout>>,
    glyphs: &Rc<RefCell<Vec<Glyph>>>,
    live: &[Flight],
    snap: &MpscSnapshot,
) {
    {
        let live_keys: BTreeSet<u64> = live.iter().map(|f| f.key).collect();
        glyphs
            .borrow_mut()
            .retain(|g| !g.tracked || live_keys.contains(&g.key));
    }
    let base_slot = snap.buffer.len() + glyphs.borrow().iter().filter(|g| g.tracked).count();
    let pending: Vec<Glyph> = {
        let known: BTreeSet<u64> = glyphs.borrow().iter().map(|g| g.key).collect();
        let (w, h) = canvas.size();
        let layout = layout.borrow();
        live.iter()
            .filter(|f| !known.contains(&f.key))
            .filter_map(|f| {
                let pos = layout.senders.get(&f.conn).copied()?;
                Some(new_glyph(f.key, f.ch, f.conn, frac_px(pos, w, h), true))
            })
            .collect()
    };
    for (i, glyph) in pending.into_iter().enumerate() {
        push_tween(anim, canvas, layout, glyphs, glyph, base_slot + i, SEND_MS);
    }
}

/// All canvas drawing is wasm-only: on the server the stage loop never runs
/// and the game renders its static rsx diagram instead.
#[cfg(target_arch = "wasm32")]
fn draw(
    canvas: &SharedCanvasHost,
    layout: &Rc<RefCell<Layout>>,
    glyphs: &Rc<RefCell<Vec<Glyph>>>,
    snap: &MpscSnapshot,
    w: f64,
    h: f64,
) {
    canvas.paint(|ctx| {
        ctx.clear_rect(0.0, 0.0, w, h);
        let layout = layout.borrow();

        // Arrows first, behind the nodes.
        if let Some(dot) = layout.dot {
            let (dx, dy) = frac_px(dot, w, h);
            for pos in layout.senders.values() {
                let (sx, sy) = frac_px(*pos, w, h);
                draw_arrow(ctx, sx, sy, SEND_R, dx, dy, DOT_R);
            }
        }

        if let Some(dot) = layout.dot {
            let (dx, dy) = frac_px(dot, w, h);

            // Buffer strip.
            for i in 0..MPSC_CAPACITY {
                let (x, y) = slot_xy((dx, dy), i);
                let filled = snap.buffer.get(i);
                round_rect(
                    ctx,
                    x - SLOT_W / 2.0,
                    y - SLOT_H / 2.0,
                    SLOT_W,
                    SLOT_H,
                    4.0,
                    filled.is_some(),
                    filled.map(|c| palette::sender_hex(c.conn)),
                );
                if let Some(owned) = filled {
                    text(
                        ctx,
                        &owned.ch.to_string(),
                        x,
                        y,
                        "#000",
                        "bold 14px sans-serif",
                    );
                }
            }
            text(
                ctx,
                &format!("buffer, capacity {MPSC_CAPACITY}"),
                dx,
                dy + BUFF_DY + SLOT_H + 16.0,
                "#444",
                "600 12px sans-serif",
            );

            // Receiver dot on top of the strip.
            ctx.set_fill_style_str("#000");
            ctx.begin_path();
            let _ = ctx.arc(dx, dy, DOT_R, 0.0, std::f64::consts::TAU);
            ctx.fill();
            text(
                ctx,
                &snap.senders.len().to_string(),
                dx,
                dy + 1.0,
                "#fff",
                "bold 22px sans-serif",
            );
        }

        // Senders.
        for sender in &snap.senders {
            let Some(pos) = layout.senders.get(&sender.conn) else {
                continue;
            };
            let (x, y) = frac_px(*pos, w, h);
            let pending = snap
                .blocked_sends
                .iter()
                .find(|b| b.conn == sender.conn)
                .map(|b| b.ch.to_string());
            let fill = if sender.blocked {
                "#9e9e9e".to_string()
            } else {
                palette::sender_hex(sender.conn)
            };
            ctx.set_fill_style_str(fill.as_str());
            ctx.begin_path();
            let _ = ctx.arc(x, y, SEND_R, 0.0, std::f64::consts::TAU);
            ctx.fill();
            ctx.set_stroke_style_str("rgba(0,0,0,0.35)");
            ctx.set_line_width(2.0);
            ctx.stroke();
            text(
                ctx,
                pending.as_deref().unwrap_or("Tx"),
                x,
                y,
                palette::sender_text_color(sender.conn),
                "bold 15px sans-serif",
            );
        }

        // Airborne glyphs last.
        for glyph in glyphs.borrow().iter() {
            let (x, y) = (glyph.x.get(), glyph.y.get());
            ctx.set_fill_style_str("#fff");
            ctx.set_stroke_style_str(glyph.color.as_str());
            ctx.set_line_width(3.0);
            ctx.begin_path();
            let _ = ctx.arc(x, y, 12.0, 0.0, std::f64::consts::TAU);
            ctx.fill();
            ctx.stroke();
            text(
                ctx,
                &glyph.ch.to_string(),
                x,
                y,
                "#000",
                "bold 14px sans-serif",
            );
        }
    });
}

#[cfg(not(target_arch = "wasm32"))]
fn draw(
    canvas: &SharedCanvasHost,
    layout: &Rc<RefCell<Layout>>,
    glyphs: &Rc<RefCell<Vec<Glyph>>>,
    snap: &MpscSnapshot,
    w: f64,
    h: f64,
) {
    let _ = (canvas, layout, glyphs, snap, w, h);
}

#[cfg(target_arch = "wasm32")]
fn draw_arrow(
    ctx: &web_sys::CanvasRenderingContext2d,
    x1: f64,
    y1: f64,
    r1: f64,
    x2: f64,
    y2: f64,
    r2: f64,
) {
    let (dx, dy) = (x2 - x1, y2 - y1);
    let len = (dx * dx + dy * dy).sqrt();
    if len < r1 + r2 + 4.0 {
        return;
    }
    let (ux, uy) = (dx / len, dy / len);
    let (sx, sy) = (x1 + ux * r1, y1 + uy * r1);
    let (ex, ey) = (x2 - ux * r2, y2 - uy * r2);
    ctx.set_global_alpha(0.55);
    ctx.set_stroke_style_str("#37474f");
    ctx.set_line_width(2.5);
    ctx.begin_path();
    ctx.move_to(sx, sy);
    ctx.line_to(ex, ey);
    ctx.stroke();
    let a = 9.0;
    ctx.set_fill_style_str("#37474f");
    ctx.begin_path();
    ctx.move_to(ex, ey);
    ctx.line_to(ex - ux * a - uy * a * 0.6, ey - uy * a + ux * a * 0.6);
    ctx.line_to(ex - ux * a + uy * a * 0.6, ey - uy * a - ux * a * 0.6);
    ctx.close_path();
    ctx.fill();
    ctx.set_global_alpha(1.0);
}

#[cfg(target_arch = "wasm32")]
fn text(ctx: &web_sys::CanvasRenderingContext2d, s: &str, x: f64, y: f64, color: &str, font: &str) {
    ctx.set_fill_style_str(color);
    ctx.set_font(font);
    ctx.set_text_align("center");
    ctx.set_text_baseline("middle");
    let _ = ctx.fill_text(s, x, y);
}

#[cfg(target_arch = "wasm32")]
#[allow(clippy::too_many_arguments)]
fn round_rect(
    ctx: &web_sys::CanvasRenderingContext2d,
    x: f64,
    y: f64,
    w: f64,
    h: f64,
    r: f64,
    filled: bool,
    stroke: Option<String>,
) {
    ctx.set_fill_style_str("rgba(255,255,255,0.8)");
    ctx.set_stroke_style_str(stroke.as_deref().unwrap_or("#bbb"));
    ctx.set_line_width(if filled { 2.5 } else { 1.5 });
    ctx.begin_path();
    ctx.move_to(x + r, y);
    ctx.line_to(x + w - r, y);
    let _ = ctx.arc(x + w - r, y + r, r, -std::f64::consts::FRAC_PI_2, 0.0);
    ctx.line_to(x + w, y + h - r);
    let _ = ctx.arc(x + w - r, y + h - r, r, 0.0, std::f64::consts::FRAC_PI_2);
    ctx.line_to(x + r, y + h);
    let _ = ctx.arc(
        x + r,
        y + h - r,
        r,
        std::f64::consts::FRAC_PI_2,
        std::f64::consts::PI,
    );
    ctx.line_to(x, y + r);
    let _ = ctx.arc(
        x + r,
        y + r,
        r,
        std::f64::consts::PI,
        1.5 * std::f64::consts::PI,
    );
    ctx.close_path();
    ctx.fill();
    ctx.stroke();
}
