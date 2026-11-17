//! The workspace camera: which part of the 2D grid is on screen.
//!
//! When idle it sits on the current workspace. Switches animate it with a spring;
//! swipes drive it directly and hand their release velocity to the spring. The grid
//! is compacted only once the camera settles, so nothing reshuffles mid-motion.

use std::time::Duration;

use smithay::reexports::calloop::timer::{TimeoutAction, Timer};

use crate::{
    anim::{
        spring::{Animation, SpringParams},
        swipe::{self, Axis, SwipeTracker},
    },
    input::binds::Dir,
    layout::grid::{Coord, neighbour},
    state::XrossState,
};

pub fn switch_spring() -> SpringParams {
    let a = &crate::config::current().animations;
    a.params(a.workspace, 0.0005)
}
/// Space between workspaces while they slide past (logical px).
pub const WORKSPACE_GAP: i32 = 32;
/// The nudge when a move isn't allowed (workspaces per second).
const BUMP_VELOCITY: f64 = 2.0;
/// An emulated (scroll-driven) swipe ends after this long without events.
const EMULATED_IDLE: Duration = Duration::from_millis(120);

#[derive(Debug)]
pub struct Gesture {
    tracker: SwipeTracker,
    /// The logical current workspace when the swipe began.
    origin: Coord,
    /// Camera position when it began (may be mid-animation, so fractional).
    start: (f64, f64),
    /// Scroll-emulated (nested): ends on a timeout rather than a release event.
    pub emulated: bool,
    pub last_event: Duration,
}

#[derive(Debug, Default)]
pub struct Camera {
    x: Option<Animation>,
    y: Option<Animation>,
    pub gesture: Option<Gesture>,
    /// A switch happened; compact the grid once the camera settles.
    pending_compact: bool,
}

impl Camera {
    pub fn is_moving(&self) -> bool {
        self.gesture.is_some() || self.x.is_some() || self.y.is_some()
    }
}

fn axis_of(dir: Dir) -> (Axis, f64) {
    match dir {
        Dir::Left => (Axis::X, -1.0),
        Dir::Right => (Axis::X, 1.0),
        Dir::Up => (Axis::Y, -1.0),
        Dir::Down => (Axis::Y, 1.0),
    }
}

fn dirs_of(axis: Axis) -> (Dir, Dir) {
    match axis {
        Axis::X => (Dir::Left, Dir::Right),
        Axis::Y => (Dir::Up, Dir::Down),
    }
}

impl XrossState {
    fn now(&self) -> Duration {
        self.start_time.elapsed()
    }

    /// Camera position in grid coordinates.
    pub fn camera_pos(&self, now: Duration) -> (f64, f64) {
        let cur = self.workspaces.current();
        let cam = &self.camera;
        let mut pos = (cam.x.map_or(cur.0 as f64, |a| a.value(now)), cam.y.map_or(cur.1 as f64, |a| a.value(now)));
        if let Some(g) = &cam.gesture
            && let Some(axis) = g.tracker.axis()
        {
            let (neg, pos_ok) = self.gesture_allowed(g.origin, axis);
            let (origin, start) = match axis {
                Axis::X => (g.origin.0 as f64, g.start.0),
                Axis::Y => (g.origin.1 as f64, g.start.1),
            };
            let shown = origin + swipe::displayed_offset(start - origin + g.tracker.offset(), neg, pos_ok);
            match axis {
                Axis::X => pos.0 = shown,
                Axis::Y => pos.1 = shown,
            }
        }
        pos
    }

    /// Which way a swipe on `axis` from `origin` may go: (negative, positive).
    fn gesture_allowed(&self, origin: Coord, axis: Axis) -> (bool, bool) {
        let (neg, pos) = dirs_of(axis);
        let can = |dir| self.can_switch_from(origin, dir);
        (can(neg), can(pos))
    }

    /// Ease the camera onto the current workspace from wherever it is now.
    fn animate_camera_home(&mut self, velocity: Option<(Axis, f64)>) {
        let now = self.now();
        let (px, py) = self.camera_pos(now);
        let cur = self.workspaces.current();
        let v = |axis| velocity.filter(|(a, _)| *a == axis).map(|(_, v)| v);
        let mk = |prev: Option<Animation>, from: f64, to: f64, vel: Option<f64>| match (prev, vel) {
            (_, Some(vel)) => Some(Animation::new(from, to, vel, switch_spring(), now)),
            (Some(a), None) => Some(a.retarget(to, now)),
            (None, None) if (from - to).abs() > 1e-6 => Some(Animation::new(from, to, 0.0, switch_spring(), now)),
            (None, None) => None,
        };
        self.camera.x = mk(self.camera.x, px, cur.0 as f64, v(Axis::X));
        self.camera.y = mk(self.camera.y, py, cur.1 as f64, v(Axis::Y));
    }

    /// Freeze the camera where it is on screen, so changing the current workspace
    /// doesn't move it; [`Self::animate_switch`] then slides it over.
    pub fn pin_camera(&mut self) {
        let now = self.now();
        let (px, py) = self.camera_pos(now);
        let pin = |a: Option<Animation>, p: f64| a.or(Some(Animation::new(p, p, 0.0, switch_spring(), now)));
        self.camera.x = pin(self.camera.x, px);
        self.camera.y = pin(self.camera.y, py);
    }

    /// A switch landed on the current workspace: slide there.
    pub fn animate_switch(&mut self) {
        self.camera.pending_compact = true;
        self.animate_camera_home(None);
    }

    /// A move that isn't allowed: nudge the view that way and let it spring back.
    pub fn bump(&mut self, dir: Dir) {
        let now = self.now();
        let (px, py) = self.camera_pos(now);
        let cur = self.workspaces.current();
        let (axis, sign) = axis_of(dir);
        let a = |from: f64, to: i32| Some(Animation::new(from, to as f64, sign * BUMP_VELOCITY, switch_spring(), now));
        match axis {
            Axis::X => self.camera.x = a(px, cur.0),
            Axis::Y => self.camera.y = a(py, cur.1),
        }
    }

    pub fn gesture_begin(&mut self, emulated: bool) {
        let now = self.now();
        let start = self.camera_pos(now);
        self.camera.gesture = Some(Gesture {
            tracker: SwipeTracker::default(),
            origin: self.workspaces.current(),
            start,
            emulated,
            last_event: now,
        });
        if emulated {
            // Scroll events have no "fingers lifted" event; end after a pause instead.
            let res = self.loop_handle.insert_source(Timer::from_duration(EMULATED_IDLE), |_, _, state| {
                match &state.camera.gesture {
                    Some(g) if g.emulated => {
                        let idle = state.now().saturating_sub(g.last_event);
                        if idle >= EMULATED_IDLE {
                            state.gesture_end(false);
                            TimeoutAction::Drop
                        } else {
                            TimeoutAction::ToDuration(EMULATED_IDLE - idle)
                        }
                    }
                    _ => TimeoutAction::Drop,
                }
            });
            if let Err(err) = res {
                tracing::warn!(%err, "failed to arm the emulated swipe timer");
            }
        }
    }

    /// Finger movement in touchpad pixels (the direction the fingers moved).
    pub fn gesture_update(&mut self, dx: f64, dy: f64) {
        let now = self.now();
        let Some(g) = self.camera.gesture.as_mut() else { return };
        g.last_event = now;
        if let Some((axis, _)) = g.tracker.update(dx, dy, now) {
            // The swipe owns this axis now; stop any animation on it.
            match axis {
                Axis::X => self.camera.x = None,
                Axis::Y => self.camera.y = None,
            }
        }
    }

    pub fn gesture_end(&mut self, cancelled: bool) {
        let now = self.now();
        let pos = self.camera_pos(now);
        let Some(g) = self.camera.gesture.take() else { return };
        let Some(axis) = g.tracker.axis() else {
            // Never moved far enough to pick an axis.
            return;
        };
        let (neg_ok, pos_ok) = self.gesture_allowed(g.origin, axis);
        let (origin, start) = match axis {
            Axis::X => (g.origin.0 as f64, g.start.0),
            Axis::Y => (g.origin.1 as f64, g.start.1),
        };
        let offset = start - origin + g.tracker.offset();
        let mut velocity = g.tracker.velocity();
        let step = if cancelled { 0 } else { swipe::decide(offset, velocity, neg_ok, pos_ok) };

        // The camera continues from the displayed position (which may be rubber-banded).
        match axis {
            Axis::X => self.camera.x = Some(Animation::new(pos.0, pos.0, 0.0, switch_spring(), now)),
            Axis::Y => self.camera.y = Some(Animation::new(pos.1, pos.1, 0.0, switch_spring(), now)),
        }
        if (offset > 0.0 && !pos_ok) || (offset < 0.0 && !neg_ok) {
            // Released while rubber-banding: don't fling.
            velocity = 0.0;
        }
        tracing::debug!(?axis, offset, velocity, step, "swipe released");

        if step != 0 {
            let (neg, posd) = dirs_of(axis);
            let dir = if step > 0 { posd } else { neg };
            let to = neighbour(g.origin, dir);
            self.enter_workspace(to);
            self.camera.pending_compact = true;
        }
        self.animate_camera_home(Some((axis, velocity)));
    }

    /// Snap the camera onto the current workspace (the overview zooms out onto it).
    pub fn settle_camera(&mut self) {
        self.camera.x = None;
        self.camera.y = None;
        self.camera.gesture = None;
        self.camera.pending_compact = true;
    }

    /// Grid coordinates moved by `d` (compaction): move any running camera animation along.
    pub fn shift_camera(&mut self, d: (f64, f64)) {
        let now = self.now();
        let shift = |a: Option<Animation>, by: f64| {
            a.map(|a| Animation::new(a.value(now) + by, a.to() + by, a.velocity(now), switch_spring(), now))
        };
        self.camera.x = shift(self.camera.x, d.0);
        self.camera.y = shift(self.camera.y, d.1);
    }

    /// Per-frame: drop finished animations, and compact once everything has settled.
    pub fn tick_camera(&mut self) {
        let now = self.now();
        if self.camera.x.is_some_and(|a| a.is_done(now)) {
            self.camera.x = None;
        }
        if self.camera.y.is_some_and(|a| a.is_done(now)) {
            self.camera.y = None;
        }
        // Wait for the overview too: compacting mid-zoom would move workspaces under it.
        if !self.camera.is_moving() && !self.overview_active() && self.camera.pending_compact {
            self.camera.pending_compact = false;
            self.compact_workspaces();
        }
    }
}
