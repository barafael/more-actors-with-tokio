//! The stage: the presentation layer the standalone canvas games use,
//! ported for Dioxus. One transparent `<canvas>` draws the diagram (nodes,
//! arrows, buffer slots, flight glyphs) from a per-frame loop; an rsx
//! overlay positions the HUD (inputs, buttons) at node coordinates.
//!
//! Everything here is presentation only: node positions are per-viewer
//! cosmetics, never sent over the wire and never part of a snapshot.
//!
//! On non-wasm targets (SSR/export) the loop and canvas access compile to
//! no-ops; the games render their static DOM diagram until the client
//! mounts, so the export's first paint is unchanged.

pub mod anim;

use dioxus::prelude::*;
use std::cell::Cell;
#[cfg(target_arch = "wasm32")]
use std::cell::RefCell;
use std::rc::Rc;

/// A diagram node's position as a fraction of the canvas (0..1) plus a
/// pop-in/shrink-out scale (1.0 = settled). Fractions keep a layout valid
/// across resizes; each game converts to pixels as it draws.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Node {
    pub x: f64,
    pub y: f64,
    pub scale: f64,
}

impl Node {
    pub fn new(x: f64, y: f64) -> Self {
        Self { x, y, scale: 1.0 }
    }
}

pub fn clamp(v: f64, lo: f64, hi: f64) -> f64 {
    v.max(lo).min(hi)
}

/// A handle to the stage's `<canvas>`. Wasm-only details live behind the
/// host so game code compiles unchanged on the SSR server.
pub struct CanvasHost {
    size: Rc<Cell<(f64, f64)>>,
    #[cfg(target_arch = "wasm32")]
    element: Rc<RefCell<Option<web_sys::HtmlCanvasElement>>>,
    #[cfg(target_arch = "wasm32")]
    context: Rc<RefCell<Option<web_sys::CanvasRenderingContext2d>>>,
}

pub type SharedCanvasHost = Rc<CanvasHost>;

impl CanvasHost {
    pub fn new() -> Rc<Self> {
        Rc::new(Self {
            size: Rc::new(Cell::new((800.0, 450.0))),
            #[cfg(target_arch = "wasm32")]
            element: Rc::new(RefCell::new(None)),
            #[cfg(target_arch = "wasm32")]
            context: Rc::new(RefCell::new(None)),
        })
    }

    /// Wire into the canvas element's `onmounted`. Wasm grabs the element
    /// and its 2d context; native is a no-op (the static diagram stays).
    pub fn handle_mounted(&self, ev: &MountedEvent) {
        #[cfg(target_arch = "wasm32")]
        {
            use wasm_bindgen::JsCast;
            let Some(element) = ev.data().downcast::<web_sys::Element>().cloned() else {
                return;
            };
            let Ok(canvas) = element.dyn_into::<web_sys::HtmlCanvasElement>() else {
                return;
            };
            if let Ok(Some(obj)) = canvas.get_context("2d") {
                *self.context.borrow_mut() =
                    obj.dyn_into::<web_sys::CanvasRenderingContext2d>().ok();
            }
            *self.element.borrow_mut() = Some(canvas);
        }
        #[cfg(not(target_arch = "wasm32"))]
        {
            let _ = ev;
        }
    }

    /// CSS-pixel size of the canvas (the coordinate space all drawing and
    /// hit-testing uses).
    pub fn size(&self) -> (f64, f64) {
        self.size.get()
    }

    /// Keep the backing store matched to the CSS size, dpr-aware. Returns
    /// true when the size changed. Draw code applies no transform of its
    /// own: the context transform is set here, in CSS-pixel units.
    #[cfg(target_arch = "wasm32")]
    fn sync_size(&self) -> bool {
        let Some(canvas) = self.element.borrow().clone() else {
            return false;
        };
        let (w, h) = (canvas.client_width() as f64, canvas.client_height() as f64);
        if w <= 0.0 || h <= 0.0 {
            return false;
        }
        let (sw, sh) = self.size.get();
        if (w - sw).abs() < 0.5 && (h - sh).abs() < 0.5 {
            return false;
        }
        let dpr = web_sys::window()
            .map(|w| w.device_pixel_ratio())
            .unwrap_or(1.0);
        canvas.set_width((w * dpr).round() as u32);
        canvas.set_height((h * dpr).round() as u32);
        if let Some(ctx) = self.context.borrow().clone() {
            let _ = ctx.set_transform(dpr, 0.0, 0.0, dpr, 0.0, 0.0);
        }
        self.size.set((w, h));
        true
    }

    /// Run `paint` against the stage context, if the canvas is mounted.
    #[cfg(target_arch = "wasm32")]
    pub fn paint(&self, f: impl FnOnce(&web_sys::CanvasRenderingContext2d)) {
        if let Some(ctx) = self.context.borrow().clone() {
            f(&ctx);
        }
    }
}

/// Ends the frame loop (call from `use_drop`). Also drops the re-arming
/// closure, so a stopped stage leaves no JS state behind.
pub struct Stop {
    stopped: Rc<Cell<bool>>,
    #[cfg(target_arch = "wasm32")]
    _keepalive: Rc<RefCell<Option<FrameClosure>>>,
}

#[cfg(target_arch = "wasm32")]
type FrameClosure = wasm_bindgen::closure::Closure<dyn FnMut(f64)>;

impl Stop {
    pub fn stop(&self) {
        self.stopped.set(true);
        #[cfg(target_arch = "wasm32")]
        {
            *self._keepalive.borrow_mut() = None;
        }
    }
}

/// Run `draw(dt_ms, width, height)` once per animation frame (wasm rAF;
/// native no-op — the static diagram needs no loop). `dt_ms` is clamped so
/// a throttled background tab never makes tweens jump.
pub fn start_frame_loop(
    host: &SharedCanvasHost,
    mut draw: impl FnMut(f64, f64, f64) + 'static,
) -> Stop {
    let stopped = Rc::new(Cell::new(false));

    #[cfg(target_arch = "wasm32")]
    let keepalive: Rc<RefCell<Option<FrameClosure>>> = {
        use wasm_bindgen::JsCast;
        let host = host.clone();
        let stop_flag = stopped.clone();
        // Self-referential: the closure re-registers itself each frame, so
        // `keepalive` must own one strong reference while running. `Stop`
        // clears it, breaking the cycle.
        let frame: Rc<RefCell<Option<FrameClosure>>> = Rc::new(RefCell::new(None));
        let rearm = frame.clone();
        let mut last = crate::sim::now_ms();
        *frame.borrow_mut() = Some(wasm_bindgen::closure::Closure::new(move |_t| {
            if stop_flag.get() {
                return;
            }
            host.sync_size();
            let now = crate::sim::now_ms();
            let dt = (now - last).clamp(0.0, 50.0);
            last = now;
            let (w, h) = host.size();
            draw(dt, w, h);
            let re_fn: Option<js_sys::Function> = rearm
                .borrow()
                .as_ref()
                .map(|c| c.as_ref().unchecked_ref::<js_sys::Function>().clone());
            if let (Some(window), Some(re)) = (web_sys::window(), re_fn) {
                let _ = window.request_animation_frame(&re);
            }
        }));
        let first: Option<js_sys::Function> = frame
            .borrow()
            .as_ref()
            .map(|c| c.as_ref().unchecked_ref::<js_sys::Function>().clone());
        if let (Some(window), Some(re)) = (web_sys::window(), first) {
            let _ = window.request_animation_frame(&re);
        }
        frame
    };

    #[cfg(not(target_arch = "wasm32"))]
    {
        let _ = host;
        let _ = &mut draw;
    }

    Stop {
        stopped,
        #[cfg(target_arch = "wasm32")]
        _keepalive: keepalive,
    }
}
