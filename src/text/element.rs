use crate::geometry::LogicalPx;
use crate::input::CommonProps;
use crate::render::Color;

use super::font::{Font, TextAlign, TextBoundsMode, TextOverflow, TextSpan};
use super::shared_str::SharedStr;

/// Styled text primitive supporting plain strings, rich inline [`TextSpan`]s, line clamping,
/// and ellipsis overflow.
#[must_use]
#[derive(Clone)]
pub struct Text<M = ()> {
    pub(crate) common: CommonProps<M>,
    pub(crate) content: SharedStr,
    pub(crate) spans: Option<Box<[TextSpan]>>,
    pub(crate) font_size: LogicalPx,
    pub(crate) line_height_multiplier: f32,
    pub(crate) color: Color,
    pub(crate) font: Font,
    pub(crate) align: TextAlign,
    pub(crate) bounds_mode: TextBoundsMode,
    pub(crate) overflow: TextOverflow,
    pub(crate) max_lines: Option<usize>,
}

impl<M: 'static> Text<M> {
    /// Creates a new text element.
    pub fn new(content: impl Into<SharedStr>) -> Self {
        Self {
            common: CommonProps::default(),
            content: content.into(),
            spans: None,
            font_size: LogicalPx(14.0),
            line_height_multiplier: 1.25,
            color: Color::WHITE,
            font: Font::default(),
            align: TextAlign::Left,
            bounds_mode: TextBoundsMode::Typographic,
            overflow: TextOverflow::Clip,
            max_lines: None,
        }
    }

    /// Creates a rich multi-style text element from styled [`TextSpan`] segments.
    ///
    /// Also concatenates the span text into the element's plain content string so accessibility
    /// bridges and fallback renderers read the full paragraph cleanly.
    pub fn rich(spans: impl IntoIterator<Item = TextSpan>) -> Self {
        let vec: Vec<TextSpan> = spans.into_iter().collect();
        let mut combined = String::new();
        for sp in &vec {
            combined.push_str(sp.text.as_str());
        }
        Self {
            content: SharedStr::from(combined),
            spans: (!vec.is_empty()).then(|| vec.into_boxed_slice()),
            ..Self::new(SharedStr::EMPTY)
        }
    }

    /// Appends an inline [`TextSpan`] to this text element, converting plain text into rich spans
    /// if needed.
    pub fn span(mut self, span: impl Into<TextSpan>) -> Self {
        let span = span.into();
        let mut vec = match self.spans.take() {
            Some(existing) => existing.into_vec(),
            None => {
                if self.content.is_empty() {
                    Vec::new()
                } else {
                    vec![TextSpan::new(self.content.clone())]
                }
            }
        };
        let mut combined = String::with_capacity(self.content.len() + span.text.len());
        combined.push_str(self.content.as_str());
        combined.push_str(span.text.as_str());
        vec.push(span);
        self.content = SharedStr::from(combined);
        self.spans = Some(vec.into_boxed_slice());
        self
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

    /// Sets how overflowing text is truncated when constrained by width or [`Self::max_lines`].
    pub fn overflow(mut self, overflow: TextOverflow) -> Self {
        self.overflow = overflow;
        self
    }

    /// Convenience shorthand for `.overflow(TextOverflow::EllipsisEnd)`.
    pub fn ellipsis(mut self) -> Self {
        self.overflow = TextOverflow::EllipsisEnd;
        self
    }

    /// Limits the maximum number of visible visual lines (`>= 1`).
    pub fn max_lines(mut self, lines: usize) -> Self {
        self.max_lines = Some(lines.max(1));
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
            spans: self.spans,
            font_size: self.font_size,
            line_height_multiplier: self.line_height_multiplier,
            color: self.color,
            font: self.font,
            align: self.align,
            bounds_mode: self.bounds_mode,
            overflow: self.overflow,
            max_lines: self.max_lines,
        }
    }
}
