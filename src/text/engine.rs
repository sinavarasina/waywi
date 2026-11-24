#[cfg(feature = "text")]
use std::collections::HashMap;
#[cfg(feature = "text")]
use std::sync::OnceLock;

#[cfg(feature = "text")]
use super::font::{Font, FontStyle, TextAlign, TextBoundsMode};
#[cfg(feature = "text")]
use super::shared_str::SharedStr;
#[cfg(feature = "text")]
use crate::geometry::{LogicalPx, LogicalSize, ScaleFactor};
#[cfg(feature = "text")]
use crate::render::Color;

#[cfg(feature = "text")]
#[derive(Clone, Copy)]
pub(crate) struct TextDrawParams<'a> {
    pub(crate) content: &'a SharedStr,
    pub(crate) style: super::TextMeasureStyle<'a>,
    pub(crate) wrap_to_width: bool,
    pub(crate) scale: crate::geometry::ScaleFactor,
}

#[cfg(feature = "text")]
pub(crate) struct TextRasterTarget<'a> {
    pub(crate) dst: &'a mut [u8],
    pub(crate) width: u32,
    pub(crate) height: u32,
    pub(crate) clip: crate::geometry::BufferRect,
}

/// Shared `cosmic-text` font system and glyph cache backed by a process-cached Fontconfig
/// `fontdb::Database`.
#[cfg(feature = "text")]
pub(crate) struct TextEngine {
    font_system: cosmic_text::FontSystem,
    swash_cache: cosmic_text::SwashCache,
    buffer_cache: HashMap<TextShapeKey, CachedTextBuffer>,
    max_cached_buffers: usize,
    stale_frame_age: u64,
}

#[cfg(feature = "text")]
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
struct TextShapeKey {
    text: SharedStr,
    font_size_milli: u32,
    line_height_milli: u32,
    max_width_milli: Option<u32>,
    wrap_to_width: bool,
    scale_120: u32,
    font: Font,
    align: TextAlign,
    bounds_mode: TextBoundsMode,
}

#[cfg(feature = "text")]
struct CachedTextBuffer {
    buffer: cosmic_text::Buffer,
    logical_size: LogicalSize,
    draw_offset_px: (i32, i32),
    last_used_frame: u64,
}

#[cfg(feature = "text")]
fn shared_system_font_db() -> fontdb::Database {
    static SYSTEM_FONT_DB: OnceLock<fontdb::Database> = OnceLock::new();
    SYSTEM_FONT_DB
        .get_or_init(|| {
            let mut db = fontdb::Database::new();
            db.load_system_fonts();
            db
        })
        .clone()
}

#[cfg(feature = "text")]
impl TextEngine {
    /// Initializes a `FontSystem` from the process-cached system Fontconfig database with default limits.
    #[cfg(test)]
    pub(crate) fn new() -> Self {
        Self::with_config(256, 60, true)
    }

    /// Initializes a `FontSystem` with explicit shaping-cache limits and Fontconfig loading policy.
    pub(crate) fn with_config(
        max_cached_buffers: usize,
        stale_frame_age: u64,
        load_system_fonts: bool,
    ) -> Self {
        let db = if load_system_fonts {
            shared_system_font_db()
        } else {
            fontdb::Database::new()
        };
        let locale = std::env::var("LANG")
            .unwrap_or_else(|_| "en-US".to_owned())
            .split('.')
            .next()
            .unwrap_or("en-US")
            .replace('_', "-");
        let font_system = cosmic_text::FontSystem::new_with_locale_and_db(locale, db);

        Self {
            font_system,
            swash_cache: cosmic_text::SwashCache::new(),
            buffer_cache: HashMap::new(),
            max_cached_buffers: max_cached_buffers.max(8),
            stale_frame_age: stale_frame_age.max(1),
        }
    }

    pub(crate) fn trim(&mut self) {
        self.buffer_cache.clear();
        self.buffer_cache.shrink_to_fit();
        self.swash_cache = cosmic_text::SwashCache::new();
    }

    pub(crate) fn measure_text(
        &mut self,
        content: &SharedStr,
        style: super::TextMeasureStyle<'_>,
        scale: ScaleFactor,
        frame_seq: u64,
    ) -> LogicalSize {
        let params = TextDrawParams {
            content,
            style,
            wrap_to_width: style.max_width.is_some(),
            scale,
        };
        let key = Self::make_key(params);
        self.ensure_shaped(key, params, frame_seq).logical_size
    }

    /// Measures shaped text bounds together with the exact proportional `(x, y)` coordinates of
    /// `cursor_byte` (clamped safely to a UTF-8 char boundary).
    pub(crate) fn measure_text_cursor(
        &mut self,
        content: &SharedStr,
        style: super::TextMeasureStyle<'_>,
        cursor_byte: usize,
        frame_seq: u64,
    ) -> super::TextCursorMetrics {
        let default_lh = compute_line_height_px(style.font_size, style.line_height_multiplier, 1.0);
        let text = content.as_str();
        if text.is_empty() {
            let x = match style.align {
                TextAlign::Left => 0.0,
                TextAlign::Center => style.max_width.map_or(0.0, |w| w.0.max(0.0) * 0.5),
                TextAlign::Right => style.max_width.map_or(0.0, |w| w.0.max(0.0)),
            };
            return super::TextCursorMetrics {
                size: LogicalSize::new(LogicalPx::ZERO, LogicalPx(default_lh.ceil())),
                cursor_x: LogicalPx(x),
                cursor_y: LogicalPx::ZERO,
                line_height: LogicalPx(default_lh),
            };
        }

        let params = TextDrawParams {
            content,
            style,
            wrap_to_width: style.max_width.is_some(),
            scale: crate::geometry::ScaleFactor::ONE,
        };
        let key = Self::make_key(params);
        let shaped = self.ensure_shaped(key, params, frame_seq);
        let size = shaped.logical_size;

        let clamped = crate::input::text_input::snap_char_boundary(text, cursor_byte);
        let prefix = &text[..clamped];
        let line_i = prefix.bytes().filter(|&b| b == b'\n').count();
        let line_start = prefix.rfind('\n').map_or(0, |pos| pos + 1);
        let local_byte = clamped - line_start;

        let mut exact: Option<(f32, f32, f32)> = None;
        let mut line_fallback: Option<(f32, f32, f32)> = None;
        let mut last_run_line = None;

        for run in shaped.buffer.layout_runs() {
            last_run_line = Some((run.line_i, run.line_top, run.line_height));
            if run.line_i != line_i {
                continue;
            }
            let c_idx = local_byte.min(run.text.len());
            let cursor = cosmic_text::Cursor::new(line_i, c_idx);
            if let Some(x) = run.cursor_position(&cursor) {
                exact = Some((x, run.line_top, run.line_height));
                break;
            }
            line_fallback = Some((run.line_w, run.line_top, run.line_height));
        }

        let (cx, cy, lh) = if let Some(hit) = exact {
            hit
        } else if let Some(fb) = line_fallback {
            fb
        } else {
            let y = last_run_line.map_or((line_i as f32) * default_lh, |(li, top, height)| {
                top + ((line_i.saturating_sub(li)) as f32) * height
            });
            (0.0, y, default_lh)
        };

        super::TextCursorMetrics {
            size,
            cursor_x: LogicalPx(cx),
            cursor_y: LogicalPx(cy),
            line_height: LogicalPx(lh),
        }
    }

    fn make_key(params: TextDrawParams<'_>) -> TextShapeKey {
        let s = params.style;
        // Unwrapped left-aligned text has identical layout regardless of container width, so
        // normalizing `max_width_milli` shares shaped buffers between auto layout and draw passes.
        let max_width_milli = if params.wrap_to_width || s.align != TextAlign::Left {
            s.max_width.map(|w| (w.0.max(0.0) * 1000.0).round() as u32)
        } else {
            None
        };
        TextShapeKey {
            text: params.content.clone(),
            font_size_milli: (s.font_size.0.max(1.0) * 1000.0).round() as u32,
            line_height_milli: (s.line_height_multiplier.max(0.5) * 1000.0).round() as u32,
            max_width_milli,
            wrap_to_width: params.wrap_to_width,
            scale_120: params.scale.as_120ths(),
            font: s.font.clone(),
            align: s.align,
            bounds_mode: s.bounds_mode,
        }
    }

    fn ensure_shaped(
        &mut self,
        key: TextShapeKey,
        params: TextDrawParams<'_>,
        frame_seq: u64,
    ) -> &mut CachedTextBuffer {
        let s = params.style;
        let stale_age = self.stale_frame_age;
        if (frame_seq.is_multiple_of(32) || self.buffer_cache.len() >= self.max_cached_buffers)
            && !self.buffer_cache.contains_key(&key)
        {
            self.buffer_cache
                .retain(|_, v| frame_seq.saturating_sub(v.last_used_frame) < stale_age);
            while self.buffer_cache.len() >= self.max_cached_buffers {
                let Some(oldest) = self
                    .buffer_cache
                    .iter()
                    .min_by_key(|(_, v)| v.last_used_frame)
                    .map(|(k, _)| k.clone())
                else {
                    break;
                };
                self.buffer_cache.remove(&oldest);
            }
            if self.buffer_cache.capacity()
                > (self.buffer_cache.len().saturating_mul(4)).max(self.max_cached_buffers)
            {
                self.buffer_cache
                    .shrink_to((self.buffer_cache.len().saturating_mul(2)).max(32));
            }
        }

        let font_system = &mut self.font_system;
        let swash_cache = &mut self.swash_cache;
        let scale = params.scale.as_f32();
        let entry = self.buffer_cache.entry(key).or_insert_with(|| {
            let fs_px = (s.font_size.0.max(1.0) * scale).max(1.0);
            let lh_px = compute_line_height_px(s.font_size, s.line_height_multiplier, scale);
            let metrics = cosmic_text::Metrics::new(fs_px, lh_px);
            let mut buffer = cosmic_text::Buffer::new(font_system, metrics);

            buffer.set_wrap(if params.wrap_to_width {
                cosmic_text::Wrap::WordOrGlyph
            } else {
                cosmic_text::Wrap::None
            });
            let width_px = s.max_width.map(|w| (w.0 * scale).max(1.0));
            buffer.set_size(width_px, None);

            let family = match (&s.font.family, s.font.monospace) {
                (Some(name), _) => match name.as_str() {
                    "sans-serif" | "sans serif" => cosmic_text::Family::SansSerif,
                    "monospace" => cosmic_text::Family::Monospace,
                    "serif" => cosmic_text::Family::Serif,
                    "cursive" => cosmic_text::Family::Cursive,
                    "fantasy" => cosmic_text::Family::Fantasy,
                    other => cosmic_text::Family::Name(other),
                },
                (None, true) => cosmic_text::Family::Monospace,
                (None, false) => cosmic_text::Family::SansSerif,
            };
            let attrs = cosmic_text::Attrs::new()
                .family(family)
                .weight(cosmic_text::Weight(s.font.weight.to_numeric()))
                .style(match s.font.style {
                    FontStyle::Normal => cosmic_text::Style::Normal,
                    FontStyle::Italic => cosmic_text::Style::Italic,
                });
            let c_align = match s.align {
                TextAlign::Left => Some(cosmic_text::Align::Left),
                TextAlign::Center => Some(cosmic_text::Align::Center),
                TextAlign::Right => Some(cosmic_text::Align::Right),
            };
            buffer.set_text(
                params.content.as_str(),
                &attrs,
                cosmic_text::Shaping::Advanced,
                c_align,
            );
            buffer.shape_until_scroll(font_system, false);

            let mut max_w = 0.0_f32;
            let mut line_count = 0_usize;
            for run in buffer.layout_runs() {
                max_w = max_w.max(run.line_w);
                line_count += 1;
            }
            let total_h = (line_count.max(1) as f32) * lh_px;
            let mut logical_size = LogicalSize::new(
                LogicalPx((max_w / scale).ceil()),
                LogicalPx((total_h / scale).ceil()),
            );
            let mut draw_offset_px = (0_i32, 0_i32);

            if s.bounds_mode == TextBoundsMode::Optical && !params.content.is_empty() {
                let mut min_x = i32::MAX;
                let mut min_y = i32::MAX;
                let mut max_x = i32::MIN;
                let mut max_y = i32::MIN;
                let probe_color = cosmic_text::Color::rgba(255, 255, 255, 255);
                buffer.draw(font_system, swash_cache, probe_color, |px, py, w, h, c| {
                    if c.a() > 0 && w > 0 && h > 0 {
                        min_x = min_x.min(px);
                        min_y = min_y.min(py);
                        max_x = max_x.max(px + w as i32);
                        max_y = max_y.max(py + h as i32);
                    }
                });
                if min_x < max_x && min_y < max_y {
                    let ink_w_px = (max_x - min_x) as f32;
                    let ink_h_px = (max_y - min_y) as f32;
                    logical_size = LogicalSize::new(
                        LogicalPx((ink_w_px / scale).ceil()),
                        LogicalPx((ink_h_px / scale).ceil()),
                    );
                    let offset_x = if let Some(box_w) = width_px
                        && params.wrap_to_width
                    {
                        match s.align {
                            TextAlign::Left => -min_x,
                            TextAlign::Center => ((box_w - ink_w_px) * 0.5).round() as i32 - min_x,
                            TextAlign::Right => (box_w - ink_w_px).round() as i32 - min_x,
                        }
                    } else {
                        -min_x
                    };
                    draw_offset_px = (offset_x, -min_y);
                }
            }

            CachedTextBuffer {
                buffer,
                logical_size,
                draw_offset_px,
                last_used_frame: frame_seq,
            }
        });
        entry.last_used_frame = frame_seq;
        entry
    }

    /// Rasterizes text directly onto an RGBA8888 premultiplied pixel slice clipped to `target.clip`.
    pub(crate) fn draw_text_premultiplied_rgba(
        &mut self,
        target: &mut TextRasterTarget<'_>,
        origin: (f32, f32),
        params: TextDrawParams<'_>,
        color: Color,
        opacity: f32,
        frame_seq: u64,
    ) {
        if params.content.is_empty() || color.a == 0 || opacity <= 0.0 {
            return;
        }
        let scale_f = params.scale.as_f32();
        let key = Self::make_key(params);
        let clip = target.clip;
        let dst = &mut *target.dst;
        let dst_w = target.width;
        let dst_h = target.height;
        self.ensure_shaped(key.clone(), params, frame_seq);
        let Self {
            font_system,
            swash_cache,
            buffer_cache,
            ..
        } = self;
        let cached = buffer_cache.get_mut(&key).expect("shaped buffer cached");
        let base_alpha = (f32::from(color.a) * opacity.clamp(0.0, 1.0)).round() as u8;
        let text_color = cosmic_text::Color::rgba(color.r, color.g, color.b, base_alpha);
        let ox = (origin.0 * scale_f).round() as i32 + cached.draw_offset_px.0;
        let oy = (origin.1 * scale_f).round() as i32 + cached.draw_offset_px.1;

        cached.buffer.draw(
            font_system,
            swash_cache,
            text_color,
            |px, py, w, h, glyph_color| {
                if w == 0 || h == 0 {
                    return;
                }
                let gx = ox + px;
                let gy = oy + py;
                let ga = glyph_color.a();
                if ga == 0 {
                    return;
                }
                let gr = glyph_color.r();
                let gg = glyph_color.g();
                let gb = glyph_color.b();

                let x0 = gx.max(clip.x);
                let y0 = gy.max(clip.y);
                let x1 = (gx + w as i32).min(clip.x + clip.width).min(dst_w as i32);
                let y1 = (gy + h as i32).min(clip.y + clip.height).min(dst_h as i32);
                if x1 <= x0 || y1 <= y0 {
                    return;
                }

                let sa = u32::from(ga);
                let sr_pm = (u32::from(gr) * sa + 127) / 255;
                let sg_pm = (u32::from(gg) * sa + 127) / 255;
                let sb_pm = (u32::from(gb) * sa + 127) / 255;
                let inv_sa = 255 - sa;

                for yy in y0..y1 {
                    let row = (yy as usize) * (dst_w as usize) * 4;
                    for xx in x0..x1 {
                        let idx = row + (xx as usize) * 4;
                        let dr = u32::from(dst[idx]);
                        let dg = u32::from(dst[idx + 1]);
                        let db = u32::from(dst[idx + 2]);
                        let da = u32::from(dst[idx + 3]);

                        dst[idx] = (sr_pm + (dr * inv_sa + 127) / 255).min(255) as u8;
                        dst[idx + 1] = (sg_pm + (dg * inv_sa + 127) / 255).min(255) as u8;
                        dst[idx + 2] = (sb_pm + (db * inv_sa + 127) / 255).min(255) as u8;
                        dst[idx + 3] = (sa + (da * inv_sa + 127) / 255).min(255) as u8;
                    }
                }
            },
        );
    }
}

#[cfg(feature = "text")]
#[inline]
fn compute_line_height_px(font_size: LogicalPx, multiplier: f32, scale: f32) -> f32 {
    let fs_px = (font_size.0.max(1.0) * scale).max(1.0);
    (fs_px * multiplier.max(0.5)).max(1.0)
}

#[cfg(all(test, feature = "text"))]
mod tests {
    use super::*;
    use crate::geometry::ScaleFactor;
    use crate::text::TextMeasureStyle;

    #[test]
    fn unconstrained_auto_text_does_not_wrap_across_scales_or_font_loads() {
        let mut engine = TextEngine::new();
        let font = Font::monospace();
        for sample in ["[1]:", "terminal", "workspace — editor"] {
            let content = SharedStr::from(sample);
            let measured = engine.measure_text(
                &content,
                TextMeasureStyle {
                    font_size: LogicalPx(12.0),
                    line_height_multiplier: 1.15,
                    max_width: None,
                    font: &font,
                    align: TextAlign::Left,
                    bounds_mode: TextBoundsMode::Typographic,
                },
                ScaleFactor::ONE,
                1,
            );
            // Draw with wrap_to_width = false against exact measured width at both 1.0x and 1.25x.
            for scale_120 in [120, 150] {
                let scale = match scale_120 {
                    150 => ScaleFactor::from_120ths(150),
                    _ => ScaleFactor::ONE,
                };
                let params = TextDrawParams {
                    content: &content,
                    style: TextMeasureStyle {
                        font_size: LogicalPx(12.0),
                        line_height_multiplier: 1.15,
                        max_width: Some(measured.width),
                        font: &font,
                        align: TextAlign::Left,
                        bounds_mode: TextBoundsMode::Typographic,
                    },
                    wrap_to_width: false,
                    scale,
                };
                let key = TextEngine::make_key(params);
                let shaped = engine.ensure_shaped(key, params, 2);
                assert_eq!(
                    shaped.buffer.layout_runs().count(),
                    1,
                    "`{sample}` wrapped unexpectedly at scale {:?}",
                    scale
                );
            }
        }
    }

    #[test]
    fn proportional_cursor_x_tracks_glyph_widths_middle_and_utf8() {
        let mut engine = TextEngine::new();
        let font = Font::sans();
        let style = TextMeasureStyle {
            font_size: LogicalPx(16.0),
            line_height_multiplier: 1.25,
            max_width: None,
            font: &font,
            align: TextAlign::Left,
            bounds_mode: TextBoundsMode::Typographic,
        };

        let m_empty = engine.measure_text_cursor(&SharedStr::EMPTY, style, 0, 1);
        assert_eq!(m_empty.cursor_x, LogicalPx::ZERO);
        assert_eq!(m_empty.cursor_y, LogicalPx::ZERO);
        assert!(m_empty.line_height.0 > 16.0);

        let narrow = engine.measure_text_cursor(&SharedStr::from("iii"), style, 3, 2);
        let wide = engine.measure_text_cursor(&SharedStr::from("WWW"), style, 3, 3);
        assert!(
            narrow.cursor_x.0 > 0.0 && narrow.cursor_x.0 < wide.cursor_x.0,
            "proportional sans font must measure `iii` ({:?}) narrower than `WWW` ({:?})",
            narrow.cursor_x,
            wide.cursor_x
        );

        let mixed = SharedStr::from("iiWW");
        let start = engine.measure_text_cursor(&mixed, style, 0, 4);
        let mid = engine.measure_text_cursor(&mixed, style, 2, 4);
        let end = engine.measure_text_cursor(&mixed, style, 4, 4);
        assert_eq!(start.cursor_x, LogicalPx::ZERO);
        assert!(mid.cursor_x.0 > 0.0 && mid.cursor_x.0 < end.cursor_x.0);
        // First half ("ii") must take strictly less than half the width of "iiWW".
        assert!(mid.cursor_x.0 < end.cursor_x.0 * 0.5);

        // Unaligned byte offset into multi-byte UTF-8 and multiline newline increments cursor_y.
        let multi = SharedStr::from("a\n日本");
        let line1 = engine.measure_text_cursor(&multi, style, 3, 5); // lands inside '日' -> snaps to line 1 col 0
        let line1_end = engine.measure_text_cursor(&multi, style, multi.len(), 5);
        assert!(line1.cursor_y.0 >= line1.line_height.0 - 0.01);
        assert_eq!(line1.cursor_x, LogicalPx::ZERO);
        assert!(line1_end.cursor_x.0 > 0.0);
    }

    #[test]
    fn optical_bounds_tightly_frames_rendered_ink_at_origin_zero() {
        let mut engine = TextEngine::new();
        let font = Font::sans();
        let content = SharedStr::from("O");
        let typo_style = TextMeasureStyle {
            font_size: LogicalPx(24.0),
            line_height_multiplier: 1.4,
            max_width: None,
            font: &font,
            align: TextAlign::Left,
            bounds_mode: TextBoundsMode::Typographic,
        };
        let optical_style = TextMeasureStyle {
            bounds_mode: TextBoundsMode::Optical,
            ..typo_style
        };

        let typo_size = engine.measure_text(&content, typo_style, ScaleFactor::ONE, 1);
        let opt_size = engine.measure_text(&content, optical_style, ScaleFactor::ONE, 1);
        assert!(opt_size.width.0 > 0.0 && opt_size.height.0 > 0.0);
        // Tight optical ink height must be strictly smaller than generous 1.4x typographic line height.
        assert!(opt_size.height.0 < typo_size.height.0);

        let w = (opt_size.width.0.ceil() as u32) + 8;
        let h = (opt_size.height.0.ceil() as u32) + 8;
        let mut buf = vec![0u8; (w * h * 4) as usize];
        let mut target = TextRasterTarget {
            dst: &mut buf,
            width: w,
            height: h,
            clip: crate::geometry::BufferRect {
                x: 0,
                y: 0,
                width: w as i32,
                height: h as i32,
            },
        };
        engine.draw_text_premultiplied_rgba(
            &mut target,
            (4.0, 4.0),
            TextDrawParams {
                content: &content,
                style: optical_style,
                wrap_to_width: false,
                scale: ScaleFactor::ONE,
            },
            Color::WHITE,
            1.0,
            2,
        );

        let mut min_x = u32::MAX;
        let mut min_y = u32::MAX;
        let mut max_x = 0u32;
        let mut max_y = 0u32;
        for y in 0..h {
            for x in 0..w {
                if buf[((y * w + x) * 4 + 3) as usize] > 0 {
                    min_x = min_x.min(x);
                    min_y = min_y.min(y);
                    max_x = max_x.max(x);
                    max_y = max_y.max(y);
                }
            }
        }
        // Rendered ink starts flush at the requested origin (4, 4) and fits inside opt_size!
        assert_eq!(min_x, 4);
        assert_eq!(min_y, 4);
        assert_eq!(max_x - min_x + 1, opt_size.width.0.ceil() as u32);
        assert_eq!(max_y - min_y + 1, opt_size.height.0.ceil() as u32);
    }
}
