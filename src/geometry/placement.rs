use super::primitives::Point;
use super::units::{LogicalPx, Percent};

/// Element positioning relative to its surface or parent container.
#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub enum Position {
    /// Positioned automatically by parent layout flow (e.g. `Row`, `Column`, or `(0, 0)`).
    #[default]
    Auto,
    /// Absolute logical pixel offset from the top-left of the owning surface/coordinate parent.
    Absolute { x: LogicalPx, y: LogicalPx },
    /// Percentage of the owning surface's logical dimensions, plus optional logical pixel offset.
    RelativeToSurface {
        x: Percent,
        y: Percent,
        offset_x: LogicalPx,
        offset_y: LogicalPx,
    },
    /// Percentage of the immediate parent container's logical dimensions, plus optional offset.
    RelativeToParent {
        x: Percent,
        y: Percent,
        offset_x: LogicalPx,
        offset_y: LogicalPx,
    },
}

impl Position {
    #[inline]
    #[must_use]
    pub const fn absolute(x: LogicalPx, y: LogicalPx) -> Self {
        Self::Absolute { x, y }
    }

    #[inline]
    #[must_use]
    pub const fn relative_to_surface(x: Percent, y: Percent) -> Self {
        Self::RelativeToSurface {
            x,
            y,
            offset_x: LogicalPx::ZERO,
            offset_y: LogicalPx::ZERO,
        }
    }

    #[inline]
    #[must_use]
    pub const fn relative_to_parent(x: Percent, y: Percent) -> Self {
        Self::RelativeToParent {
            x,
            y,
            offset_x: LogicalPx::ZERO,
            offset_y: LogicalPx::ZERO,
        }
    }

    #[inline]
    #[must_use]
    pub fn with_offset(self, dx: LogicalPx, dy: LogicalPx) -> Self {
        match self {
            Self::Auto => Self::Absolute { x: dx, y: dy },
            Self::Absolute { x, y } => Self::Absolute {
                x: x + dx,
                y: y + dy,
            },
            Self::RelativeToSurface {
                x,
                y,
                offset_x,
                offset_y,
            } => Self::RelativeToSurface {
                x,
                y,
                offset_x: offset_x + dx,
                offset_y: offset_y + dy,
            },
            Self::RelativeToParent {
                x,
                y,
                offset_x,
                offset_y,
            } => Self::RelativeToParent {
                x,
                y,
                offset_x: offset_x + dx,
                offset_y: offset_y + dy,
            },
        }
    }
}

/// Element-local anchor point used to align an element relative to its resolved position.
///
/// Distinct from surface edge [`Anchor`](crate::surface::Anchor), which controls compositor
/// layer-shell attachment.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub enum ElementAnchor {
    #[default]
    TopLeft,
    TopCenter,
    TopRight,
    CenterLeft,
    Center,
    CenterRight,
    BottomLeft,
    BottomCenter,
    BottomRight,
}

impl ElementAnchor {
    /// Returns the translation offset subtracted from the anchor coordinate to obtain top-left.
    #[must_use]
    pub fn top_left_offset(self, width: LogicalPx, height: LogicalPx) -> Point {
        let x = match self {
            Self::TopLeft | Self::CenterLeft | Self::BottomLeft => LogicalPx::ZERO,
            Self::TopCenter | Self::Center | Self::BottomCenter => width / 2.0,
            Self::TopRight | Self::CenterRight | Self::BottomRight => width,
        };
        let y = match self {
            Self::TopLeft | Self::TopCenter | Self::TopRight => LogicalPx::ZERO,
            Self::CenterLeft | Self::Center | Self::CenterRight => height / 2.0,
            Self::BottomLeft | Self::BottomCenter | Self::BottomRight => height,
        };
        Point::new(x, y)
    }
}
