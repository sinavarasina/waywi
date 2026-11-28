pub mod element;
pub(crate) mod engine;
pub mod font;
pub mod shared_str;

pub use element::Text;
#[cfg(feature = "text")]
pub(crate) use engine::TextEngine;
pub use font::{Font, FontStyle, FontWeight, TextAlign, TextBoundsMode, TextOverflow, TextSpan};
pub use shared_str::SharedStr;

/// Measured layout bounds and visual cursor coordinates for shaped text.
#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub struct TextCursorMetrics {
    /// Total bounding size of the shaped text in logical pixels.
    pub size: crate::geometry::LogicalSize,
    /// Visual X offset of the queried byte index relative to the text origin, in logical pixels.
    pub cursor_x: crate::geometry::LogicalPx,
    /// Visual Y offset of the line containing the cursor relative to the text origin, in logical pixels.
    pub cursor_y: crate::geometry::LogicalPx,
    /// Line height of the cursor's line in logical pixels.
    pub line_height: crate::geometry::LogicalPx,
}

/// Measured selection highlight rectangles for a UTF-8 byte range in shaped text.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct TextSelectionMetrics {
    /// Total bounding size of the shaped text in logical pixels.
    pub size: crate::geometry::LogicalSize,
    /// One logical rectangle per visual line intersecting the selection range.
    pub rects: Vec<crate::geometry::Bounds>,
}

/// Optional typography parameters passed to [`EffectCtx::measure_text_cursor_with`](crate::effect::EffectCtx::measure_text_cursor_with),
/// [`EffectCtx::measure_text_selection`](crate::effect::EffectCtx::measure_text_selection), and
/// [`EffectCtx::hit_test_text_byte`](crate::effect::EffectCtx::hit_test_text_byte).
#[derive(Debug, Clone, PartialEq)]
pub struct TextLayoutOptions {
    /// Font specification (default: sans-serif normal).
    pub font: Font,
    /// Font size in logical pixels (default: `14.0`).
    pub font_size: crate::geometry::LogicalPx,
    /// Line height multiplier (default: `1.25`, matching [`Text::new`]).
    pub line_height_multiplier: f32,
    /// Optional wrapping width constraint in logical pixels (`None` = single-line unless `\n`).
    pub max_width: Option<crate::geometry::LogicalPx>,
    /// Horizontal text alignment within `max_width`.
    pub align: TextAlign,
    /// Bounding-box metric (`Typographic` default or tight `Optical` ink bounds).
    pub bounds_mode: TextBoundsMode,
    /// Overflow truncation behavior (default: [`TextOverflow::Clip`]).
    pub overflow: TextOverflow,
    /// Optional maximum number of visual lines (`None` = unlimited).
    pub max_lines: Option<usize>,
}

impl Default for TextLayoutOptions {
    fn default() -> Self {
        Self {
            font: Font::default(),
            font_size: crate::geometry::LogicalPx(14.0),
            line_height_multiplier: 1.25,
            max_width: None,
            align: TextAlign::Left,
            bounds_mode: TextBoundsMode::Typographic,
            overflow: TextOverflow::Clip,
            max_lines: None,
        }
    }
}

impl TextLayoutOptions {
    /// Creates layout options with the given `font` and `font_size`.
    #[must_use]
    pub fn new(font: Font, font_size: crate::geometry::LogicalPx) -> Self {
        Self {
            font,
            font_size,
            ..Self::default()
        }
    }

    /// Sets the line height multiplier (`>= 0.5`).
    #[must_use]
    pub fn line_height(mut self, multiplier: f32) -> Self {
        self.line_height_multiplier = multiplier.max(0.5);
        self
    }

    /// Sets the optional wrap width in logical pixels.
    #[must_use]
    pub fn max_width(mut self, width: Option<crate::geometry::LogicalPx>) -> Self {
        self.max_width = width;
        self
    }

    /// Sets horizontal alignment.
    #[must_use]
    pub fn align(mut self, align: TextAlign) -> Self {
        self.align = align;
        self
    }

    /// Sets the bounding-box measurement mode (`Typographic` or `Optical`).
    #[must_use]
    pub fn bounds_mode(mut self, mode: TextBoundsMode) -> Self {
        self.bounds_mode = mode;
        self
    }

    /// Sets overflow truncation behavior (`Clip` or `Ellipsis*`).
    #[must_use]
    pub fn overflow(mut self, overflow: TextOverflow) -> Self {
        self.overflow = overflow;
        self
    }

    /// Convenience shorthand for `.overflow(TextOverflow::EllipsisEnd)`.
    #[must_use]
    pub fn ellipsis(mut self) -> Self {
        self.overflow = TextOverflow::EllipsisEnd;
        self
    }

    /// Limits visible visual lines (`>= 1`).
    #[must_use]
    pub fn max_lines(mut self, lines: Option<usize>) -> Self {
        self.max_lines = lines.map(|n| n.max(1));
        self
    }
}

/// Grouped typography parameters passed when shaping/measuring text.
#[derive(Clone, Copy)]
pub(crate) struct TextMeasureStyle<'a> {
    pub(crate) font_size: crate::geometry::LogicalPx,
    pub(crate) line_height_multiplier: f32,
    pub(crate) max_width: Option<crate::geometry::LogicalPx>,
    pub(crate) font: &'a Font,
    pub(crate) align: TextAlign,
    pub(crate) bounds_mode: TextBoundsMode,
    pub(crate) overflow: TextOverflow,
    pub(crate) max_lines: Option<usize>,
    pub(crate) spans: Option<&'a [TextSpan]>,
}

impl<'a> TextMeasureStyle<'a> {
    #[cfg(feature = "text")]
    #[inline]
    pub(crate) fn from_options(opts: &'a TextLayoutOptions) -> Self {
        Self {
            font_size: opts.font_size,
            line_height_multiplier: opts.line_height_multiplier,
            max_width: opts.max_width,
            font: &opts.font,
            align: opts.align,
            bounds_mode: opts.bounds_mode,
            overflow: opts.overflow,
            max_lines: opts.max_lines,
            spans: None,
        }
    }
}
