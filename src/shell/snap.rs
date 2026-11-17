//! Applies the snap layout model ([`crate::layout::snap`]) to real windows:
//! snap binds, client maximize/fullscreen requests, and un-snapping on drag.

use std::{cell::Cell, time::Duration};

use smithay::{
    desktop::{Window, WindowSurface},
    reexports::wayland_protocols::xdg::{
        decoration::zv1::server::zxdg_toplevel_decoration_v1::Mode as DecorationMode,
        shell::server::xdg_toplevel::State as XdgState,
    },
    utils::{IsAlive, Logical, Point, Rectangle, Size},
};

use crate::anim::spring::{Animation, SpringParams};

use super::{
    ssd, wants_ssd,
    workspace::{WsId, workspace_of},
};
use crate::{
    input::binds::Dir,
    layout::snap::{Slot, Tiling},
    state::XrossState,
};

/// Per-window WM state, kept in the window's user data.
#[derive(Default)]
struct WinState {
    /// Floating geometry to return to when un-snapped (content rect).
    restore: Cell<Option<Rectangle<i32, Logical>>>,
    fullscreen: Cell<bool>,
    /// Requested before the window mapped; applied when it does.
    initial_maximized: Cell<bool>,
    /// Geometry change in progress (snap, maximize, restore...).
    geo_anim: Cell<Option<GeoAnim>>,
    /// Fade/zoom-in after mapping, 0 -> 1.
    open_anim: Cell<Option<Animation>>,
}

/// Animates the drawn rect from where the window was to where it's going. Stored
/// relative to the window's actual location, so it composes with interactive moves.
#[derive(Debug, Clone, Copy)]
struct GeoAnim {
    /// (from - to) as x, y, w, h.
    delta: (f64, f64, f64, f64),
    to_size: Size<i32, Logical>,
    /// Goes 1 -> 0: the fraction of `delta` still to cover.
    anim: Animation,
}

fn window_spring() -> SpringParams {
    let a = &crate::config::current().animations;
    a.params(a.window, 0.001)
}

fn open_spring() -> SpringParams {
    let a = &crate::config::current().animations;
    a.params(a.open, 0.001)
}
/// New windows grow from this scale.
pub const OPEN_SCALE: f64 = 0.92;

/// Open-animation progress (0 -> 1), while it runs.
pub fn open_progress(window: &Window, now: Duration) -> Option<f64> {
    win_state(window).open_anim.get().map(|a| a.value(now))
}

pub fn start_open_animation(window: &Window, now: Duration) {
    win_state(window).open_anim.set(Some(Animation::new(0.0, 1.0, 0.0, open_spring(), now)));
}

/// Forget finished animations; returns whether any are still running.
pub fn tick_window_animations<'a>(windows: impl Iterator<Item = &'a Window>, now: Duration) -> bool {
    let mut running = false;
    for w in windows {
        let st = win_state(w);
        if st.geo_anim.get().is_some_and(|g| g.anim.is_done(now)) {
            st.geo_anim.set(None);
        }
        if st.open_anim.get().is_some_and(|a| a.is_done(now)) {
            st.open_anim.set(None);
        }
        running |= st.geo_anim.get().is_some() || st.open_anim.get().is_some();
    }
    running
}

fn win_state(window: &Window) -> &WinState {
    window.user_data().insert_if_missing(WinState::default);
    window.user_data().get::<WinState>().unwrap()
}

pub fn is_fullscreen(window: &Window) -> bool {
    win_state(window).fullscreen.get()
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Mode {
    Floating,
    Tiled,
    Maximized,
    Fullscreen,
}

/// Pixels the pointer must travel before dragging a snapped window un-snaps it.
pub const UNSNAP_THRESHOLD: f64 = 8.0;

impl XrossState {
    /// Content rect (excluding any title bar) of a mapped window, in global coordinates.
    pub fn window_rect(&self, window: &Window) -> Option<Rectangle<i32, Logical>> {
        Some(Rectangle::new(self.space.element_location(window)?, window.geometry().size))
    }

    /// Where the window's content is drawn right now (mid-animation, this differs from
    /// its actual geometry), in global coordinates.
    pub fn rendered_rect(&self, window: &Window, now: Duration) -> Option<Rectangle<f64, Logical>> {
        let loc = self.space.element_location(window)?.to_f64();
        let Some(g) = win_state(window).geo_anim.get() else {
            return Some(Rectangle::new(loc, window.geometry().size.to_f64()));
        };
        let v = g.anim.value(now);
        let (dx, dy, dw, dh) = g.delta;
        Some(Rectangle::new(
            (loc.x + dx * v, loc.y + dy * v).into(),
            ((g.to_size.w as f64 + dw * v).max(1.0), (g.to_size.h as f64 + dh * v).max(1.0)).into(),
        ))
    }

    /// Start animating from where the window is drawn now to `to` (content rect).
    fn animate_geometry(&mut self, window: &Window, to: Rectangle<i32, Logical>) {
        let now = self.start_time.elapsed();
        if !self.is_visible(window) {
            return;
        }
        let Some(from) = self.rendered_rect(window, now) else { return };
        let to_f = to.to_f64();
        let delta =
            (from.loc.x - to_f.loc.x, from.loc.y - to_f.loc.y, from.size.w - to_f.size.w, from.size.h - to_f.size.h);
        if [delta.0, delta.1, delta.2, delta.3].iter().all(|d| d.abs() < 1.0) {
            return;
        }
        let anim = Animation::new(1.0, 0.0, 0.0, window_spring(), now);
        win_state(window).geo_anim.set(Some(GeoAnim { delta, to_size: to.size, anim }));
    }

    pub fn output_rect(&self) -> Rectangle<i32, Logical> {
        self.space
            .outputs()
            .next()
            .and_then(|o| self.space.output_geometry(o))
            .unwrap_or_else(|| Rectangle::from_size((1280, 720).into()))
    }

    pub fn is_snapped(&self, window: &Window) -> bool {
        self.slot_of(window).is_some() || is_fullscreen(window)
    }

    /// Mod+arrow on the focused window, including the quick-succession quadrant combo.
    pub fn snap_focused(&mut self, dir: Dir) {
        let Some(window) = self.focused_window() else {
            return;
        };
        // Override-redirect windows have no workspace and never snap.
        let Some(ws) = workspace_of(&window) else {
            return;
        };
        let was_fullscreen = win_state(&window).fullscreen.replace(false);

        let res = self.snap_combo.press(window.clone(), dir, self.start_time.elapsed());
        let snapshot = self.snap_snapshot.clone().filter(|_| res.rewind);
        let starts_combo = snapshot.is_none();
        let Some(tiling) = self.tiling_mut(ws) else { return };
        let before = tiling.clone();
        if let Some(snapshot) = snapshot {
            *tiling = snapshot;
        }
        tiling.assign(&window, res.target);
        if starts_combo {
            self.snap_snapshot = Some(before.clone());
        }
        tracing::debug!(target = ?res.target, rewind = res.rewind, "snap");
        // Coming out of fullscreen, the restore rect is already right; don't overwrite it.
        self.apply_tiling_change(ws, &before, Some(&window), was_fullscreen);
        if was_fullscreen {
            self.apply_geometry(&window);
        }
    }

    /// Move `window` to `target` outside of a combo (client requests, drags).
    pub fn set_window_slot(&mut self, window: &Window, target: Option<Slot>) {
        self.snap_combo.cancel();
        self.snap_snapshot = None;
        let Some(ws) = workspace_of(window) else { return };
        let Some(tiling) = self.tiling_mut(ws) else { return };
        let before = tiling.clone();
        tiling.assign(window, target);
        self.apply_tiling_change(ws, &before, Some(window), false);
    }

    pub fn set_fullscreen(&mut self, window: &Window, on: bool) {
        let state = win_state(window);
        if state.fullscreen.get() != on {
            if on && self.slot_of(window).is_none() {
                state.restore.set(self.window_rect(window));
            }
            state.fullscreen.set(on);
        }
        // Always answer, even when nothing changed: clients wait for a configure.
        self.apply_geometry(window);
        if on && self.is_visible(window) {
            self.focus_window(Some(window), smithay::utils::SERIAL_COUNTER.next_serial());
        }
    }

    /// A window is going away: drop it from the layout and let its neighbours fill in.
    pub fn forget_window(&mut self, window: &Window) {
        self.forget_mru(window);
        self.snap_combo.cancel();
        self.snap_snapshot = None;
        let Some(ws) = workspace_of(window) else { return };
        let Some(tiling) = self.tiling_mut(ws) else { return };
        let before = tiling.clone();
        tiling.remove(window);
        self.apply_tiling_change(ws, &before, None, false);
    }

    /// Called right after a new window is mapped as floating.
    pub fn apply_initial_state(&mut self, window: &Window) {
        let state = win_state(window);
        if state.initial_maximized.take() {
            self.set_window_slot(window, Some(Slot::Maximized));
        }
        if state.fullscreen.get() {
            self.set_fullscreen(window, true);
        }
    }

    pub fn fullscreen_before_map(&mut self, window: &Window, on: bool) {
        win_state(window).fullscreen.set(on);
        let size = on.then(|| self.output_rect().size);
        configure_before_map(window, XdgState::Fullscreen, on, size);
    }

    /// A maximize request for a window that hasn't mapped yet: configure it maximized
    /// so its first buffer already has the right size.
    pub fn maximize_before_map(&mut self, window: &Window) {
        win_state(window).initial_maximized.set(true);
        let mut size = Slot::Maximized.rect(self.usable_area()).size;
        // Nothing is committed yet, so go by the decoration mode we're about to send.
        let ssd = window
            .toplevel()
            .is_some_and(|t| t.with_pending_state(|s| s.decoration_mode == Some(DecorationMode::ServerSide)));
        if ssd {
            size.h -= ssd::TITLE_HEIGHT;
        }
        configure_before_map(window, XdgState::Maximized, true, Some(size));
    }

    /// Re-apply geometry to everything snapped, on every workspace (the usable area changed).
    pub fn relayout(&mut self) {
        let snapped: Vec<Window> = self.space.elements().filter(|w| self.is_snapped(w)).cloned().collect();
        for w in snapped {
            self.apply_geometry(&w);
        }
    }

    /// Dragging a snapped window un-snaps it, keeping the grab point under the pointer.
    /// Returns the window's new location.
    pub fn unsnap_for_drag(&mut self, window: &Window, pointer: Point<f64, Logical>) -> Point<i32, Logical> {
        let Some(cur) = self.window_rect(window) else {
            return Point::default();
        };
        let state = win_state(window);
        let size = state.restore.get().map(|r| r.size).unwrap_or(cur.size);
        let rel_x = (pointer.x - cur.loc.x as f64) / cur.size.w.max(1) as f64;
        let mut rel_y = pointer.y - cur.loc.y as f64;
        if rel_y > size.h as f64 {
            rel_y = size.h as f64 / 2.0;
        }
        let loc = Point::<f64, Logical>::from((pointer.x - rel_x * size.w as f64, pointer.y - rel_y)).to_i32_round();
        state.restore.set(Some(Rectangle::new(loc, size)));
        state.fullscreen.set(false);
        self.set_window_slot(window, None);
        // A window that was only fullscreen (not tiled) isn't picked up as a tiling change.
        self.apply_geometry(window);
        loc
    }

    /// Resizing a snapped window turns it into a floating one at its current geometry.
    pub fn unsnap_in_place(&mut self, window: &Window) {
        if !self.is_snapped(window) {
            return;
        }
        let state = win_state(window);
        state.restore.set(self.window_rect(window));
        state.fullscreen.set(false);
        self.set_window_slot(window, None);
        self.apply_geometry(window);
    }

    /// Configure every window whose slot changed since `before`. `focus` is the window
    /// the user acted on; it is raised above the windows it displaced. `keep_restore`
    /// means its restore rect is already saved.
    pub fn apply_tiling_change(
        &mut self,
        ws: WsId,
        before: &Tiling<Window>,
        focus: Option<&Window>,
        keep_restore: bool,
    ) {
        let after = self.tiling(ws).cloned().unwrap_or_default();
        let mut changed: Vec<Window> = Vec::new();
        for (w, _) in before.iter().chain(after.iter()) {
            let moved = before.slot_of(w) != after.slot_of(w);
            if moved && !changed.contains(w) && w.alive() && self.space.element_location(w).is_some() {
                changed.push(w.clone());
            }
        }
        for w in &changed {
            let is_focus = focus == Some(w);
            if before.slot_of(w).is_none() && !(is_focus && keep_restore) && !is_fullscreen(w) {
                // Leaving the floating layer: remember where to come back to.
                win_state(w).restore.set(self.window_rect(w));
            }
            self.apply_geometry(w);
        }
        if let Some(w) = focus
            && changed.contains(w)
        {
            self.space.raise_element(w, true);
        }
    }

    fn slot_content_rect(&self, window: &Window, slot: Slot) -> Rectangle<i32, Logical> {
        let mut rect = slot.rect_with_gaps(self.usable_area(), crate::config::current().layout.gaps);
        if wants_ssd(window) {
            rect.loc.y += ssd::TITLE_HEIGHT;
            rect.size.h -= ssd::TITLE_HEIGHT;
        }
        rect
    }

    /// Put a window where its current state says it belongs.
    pub fn apply_geometry(&mut self, window: &Window) {
        let state = win_state(window);
        let (rect, size, mode) = if state.fullscreen.get() {
            let r = self.output_rect();
            (Some(r), Some(r.size), Mode::Fullscreen)
        } else if let Some(slot) = self.slot_of(window) {
            let r = self.slot_content_rect(window, slot);
            let mode = if slot == Slot::Maximized { Mode::Maximized } else { Mode::Tiled };
            (Some(r), Some(r.size), mode)
        } else {
            // Floating: back to the remembered geometry, or let the client choose.
            let r = state.restore.get();
            (r, r.map(|r| r.size), Mode::Floating)
        };
        tracing::trace!(?rect, ?mode, x11 = window.x11_surface().is_some(), "apply geometry");
        self.configure_window(window, rect, size, mode);
    }

    fn configure_window(
        &mut self,
        window: &Window,
        rect: Option<Rectangle<i32, Logical>>,
        size: Option<Size<i32, Logical>>,
        mode: Mode,
    ) {
        match window.underlying_surface() {
            WindowSurface::Wayland(t) => {
                t.with_pending_state(|s| {
                    for st in [
                        XdgState::Maximized,
                        XdgState::Fullscreen,
                        XdgState::TiledLeft,
                        XdgState::TiledRight,
                        XdgState::TiledTop,
                        XdgState::TiledBottom,
                    ] {
                        s.states.unset(st);
                    }
                    match mode {
                        Mode::Floating => {}
                        Mode::Fullscreen => {
                            s.states.set(XdgState::Fullscreen);
                        }
                        Mode::Maximized | Mode::Tiled => {
                            if mode == Mode::Maximized {
                                s.states.set(XdgState::Maximized);
                            }
                            // Tells CSD clients to drop shadows and rounded corners.
                            for st in
                                [XdgState::TiledLeft, XdgState::TiledRight, XdgState::TiledTop, XdgState::TiledBottom]
                            {
                                s.states.set(st);
                            }
                        }
                    }
                    s.size = size;
                });
                if t.is_initial_configure_sent() {
                    t.send_pending_configure();
                }
            }
            WindowSurface::X11(x) => {
                let _ = x.set_maximized(mode == Mode::Maximized);
                let _ = x.set_fullscreen(mode == Mode::Fullscreen);
                let geo = rect.unwrap_or_else(|| x.geometry());
                let _ = x.configure(Some(Rectangle::new(geo.loc, size.unwrap_or(geo.size))));
            }
        }
        if let Some(rect) = rect {
            let to_size = size.unwrap_or_else(|| window.geometry().size);
            self.animate_geometry(window, Rectangle::new(rect.loc, to_size));
            self.space.relocate_element(window, rect.loc);
        }
    }
}

fn configure_before_map(window: &Window, state: XdgState, on: bool, size: Option<Size<i32, Logical>>) {
    let Some(t) = window.toplevel() else { return };
    t.with_pending_state(|s| {
        if on {
            s.states.set(state);
        } else {
            s.states.unset(state);
        }
        s.size = size;
    });
    // The request may come after the initial configure; answer it now if so.
    if t.is_initial_configure_sent() {
        t.send_pending_configure();
    }
}
