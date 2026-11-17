//! The overview: the workspace grid zoomed out to 1/4, GNOME style.
//!
//! Everything is one transform driven by the overview progress `p` (0 = normal view,
//! 1 = overview): workspaces scale from 1 to [`OVERVIEW_SCALE`] and the view centre
//! moves from the workspace camera to the overview's own centre. At `p = 0` this is
//! exactly the normal sliding view, so opening and closing is a continuous zoom and a
//! 4-finger swipe can scrub it.
//!
//! In the overview: click a workspace (or a window) to go there, arrows + Enter to
//! pick, Esc to leave, two-finger scroll to pan, and drag windows between workspaces.
//! Dropping on an empty slot next to an active workspace creates one there.

use std::{collections::HashMap, time::Duration};

use smithay::{
    desktop::Window,
    reexports::calloop::timer::{TimeoutAction, Timer},
    utils::{IsAlive, Logical, Point, Rectangle, SERIAL_COUNTER, Size},
};

use super::{
    camera::WORKSPACE_GAP,
    has_ssd, ssd,
    workspace::{WsId, workspace_of},
};
use crate::{
    anim::{
        spring::{Animation, SpringParams},
        swipe::{self, Axis, SwipeTracker},
    },
    input::binds::Dir,
    layout::grid::{Coord, neighbour},
    state::XrossState,
};

pub const OVERVIEW_SCALE: f64 = 0.25;
/// Gap between workspaces in the overview (logical px).
const OVERVIEW_GAP: f64 = 24.0;
pub const OVERVIEW_SPRING: SpringParams = SpringParams::new(1.0, 800.0, 0.0005);
/// Spring for the view centre and for workspaces sliding into place after compaction.
const PAN_SPRING: SpringParams = SpringParams::new(1.0, 600.0, 0.0005);
/// Pointer travel before a press on a window becomes a drag.
const DRAG_THRESHOLD: f64 = 6.0;
/// An emulated (scroll-driven) overview swipe ends after this long without events.
const EMULATED_IDLE: Duration = Duration::from_millis(120);
/// A pan snaps after this long without scroll events (nested; see `overview_pan_idle`).
const PAN_IDLE: Duration = Duration::from_millis(120);
/// On the TTY, libinput reports the fingers lifting; this is only a fallback.
const PAN_IDLE_FALLBACK: Duration = Duration::from_millis(1000);
/// Shortest span the release velocity is measured over (one frame).
const PAN_MIN_DT: f64 = 0.016;
/// Fastest a released pan may coast (workspaces per second).
const PAN_MAX_SPEED: f64 = 6.0;
/// Pan velocity is measured over this much recent history.
const PAN_VELOCITY_WINDOW: Duration = Duration::from_millis(100);
/// How far ahead (seconds) a released pan coasts when picking where to snap.
const PAN_PROJECTION_S: f64 = 0.15;

/// A workspace (or empty slot) as drawn this frame, output-local.
#[derive(Debug, Clone, Copy)]
pub struct WsView {
    pub id: Option<WsId>,
    pub coord: Coord,
    pub rect: Rectangle<f64, Logical>,
    pub scale: f64,
}

#[derive(Debug, Default)]
struct Pan {
    /// (time, centre) samples for the release velocity.
    history: Vec<(Duration, (f64, f64))>,
    last_event: Duration,
}

/// What's under the pointer in the overview.
struct OverviewHit {
    /// The workspace or empty slot.
    coord: Coord,
    /// The window there, with the view it's drawn in.
    window: Option<(Window, WsView)>,
}

#[derive(Debug)]
struct OverviewGesture {
    tracker: SwipeTracker,
    start: f64,
    emulated: bool,
    last_event: Duration,
}

#[derive(Debug)]
struct Press {
    pos: Point<f64, Logical>,
    coord: Option<Coord>,
    window: Option<Window>,
    /// Pointer offset from the window's content origin, in unscaled logical px.
    grab: Point<f64, Logical>,
    dragging: bool,
}

#[derive(Debug, Default)]
pub struct Overview {
    open: bool,
    progress: Option<Animation>,
    gesture: Option<OverviewGesture>,
    center: (f64, f64),
    center_anim: (Option<Animation>, Option<Animation>),
    pub selected: Coord,
    press: Option<Press>,
    /// Two/three-finger pan in progress.
    pan: Option<Pan>,
    /// Workspaces sliding to their new place after compaction, by id: offsets in grid units.
    shifts: HashMap<WsId, (Animation, Animation)>,
}

/// Per-frame transform parameters.
struct Frame {
    p: f64,
    scale: f64,
    spacing: (f64, f64),
    center: (f64, f64),
    out_size: Size<f64, Logical>,
}

impl XrossState {
    pub fn overview_progress(&self, now: Duration) -> f64 {
        let ov = &self.overview;
        if let Some(g) = &ov.gesture {
            let raw = g.start + if g.tracker.axis() == Some(Axis::Y) { g.tracker.offset() } else { 0.0 };
            return match raw {
                r if r > 1.0 => 1.0 + swipe::rubber_band(r - 1.0),
                r if r < 0.0 => swipe::rubber_band(r),
                r => r,
            };
        }
        ov.progress.map_or(if ov.open { 1.0 } else { 0.0 }, |a| a.value(now))
    }

    /// Whether the overview is on screen at all (open, opening, closing or being swiped).
    pub fn overview_active(&self) -> bool {
        self.overview.open || self.overview.gesture.is_some() || self.overview.progress.is_some()
    }

    /// Whether the overview owns input (fully open or opening).
    pub fn overview_open(&self) -> bool {
        self.overview.open
    }

    fn overview_center(&self, now: Duration) -> (f64, f64) {
        let ov = &self.overview;
        (ov.center_anim.0.map_or(ov.center.0, |a| a.value(now)), ov.center_anim.1.map_or(ov.center.1, |a| a.value(now)))
    }

    fn set_center_target(&mut self, to: (f64, f64)) {
        let now = self.start_time.elapsed();
        let cur = self.overview_center(now);
        let ov = &mut self.overview;
        ov.center_anim = (
            Some(ov.center_anim.0.map_or(Animation::new(cur.0, to.0, 0.0, PAN_SPRING, now), |a| a.retarget(to.0, now))),
            Some(ov.center_anim.1.map_or(Animation::new(cur.1, to.1, 0.0, PAN_SPRING, now), |a| a.retarget(to.1, now))),
        );
        ov.center = to;
    }

    fn frame(&self, now: Duration) -> Frame {
        let p = self.overview_progress(now);
        let pc = p.clamp(0.0, 1.0);
        let out = self.output_rect().size.to_f64();
        let scale = 1.0 - (1.0 - OVERVIEW_SCALE) * p;
        let gap = WORKSPACE_GAP as f64 * (1.0 - pc) + OVERVIEW_GAP * pc;
        // Where the zoom ends up at p = 0: the camera, except while swiping the open
        // overview closed, which lands on the selected workspace.
        let home = if self.overview.open && self.overview.gesture.is_some() {
            let s = self.overview.selected;
            (s.0 as f64, s.1 as f64)
        } else {
            self.camera_pos(now)
        };
        let ovc = self.overview_center(now);
        let center = (home.0 + (ovc.0 - home.0) * pc, home.1 + (ovc.1 - home.1) * pc);
        Frame { p, scale, spacing: (out.w * scale + gap, out.h * scale + gap), center, out_size: out }
    }

    fn shift_of(&self, id: WsId, now: Duration) -> (f64, f64) {
        self.overview.shifts.get(&id).map_or((0.0, 0.0), |(x, y)| (x.value(now), y.value(now)))
    }

    fn tile_rect(f: &Frame, coord: (f64, f64)) -> Rectangle<f64, Logical> {
        let size = Size::from((f.out_size.w * f.scale, f.out_size.h * f.scale));
        let cx = f.out_size.w / 2.0 + (coord.0 - f.center.0) * f.spacing.0;
        let cy = f.out_size.h / 2.0 + (coord.1 - f.center.1) * f.spacing.1;
        Rectangle::new((cx - size.w / 2.0, cy - size.h / 2.0).into(), size)
    }

    /// Workspaces on screen this frame (output-local rects).
    pub fn workspace_views(&self, now: Duration) -> Vec<WsView> {
        let f = self.frame(now);
        let screen = Rectangle::from_size(f.out_size);
        self.workspaces
            .iter()
            .filter_map(|((x, y), ws)| {
                let (sx, sy) = self.shift_of(ws.id, now);
                let rect = Self::tile_rect(&f, (x as f64 + sx, y as f64 + sy));
                rect.overlaps(screen).then_some(WsView { id: Some(ws.id), coord: (x, y), rect, scale: f.scale })
            })
            .collect()
    }

    /// Empty drop-target slots (only while the overview is showing).
    pub fn slot_views(&self, now: Duration) -> Vec<WsView> {
        let f = self.frame(now);
        if f.p <= 0.0 {
            return Vec::new();
        }
        let space = &self.space;
        let has = |id: WsId| space.elements().any(|w| workspace_of(w) == Some(id));
        self.workspaces
            .slots(|ws| has(ws.id))
            .into_iter()
            .map(|c| WsView { id: None, coord: c, rect: Self::tile_rect(&f, (c.0 as f64, c.1 as f64)), scale: f.scale })
            .collect()
    }

    /// The selection ring's rect, when the overview shows.
    pub fn selection_rect(&self, now: Duration) -> Option<Rectangle<f64, Logical>> {
        let f = self.frame(now);
        if f.p <= 0.0 {
            return None;
        }
        let c = self.overview.selected;
        let shift = self.workspaces.get(c).map_or((0.0, 0.0), |ws| self.shift_of(ws.id, now));
        Some(Self::tile_rect(&f, (c.0 as f64 + shift.0, c.1 as f64 + shift.1)))
    }

    /// Toggling it closed goes to the selected workspace (Esc is the way to back out).
    pub fn toggle_overview(&mut self) {
        if self.overview.open { self.close_overview(Some(self.overview.selected)) } else { self.open_overview() }
    }

    pub fn open_overview(&mut self) {
        let now = self.start_time.elapsed();
        self.switcher_cancel();
        let p = self.overview_progress(now);
        if !self.overview_active() {
            // Start from wherever the camera is, then glide to centre the current one.
            let cam = self.camera_pos(now);
            self.overview.center = cam;
            self.overview.center_anim = (None, None);
        }
        let cur = self.workspaces.current();
        self.overview.selected = cur;
        self.overview.open = true;
        self.overview.progress = Some(Animation::new(p, 1.0, 0.0, OVERVIEW_SPRING, now));
        self.set_center_target((cur.0 as f64, cur.1 as f64));
        self.refresh_pointer_focus();
        tracing::debug!("overview open");
    }

    /// Leave the overview, optionally going to `target` (creating a workspace there if
    /// it's an empty slot).
    pub fn close_overview(&mut self, target: Option<Coord>) {
        let now = self.start_time.elapsed();
        let p = self.overview_progress(now);
        if let Some(to) = target
            && to != self.workspaces.current()
        {
            self.enter_workspace(to);
        }
        // The normal view should land exactly on the current workspace.
        self.settle_camera();
        let cur = self.workspaces.current();
        self.set_center_target((cur.0 as f64, cur.1 as f64));
        self.overview.open = false;
        self.overview.press = None;
        self.overview.progress = Some(Animation::new(p, 0.0, 0.0, OVERVIEW_SPRING, now));
        self.refresh_pointer_focus();
        tracing::debug!(?target, "overview close");
    }

    /// Per-frame: retire finished animations.
    pub fn tick_overview(&mut self) {
        let now = self.start_time.elapsed();
        let ov = &mut self.overview;
        if ov.progress.is_some_and(|a| a.is_done(now)) {
            ov.progress = None;
        }
        if ov.center_anim.0.is_some_and(|a| a.is_done(now)) {
            ov.center_anim.0 = None;
        }
        if ov.center_anim.1.is_some_and(|a| a.is_done(now)) {
            ov.center_anim.1 = None;
        }
        ov.shifts.retain(|_, (x, y)| !(x.is_done(now) && y.is_done(now)));
    }

    // --- Keyboard ---

    /// A key while the overview is open. Returns whether it was used.
    pub fn overview_key(&mut self, sym: smithay::input::keyboard::Keysym) -> bool {
        use smithay::input::keyboard::Keysym;
        let dir = match sym {
            Keysym::Left => Some(Dir::Left),
            Keysym::Right => Some(Dir::Right),
            Keysym::Up => Some(Dir::Up),
            Keysym::Down => Some(Dir::Down),
            _ => None,
        };
        if let Some(dir) = dir {
            let to = neighbour(self.overview.selected, dir);
            let now = self.start_time.elapsed();
            let selectable = self.workspaces.get(to).is_some() || self.slot_views(now).iter().any(|s| s.coord == to);
            if selectable {
                self.overview.selected = to;
                self.set_center_target((to.0 as f64, to.1 as f64));
            }
            return true;
        }
        match sym {
            Keysym::Return | Keysym::KP_Enter | Keysym::space => {
                self.overview_close_pending = Some(Some(self.overview.selected));
                true
            }
            Keysym::Escape => {
                self.overview_close_pending = Some(None);
                true
            }
            _ => false,
        }
    }

    // --- Pointer ---

    /// What's under a global position: (workspace or slot coord, its id, window there).
    fn overview_hit(&self, pos: Point<f64, Logical>) -> Option<OverviewHit> {
        let now = self.start_time.elapsed();
        let local = pos - self.output_rect().loc.to_f64();
        for view in self.workspace_views(now) {
            if !view.rect.contains(local) {
                continue;
            }
            let id = view.id?;
            // Back into the workspace's own (unscaled, global) coordinates.
            let inner = (local - view.rect.loc).downscale(view.scale) + self.output_rect().loc.to_f64();
            let window = self
                .space
                .elements()
                .rev()
                .filter(|w| workspace_of(w) == Some(id))
                .find(|w| {
                    self.rendered_rect(w, now).is_some_and(|r| {
                        let mut r = r;
                        if has_ssd(w) {
                            r.loc.y -= ssd::TITLE_HEIGHT as f64;
                            r.size.h += ssd::TITLE_HEIGHT as f64;
                        }
                        r.contains(inner)
                    })
                })
                .cloned();
            return Some(OverviewHit { coord: view.coord, window: window.map(|w| (w, view)) });
        }
        self.slot_views(now)
            .into_iter()
            .find(|s| s.rect.contains(local))
            .map(|s| OverviewHit { coord: s.coord, window: None })
    }

    pub fn overview_button(&mut self, pressed: bool, pos: Point<f64, Logical>) {
        let now = self.start_time.elapsed();
        if pressed {
            let hit = self.overview_hit(pos);
            let (coord, window) = match hit {
                Some(OverviewHit { coord, window }) => (Some(coord), window),
                None => (None, None),
            };
            let grab = match &window {
                Some((w, view)) => {
                    let local = pos - self.output_rect().loc.to_f64();
                    let inner = (local - view.rect.loc).downscale(view.scale) + self.output_rect().loc.to_f64();
                    self.rendered_rect(w, now).map_or(Point::default(), |r| inner - r.loc)
                }
                None => Point::default(),
            };
            self.overview.press = Some(Press { pos, coord, window: window.map(|(w, _)| w), grab, dragging: false });
            return;
        }

        let Some(press) = self.overview.press.take() else { return };
        let target = self.overview_hit(pos).map(|h| h.coord);
        if press.dragging {
            if let (Some(window), Some(to)) = (press.window, target) {
                self.drop_window(&window, to);
            }
        } else if target.is_some() && target == press.coord {
            // A click: go there (focusing the clicked window, if any).
            self.overview_close_pending = Some(target);
            self.overview_focus_pending = press.window;
        } else if press.coord.is_none() && target.is_none() {
            // Clicking the background leaves, to the selected workspace.
            self.overview_close_pending = Some(Some(self.overview.selected));
        }
    }

    pub fn overview_motion(&mut self, pos: Point<f64, Logical>) {
        if let Some(press) = self.overview.press.as_mut()
            && press.window.is_some()
            && !press.dragging
        {
            let d = pos - press.pos;
            press.dragging = d.x.hypot(d.y) > DRAG_THRESHOLD;
        }
    }

    /// The window being dragged and where its content goes (output-local), if any.
    pub fn dragged_window(&self, now: Duration) -> Option<(Window, Rectangle<f64, Logical>, f64)> {
        let press = self.overview.press.as_ref().filter(|p| p.dragging)?;
        let window = press.window.clone()?;
        let f = self.frame(now);
        let rendered = self.rendered_rect(&window, now)?;
        let pointer = self.seat.get_pointer()?.current_location() - self.output_rect().loc.to_f64();
        let loc = pointer - press.grab.upscale(f.scale);
        Some((window, Rectangle::new(loc, rendered.size.upscale(f.scale)), f.scale))
    }

    pub fn is_dragged(&self, window: &Window) -> bool {
        self.overview.press.as_ref().is_some_and(|p| p.dragging && p.window.as_ref() == Some(window))
    }

    /// Move a window to the workspace (or empty slot) at `to`, then tidy the grid.
    fn drop_window(&mut self, window: &Window, to: Coord) {
        if !window.alive() {
            return;
        }
        let next_id = &mut self.next_ws_id;
        self.workspaces.insert(to, || Self::new_workspace(next_id));
        let Some(to_id) = self.workspaces.get(to).map(|ws| ws.id) else { return };
        if workspace_of(window) == Some(to_id) {
            return;
        }
        tracing::debug!(?to, "overview drop");
        self.transfer_window(window, to_id);
        self.overview.selected = to;
        self.compact_animated();
    }

    /// Compact the grid, sliding moved workspaces (and the view) instead of jumping.
    fn compact_animated(&mut self) {
        let now = self.start_time.elapsed();
        let before: HashMap<WsId, Coord> = self.workspaces.iter().map(|(c, ws)| (ws.id, c)).collect();
        let old_current = self.workspaces.current();
        let selected_id = self.workspaces.get(self.overview.selected).map(|ws| ws.id);
        self.compact_workspaces();

        for (c, ws) in self.workspaces.iter() {
            let Some(&b) = before.get(&ws.id) else { continue };
            if b == c {
                continue;
            }
            let (sx, sy) = self.overview.shifts.get(&ws.id).map_or((0.0, 0.0), |(x, y)| (x.value(now), y.value(now)));
            let (dx, dy) = ((b.0 - c.0) as f64 + sx, (b.1 - c.1) as f64 + sy);
            self.overview.shifts.insert(
                ws.id,
                (Animation::new(dx, 0.0, 0.0, PAN_SPRING, now), Animation::new(dy, 0.0, 0.0, PAN_SPRING, now)),
            );
        }
        // Keep the view still: move the centre with the current workspace.
        let cur = self.workspaces.current();
        let d = ((cur.0 - old_current.0) as f64, (cur.1 - old_current.1) as f64);
        if d != (0.0, 0.0) {
            let ov = &mut self.overview;
            ov.center = (ov.center.0 + d.0, ov.center.1 + d.1);
            let shift = |a: Option<Animation>, by: f64| {
                a.map(|a| Animation::new(a.value(now) + by, a.to() + by, a.velocity(now), PAN_SPRING, now))
            };
            ov.center_anim = (shift(ov.center_anim.0, d.0), shift(ov.center_anim.1, d.1));
            self.shift_camera(d);
        }
        self.overview.selected = selected_id
            .and_then(|id| self.workspaces.iter().find(|(_, ws)| ws.id == id).map(|(c, _)| c))
            .unwrap_or(cur);
    }

    /// Pan the map by `dx, dy` screen pixels (two- or three-finger swipes: content follows
    /// the fingers). The selection follows the workspace nearest the centre; on release,
    /// [`Self::overview_pan_end`] snaps onto one. With `idle`, it also snaps after that
    /// long without movement (for sources that may never say the fingers lifted).
    pub fn overview_pan(&mut self, dx: f64, dy: f64, idle: Option<Duration>) {
        let now = self.start_time.elapsed();
        let f = self.frame(now);
        let cur = self.overview_center(now);
        let to = (cur.0 + dx / f.spacing.0, cur.1 + dy / f.spacing.1);
        self.overview.center_anim = (None, None);
        self.overview.center = to;
        if let Some(c) = self.nearest_workspace(to) {
            self.overview.selected = c;
        }

        let starting = self.overview.pan.is_none();
        let pan = self.overview.pan.get_or_insert_with(Pan::default);
        pan.last_event = now;
        pan.history.push((now, to));
        pan.history.retain(|(t, _)| now.saturating_sub(*t) <= PAN_VELOCITY_WINDOW);
        if let Some(limit) = idle.filter(|_| starting) {
            let res = self.loop_handle.insert_source(Timer::from_duration(limit), move |_, _, state| {
                match &state.overview.pan {
                    Some(p) => {
                        let idle = state.start_time.elapsed().saturating_sub(p.last_event);
                        if idle >= limit {
                            state.overview_pan_end();
                            TimeoutAction::Drop
                        } else {
                            TimeoutAction::ToDuration(limit - idle)
                        }
                    }
                    None => TimeoutAction::Drop,
                }
            });
            if let Err(err) = res {
                tracing::warn!(%err, "failed to arm the overview pan timer");
            }
        }
    }

    /// How long a scroll pan may pause before it snaps: libinput sends a scroll-stop
    /// when the fingers lift, so the TTY only needs a fallback; nested may get no stop.
    pub fn overview_pan_idle(&self) -> Duration {
        if self.udev.is_some() { PAN_IDLE_FALLBACK } else { PAN_IDLE }
    }

    /// Fingers lifted: coast by the release velocity and settle on the nearest workspace.
    pub fn overview_pan_end(&mut self) {
        let Some(pan) = self.overview.pan.take() else { return };
        let now = self.start_time.elapsed();
        let velocity = match (pan.history.first(), pan.history.last()) {
            (Some((t0, a)), Some((t1, b))) if t1 > t0 => {
                // At least a frame, so a burst of events can't fake a huge flick.
                let dt = (*t1 - *t0).as_secs_f64().max(PAN_MIN_DT);
                let v = ((b.0 - a.0) / dt, (b.1 - a.1) / dt);
                let speed = v.0.hypot(v.1);
                if speed > PAN_MAX_SPEED { (v.0 * PAN_MAX_SPEED / speed, v.1 * PAN_MAX_SPEED / speed) } else { v }
            }
            _ => (0.0, 0.0),
        };
        let cur = self.overview_center(now);
        let projected = (cur.0 + velocity.0 * PAN_PROJECTION_S, cur.1 + velocity.1 * PAN_PROJECTION_S);
        let Some(target) = self.nearest_workspace(projected) else { return };
        tracing::debug!(?cur, ?velocity, ?target, "overview pan snap");
        self.overview.selected = target;
        let to = (target.0 as f64, target.1 as f64);
        self.overview.center = to;
        self.overview.center_anim = (
            Some(Animation::new(cur.0, to.0, velocity.0, PAN_SPRING, now)),
            Some(Animation::new(cur.1, to.1, velocity.1, PAN_SPRING, now)),
        );
    }

    /// A mouse wheel notch: one workspace that way.
    pub fn overview_wheel_step(&mut self, dir: Dir) {
        let cur = self.overview.selected;
        let mut c = neighbour(cur, dir);
        // Skip over holes to the next workspace in that direction, if any.
        for _ in 0..8 {
            if self.workspaces.get(c).is_some() {
                self.overview.selected = c;
                self.set_center_target((c.0 as f64, c.1 as f64));
                return;
            }
            c = neighbour(c, dir);
        }
    }

    fn nearest_workspace(&self, at: (f64, f64)) -> Option<Coord> {
        let dist = |c: Coord| (c.0 as f64 - at.0).powi(2) + (c.1 as f64 - at.1).powi(2);
        self.workspaces.iter().map(|(c, _)| c).min_by(|a, b| dist(*a).total_cmp(&dist(*b)))
    }

    // --- 4-finger swipe ---

    pub fn overview_gesture_begin(&mut self, emulated: bool) {
        let now = self.start_time.elapsed();
        let start = self.overview_progress(now).clamp(0.0, 1.0);
        if !self.overview_active() {
            let cam = self.camera_pos(now);
            let cur = self.workspaces.current();
            self.overview.center = cam;
            self.overview.center_anim = (None, None);
            self.overview.selected = cur;
            self.set_center_target((cur.0 as f64, cur.1 as f64));
            self.switcher_cancel();
        }
        self.overview.progress = None;
        self.overview.gesture =
            Some(OverviewGesture { tracker: SwipeTracker::default(), start, emulated, last_event: now });
        if emulated {
            let res = self.loop_handle.insert_source(Timer::from_duration(EMULATED_IDLE), |_, _, state| {
                match &state.overview.gesture {
                    Some(g) if g.emulated => {
                        let idle = state.start_time.elapsed().saturating_sub(g.last_event);
                        if idle >= EMULATED_IDLE {
                            state.overview_gesture_end(false);
                            TimeoutAction::Drop
                        } else {
                            TimeoutAction::ToDuration(EMULATED_IDLE - idle)
                        }
                    }
                    _ => TimeoutAction::Drop,
                }
            });
            if let Err(err) = res {
                tracing::warn!(%err, "failed to arm the emulated overview swipe timer");
            }
        }
    }

    pub fn overview_gesture_update(&mut self, dx: f64, dy: f64) {
        let now = self.start_time.elapsed();
        if let Some(g) = self.overview.gesture.as_mut() {
            g.last_event = now;
            g.tracker.update(dx, dy, now);
        }
    }

    pub fn overview_gesture_end(&mut self, cancelled: bool) {
        let now = self.start_time.elapsed();
        let p = self.overview_progress(now);
        let Some(g) = self.overview.gesture.take() else { return };
        let velocity = if g.tracker.axis() == Some(Axis::Y) { g.tracker.velocity() } else { 0.0 };
        let open = if cancelled { g.start >= 0.5 } else { p.clamp(0.0, 1.0) + velocity * 0.15 > 0.5 };
        tracing::debug!(p, velocity, open, "overview swipe released");
        if open {
            self.overview.open = true;
            self.overview.progress = Some(Animation::new(p, 1.0, velocity, OVERVIEW_SPRING, now));
            self.refresh_pointer_focus();
        } else {
            self.overview.open = true; // so close_overview runs its full path
            // Land on the selected workspace (the current one, if it was opening).
            self.close_overview(Some(self.overview.selected));
            self.overview.progress = Some(Animation::new(p, 0.0, velocity, OVERVIEW_SPRING, now));
        }
    }

    pub fn overview_gesture_active(&self) -> bool {
        self.overview.gesture.is_some()
    }

    pub fn overview_gesture_emulated(&self) -> bool {
        self.overview.gesture.as_ref().is_some_and(|g| g.emulated)
    }

    /// Run deferred overview actions (they change keyboard focus, so they can't run
    /// inside the keyboard filter or mid-button-handling).
    pub fn run_overview_pending(&mut self) {
        if self.is_locked() {
            self.overview_toggle_pending = false;
            self.overview_close_pending = None;
            self.overview_focus_pending = None;
            return;
        }
        if std::mem::take(&mut self.overview_toggle_pending) {
            self.toggle_overview();
        }
        if let Some(target) = self.overview_close_pending.take() {
            if self.overview.open {
                self.close_overview(target);
            }
            if let Some(w) = self.overview_focus_pending.take()
                && w.alive()
                && self.is_visible(&w)
            {
                self.focus_window(Some(&w), SERIAL_COUNTER.next_serial());
            }
        }
    }
}
