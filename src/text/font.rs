use crate::render::Color;

use super::shared_str::SharedStr;

/// Font weight selection.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub enum FontWeight {
    Thin,
    Light,
    #[default]
    Normal,
    Medium,
    SemiBold,
    Bold,
    ExtraBold,
}

impl FontWeight {
    #[must_use]
    pub const fn to_numeric(self) -> u16 {
        match self {
            Self::Thin => 100,
            Self::Light => 300,
            Self::Normal => 400,
            Self::Medium => 500,
            Self::SemiBold => 600,
            Self::Bold => 700,
            Self::ExtraBold => 800,
        }
    }
}

/// Font slant style.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub enum FontStyle {
    #[default]
    Normal,
    Italic,
}

/// Font descriptor for [`Text`](super::element::Text).
#[derive(Debug, Clone, PartialEq, Eq, Hash, Default)]
pub struct Font {
    pub family: Option<SharedStr>,
    pub weight: FontWeight,
    pub style: FontStyle,
    pub monospace: bool,
}

impl Font {
    #[must_use]
    pub const fn sans() -> Self {
        Self {
            family: None,
            weight: FontWeight::Normal,
            style: FontStyle::Normal,
            monospace: false,
        }
    }

    #[must_use]
    pub const fn monospace() -> Self {
        Self {
            family: None,
            weight: FontWeight::Normal,
            style: FontStyle::Normal,
            monospace: true,
        }
    }

    #[must_use]
    pub fn family(family: impl Into<SharedStr>) -> Self {
        Self {
            family: Some(family.into()),
            ..Self::sans()
        }
    }

    #[must_use]
    pub const fn weight(mut self, weight: FontWeight) -> Self {
        self.weight = weight;
        self
    }

    #[must_use]
    pub const fn style(mut self, style: FontStyle) -> Self {
        self.style = style;
        self
    }
}

/// Horizontal text alignment within the text layout box.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub enum TextAlign {
    #[default]
    Left,
    Center,
    Right,
}

/// Bounding-box metric used to measure and position [`Text`](super::element::Text) elements.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub enum TextBoundsMode {
    /// Standard typographic advance width and line-height box (default; preserves consistent
    /// baselines for words, numbers, and paragraphs).
    #[default]
    Typographic,
    /// Tight actual glyph ink bounding box measured from rasterized glyph extents (ideal for
    /// centered single icon glyphs such as Nerd Font PUA symbols, arrows, or badges).
    Optical,
}

/// Overflow truncation behavior when shaped text exceeds its layout width or `max_lines` limit.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub enum TextOverflow {
    /// Wrap or clip text without inserting an ellipsis (default).
    #[default]
    Clip,
    /// Truncate the start of the final visible line with an ellipsis (`…end`).
    EllipsisStart,
    /// Truncate the middle of the final visible line with an ellipsis (`sta…end`).
    EllipsisMiddle,
    /// Truncate the end of the final visible line with an ellipsis (`start…`).
    EllipsisEnd,
}

/// Styled inline span inside a multi-style [`Text`](super::element::Text) element.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct TextSpan {
    /// Text content of this span.
    pub text: SharedStr,
    /// Optional color override (`None` inherits the parent [`Text`](super::element::Text) color).
    pub color: Option<Color>,
    /// Optional font override (`None` inherits the parent [`Text`](super::element::Text) font).
    pub font: Option<Font>,
}

impl TextSpan {
    /// Creates an unstyled text span inheriting the parent `Text` color and font.
    #[must_use]
    pub fn new(text: impl Into<SharedStr>) -> Self {
        Self {
            text: text.into(),
            color: None,
            font: None,
        }
    }

    /// Overrides the text color for this span.
    #[must_use]
    pub const fn color(mut self, color: Color) -> Self {
        self.color = Some(color);
        self
    }

    /// Overrides the font descriptor for this span.
    #[must_use]
    pub fn font(mut self, font: Font) -> Self {
        self.font = Some(font);
        self
    }
}

impl From<&'static str> for TextSpan {
    fn from(s: &'static str) -> Self {
        Self::new(s)
    }
}

impl From<String> for TextSpan {
    fn from(s: String) -> Self {
        Self::new(s)
    }
}

impl From<SharedStr> for TextSpan {
    fn from(s: SharedStr) -> Self {
        Self::new(s)
    }
}
