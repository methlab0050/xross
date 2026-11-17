//! Window management: lookups, placement, focus, and hit-testing.

pub mod camera;
pub mod layer;
pub mod overview;
pub mod snap;
pub mod ssd;
pub mod switcher;
pub mod workspace;
pub mod x11;
pub mod xdg;

use smithay::{
    desktop::{LayerSurface, Window, WindowSurface, WindowSurfaceType, layer_map_for_output},
    reexports::{
        wayland_protocols::xdg::decoration::zv1::server::zxdg_toplevel_decoration_v1::Mode as DecorationMode,
        wayland_server::protocol::wl_surface::WlSurface,
    },
    utils::{Logical, Point, Rectangle, SERIAL_COUNTER, Serial},
    wayland::{seat::WaylandFocus, shell::wlr_layer::Layer},
};

use crate::{focus::KeyboardFocusTarget, state::XrossState};

/// What the pointer is over.
#[derive(Debug, Clone)]
pub enum PointerHit {
    /// A client surface; the point is the surface origin in global coordinates.
    Surface { surface: WlSurface, origin: Point<f64, Logical>, window: Option<Window>, layer: Option<LayerSurface> },
    /// A compositor-drawn title bar.
    TitleBar(Window),
    /// The close button on a compositor-drawn title bar.
    CloseButton(Window),
}

impl PointerHit {
    pub fn window(&self) -> Option<&Window> {
        match self {
            PointerHit::Surface { window, .. } => window.as_ref(),
            PointerHit::TitleBar(w) | PointerHit::CloseButton(w) => Some(w),
        }
    }

    pub fn surface(&self) -> Option<(WlSurface, Point<f64, Logical>)> {
        match self {
            PointerHit::Surface { surface, origin, .. } => Some((surface.clone(), *origin)),
            _ => None,
        }
    }
}

/// Whether this window gets a compositor title bar when not fullscreen.
pub fn wants_ssd(window: &Window) -> bool {
    match window.underlying_surface() {
        WindowSurface::Wayland(t) => {
            t.with_committed_state(|state| state.is_some_and(|s| s.decoration_mode == Some(DecorationMode::ServerSide)))
        }
        WindowSurface::X11(s) => !s.is_decorated() && !s.is_override_redirect(),
    }
}

/// Whether the compositor currently draws a title bar for this window.
pub fn has_ssd(window: &Window) -> bool {
    match window.underlying_surface() {
        WindowSurface::Wayland(t) => t.with_committed_state(|state| {
            state.is_some_and(|s| {
                s.decoration_mode == Some(DecorationMode::ServerSide)
                    && !s.states.contains(
                        smithay::reexports::wayland_protocols::xdg::shell::server::xdg_toplevel::State::Fullscreen,
                    )
            })
        }),
        WindowSurface::X11(s) => !s.is_decorated() && !s.is_override_redirect() && !s.is_fullscreen(),
    }
}

impl XrossState {
    pub fn window_for_surface(&self, surface: &WlSurface) -> Option<Window> {
        self.space.elements().find(|w| w.wl_surface().is_some_and(|s| &*s == surface)).cloned()
    }

    /// Usable area of the (single) output: output geometry minus layer-shell exclusive zones.
    pub fn usable_area(&self) -> Rectangle<i32, Logical> {
        let Some(output) = self.space.outputs().next() else {
            return Rectangle::from_size((1280, 720).into());
        };
        let geo = self.space.output_geometry(output).unwrap_or_default();
        let zone = layer_map_for_output(output).non_exclusive_zone();
        Rectangle::new(geo.loc + zone.loc, zone.size)
    }

    /// Map a new window centered in the usable area (accounting for its title bar),
    /// cascading slightly so successive windows don't stack exactly.
    pub fn place_new_window(&mut self, window: Window) {
        let area = self.usable_area();
        let size = window.geometry().size;
        let title = if has_ssd(&window) { ssd::TITLE_HEIGHT } else { 0 };

        let cascade = 32 * (self.visible_windows().count() as i32 % 6);
        let x = area.loc.x + ((area.size.w - size.w) / 2).max(0) + cascade;
        let y = area.loc.y + title + ((area.size.h - size.h - title) / 2).max(0) + cascade;
        let loc = Point::from((x, y));
        tracing::debug!(?loc, ?size, x11 = window.x11_surface().is_some(), "placing new window");

        if let Some(x11) = window.x11_surface() {
            let _ = x11.configure(Some(Rectangle::new(loc, size)));
        }
        self.adopt_window(&window);
        snap::start_open_animation(&window, self.start_time.elapsed());
        self.space.map_element(window.clone(), loc, false);
        self.focus_window(Some(&window), SERIAL_COUNTER.next_serial());
        self.apply_initial_state(&window);
    }

    /// Raise + activate a window and give it keyboard focus. `None` clears focus.
    pub fn focus_window(&mut self, window: Option<&Window>, serial: Serial) {
        // The lock surface keeps the keyboard until unlock.
        if self.is_locked() {
            return;
        }
        let keyboard = self.seat.get_keyboard().unwrap();
        match window {
            Some(window) => {
                self.space.raise_element(window, true);
                if let (Some(x11), Some(xwm)) = (window.x11_surface(), self.xwm.as_mut()) {
                    let _ = xwm.raise_window(x11);
                }
                keyboard.set_focus(self, Some(KeyboardFocusTarget::Window(window.clone())), serial);
                self.touch_mru(window);
            }
            None => {
                for w in self.space.elements() {
                    w.set_activated(false);
                }
                keyboard.set_focus(self, None, serial);
            }
        }
        for w in self.space.elements() {
            if let Some(t) = w.toplevel() {
                t.send_pending_configure();
            }
        }
    }

    /// Focus the top-most window (after the focused one went away).
    pub fn focus_top_window(&mut self) {
        let top =
            self.visible_windows().rev().find(|w| !w.x11_surface().is_some_and(|x| x.is_override_redirect())).cloned();
        self.focus_window(top.as_ref(), SERIAL_COUNTER.next_serial());
    }

    pub fn focused_window(&self) -> Option<Window> {
        self.seat.get_keyboard()?.current_focus().and_then(|f| f.window().cloned())
    }

    /// Ask a window to close (xdg close / WM_DELETE_WINDOW).
    pub fn close_window(&self, window: &Window) {
        match window.underlying_surface() {
            WindowSurface::Wayland(t) => t.send_close(),
            WindowSurface::X11(s) => {
                let _ = s.close();
            }
        }
    }

    /// Everything under the pointer, in stacking order:
    /// overlay/top layers, then windows (incl. title bars), then bottom/background layers.
    pub fn pointer_hit(&self, pos: Point<f64, Logical>) -> Option<PointerHit> {
        // The overview draws everything itself; clients get no pointer focus meanwhile.
        if self.is_locked() {
            // Only the lock surface (covering the output) gets the pointer.
            let surface = self.lock_surface()?.clone();
            let origin = self.output_rect().loc.to_f64();
            return Some(PointerHit::Surface { surface, origin, window: None, layer: None });
        }
        if self.overview_active() {
            return None;
        }
        let output = self.space.outputs().next()?;
        let output_loc = self.space.output_geometry(output)?.loc;
        let layers = layer_map_for_output(output);

        let layer_hit = |layer_kind: Layer| {
            let rel = pos - output_loc.to_f64();
            let layer = layers.layer_under(layer_kind, rel)?;
            let layer_loc = layers.layer_geometry(layer)?.loc + output_loc;
            layer.surface_under(pos - layer_loc.to_f64(), WindowSurfaceType::ALL).map(|(surface, loc)| {
                PointerHit::Surface {
                    surface,
                    origin: (loc + layer_loc).to_f64(),
                    window: None,
                    layer: Some(layer.clone()),
                }
            })
        };

        // Same order as rendering: a fullscreen window on top covers the Top layer.
        let fullscreen_on_top = self.fullscreen_on_top();
        if let Some(hit) = layer_hit(Layer::Overlay) {
            return Some(hit);
        }
        if !fullscreen_on_top && let Some(hit) = layer_hit(Layer::Top) {
            return Some(hit);
        }
        if let Some(hit) = self.window_hit(pos) {
            return Some(hit);
        }
        if fullscreen_on_top && let Some(hit) = layer_hit(Layer::Top) {
            return Some(hit);
        }
        layer_hit(Layer::Bottom).or_else(|| layer_hit(Layer::Background))
    }

    /// Whether the top-most window is fullscreen.
    pub fn fullscreen_on_top(&self) -> bool {
        self.visible_windows()
            .rev()
            .find(|w| !w.x11_surface().is_some_and(|x| x.is_override_redirect()))
            .is_some_and(snap::is_fullscreen)
    }

    fn window_hit(&self, pos: Point<f64, Logical>) -> Option<PointerHit> {
        for window in self.visible_windows().rev() {
            let loc = self.space.element_location(window)?;
            let geo = window.geometry();

            if has_ssd(window) {
                match ssd::hit_test(Rectangle::new(loc, geo.size), pos) {
                    Some(ssd::DecorationHit::Close) => {
                        return Some(PointerHit::CloseButton(window.clone()));
                    }
                    Some(ssd::DecorationHit::TitleBar) => {
                        return Some(PointerHit::TitleBar(window.clone()));
                    }
                    None => {}
                }
            }

            let render_loc = loc - geo.loc;
            if let Some((surface, surface_loc)) =
                window.surface_under(pos - render_loc.to_f64(), WindowSurfaceType::ALL)
            {
                return Some(PointerHit::Surface {
                    surface,
                    origin: (surface_loc + render_loc).to_f64(),
                    window: Some(window.clone()),
                    layer: None,
                });
            }
        }
        None
    }
}
