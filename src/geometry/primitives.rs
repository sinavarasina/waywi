use super::units::{BufferPx, LogicalPx, Percent, ScaleFactor};

/// 2D point in logical pixels.
#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub struct Point {
    pub x: LogicalPx,
    pub y: LogicalPx,
}

impl Point {
    pub const ZERO: Self = Self {
        x: LogicalPx::ZERO,
        y: LogicalPx::ZERO,
    };

    #[inline]
    #[must_use]
    pub const fn new(x: LogicalPx, y: LogicalPx) -> Self {
        Self { x, y }
    }
}

/// Resolved logical dimensions.
#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub struct LogicalSize {
    pub width: LogicalPx,
    pub height: LogicalPx,
}

impl LogicalSize {
    pub const ZERO: Self = Self {
        width: LogicalPx::ZERO,
        height: LogicalPx::ZERO,
    };

    #[inline]
    #[must_use]
    pub const fn new(width: LogicalPx, height: LogicalPx) -> Self {
        Self { width, height }
    }

    #[inline]
    #[must_use]
    pub fn to_buffer_ceil(self, scale: ScaleFactor) -> (BufferPx, BufferPx) {
        (
            self.width.to_buffer_ceil(scale),
            self.height.to_buffer_ceil(scale),
        )
    }
}

/// Layout dimension specification.
#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub enum Length {
    /// Fixed logical dimension.
    Px(LogicalPx),
    /// Percentage of the owning container or surface dimension.
    Percent(Percent),
    /// Fill all remaining available space along this axis.
    Fill,
    /// Intrinsic content-derived dimension.
    #[default]
    Auto,
}

impl From<LogicalPx> for Length {
    #[inline]
    fn from(value: LogicalPx) -> Self {
        Self::Px(value)
    }
}

impl From<Percent> for Length {
    #[inline]
    fn from(value: Percent) -> Self {
        Self::Percent(value)
    }
}

/// Logical width and height pair.
#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub struct Size {
    pub width: Length,
    pub height: Length,
}

impl Size {
    #[inline]
    #[must_use]
    pub fn new(width: impl Into<Length>, height: impl Into<Length>) -> Self {
        Self {
            width: width.into(),
            height: height.into(),
        }
    }

    #[inline]
    #[must_use]
    pub const fn fill() -> Self {
        Self {
            width: Length::Fill,
            height: Length::Fill,
        }
    }

    #[inline]
    #[must_use]
    pub const fn auto() -> Self {
        Self {
            width: Length::Auto,
            height: Length::Auto,
        }
    }
}

impl<W: Into<Length>, H: Into<Length>> From<(W, H)> for Size {
    #[inline]
    fn from((w, h): (W, H)) -> Self {
        Self::new(w, h)
    }
}

impl From<LogicalSize> for Size {
    #[inline]
    fn from(size: LogicalSize) -> Self {
        Self::new(size.width, size.height)
    }
}

impl From<LogicalPx> for Edges {
    #[inline]
    fn from(all: LogicalPx) -> Self {
        Self::all(all)
    }
}

impl From<(LogicalPx, LogicalPx)> for Edges {
    #[inline]
    fn from((vertical, horizontal): (LogicalPx, LogicalPx)) -> Self {
        Self::symmetric(vertical, horizontal)
    }
}

/// Logical edge insets (margins or padding).
#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub struct Edges {
    pub top: LogicalPx,
    pub right: LogicalPx,
    pub bottom: LogicalPx,
    pub left: LogicalPx,
}

impl Edges {
    pub const ZERO: Self = Self {
        top: LogicalPx::ZERO,
        right: LogicalPx::ZERO,
        bottom: LogicalPx::ZERO,
        left: LogicalPx::ZERO,
    };

    #[inline]
    #[must_use]
    pub const fn new(top: LogicalPx, right: LogicalPx, bottom: LogicalPx, left: LogicalPx) -> Self {
        Self {
            top,
            right,
            bottom,
            left,
        }
    }

    #[inline]
    #[must_use]
    pub const fn all(v: LogicalPx) -> Self {
        Self::new(v, v, v, v)
    }

    #[inline]
    #[must_use]
    pub const fn symmetric(vertical: LogicalPx, horizontal: LogicalPx) -> Self {
        Self::new(vertical, horizontal, vertical, horizontal)
    }

    #[inline]
    #[must_use]
    pub const fn top(v: LogicalPx) -> Self {
        Self::new(v, LogicalPx::ZERO, LogicalPx::ZERO, LogicalPx::ZERO)
    }

    #[inline]
    #[must_use]
    pub const fn right(v: LogicalPx) -> Self {
        Self::new(LogicalPx::ZERO, v, LogicalPx::ZERO, LogicalPx::ZERO)
    }

    #[inline]
    #[must_use]
    pub const fn bottom(v: LogicalPx) -> Self {
        Self::new(LogicalPx::ZERO, LogicalPx::ZERO, v, LogicalPx::ZERO)
    }

    #[inline]
    #[must_use]
    pub const fn left(v: LogicalPx) -> Self {
        Self::new(LogicalPx::ZERO, LogicalPx::ZERO, LogicalPx::ZERO, v)
    }

    #[inline]
    #[must_use]
    pub const fn top_left(top: LogicalPx, left: LogicalPx) -> Self {
        Self::new(top, LogicalPx::ZERO, LogicalPx::ZERO, left)
    }

    #[inline]
    #[must_use]
    pub const fn top_right(top: LogicalPx, right: LogicalPx) -> Self {
        Self::new(top, right, LogicalPx::ZERO, LogicalPx::ZERO)
    }

    #[inline]
    #[must_use]
    pub const fn bottom_left(bottom: LogicalPx, left: LogicalPx) -> Self {
        Self::new(LogicalPx::ZERO, LogicalPx::ZERO, bottom, left)
    }

    #[inline]
    #[must_use]
    pub const fn bottom_right(bottom: LogicalPx, right: LogicalPx) -> Self {
        Self::new(LogicalPx::ZERO, right, bottom, LogicalPx::ZERO)
    }
}
