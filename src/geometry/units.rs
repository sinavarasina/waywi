use std::ops::{Add, Div, Mul, Neg, Sub};

/// Surface-local logical coordinate unit, independent of physical output scaling.
#[derive(Debug, Clone, Copy, PartialEq, PartialOrd, Default)]
pub struct LogicalPx(pub f32);

impl LogicalPx {
    pub const ZERO: Self = Self(0.0);

    #[inline]
    #[must_use]
    pub const fn new(value: f32) -> Self {
        Self(value)
    }

    #[inline]
    #[must_use]
    pub const fn get(self) -> f32 {
        self.0
    }

    #[inline]
    #[must_use]
    pub fn round_i32(self) -> i32 {
        self.0.round() as i32
    }

    #[inline]
    #[must_use]
    pub fn ceil_u32(self) -> u32 {
        self.0.max(0.0).ceil() as u32
    }

    #[inline]
    #[must_use]
    pub fn floor_i32(self) -> i32 {
        self.0.floor() as i32
    }

    #[inline]
    #[must_use]
    pub fn min(self, other: Self) -> Self {
        Self(self.0.min(other.0))
    }

    #[inline]
    #[must_use]
    pub fn max(self, other: Self) -> Self {
        Self(self.0.max(other.0))
    }

    #[inline]
    #[must_use]
    pub fn clamp(self, min: Self, max: Self) -> Self {
        Self(self.0.clamp(min.0, max.0))
    }

    /// Converts a logical coordinate/length into physical buffer pixels under `scale`.
    #[inline]
    #[must_use]
    pub fn to_buffer(self, scale: ScaleFactor) -> BufferPx {
        BufferPx((self.0 * scale.as_f32()).round() as i32)
    }

    /// Converts a logical dimension into a non-zero rounded-up physical buffer extent.
    #[inline]
    #[must_use]
    pub fn to_buffer_ceil(self, scale: ScaleFactor) -> BufferPx {
        BufferPx((self.0.max(0.0) * scale.as_f32()).ceil() as i32)
    }
}

impl Add for LogicalPx {
    type Output = Self;
    #[inline]
    fn add(self, rhs: Self) -> Self::Output {
        Self(self.0 + rhs.0)
    }
}

impl Sub for LogicalPx {
    type Output = Self;
    #[inline]
    fn sub(self, rhs: Self) -> Self::Output {
        Self(self.0 - rhs.0)
    }
}

impl Mul<f32> for LogicalPx {
    type Output = Self;
    #[inline]
    fn mul(self, rhs: f32) -> Self::Output {
        Self(self.0 * rhs)
    }
}

impl Div<f32> for LogicalPx {
    type Output = Self;
    #[inline]
    fn div(self, rhs: f32) -> Self::Output {
        Self(self.0 / rhs)
    }
}

impl Neg for LogicalPx {
    type Output = Self;
    #[inline]
    fn neg(self) -> Self::Output {
        Self(-self.0)
    }
}

/// Physical pixel unit inside a Wayland `wl_shm` buffer.
///
/// Kept distinct from [`LogicalPx`] so logical surface coordinates and physical buffer pixels
/// cannot be mixed silently.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Default)]
pub struct BufferPx(pub i32);

impl BufferPx {
    pub const ZERO: Self = Self(0);

    #[inline]
    #[must_use]
    pub const fn new(value: i32) -> Self {
        Self(value)
    }

    #[inline]
    #[must_use]
    pub const fn get(self) -> i32 {
        self.0
    }

    #[inline]
    #[must_use]
    pub fn as_u32(self) -> u32 {
        self.0.max(0) as u32
    }

    #[inline]
    #[must_use]
    pub fn to_logical(self, scale: ScaleFactor) -> LogicalPx {
        LogicalPx(self.0 as f32 / scale.as_f32())
    }
}

impl Add for BufferPx {
    type Output = Self;
    #[inline]
    fn add(self, rhs: Self) -> Self::Output {
        Self(self.0 + rhs.0)
    }
}

impl Sub for BufferPx {
    type Output = Self;
    #[inline]
    fn sub(self, rhs: Self) -> Self::Output {
        Self(self.0 - rhs.0)
    }
}

/// Percentage value (`0.0..=100.0` nominal, unclamped).
#[derive(Debug, Clone, Copy, PartialEq, PartialOrd, Default)]
pub struct Percent(pub f32);

impl Percent {
    #[inline]
    #[must_use]
    pub const fn new(value: f32) -> Self {
        Self(value)
    }

    #[inline]
    #[must_use]
    pub const fn get(self) -> f32 {
        self.0
    }

    #[inline]
    #[must_use]
    pub fn fraction(self) -> f32 {
        self.0 / 100.0
    }

    #[inline]
    #[must_use]
    pub fn of(self, extent: LogicalPx) -> LogicalPx {
        LogicalPx(extent.0 * self.fraction())
    }
}

/// Exact scale factor represented in `1/120` increments per `wp-fractional-scale-v1`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct ScaleFactor(u32);

impl ScaleFactor {
    /// Unit scale `1.0` (`120 / 120`).
    pub const ONE: Self = Self(120);

    /// Creates a scale factor from an integer scale (clamped to `>= 1`).
    #[inline]
    #[must_use]
    pub const fn from_integer(scale: i32) -> Self {
        let clamped = if scale < 1 { 1 } else { scale as u32 };
        Self(clamped.saturating_mul(120))
    }

    /// Creates a scale factor from `wp_fractional_scale_v1::preferred_scale` (`scale / 120`).
    #[inline]
    #[must_use]
    pub const fn from_120ths(numerator: u32) -> Self {
        Self(if numerator == 0 { 120 } else { numerator })
    }

    /// Creates a scale factor from a floating-point multiplier (`>= 0.25`).
    #[inline]
    #[must_use]
    pub fn from_f32(scale: f32) -> Self {
        let n = (scale.max(0.25) * 120.0).round() as u32;
        Self(n.max(30))
    }

    #[inline]
    #[must_use]
    pub const fn as_120ths(self) -> u32 {
        self.0
    }

    #[inline]
    #[must_use]
    pub fn as_f32(self) -> f32 {
        self.0 as f32 / 120.0
    }

    #[inline]
    #[must_use]
    pub fn integer_ceil(self) -> i32 {
        self.0.div_ceil(120).max(1) as i32
    }

    #[inline]
    #[must_use]
    pub fn is_integer(self) -> bool {
        self.0.is_multiple_of(120)
    }
}

impl Default for ScaleFactor {
    fn default() -> Self {
        Self::ONE
    }
}

/// Extension trait providing `.logical_px()` on `i32` and `f32`.
///
/// Implemented only for `i32` and `f32` so integer and float literals infer unambiguously.
pub trait LogicalPxExt {
    fn logical_px(self) -> LogicalPx;
}

impl LogicalPxExt for i32 {
    #[inline]
    fn logical_px(self) -> LogicalPx {
        LogicalPx(self as f32)
    }
}

impl LogicalPxExt for f32 {
    #[inline]
    fn logical_px(self) -> LogicalPx {
        LogicalPx(self)
    }
}

/// Extension trait providing `.percent()` on `i32` and `f32`.
pub trait PercentExt {
    fn percent(self) -> Percent;
}

impl PercentExt for i32 {
    #[inline]
    fn percent(self) -> Percent {
        Percent(self as f32)
    }
}

impl PercentExt for f32 {
    #[inline]
    fn percent(self) -> Percent {
        Percent(self)
    }
}
