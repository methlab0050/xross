//! Swipe tracking for workspace gestures: the view follows the fingers 1:1, locked
//! to whichever axis moved first, and on release the target is picked from where the
//! swipe would coast to given its velocity, so a quick flick switches even if the
//! fingers barely moved.
//!
//! Offsets are in workspaces, signed in the camera's direction: fingers moving left
//! pull in the workspace on the right (offset > 0 on X), like content following them.

use std::time::Duration;

/// Finger travel (in touchpad "pixels") for one full workspace (`gestures.swipe_px_per_workspace`).
pub fn px_per_workspace() -> f64 {
    crate::config::current().gestures.swipe_px_per_workspace.max(1.0)
}
/// Travel before the swipe commits to an axis.
const LOCK_THRESHOLD_PX: f64 = 12.0;
/// Velocity is measured over this much recent history.
const VELOCITY_WINDOW: Duration = Duration::from_millis(100);
/// How far ahead (seconds) the release velocity is projected.
const PROJECTION_S: f64 = 0.15;
/// Rubber-banding never pulls further than this (in workspaces).
const RUBBER_LIMIT: f64 = 0.12;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Axis {
    X,
    Y,
}

#[derive(Debug, Default)]
pub struct SwipeTracker {
    accum: (f64, f64),
    axis: Option<Axis>,
    /// (time, offset) samples on the locked axis.
    history: Vec<(Duration, f64)>,
}

impl SwipeTracker {
    pub fn axis(&self) -> Option<Axis> {
        self.axis
    }

    /// Feed a finger delta. Returns the raw offset (in workspaces) once an axis is locked.
    pub fn update(&mut self, dx: f64, dy: f64, now: Duration) -> Option<(Axis, f64)> {
        self.accum.0 += dx;
        self.accum.1 += dy;
        if self.axis.is_none() {
            let (ax, ay) = (self.accum.0.abs(), self.accum.1.abs());
            if ax.max(ay) < LOCK_THRESHOLD_PX {
                return None;
            }
            self.axis = Some(if ax >= ay { Axis::X } else { Axis::Y });
        }
        let axis = self.axis?;
        let offset = self.offset();
        self.history.push((now, offset));
        self.history.retain(|(t, _)| now.saturating_sub(*t) <= VELOCITY_WINDOW);
        Some((axis, offset))
    }

    /// Current raw offset along the locked axis (0 before locking).
    pub fn offset(&self) -> f64 {
        match self.axis {
            Some(Axis::X) => -self.accum.0 / px_per_workspace(),
            Some(Axis::Y) => -self.accum.1 / px_per_workspace(),
            None => 0.0,
        }
    }

    /// Workspaces per second, from recent history.
    pub fn velocity(&self) -> f64 {
        match (self.history.first(), self.history.last()) {
            (Some((t0, o0)), Some((t1, o1))) if t1 > t0 => (o1 - o0) / (*t1 - *t0).as_secs_f64(),
            _ => 0.0,
        }
    }
}

/// Resistance past the allowed range: grows with the pull but never beyond the limit.
pub fn rubber_band(overshoot: f64) -> f64 {
    let x = overshoot.abs();
    overshoot.signum() * RUBBER_LIMIT * (1.0 - 1.0 / (x / RUBBER_LIMIT + 1.0))
}

/// What to show for a raw offset: up to one workspace each way where moving is
/// allowed, rubber-banding where it isn't.
pub fn displayed_offset(offset: f64, allowed_neg: bool, allowed_pos: bool) -> f64 {
    match offset {
        o if o > 0.0 && !allowed_pos => rubber_band(o),
        o if o < 0.0 && !allowed_neg => rubber_band(o),
        o => o.clamp(-1.0, 1.0),
    }
}

/// Where to settle on release: -1, 0 or +1 workspaces from the start.
pub fn decide(offset: f64, velocity: f64, allowed_neg: bool, allowed_pos: bool) -> i32 {
    let projected = offset.clamp(-1.0, 1.0) + velocity * PROJECTION_S;
    if projected > 0.5 && allowed_pos {
        1
    } else if projected < -0.5 && allowed_neg {
        -1
    } else {
        0
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ms(n: u64) -> Duration {
        Duration::from_millis(n)
    }

    #[test]
    fn locks_to_the_dominant_axis_after_the_threshold() {
        let mut s = SwipeTracker::default();
        assert_eq!(s.update(-5.0, 1.0, ms(0)), None);
        let (axis, off) = s.update(-10.0, 2.0, ms(10)).unwrap();
        assert_eq!(axis, Axis::X);
        assert!((off - 15.0 / px_per_workspace()).abs() < 1e-9, "fingers left -> view moves right");
        // Later vertical movement doesn't change the axis.
        s.update(0.0, -200.0, ms(20));
        assert_eq!(s.axis(), Some(Axis::X));
    }

    #[test]
    fn follows_one_to_one() {
        let mut s = SwipeTracker::default();
        s.update(0.0, -px_per_workspace() / 2.0, ms(0));
        assert!((s.offset() - 0.5).abs() < 1e-9);
    }

    #[test]
    fn velocity_from_recent_samples() {
        let mut s = SwipeTracker::default();
        s.update(-30.0, 0.0, ms(0));
        s.update(-30.0, 0.0, ms(50));
        // 30px in 50ms, whatever the sensitivity is tuned to.
        let expected = 30.0 / px_per_workspace() / 0.05;
        assert!((s.velocity() - expected).abs() < 1e-6);
    }

    #[test]
    fn rubber_band_is_bounded_and_monotonic() {
        let mut prev = 0.0;
        for i in 1..100 {
            let v = rubber_band(i as f64 * 0.1);
            assert!(v > prev && v < RUBBER_LIMIT);
            prev = v;
        }
        assert!(rubber_band(-1.0) < 0.0);
    }

    #[test]
    fn displayed_offset_respects_allowed_directions() {
        assert_eq!(displayed_offset(0.7, true, true), 0.7);
        assert_eq!(displayed_offset(1.7, true, true), 1.0);
        assert!(displayed_offset(0.7, true, false) < RUBBER_LIMIT);
        assert!(displayed_offset(-0.7, false, true) > -RUBBER_LIMIT);
    }

    #[test]
    fn decisions() {
        assert_eq!(decide(0.6, 0.0, true, true), 1, "past halfway");
        assert_eq!(decide(0.3, 0.0, true, true), 0, "not far enough");
        assert_eq!(decide(0.15, 3.0, true, true), 1, "a flick carries it over");
        assert_eq!(decide(0.6, -3.0, true, true), 0, "flicking back cancels");
        assert_eq!(decide(0.9, 0.0, true, false), 0, "can't go where it isn't allowed");
        assert_eq!(decide(-0.8, 0.0, true, false), -1);
    }
}
