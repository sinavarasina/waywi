//! Pure-mechanism animation primitives, VSync frame subscriptions, and opt-in retained
//! transitions.
//!
//! Designed around WayWi's zero-idle-CPU policy:
//! - Mathematical primitives ([`Easing`], [`Lerp`], [`SpringConfig`], [`Spring`], [`Tween`],
//!   [`Animated`]) are zero-dependency, stack-allocated, and deterministic.
//! - [`Animation::frames`] ticks strictly on Wayland `wl_surface.frame` VSync callbacks (no
//!   fixed 16ms polling timer, automatic 60/120/144/240Hz adaptation, and 0 wakeups during DPMS
//!   off or fullscreen occlusion).
//! - Opt-in element transitions (`.transition_opacity(spec)` / `.transition_transform(spec)`)
//!   advance directly on retained nodes without rebuilding `view(state)` and stop completely the
//!   instant they settle.

use std::time::{Duration, Instant};

use crate::arena::FrameSharedFn;
use crate::geometry::{Bounds, LogicalPx, LogicalSize, Percent, Point, Transform};
use crate::render::Color;
use crate::subscription::{IntoSubscriptionKey, Subscription, SubscriptionSpec};

/// Maximum physics sub-step (4 ms) for unconditional spring stability across frame hitches.
const SPRING_MAX_STEP_SECS: f32 = 0.004;

/// Pure-mechanism easing curves mapping normalized progress `t in [0.0, 1.0]` to eased progress.
#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub enum Easing {
    #[default]
    Linear,
    QuadIn,
    QuadOut,
    QuadInOut,
    CubicIn,
    CubicOut,
    CubicInOut,
    QuartOut,
    ExpoOut,
    BackOut,
    /// CSS-compatible cubic Bézier curve `(x1, y1, x2, y2)` with `x1, x2 in [0.0, 1.0]`.
    CubicBezier(f32, f32, f32, f32),
}

impl Easing {
    /// Evaluates this easing curve at normalized time `t` (clamped to `0.0..=1.0`).
    #[must_use]
    pub fn sample(self, t: f32) -> f32 {
        let t = t.clamp(0.0, 1.0);
        if t <= 0.0 {
            return 0.0;
        }
        if t >= 1.0 {
            return 1.0;
        }
        match self {
            Self::Linear => t,
            Self::QuadIn => t * t,
            Self::QuadOut => 1.0 - (1.0 - t) * (1.0 - t),
            Self::QuadInOut => {
                if t < 0.5 {
                    2.0 * t * t
                } else {
                    let u = -2.0 * t + 2.0;
                    1.0 - (u * u) * 0.5
                }
            }
            Self::CubicIn => t * t * t,
            Self::CubicOut => {
                let u = 1.0 - t;
                1.0 - u * u * u
            }
            Self::CubicInOut => {
                if t < 0.5 {
                    4.0 * t * t * t
                } else {
                    let u = -2.0 * t + 2.0;
                    1.0 - (u * u * u) * 0.5
                }
            }
            Self::QuartOut => {
                let u = 1.0 - t;
                1.0 - u * u * u * u
            }
            Self::ExpoOut => 1.0 - 2.0f32.powf(-10.0 * t),
            Self::BackOut => {
                const C1: f32 = 1.701_58;
                const C3: f32 = C1 + 1.0;
                let u = t - 1.0;
                1.0 + C3 * u * u * u + C1 * u * u
            }
            Self::CubicBezier(x1, y1, x2, y2) => {
                sample_cubic_bezier(x1.clamp(0.0, 1.0), y1, x2.clamp(0.0, 1.0), y2, t)
            }
        }
    }
}

fn bezier_coord(p1: f32, p2: f32, u: f32) -> f32 {
    let inv = 1.0 - u;
    3.0 * inv * inv * u * p1 + 3.0 * inv * u * u * p2 + u * u * u
}

fn bezier_deriv(p1: f32, p2: f32, u: f32) -> f32 {
    let inv = 1.0 - u;
    3.0 * inv * inv * p1 + 6.0 * inv * u * (p2 - p1) + 3.0 * u * u * (1.0 - p2)
}

fn sample_cubic_bezier(x1: f32, y1: f32, x2: f32, y2: f32, target_x: f32) -> f32 {
    // Solve B_x(u) = target_x via Newton-Raphson with safe bisection fallback.
    let mut lo = 0.0f32;
    let mut hi = 1.0f32;
    let mut u = target_x;
    for _ in 0..8 {
        let x = bezier_coord(x1, x2, u);
        let err = x - target_x;
        if err.abs() < 1e-5 {
            break;
        }
        if err < 0.0 {
            lo = u;
        } else {
            hi = u;
        }
        let d = bezier_deriv(x1, x2, u);
        if d.abs() > 1e-6 {
            let next = u - err / d;
            u = if next > lo && next < hi {
                next
            } else {
                0.5 * (lo + hi)
            };
        } else {
            u = 0.5 * (lo + hi);
        }
    }
    bezier_coord(y1, y2, u)
}

/// Linear interpolation trait for core WayWi scalar, geometry, color, and transform types.
pub trait Lerp: Copy {
    /// Linearly interpolates between `self` (`t = 0.0`) and `other` (`t = 1.0`).
    /// Note: `t` may overshoot `[0.0, 1.0]` for curves such as [`Easing::BackOut`] or springs.
    #[must_use]
    fn lerp(self, other: Self, t: f32) -> Self;
}

impl Lerp for f32 {
    #[inline]
    fn lerp(self, other: Self, t: f32) -> Self {
        self + (other - self) * t
    }
}

impl Lerp for f64 {
    #[inline]
    fn lerp(self, other: Self, t: f32) -> Self {
        self + (other - self) * f64::from(t)
    }
}

impl Lerp for LogicalPx {
    #[inline]
    fn lerp(self, other: Self, t: f32) -> Self {
        LogicalPx(self.0.lerp(other.0, t))
    }
}

impl Lerp for Percent {
    #[inline]
    fn lerp(self, other: Self, t: f32) -> Self {
        Percent(self.0.lerp(other.0, t))
    }
}

impl Lerp for Point {
    #[inline]
    fn lerp(self, other: Self, t: f32) -> Self {
        Point::new(self.x.lerp(other.x, t), self.y.lerp(other.y, t))
    }
}

impl Lerp for LogicalSize {
    #[inline]
    fn lerp(self, other: Self, t: f32) -> Self {
        LogicalSize::new(
            self.width.lerp(other.width, t),
            self.height.lerp(other.height, t),
        )
    }
}

impl Lerp for Bounds {
    #[inline]
    fn lerp(self, other: Self, t: f32) -> Self {
        Bounds::new(
            self.x.lerp(other.x, t),
            self.y.lerp(other.y, t),
            self.width.lerp(other.width, t),
            self.height.lerp(other.height, t),
        )
    }
}

impl Lerp for Color {
    #[inline]
    fn lerp(self, other: Self, t: f32) -> Self {
        let ch = |a: u8, b: u8| {
            (f32::from(a).lerp(f32::from(b), t))
                .round()
                .clamp(0.0, 255.0) as u8
        };
        Color::rgba(
            ch(self.r, other.r),
            ch(self.g, other.g),
            ch(self.b, other.b),
            ch(self.a, other.a),
        )
    }
}

impl Lerp for Transform {
    #[inline]
    fn lerp(self, other: Self, t: f32) -> Self {
        Transform {
            translate_x: self.translate_x.lerp(other.translate_x, t),
            translate_y: self.translate_y.lerp(other.translate_y, t),
            scale_x: self.scale_x.lerp(other.scale_x, t),
            scale_y: self.scale_y.lerp(other.scale_y, t),
            rotation_rad: self.rotation_rad.lerp(other.rotation_rad, t),
            pivot_x: self.pivot_x.lerp(other.pivot_x, t),
            pivot_y: self.pivot_y.lerp(other.pivot_y, t),
        }
    }
}

/// Physical parameters for damped harmonic [`Spring`] integration.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct SpringConfig {
    pub stiffness: f32,
    pub damping: f32,
    pub mass: f32,
    pub rest_delta: f32,
    pub rest_velocity: f32,
}

impl Default for SpringConfig {
    fn default() -> Self {
        Self::critically_damped(220.0)
    }
}

impl SpringConfig {
    /// Creates a spring with explicit `stiffness` (`k`) and `damping` (`c`), `mass = 1.0`.
    #[must_use]
    pub const fn new(stiffness: f32, damping: f32) -> Self {
        Self {
            stiffness,
            damping,
            mass: 1.0,
            rest_delta: 0.001,
            rest_velocity: 0.001,
        }
    }

    /// Creates a critically damped spring (`c = 2 * sqrt(k * m)`) with zero overshoot.
    #[must_use]
    pub fn critically_damped(stiffness: f32) -> Self {
        let k = stiffness.max(1e-3);
        Self::new(k, 2.0 * k.sqrt())
    }

    #[must_use]
    pub const fn with_mass(mut self, mass: f32) -> Self {
        self.mass = mass;
        self
    }

    #[must_use]
    pub const fn with_precision(mut self, rest_delta: f32, rest_velocity: f32) -> Self {
        self.rest_delta = rest_delta;
        self.rest_velocity = rest_velocity;
        self
    }
}

/// Sub-stepped damped harmonic spring controller (`Copy`, zero allocations) with automatic
/// settlement detection so animations reach exact targets and stop waking the event loop.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Spring {
    value: f32,
    target: f32,
    velocity: f32,
    config: SpringConfig,
    settled: bool,
}

impl Spring {
    #[must_use]
    pub const fn new(initial: f32, config: SpringConfig) -> Self {
        Self {
            value: initial,
            target: initial,
            velocity: 0.0,
            config,
            settled: true,
        }
    }

    #[inline]
    #[must_use]
    pub const fn value(&self) -> f32 {
        self.value
    }

    #[inline]
    #[must_use]
    pub const fn target(&self) -> f32 {
        self.target
    }

    #[inline]
    #[must_use]
    pub const fn velocity(&self) -> f32 {
        self.velocity
    }

    #[inline]
    #[must_use]
    pub const fn is_settled(&self) -> bool {
        self.settled
    }

    /// Sets a new target value, preserving current position and velocity for smooth retargeting.
    pub fn set_target(&mut self, target: f32) {
        if !target.is_finite() {
            return;
        }
        if (self.target - target).abs() > f32::EPSILON {
            self.target = target;
            self.settled = false;
        }
    }

    /// Immediately jumps to `value` with zero velocity (`is_settled() == true`).
    pub fn snap_to(&mut self, value: f32) {
        if !value.is_finite() {
            return;
        }
        self.value = value;
        self.target = value;
        self.velocity = 0.0;
        self.settled = true;
    }

    /// Adds an instantaneous velocity impulse (in units/second), waking the spring.
    pub fn impulse(&mut self, velocity_delta: f32) {
        if velocity_delta.is_finite() && velocity_delta.abs() > f32::EPSILON {
            self.velocity += velocity_delta;
            self.settled = false;
        }
    }

    /// Advances the spring physics by `dt` (clamped at 250 ms and sub-stepped at <= 4 ms for
    /// unconditional numerical stability) and returns the updated value.
    pub fn advance(&mut self, dt: Duration) -> f32 {
        if self.settled {
            return self.value;
        }
        if !self.target.is_finite() {
            self.velocity = 0.0;
            self.settled = true;
            return self.value;
        }
        if !self.value.is_finite() || !self.velocity.is_finite() {
            self.value = self.target;
            self.velocity = 0.0;
            self.settled = true;
            return self.value;
        }
        let mut remaining = dt.as_secs_f32().min(0.25);
        if !remaining.is_finite() || remaining <= 0.0 {
            return self.value;
        }
        let k = if self.config.stiffness.is_finite() {
            self.config.stiffness.clamp(1e-3, 1e6)
        } else {
            220.0
        };
        let c = if self.config.damping.is_finite() {
            self.config.damping.clamp(0.0, 1e5)
        } else {
            2.0 * k.sqrt()
        };
        let inv_m = 1.0
            / if self.config.mass.is_finite() {
                self.config.mass.clamp(1e-3, 1e4)
            } else {
                1.0
            };
        let rest_delta = if self.config.rest_delta.is_finite() {
            self.config.rest_delta.max(1e-6)
        } else {
            0.001
        };
        let rest_velocity = if self.config.rest_velocity.is_finite() {
            self.config.rest_velocity.max(1e-6)
        } else {
            0.001
        };

        while remaining > 0.0 {
            let step = remaining.min(SPRING_MAX_STEP_SECS);
            remaining -= step;

            let disp = self.value - self.target;
            let accel = (-k * disp - c * self.velocity) * inv_m;
            self.velocity += accel * step;
            self.value += self.velocity * step;
            if !self.value.is_finite() || !self.velocity.is_finite() {
                self.value = self.target;
                self.velocity = 0.0;
                self.settled = true;
                return self.value;
            }
        }

        if (self.value - self.target).abs() <= rest_delta && self.velocity.abs() <= rest_velocity {
            self.value = self.target;
            self.velocity = 0.0;
            self.settled = true;
        }
        self.value
    }
}

/// Stateless time-based interpolation descriptor between two [`Lerp`] values.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Tween<T: Lerp> {
    pub from: T,
    pub to: T,
    pub duration: Duration,
    pub easing: Easing,
}

impl<T: Lerp> Tween<T> {
    #[must_use]
    pub const fn new(from: T, to: T, duration: Duration, easing: Easing) -> Self {
        Self {
            from,
            to,
            duration,
            easing,
        }
    }

    #[must_use]
    pub fn sample_at(&self, elapsed: Duration) -> T {
        let dur = self.duration.as_secs_f32();
        if dur <= 0.0 || elapsed >= self.duration {
            return self.to;
        }
        let t = self
            .easing
            .sample((elapsed.as_secs_f32() / dur).clamp(0.0, 1.0));
        self.from.lerp(self.to, t)
    }

    #[must_use]
    pub fn is_finished(&self, elapsed: Duration) -> bool {
        self.duration.is_zero() || elapsed >= self.duration
    }
}

/// Headless, pure-mechanism animated value controller for `Application::State` (peer of
/// [`ScrollState`](crate::input::ScrollState) and
/// [`TextInputState`](crate::input::TextInputState)).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Animated<T: Lerp + PartialEq> {
    current: T,
    from: T,
    target: T,
    duration: Duration,
    easing: Easing,
    started_at: Option<Instant>,
}

impl<T: Lerp + PartialEq> Animated<T> {
    #[must_use]
    pub const fn new(initial: T) -> Self {
        Self {
            current: initial,
            from: initial,
            target: initial,
            duration: Duration::ZERO,
            easing: Easing::Linear,
            started_at: None,
        }
    }

    /// Starts or retargets an eased transition from the current visual value at `now`.
    pub fn transition_to(&mut self, target: T, duration: Duration, easing: Easing, now: Instant) {
        let cur = self.sample(now);
        if cur == target || duration.is_zero() {
            self.snap_to(target);
            return;
        }
        self.from = cur;
        self.current = cur;
        self.target = target;
        self.duration = duration;
        self.easing = easing;
        self.started_at = Some(now);
    }

    /// Immediately jumps to `value` and stops animating.
    pub fn snap_to(&mut self, value: T) {
        self.current = value;
        self.from = value;
        self.target = value;
        self.duration = Duration::ZERO;
        self.started_at = None;
    }

    /// Samples and updates the current value at `now`, returning the interpolated value.
    pub fn sample(&mut self, now: Instant) -> T {
        let Some(start) = self.started_at else {
            return self.current;
        };
        let elapsed = now.saturating_duration_since(start);
        let tw = Tween::new(self.from, self.target, self.duration, self.easing);
        self.current = tw.sample_at(elapsed);
        if tw.is_finished(elapsed) {
            self.current = self.target;
            self.started_at = None;
        }
        self.current
    }

    #[inline]
    #[must_use]
    pub const fn value(&self) -> T {
        self.current
    }

    #[inline]
    #[must_use]
    pub const fn target(&self) -> T {
        self.target
    }

    #[inline]
    #[must_use]
    pub fn is_animating(&self, now: Instant) -> bool {
        let Some(start) = self.started_at else {
            return false;
        };
        now.saturating_duration_since(start) < self.duration
    }
}

/// Declarative transition specification for opt-in retained element transitions
/// (`.transition_opacity(spec)` and `.transition_transform(spec)`).
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum TransitionSpec {
    Tween { duration: Duration, easing: Easing },
    Spring(SpringConfig),
}

impl TransitionSpec {
    #[must_use]
    pub const fn tween(duration: Duration, easing: Easing) -> Self {
        Self::Tween { duration, easing }
    }

    #[must_use]
    pub const fn spring(config: SpringConfig) -> Self {
        Self::Spring(config)
    }
}

/// VSync animation subscription factory synchronized with Wayland `wl_surface.frame`.
pub struct Animation;

impl Animation {
    /// Subscribes to monitor-synchronized VSync frame ticks (`Instant`) while included in
    /// `Application::subscriptions(state)`.
    ///
    /// Remove this subscription from `subscriptions(state)` once your animation settles to return
    /// immediately to 0% idle CPU/GPU usage and 0 wakeups.
    pub fn frames<M: 'static>(
        key: impl IntoSubscriptionKey,
        map: impl Fn(Instant) -> M + 'static,
    ) -> Subscription<M> {
        Subscription {
            spec: SubscriptionSpec::AnimationFrame {
                key: key.into_subscription_key(),
                map: FrameSharedFn::new(map),
            },
        }
    }
}

// ---- Internal retained transition state (allocated only on nodes that opt in) ----

#[derive(Debug, Clone, Copy)]
enum ScalarDriver {
    Tween {
        from: f32,
        duration: Duration,
        easing: Easing,
        started_at: Instant,
    },
    Spring {
        spring: Spring,
        last_tick: Instant,
    },
}

#[derive(Debug, Clone, Copy)]
pub(crate) struct RetainedScalarTransition {
    spec: TransitionSpec,
    current: f32,
    target: f32,
    driver: Option<ScalarDriver>,
}

impl RetainedScalarTransition {
    pub(crate) fn new(initial: f32, spec: TransitionSpec) -> Self {
        Self {
            spec,
            current: initial,
            target: initial,
            driver: None,
        }
    }

    pub(crate) fn retarget(&mut self, new_target: f32, spec: TransitionSpec, now: Instant) {
        self.spec = spec;
        if (self.target - new_target).abs() <= f32::EPSILON {
            return;
        }
        self.target = new_target;
        if (self.current - new_target).abs() <= 1e-4 {
            self.current = new_target;
            self.driver = None;
            return;
        }
        match spec {
            TransitionSpec::Tween { duration, easing } => {
                if duration.is_zero() {
                    self.current = new_target;
                    self.driver = None;
                } else {
                    self.driver = Some(ScalarDriver::Tween {
                        from: self.current,
                        duration,
                        easing,
                        started_at: now,
                    });
                }
            }
            TransitionSpec::Spring(cfg) => {
                let mut sp = match self.driver {
                    Some(ScalarDriver::Spring { spring, .. }) => {
                        let mut s = Spring::new(self.current, cfg);
                        s.impulse(spring.velocity());
                        s
                    }
                    _ => Spring::new(self.current, cfg),
                };
                sp.set_target(new_target);
                self.driver = Some(ScalarDriver::Spring {
                    spring: sp,
                    last_tick: now,
                });
            }
        }
    }

    pub(crate) fn advance(&mut self, now: Instant) -> bool {
        let prev = self.current;
        match self.driver {
            None => false,
            Some(ScalarDriver::Tween {
                from,
                duration,
                easing,
                started_at,
            }) => {
                let elapsed = now.saturating_duration_since(started_at);
                let tw = Tween::new(from, self.target, duration, easing);
                self.current = tw.sample_at(elapsed);
                if tw.is_finished(elapsed) {
                    self.current = self.target;
                    self.driver = None;
                }
                (self.current - prev).abs() > f32::EPSILON || self.driver.is_none()
            }
            Some(ScalarDriver::Spring {
                mut spring,
                last_tick,
            }) => {
                let dt = now.saturating_duration_since(last_tick);
                self.current = spring.advance(dt);
                if spring.is_settled() {
                    self.current = self.target;
                    self.driver = None;
                } else {
                    self.driver = Some(ScalarDriver::Spring {
                        spring,
                        last_tick: now,
                    });
                }
                (self.current - prev).abs() > f32::EPSILON || self.driver.is_none()
            }
        }
    }

    #[inline]
    pub(crate) fn value(&self) -> f32 {
        self.current
    }

    #[inline]
    pub(crate) fn is_active(&self) -> bool {
        self.driver.is_some()
    }
}

#[derive(Debug, Clone, Copy)]
pub(crate) struct RetainedTransformTransition {
    tx: RetainedScalarTransition,
    ty: RetainedScalarTransition,
    sx: RetainedScalarTransition,
    sy: RetainedScalarTransition,
    rot: RetainedScalarTransition,
    pivot_x: f32,
    pivot_y: f32,
}

impl RetainedTransformTransition {
    pub(crate) fn new(initial: Transform, spec: TransitionSpec) -> Self {
        Self {
            tx: RetainedScalarTransition::new(initial.translate_x.0, spec),
            ty: RetainedScalarTransition::new(initial.translate_y.0, spec),
            sx: RetainedScalarTransition::new(initial.scale_x, spec),
            sy: RetainedScalarTransition::new(initial.scale_y, spec),
            rot: RetainedScalarTransition::new(initial.rotation_rad, spec),
            pivot_x: initial.pivot_x,
            pivot_y: initial.pivot_y,
        }
    }

    pub(crate) fn retarget(&mut self, target: Transform, spec: TransitionSpec, now: Instant) {
        self.tx.retarget(target.translate_x.0, spec, now);
        self.ty.retarget(target.translate_y.0, spec, now);
        self.sx.retarget(target.scale_x, spec, now);
        self.sy.retarget(target.scale_y, spec, now);
        self.rot.retarget(target.rotation_rad, spec, now);
        self.pivot_x = target.pivot_x;
        self.pivot_y = target.pivot_y;
    }

    pub(crate) fn advance(&mut self, now: Instant) -> bool {
        let mut changed = false;
        changed |= self.tx.advance(now);
        changed |= self.ty.advance(now);
        changed |= self.sx.advance(now);
        changed |= self.sy.advance(now);
        changed |= self.rot.advance(now);
        changed
    }

    #[inline]
    pub(crate) fn value(&self) -> Transform {
        Transform {
            translate_x: LogicalPx(self.tx.value()),
            translate_y: LogicalPx(self.ty.value()),
            scale_x: self.sx.value(),
            scale_y: self.sy.value(),
            rotation_rad: self.rot.value(),
            pivot_x: self.pivot_x,
            pivot_y: self.pivot_y,
        }
    }

    #[inline]
    pub(crate) fn is_active(&self) -> bool {
        self.tx.is_active()
            || self.ty.is_active()
            || self.sx.is_active()
            || self.sy.is_active()
            || self.rot.is_active()
    }
}

/// Optional boxed retained transition state attached only to elements that declare
/// `.transition_opacity(...)` or `.transition_transform(...)`.
#[derive(Debug, Clone)]
pub(crate) struct RetainedTransitions {
    pub(crate) opacity: Option<RetainedScalarTransition>,
    pub(crate) transform: Option<RetainedTransformTransition>,
}

impl RetainedTransitions {
    #[inline]
    pub(crate) fn is_active(&self) -> bool {
        self.opacity.as_ref().is_some_and(|o| o.is_active())
            || self.transform.as_ref().is_some_and(|t| t.is_active())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn easing_curves_satisfy_endpoints_and_cubic_bezier() {
        let curves = [
            Easing::Linear,
            Easing::QuadIn,
            Easing::QuadOut,
            Easing::QuadInOut,
            Easing::CubicIn,
            Easing::CubicOut,
            Easing::CubicInOut,
            Easing::QuartOut,
            Easing::ExpoOut,
            Easing::BackOut,
            Easing::CubicBezier(0.25, 0.1, 0.25, 1.0),
        ];
        for c in curves {
            assert_eq!(c.sample(0.0), 0.0);
            assert_eq!(c.sample(1.0), 1.0);
        }
        assert!(Easing::CubicOut.sample(0.5) > 0.5);
        assert!(Easing::CubicIn.sample(0.5) < 0.5);
        assert!(Easing::BackOut.sample(0.7) > 1.0);
        // Linear bezier (0,0)-(1,1) matches identity.
        assert!((Easing::CubicBezier(0.0, 0.0, 1.0, 1.0).sample(0.35) - 0.35).abs() < 1e-4);
    }

    #[test]
    fn spring_guards_against_nan_and_extreme_stiffness() {
        // Non-finite targets and impulses must never destabilize or loop forever.
        let mut s = Spring::new(1.0, SpringConfig::new(f32::NAN, f32::INFINITY));
        s.set_target(f32::NAN);
        assert!(s.is_settled());
        s.set_target(2.0);
        s.impulse(f32::NAN);
        for _ in 0..400 {
            let v = s.advance(Duration::from_millis(16));
            assert!(v.is_finite());
            if s.is_settled() {
                break;
            }
        }
        assert!(s.is_settled());
        assert!((s.value() - 2.0).abs() < 1e-4);

        // Extreme stiffness with zero damping still clamps safely and never produces NaN/Inf.
        let mut extreme = Spring::new(0.0, SpringConfig::new(1e12, 0.0).with_mass(1e-9));
        extreme.set_target(1.0);
        for _ in 0..50 {
            let v = extreme.advance(Duration::from_millis(16));
            assert!(v.is_finite());
        }
    }

    #[test]
    fn spring_integrates_smoothly_and_auto_settles_to_exact_target() {
        let mut spring = Spring::new(0.0, SpringConfig::critically_damped(300.0));
        assert!(spring.is_settled());
        spring.set_target(100.0);
        assert!(!spring.is_settled());

        for _ in 0..200 {
            spring.advance(Duration::from_millis(8));
            if spring.is_settled() {
                break;
            }
        }
        assert!(spring.is_settled(), "spring should settle cleanly");
        assert_eq!(spring.value(), 100.0);
        assert_eq!(spring.velocity(), 0.0);
    }

    #[test]
    fn animated_controller_interpolates_color_and_transform_and_stops() {
        let t0 = Instant::now();
        let mut anim = Animated::new(Color::rgba(0, 0, 0, 0));
        anim.transition_to(
            Color::rgba(200, 100, 50, 255),
            Duration::from_millis(200),
            Easing::Linear,
            t0,
        );
        assert!(anim.is_animating(t0 + Duration::from_millis(50)));
        assert_eq!(
            anim.sample(t0 + Duration::from_millis(100)),
            Color::rgba(100, 50, 25, 128)
        );
        assert_eq!(
            anim.sample(t0 + Duration::from_millis(250)),
            Color::rgba(200, 100, 50, 255)
        );
        assert!(!anim.is_animating(t0 + Duration::from_millis(250)));

        let tf = Transform::identity().lerp(Transform::identity().scale(2.0), 0.5);
        assert!((tf.scale_x - 1.5).abs() < 1e-5);
        assert!((tf.scale_y - 1.5).abs() < 1e-5);
    }
}
