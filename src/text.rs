pub mod element;
pub(crate) mod engine;
pub mod font;
pub mod shared_str;

pub use element::Text;
#[cfg(feature = "text")]
pub(crate) use engine::TextEngine;
pub use font::{Font, FontStyle, FontWeight, TextAlign};
pub use shared_str::SharedStr;

/// Grouped typography parameters passed when shaping/measuring text.
#[derive(Clone, Copy)]
pub(crate) struct TextMeasureStyle<'a> {
    pub(crate) font_size: crate::geometry::LogicalPx,
    pub(crate) line_height_multiplier: f32,
    pub(crate) max_width: Option<crate::geometry::LogicalPx>,
    pub(crate) font: &'a Font,
    pub(crate) align: TextAlign,
}
