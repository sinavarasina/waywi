use crate::geometry::LogicalPx;
use crate::input::CommonProps;
use crate::render::Color;

use super::font::{Font, TextAlign, TextBoundsMode};
use super::shared_str::SharedStr;

/// Styled text primitive.
#[must_use]
#[derive(Clone)]
pub struct Text<M = ()> {
    pub(crate) common: CommonProps<M>,
    pub(crate) content: SharedStr,
    pub(crate) font_size: LogicalPx,
    pub(crate) line_height_multiplier: f32,
    pub(crate) color: Color,
    pub(crate) font: Font,
    pub(crate) align: TextAlign,
    pub(crate) bounds_mode: TextBoundsMode,
}

impl<M: 'static> Text<M> {
    /// Creates a new text element.
    pub fn new(content: impl Into<SharedStr>) -> Self {
        Self {
            common: CommonProps::default(),
            content: content.into(),
            font_size: LogicalPx(14.0),
            line_height_multiplier: 1.25,
            color: Color::WHITE,
            font: Font::default(),
            align: TextAlign::Left,
            bounds_mode: TextBoundsMode::Typographic,
        }
    }

    pub fn font_size(mut self, size: LogicalPx) -> Self {
        self.font_size = size;
        self
    }

    pub fn line_height(mut self, multiplier: f32) -> Self {
        self.line_height_multiplier = multiplier.max(0.5);
        self
    }

    pub fn color(mut self, color: Color) -> Self {
        self.color = color;
        self
    }

    pub fn font(mut self, font: Font) -> Self {
        self.font = font;
        self
    }

    pub fn align(mut self, align: TextAlign) -> Self {
        self.align = align;
        self
    }

    /// Selects how this text element's bounds are measured and positioned
    /// ([`TextBoundsMode::Typographic`] default or tight [`TextBoundsMode::Optical`] ink bounds).
    pub fn bounds_mode(mut self, mode: TextBoundsMode) -> Self {
        self.bounds_mode = mode;
        self
    }

    /// Convenience setter toggling [`TextBoundsMode::Optical`] tight ink bounds (ideal for
    /// centering single icon glyphs such as Nerd Font PUA symbols).
    pub fn optical_bounds(mut self, optical: bool) -> Self {
        self.bounds_mode = if optical {
            TextBoundsMode::Optical
        } else {
            TextBoundsMode::Typographic
        };
        self
    }

    crate::input::impl_common_builders!();

    /// Maps emitted messages into a parent message type `N`.
    pub fn map<N: 'static, F>(self, f: F) -> Text<N>
    where
        F: Fn(M) -> N + 'static,
    {
        Text {
            common: self.common.map(f),
            content: self.content,
            font_size: self.font_size,
            line_height_multiplier: self.line_height_multiplier,
            color: self.color,
            font: self.font,
            align: self.align,
            bounds_mode: self.bounds_mode,
        }
    }
}
