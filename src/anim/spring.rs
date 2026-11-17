//! Damped springs with closed-form solutions (mass = 1).
//!
//! A spring is fully described by where it starts, where it's going, and its initial
//! velocity, so an animation can be retargeted mid-flight without a jump: start a new
//! spring from the current value *and velocity*. That's what makes gestures hand off
//! smoothly to animations.

use std::time::Duration;

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct SpringParams {
    /// 1.0 is critically damped (fastest without overshoot); < 1 bounces.
    pub damping_ratio: f64,
    pub stiffness: f64,
    /// Settled once both the distance to the target and the velocity are below this.
    pub epsilon: f64,
}

impl SpringParams {
    pub const fn new(damping_ratio: f64, stiffness: f64, epsilon: f64) -> Self {
        Self { damping_ratio, stiffness, epsilon }
    }
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Spring {
    pub from: f64,
    pub to: f64,
    pub initial_velocity: f64,
    pub params: SpringParams,
}

impl Spring {
    pub fn value_at(&self, t: f64) -> f64 {
        let x0 = self.from - self.to;
        let v0 = self.initial_velocity;
        let w0 = self.params.stiffness.sqrt();
        let zeta = self.params.damping_ratio;
        let offset = if zeta < 1.0 {
            let wd = w0 * (1.0 - zeta * zeta).sqrt();
            let envelope = (-zeta * w0 * t).exp();
            envelope * (x0 * (wd * t).cos() + (v0 + zeta * w0 * x0) / wd * (wd * t).sin())
        } else if zeta == 1.0 {
            (x0 + (v0 + w0 * x0) * t) * (-w0 * t).exp()
        } else {
            let s = (zeta * zeta - 1.0).sqrt();
            let (r1, r2) = (-w0 * (zeta - s), -w0 * (zeta + s));
            let c2 = (v0 - r1 * x0) / (r2 - r1);
            let c1 = x0 - c2;
            c1 * (r1 * t).exp() + c2 * (r2 * t).exp()
        };
        self.to + offset
    }

    pub fn velocity_at(&self, t: f64) -> f64 {
        // Central difference; the closed forms are smooth, so this is plenty accurate.
        let h = 1e-4;
        (self.value_at(t + h) - self.value_at((t - h).max(0.0))) / (t + h - (t - h).max(0.0))
    }

    pub fn is_done_at(&self, t: f64) -> bool {
        let eps = self.params.epsilon;
        (self.value_at(t) - self.to).abs() < eps && self.velocity_at(t).abs() < eps * 10.0
    }
}

/// A spring running on the compositor clock.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Animation {
    spring: Spring,
    start: Duration,
}

impl Animation {
    pub fn new(from: f64, to: f64, initial_velocity: f64, params: SpringParams, now: Duration) -> Self {
        Self { spring: Spring { from, to, initial_velocity, params }, start: now }
    }

    fn t(&self, now: Duration) -> f64 {
        now.saturating_sub(self.start).as_secs_f64()
    }

    pub fn value(&self, now: Duration) -> f64 {
        if self.is_done(now) { self.spring.to } else { self.spring.value_at(self.t(now)) }
    }

    pub fn velocity(&self, now: Duration) -> f64 {
        if self.is_done(now) { 0.0 } else { self.spring.velocity_at(self.t(now)) }
    }

    pub fn to(&self) -> f64 {
        self.spring.to
    }

    pub fn is_done(&self, now: Duration) -> bool {
        self.spring.is_done_at(self.t(now))
    }

    /// Head for a new target from wherever we are now, keeping the current velocity.
    pub fn retarget(&self, to: f64, now: Duration) -> Self {
        Self::new(self.value(now), to, self.velocity(now), self.spring.params, now)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const CRIT: SpringParams = SpringParams::new(1.0, 800.0, 0.0001);

    fn ms(n: u64) -> Duration {
        Duration::from_millis(n)
    }

    #[test]
    fn starts_at_from_and_settles_at_to() {
        let a = Animation::new(0.0, 1.0, 0.0, CRIT, ms(0));
        assert_eq!(a.value(ms(0)), 0.0);
        assert!(!a.is_done(ms(50)));
        assert!(a.is_done(ms(1000)));
        assert_eq!(a.value(ms(1000)), 1.0);
    }

    #[test]
    fn critically_damped_never_overshoots() {
        let a = Animation::new(0.0, 1.0, 0.0, CRIT, ms(0));
        let mut prev = 0.0;
        for i in 1..600 {
            let v = a.value(ms(i));
            assert!(v <= 1.0 + 1e-9, "overshot at {i}ms: {v}");
            assert!(v >= prev - 1e-9, "went backwards at {i}ms");
            prev = v;
        }
    }

    #[test]
    fn underdamped_overshoots() {
        let a = Animation::new(0.0, 1.0, 0.0, SpringParams::new(0.5, 800.0, 0.0001), ms(0));
        assert!((0..500).map(|i| a.value(ms(i))).any(|v| v > 1.0));
    }

    #[test]
    fn overdamped_settles_without_overshoot() {
        let a = Animation::new(0.0, 1.0, 0.0, SpringParams::new(1.5, 800.0, 0.0001), ms(0));
        assert!((0..2000).map(|i| a.value(ms(i))).all(|v| v <= 1.0 + 1e-9));
        assert!(a.is_done(ms(3000)));
    }

    #[test]
    fn initial_velocity_is_respected() {
        let a = Animation::new(0.0, 0.0, 5.0, CRIT, ms(0));
        assert!((a.velocity(ms(0)) - 5.0).abs() < 0.05);
        assert!(a.value(ms(20)) > 0.0, "a push moves it before it returns");
        assert!(a.is_done(ms(1000)));
    }

    #[test]
    fn retarget_is_continuous() {
        let a = Animation::new(0.0, 1.0, 0.0, CRIT, ms(0));
        let (v, vel) = (a.value(ms(40)), a.velocity(ms(40)));
        let b = a.retarget(2.0, ms(40));
        assert!((b.value(ms(40)) - v).abs() < 1e-9);
        assert!((b.velocity(ms(40)) - vel).abs() < 0.05);
        assert_eq!(b.to(), 2.0);
    }
}
