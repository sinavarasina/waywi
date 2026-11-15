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
