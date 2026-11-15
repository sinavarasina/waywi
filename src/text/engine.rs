#[cfg(feature = "text")]
use super::font::{Font, FontStyle, TextAlign};
#[cfg(feature = "text")]
use super::shared_str::SharedStr;
#[cfg(feature = "text")]
use crate::geometry::{LogicalPx, LogicalSize};
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

/// Shared `cosmic-text` font system and glyph cache with lazy system fallback discovery.
#[cfg(feature = "text")]
pub(crate) struct TextEngine {
    font_system: cosmic_text::FontSystem,
    swash_cache: cosmic_text::SwashCache,
    fallback_loaded: bool,
    buffer_cache: std::collections::HashMap<TextShapeKey, CachedTextBuffer>,
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
}

#[cfg(feature = "text")]
struct CachedTextBuffer {
    buffer: cosmic_text::Buffer,
    logical_size: LogicalSize,
    last_used_frame: u64,
}

#[cfg(feature = "text")]
impl TextEngine {
    /// Initializes a lightweight `FontSystem` loading only primary UI font files initially to
    /// avoid a 5–15 MB upfront system font scan.
    pub(crate) fn new() -> Self {
        let mut db = fontdb::Database::new();
        let loaded_primary = Self::try_load_primary_fonts(&mut db);
        let fallback_loaded = if !loaded_primary {
            db.load_system_fonts();
            true
        } else {
            false
        };
        db.set_sans_serif_family("DejaVu Sans");
        db.set_monospace_family("DejaVu Sans Mono");

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
            fallback_loaded,
            buffer_cache: std::collections::HashMap::new(),
        }
    }

    fn try_load_primary_fonts(db: &mut fontdb::Database) -> bool {
        const CANDIDATES: &[&str] = &[
            "/usr/share/fonts/noto/NotoSans-Regular.ttf",
            "/usr/share/fonts/truetype/noto/NotoSans-Regular.ttf",
            "/usr/share/fonts/TTF/DejaVuSans.ttf",
            "/usr/share/fonts/truetype/dejavu/DejaVuSans.ttf",
            "/usr/share/fonts/dejavu/DejaVuSans.ttf",
            "/usr/share/fonts/liberation/LiberationSans-Regular.ttf",
            "/usr/share/fonts/truetype/liberation/LiberationSans-Regular.ttf",
            "/usr/share/fonts/Adwaita/AdwaitaSans-Regular.ttf",
            "/usr/share/fonts/cantarell/Cantarell-VF.otf",
            "/usr/share/fonts/noto/NotoSansMono-Regular.ttf",
            "/usr/share/fonts/TTF/DejaVuSansMono.ttf",
            "/usr/share/fonts/truetype/dejavu/DejaVuSansMono.ttf",
        ];

        let mut loaded_any = false;
        for path in CANDIDATES {
            let p = std::path::Path::new(path);
            if p.exists() && db.load_font_file(p).is_ok() {
                loaded_any = true;
            }
        }
        loaded_any
    }

    fn load_system_fonts_and_invalidate(&mut self) {
        if self.fallback_loaded {
            return;
        }
        self.font_system.db_mut().load_system_fonts();
        self.fallback_loaded = true;
        // Clear any shapes measured before system fonts joined `fontdb` so measure and draw share
        // identical font metrics within and across frames.
        self.buffer_cache.clear();
    }

    fn ensure_requested_font_loaded(&mut self, font: &Font) {
        if self.fallback_loaded {
            return;
        }
        if let Some(ref fam) = font.family {
            let target = fam.as_str();
            let found = self.font_system.db().faces().any(|f| {
                f.families
                    .iter()
                    .any(|(name, _)| name.eq_ignore_ascii_case(target))
            });
            if !found {
                self.load_system_fonts_and_invalidate();
            }
        }
    }

    pub(crate) fn measure_text(
        &mut self,
        content: &SharedStr,
        style: super::TextMeasureStyle<'_>,
        frame_seq: u64,
    ) -> LogicalSize {
        let params = TextDrawParams {
            content,
            style,
            wrap_to_width: style.max_width.is_some(),
            scale: crate::geometry::ScaleFactor::ONE,
        };
        let key = Self::make_key(params);
        self.ensure_shaped(key, params, frame_seq).logical_size
    }

    fn make_key(params: TextDrawParams<'_>) -> TextShapeKey {
        let s = params.style;
        TextShapeKey {
            text: params.content.clone(),
            font_size_milli: (s.font_size.0.max(1.0) * 1000.0).round() as u32,
            line_height_milli: (s.line_height_multiplier.max(0.5) * 1000.0).round() as u32,
            max_width_milli: s.max_width.map(|w| (w.0.max(0.0) * 1000.0).round() as u32),
            wrap_to_width: params.wrap_to_width,
            scale_120: params.scale.as_120ths(),
            font: s.font.clone(),
            align: s.align,
        }
    }

    fn ensure_shaped(
        &mut self,
        key: TextShapeKey,
        params: TextDrawParams<'_>,
        frame_seq: u64,
    ) -> &mut CachedTextBuffer {
        let s = params.style;
        self.ensure_requested_font_loaded(s.font);
        if self.buffer_cache.len() >= 1024 && !self.buffer_cache.contains_key(&key) {
            self.buffer_cache
                .retain(|_, v| frame_seq.saturating_sub(v.last_used_frame) < 120);
        }

        let font_system = &mut self.font_system;
        let fallback_loaded = &mut self.fallback_loaded;
        let mut fallback_triggered = false;
        let scale = params.scale.as_f32();
        let inserted_key = key.clone();
        self.buffer_cache.entry(key).or_insert_with(|| {
            let fs_px = (s.font_size.0.max(1.0) * scale).max(1.0);
            let lh_px = (fs_px * s.line_height_multiplier.max(0.5)).max(1.0);
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
                (Some(name), _) => cosmic_text::Family::Name(name.as_str()),
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

            if !*fallback_loaded
                && buffer.layout_runs().any(|run| {
                    run.glyphs
                        .iter()
                        .any(|g| g.glyph_id == 0 && g.end > g.start)
                })
            {
                font_system.db_mut().load_system_fonts();
                *fallback_loaded = true;
                fallback_triggered = true;
                buffer.set_text(
                    params.content.as_str(),
                    &attrs,
                    cosmic_text::Shaping::Advanced,
                    c_align,
                );
                buffer.shape_until_scroll(font_system, false);
            }

            let mut max_w = 0.0_f32;
            let mut line_count = 0_usize;
            for run in buffer.layout_runs() {
                max_w = max_w.max(run.line_w);
                line_count += 1;
            }
            let total_h = (line_count.max(1) as f32) * lh_px;
            let logical_size = LogicalSize::new(
                LogicalPx((max_w / scale).ceil()),
                LogicalPx((total_h / scale).ceil()),
            );

            CachedTextBuffer {
                buffer,
                logical_size,
                last_used_frame: frame_seq,
            }
        });
        if fallback_triggered {
            // Invalidate pre-fallback shapes while preserving the newly-shaped buffer.
            let latest = self.buffer_cache.remove(&inserted_key);
            self.buffer_cache.clear();
            if let Some(buf) = latest {
                self.buffer_cache.insert(inserted_key.clone(), buf);
            }
        }
        let entry = self
            .buffer_cache
            .get_mut(&inserted_key)
            .expect("shaped buffer cached");
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
        let ox = (origin.0 * scale_f).round() as i32;
        let oy = (origin.1 * scale_f).round() as i32;

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

#[cfg(all(test, feature = "text"))]
mod tests {
    use super::*;
    use crate::geometry::ScaleFactor;
    use crate::text::TextMeasureStyle;

    #[test]
    fn unconstrained_auto_text_does_not_wrap_across_scales_or_font_loads() {
        let mut engine = TextEngine::new();
        let font = Font::family("MesloLGS Nerd Font");
        for sample in ["[4]:", "foot", "waywi-waybar — zsh"] {
            let content = SharedStr::from(sample);
            let measured = engine.measure_text(
                &content,
                TextMeasureStyle {
                    font_size: LogicalPx(12.0),
                    line_height_multiplier: 1.15,
                    max_width: None,
                    font: &font,
                    align: TextAlign::Left,
                },
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
}
