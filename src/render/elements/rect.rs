use crate::geometry::LogicalPx;
use crate::input::CommonProps;
use crate::render::style::{Border, Color, CornerRadius};

/// Rectangle primitive with fill color, corner radius, and optional border.
#[must_use]
#[derive(Clone)]
pub struct Rect<M = ()> {
    pub(crate) common: CommonProps<M>,
    pub(crate) fill: Color,
    pub(crate) corner_radius: CornerRadius,
    pub(crate) border: Option<Border>,
}

impl<M: 'static> Default for Rect<M> {
    fn default() -> Self {
        Self::new()
    }
}

impl<M: 'static> Rect<M> {
    pub fn new() -> Self {
        Self {
            common: CommonProps::default(),
            fill: Color::TRANSPARENT,
            corner_radius: CornerRadius::ZERO,
            border: None,
        }
    }

    pub fn fill(mut self, color: Color) -> Self {
        self.fill = color;
        self
    }

    pub fn corner_radius(mut self, radius: impl Into<CornerRadius>) -> Self {
        self.corner_radius = radius.into();
        self
    }

    pub fn border(mut self, width: LogicalPx, color: Color) -> Self {
        self.border = Some(Border { width, color });
        self
    }

    crate::input::impl_common_builders!();

    pub fn map<N: 'static, F>(self, f: F) -> Rect<N>
    where
        F: Fn(M) -> N + 'static,
    {
        Rect {
            common: self.common.map(f),
            fill: self.fill,
            corner_radius: self.corner_radius,
            border: self.border,
        }
    }
}
