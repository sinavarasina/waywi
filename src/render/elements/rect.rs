use crate::geometry::LogicalPx;
use crate::input::CommonProps;
use crate::render::style::{Border, BoxShadow, Color, CornerRadius, Fill};

/// Rectangle primitive with solid or gradient fill, corner radius, optional border, and optional shadow.
#[must_use]
#[derive(Clone)]
pub struct Rect<M = ()> {
    pub(crate) common: CommonProps<M>,
    pub(crate) fill: Fill,
    pub(crate) corner_radius: CornerRadius,
    pub(crate) border: Option<Border>,
    pub(crate) shadow: Option<BoxShadow>,
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
            fill: Fill::Solid(Color::TRANSPARENT),
            corner_radius: CornerRadius::ZERO,
            border: None,
            shadow: None,
        }
    }

    /// Sets the fill brush (accepts [`Color`], [`LinearGradient`](crate::render::LinearGradient),
    /// [`RadialGradient`](crate::render::RadialGradient), or [`Fill`]).
    pub fn fill(mut self, fill: impl Into<Fill>) -> Self {
        self.fill = fill.into();
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

    /// Attaches a drop shadow rendered beneath this rectangle.
    pub fn shadow(mut self, shadow: BoxShadow) -> Self {
        self.shadow = Some(shadow);
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
            shadow: self.shadow,
        }
    }
}
