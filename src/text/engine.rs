#[cfg(feature = "text")]
use std::collections::{HashMap, HashSet};
#[cfg(feature = "text")]
use std::path::Path;
#[cfg(feature = "text")]
use std::sync::{Arc, OnceLock};

#[cfg(feature = "text")]
use super::font::{Font, FontStyle, TextAlign, TextBoundsMode, TextOverflow, TextSpan};
#[cfg(feature = "text")]
use super::shared_str::SharedStr;
#[cfg(feature = "text")]
use crate::error::{Result, WaywiError};
#[cfg(feature = "text")]
use crate::geometry::{Bounds, LogicalPx, LogicalSize, Point, ScaleFactor};
#[cfg(feature = "text")]
use crate::render::Color;

#[cfg(feature = "text")]
const MAX_FONT_FILE_BYTES: u64 = 64 * 1024 * 1024;

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
/// `fontdb::Database` plus any application-registered custom font binaries.
#[cfg(feature = "text")]
pub(crate) struct TextEngine {
    font_system: cosmic_text::FontSystem,
    swash_cache: cosmic_text::SwashCache,
    buffer_cache: HashMap<TextShapeKey, CachedTextBuffer>,
    locale: String,
    load_system_fonts: bool,
    custom_fonts: Vec<Arc<dyn AsRef<[u8]> + Send + Sync>>,
    custom_font_revision: u64,
    max_cached_buffers: usize,
    stale_frame_age: u64,
    max_mapped_font_files: usize,
    font_system_bloated: bool,
    bloated_at_frame: u64,
}

#[cfg(feature = "text")]
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
struct TextShapeKey {
    text: SharedStr,
    spans: Option<Box<[TextSpan]>>,
    font_size_milli: u32,
    line_height_milli: u32,
    max_width_milli: Option<u32>,
    wrap_to_width: bool,
    scale_120: u32,
    font: Font,
    align: TextAlign,
    bounds_mode: TextBoundsMode,
    overflow: TextOverflow,
    max_lines: Option<usize>,
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
        Self::with_config(256, 60, true, 32)
    }

    /// Initializes a `FontSystem` with explicit shaping-cache limits, Fontconfig loading policy,
    /// and automatic mapped-font reclamation threshold.
    pub(crate) fn with_config(
        max_cached_buffers: usize,
        stale_frame_age: u64,
        load_system_fonts: bool,
        max_mapped_font_files: usize,
    ) -> Self {
        let locale = std::env::var("LANG")
            .unwrap_or_else(|_| "en-US".to_owned())
            .split('.')
            .next()
            .unwrap_or("en-US")
            .replace('_', "-");
        let font_system = Self::build_font_system(locale.clone(), load_system_fonts, &[]);

        Self {
            font_system,
            swash_cache: cosmic_text::SwashCache::new(),
            buffer_cache: HashMap::new(),
            locale,
            load_system_fonts,
            custom_fonts: Vec::new(),
            custom_font_revision: 0,
            max_cached_buffers: max_cached_buffers.max(8),
            stale_frame_age: stale_frame_age.max(1),
            max_mapped_font_files,
            font_system_bloated: false,
            bloated_at_frame: 0,
        }
    }

    fn build_font_system(
        locale: String,
        load_system_fonts: bool,
        custom_fonts: &[Arc<dyn AsRef<[u8]> + Send + Sync>],
    ) -> cosmic_text::FontSystem {
        let mut db = if load_system_fonts {
            shared_system_font_db()
        } else {
            fontdb::Database::new()
        };
        for bytes in custom_fonts {
            db.load_font_source(fontdb::Source::Binary(Arc::clone(bytes)));
        }
        cosmic_text::FontSystem::new_with_locale_and_db(locale, db)
    }

    fn rebuild_font_system(&mut self) {
        // Cloning `SYSTEM_FONT_DB` preserves deterministic `fontdb::ID` slotmap keys while
        // resetting every face source back to unmapped `Source::File`, dropping all `Arc<Mmap>`s.
        self.font_system = Self::build_font_system(
            self.locale.clone(),
            self.load_system_fonts,
            &self.custom_fonts,
        );
        self.font_system_bloated = false;
        self.bloated_at_frame = 0;
    }

    #[cfg_attr(not(feature = "renderer-gpu"), allow(dead_code))]
    #[must_use]
    pub(crate) const fn custom_font_revision(&self) -> u64 {
        self.custom_font_revision
    }

    /// Registers custom font bytes (TTF/OTF/TTC/WOFF) into the active `FontSystem` and retains
    /// them across `trim()` / fallback-storm rebuilds. Returns the number of loaded font faces.
    pub(crate) fn load_font_source(
        &mut self,
        bytes: Arc<dyn AsRef<[u8]> + Send + Sync>,
    ) -> Result<usize> {
        if (*bytes).as_ref().is_empty() {
            return Err(WaywiError::AssetDecode("font data is empty".to_owned()));
        }
        let before = self.font_system.db().len();
        self.font_system
            .db_mut()
            .load_font_source(fontdb::Source::Binary(Arc::clone(&bytes)));
        let added = self.font_system.db().len().saturating_sub(before);
        if added == 0 {
            return Err(WaywiError::AssetDecode(
                "no valid font faces found in provided font bytes".to_owned(),
            ));
        }
        self.custom_fonts.push(bytes);
        self.custom_font_revision = self.custom_font_revision.wrapping_add(1);
        self.buffer_cache.clear();
        self.swash_cache = cosmic_text::SwashCache::new();
        Ok(added)
    }

    /// Reads a bounded font file from disk and registers its faces into the active `FontSystem`.
    pub(crate) fn load_font_file(&mut self, path: &Path) -> Result<usize> {
        let meta = std::fs::metadata(path)?;
        if meta.len() > MAX_FONT_FILE_BYTES {
            return Err(WaywiError::AssetDecode(format!(
                "font file {} exceeds max size ({} > {} bytes)",
                path.display(),
                meta.len(),
                MAX_FONT_FILE_BYTES
            )));
        }
        let bytes = std::fs::read(path)?;
        self.load_font_source(Arc::new(bytes))
    }

    fn has_mapped_fonts(&self) -> bool {
        self.font_system
            .db()
            .faces()
            .any(|face| matches!(face.source, fontdb::Source::SharedFile(..)))
    }

    fn mapped_font_file_count(&self) -> usize {
        let mut paths: HashSet<&Path> = HashSet::new();
        for face in self.font_system.db().faces() {
            if let fontdb::Source::SharedFile(path, _) = &face.source {
                paths.insert(path.as_path());
            }
        }
        paths.len()
    }

    fn exceeds_mapped_font_limit(&self) -> bool {
        let limit = self.max_mapped_font_files;
        if limit == 0 {
            return false;
        }
        // Fast zero-allocation pre-check: if total `SharedFile` faces <= limit, unique files are
        // guaranteed <= limit as well.
        let shared_faces = self
            .font_system
            .db()
            .faces()
            .filter(|face| matches!(face.source, fontdb::Source::SharedFile(..)))
            .count();
        if shared_faces <= limit {
            return false;
        }
        self.mapped_font_file_count() > limit
    }

    /// Reclaims mmapped fallback fonts at the end of a rendered frame if shaping triggered a
    /// fallback storm, while keeping `buffer_cache` and `swash_cache` warm so active strings do
    /// not reshape or re-scan on subsequent frames.
    pub(crate) fn finish_frame(&mut self) {
        if self.font_system_bloated {
            self.rebuild_font_system();
        }
    }

    pub(crate) fn trim(&mut self) {
        self.buffer_cache.clear();
        self.buffer_cache.shrink_to_fit();
        self.swash_cache = cosmic_text::SwashCache::new();
        if self.font_system_bloated || self.has_mapped_fonts() {
            self.rebuild_font_system();
        }
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

    /// Measures logical highlight rectangles for the UTF-8 byte range `start_byte..end_byte`
    /// across single-line and wrapped/multi-line shaped text.
    pub(crate) fn measure_text_selection(
        &mut self,
        content: &SharedStr,
        style: super::TextMeasureStyle<'_>,
        start_byte: usize,
        end_byte: usize,
        frame_seq: u64,
    ) -> super::TextSelectionMetrics {
        let text = content.as_str();
        let default_lh = compute_line_height_px(style.font_size, style.line_height_multiplier, 1.0);
        if text.is_empty() {
            return super::TextSelectionMetrics {
                size: LogicalSize::new(LogicalPx::ZERO, LogicalPx(default_lh.ceil())),
                rects: Vec::new(),
            };
        }

        let (raw_lo, raw_hi) = if start_byte <= end_byte {
            (start_byte, end_byte)
        } else {
            (end_byte, start_byte)
        };
        let lo = crate::input::text_input::snap_char_boundary(text, raw_lo);
        let hi = crate::input::text_input::snap_char_boundary(text, raw_hi);

        let params = TextDrawParams {
            content,
            style,
            wrap_to_width: style.max_width.is_some(),
            scale: ScaleFactor::ONE,
        };
        let key = Self::make_key(params);
        let shaped = self.ensure_shaped(key, params, frame_seq);
        let size = shaped.logical_size;
        if lo >= hi {
            return super::TextSelectionMetrics {
                size,
                rects: Vec::new(),
            };
        }

        let line_starts = compute_line_byte_starts(text);
        let mut rects = Vec::new();
        for run in shaped.buffer.layout_runs() {
            let Some(&line_global_start) = line_starts.get(run.line_i) else {
                continue;
            };
            let line_len = run.text.len();
            let line_global_end = line_global_start + line_len;
            if hi <= line_global_start || lo >= line_global_end {
                continue;
            }

            let run_local_lo = lo.saturating_sub(line_global_start).min(line_len);
            let run_local_hi = hi.saturating_sub(line_global_start).min(line_len);
            if run_local_lo >= run_local_hi {
                continue;
            }

            // Compute visual X span of glyphs intersecting `[run_local_lo, run_local_hi)` in this run.
            let mut min_x = f32::INFINITY;
            let mut max_x = f32::NEG_INFINITY;
            for glyph in run.glyphs {
                if glyph.end <= run_local_lo || glyph.start >= run_local_hi {
                    continue;
                }
                min_x = min_x.min(glyph.x);
                max_x = max_x.max(glyph.x + glyph.w);
            }
            if !min_x.is_finite() || !max_x.is_finite() {
                let c0 = cosmic_text::Cursor::new(run.line_i, run_local_lo);
                let c1 = cosmic_text::Cursor::new(run.line_i, run_local_hi);
                if let (Some(x0), Some(x1)) = (run.cursor_position(&c0), run.cursor_position(&c1)) {
                    min_x = x0.min(x1);
                    max_x = x0.max(x1);
                }
            }
            if min_x.is_finite() && max_x.is_finite() && max_x > min_x {
                rects.push(Bounds::new(
                    LogicalPx(min_x),
                    LogicalPx(run.line_top),
                    LogicalPx(max_x - min_x),
                    LogicalPx(run.line_height),
                ));
            }
        }

        super::TextSelectionMetrics { size, rects }
    }

    /// Maps a logical `(x, y)` point relative to the text origin to the nearest UTF-8 byte offset.
    pub(crate) fn hit_test_text_byte(
        &mut self,
        content: &SharedStr,
        style: super::TextMeasureStyle<'_>,
        point: Point,
        frame_seq: u64,
    ) -> usize {
        let text = content.as_str();
        if text.is_empty() {
            return 0;
        }
        let params = TextDrawParams {
            content,
            style,
            wrap_to_width: style.max_width.is_some(),
            scale: ScaleFactor::ONE,
        };
        let key = Self::make_key(params);
        let shaped = self.ensure_shaped(key, params, frame_seq);
        let line_starts = compute_line_byte_starts(text);

        if let Some(cursor) = shaped.buffer.hit(point.x.0, point.y.0)
            && let Some(&line_start) = line_starts.get(cursor.line)
        {
            return crate::input::text_input::snap_char_boundary(
                text,
                line_start.saturating_add(cursor.index),
            );
        }

        // Fallback when pointer is outside exact glyph boxes (before start, past line end, or below).
        let mut best_line: Option<(usize, usize, f32, f32)> = None;
        for run in shaped.buffer.layout_runs() {
            let dist_y = if point.y.0 < run.line_top {
                run.line_top - point.y.0
            } else if point.y.0 > run.line_top + run.line_height {
                point.y.0 - (run.line_top + run.line_height)
            } else {
                0.0
            };
            if best_line.is_none_or(|(_, _, best_d, _)| dist_y < best_d) {
                best_line = Some((run.line_i, run.text.len(), dist_y, run.line_w));
            }
        }
        let Some((line_i, line_len, _, _)) = best_line else {
            return 0;
        };
        let line_start = line_starts.get(line_i).copied().unwrap_or(0);
        if point.x.0 <= 0.0 {
            line_start
        } else {
            crate::input::text_input::snap_char_boundary(text, line_start.saturating_add(line_len))
        }
    }

    fn make_key(params: TextDrawParams<'_>) -> TextShapeKey {
        let s = params.style;
        // Unwrapped left-aligned non-ellipsized text has identical layout regardless of container
        // width, so normalizing `max_width_milli` shares shaped buffers between layout and draw.
        let max_width_milli = if params.wrap_to_width
            || s.align != TextAlign::Left
            || s.overflow != TextOverflow::Clip
        {
            s.max_width.map(|w| (w.0.max(0.0) * 1000.0).round() as u32)
        } else {
            None
        };
        TextShapeKey {
            text: params.content.clone(),
            spans: s.spans.map(|sp| sp.to_vec().into_boxed_slice()),
            font_size_milli: (s.font_size.0.max(1.0) * 1000.0).round() as u32,
            line_height_milli: (s.line_height_multiplier.max(0.5) * 1000.0).round() as u32,
            max_width_milli,
            wrap_to_width: params.wrap_to_width,
            scale_120: params.scale.as_120ths(),
            font: s.font.clone(),
            align: s.align,
            bounds_mode: s.bounds_mode,
            overflow: s.overflow,
            max_lines: s.max_lines,
        }
    }

    fn ensure_shaped(
        &mut self,
        key: TextShapeKey,
        params: TextDrawParams<'_>,
        frame_seq: u64,
    ) -> &mut CachedTextBuffer {
        if self.font_system_bloated && frame_seq > self.bloated_at_frame {
            self.rebuild_font_system();
        }

        if self.buffer_cache.contains_key(&key) {
            let entry = self
                .buffer_cache
                .get_mut(&key)
                .expect("checked cached shaped buffer");
            entry.last_used_frame = frame_seq;
            return entry;
        }

        let s = params.style;
        let stale_age = self.stale_frame_age;
        if frame_seq.is_multiple_of(32) || self.buffer_cache.len() >= self.max_cached_buffers {
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

        let scale = params.scale.as_f32();
        let cached = {
            let font_system = &mut self.font_system;
            let swash_cache = &mut self.swash_cache;
            let fs_px = (s.font_size.0.max(1.0) * scale).max(1.0);
            let lh_px = compute_line_height_px(s.font_size, s.line_height_multiplier, scale);
            let metrics = cosmic_text::Metrics::new(fs_px, lh_px);
            let mut buffer = cosmic_text::Buffer::new(font_system, metrics);

            let effective_wrap = if params.wrap_to_width
                || (s.overflow != TextOverflow::Clip && s.max_lines.is_some_and(|n| n > 1))
            {
                cosmic_text::Wrap::WordOrGlyph
            } else {
                cosmic_text::Wrap::None
            };
            buffer.set_wrap(effective_wrap);
            let ellipsize = match s.overflow {
                TextOverflow::Clip => cosmic_text::Ellipsize::None,
                TextOverflow::EllipsisStart => cosmic_text::Ellipsize::Start(
                    cosmic_text::EllipsizeHeightLimit::Lines(s.max_lines.unwrap_or(1).max(1)),
                ),
                TextOverflow::EllipsisMiddle => cosmic_text::Ellipsize::Middle(
                    cosmic_text::EllipsizeHeightLimit::Lines(s.max_lines.unwrap_or(1).max(1)),
                ),
                TextOverflow::EllipsisEnd => cosmic_text::Ellipsize::End(
                    cosmic_text::EllipsizeHeightLimit::Lines(s.max_lines.unwrap_or(1).max(1)),
                ),
            };
            buffer.set_ellipsize(ellipsize);

            let width_px = s.max_width.map(|w| (w.0 * scale).max(1.0));
            let height_px = s.max_lines.map(|n| (n.max(1) as f32) * lh_px);
            buffer.set_size(width_px, height_px);

            let attrs = font_to_cosmic_attrs(s.font);
            let c_align = match s.align {
                TextAlign::Left => Some(cosmic_text::Align::Left),
                TextAlign::Center => Some(cosmic_text::Align::Center),
                TextAlign::Right => Some(cosmic_text::Align::Right),
            };
            if let Some(spans) = s.spans
                && !spans.is_empty()
            {
                let span_attrs: Vec<(&str, cosmic_text::Attrs<'_>)> = spans
                    .iter()
                    .map(|sp| {
                        let mut a = match sp.font.as_ref() {
                            Some(f) => font_to_cosmic_attrs(f),
                            None => attrs.clone(),
                        };
                        if let Some(c) = sp.color {
                            a = a.color(cosmic_text::Color::rgba(c.r, c.g, c.b, c.a));
                        }
                        (sp.text.as_str(), a)
                    })
                    .collect();
                buffer.set_rich_text(span_attrs, &attrs, cosmic_text::Shaping::Advanced, c_align);
            } else {
                buffer.set_text(
                    params.content.as_str(),
                    &attrs,
                    cosmic_text::Shaping::Advanced,
                    c_align,
                );
            }
            buffer.shape_until_scroll(font_system, false);

            let mut max_w = 0.0_f32;
            let mut line_count = 0_usize;
            for run in buffer.layout_runs() {
                if let Some(limit) = s.max_lines
                    && line_count >= limit.max(1)
                {
                    break;
                }
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
        };

        if !self.font_system_bloated && self.exceeds_mapped_font_limit() {
            self.font_system_bloated = true;
            self.bloated_at_frame = frame_seq;
        }

        self.buffer_cache.entry(key).or_insert(cached)
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
        let clamped_opacity = opacity.clamp(0.0, 1.0);
        let text_color = cosmic_text::Color::rgba(color.r, color.g, color.b, color.a);
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
                let ga = if clamped_opacity < 1.0 {
                    (f32::from(glyph_color.a()) * clamped_opacity).round() as u8
                } else {
                    glyph_color.a()
                };
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

#[cfg(feature = "text")]
fn font_to_cosmic_attrs(font: &Font) -> cosmic_text::Attrs<'_> {
    let family = match (&font.family, font.monospace) {
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
    cosmic_text::Attrs::new()
        .family(family)
        .weight(cosmic_text::Weight(font.weight.to_numeric()))
        .style(match font.style {
            FontStyle::Normal => cosmic_text::Style::Normal,
            FontStyle::Italic => cosmic_text::Style::Italic,
        })
}

#[cfg(feature = "text")]
fn compute_line_byte_starts(text: &str) -> Vec<usize> {
    let mut starts = vec![0usize];
    for (idx, b) in text.bytes().enumerate() {
        if b == b'\n' {
            starts.push(idx + 1);
        }
    }
    starts
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
                    overflow: TextOverflow::Clip,
                    max_lines: None,
                    spans: None,
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
                        overflow: TextOverflow::Clip,
                        max_lines: None,
                        spans: None,
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
            overflow: TextOverflow::Clip,
            max_lines: None,
            spans: None,
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
            overflow: TextOverflow::Clip,
            max_lines: None,
            spans: None,
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

    #[test]
    fn trim_rebuilds_font_system_and_unmaps_loaded_fonts() {
        let mut engine = TextEngine::new();
        assert_eq!(engine.mapped_font_file_count(), 0);
        if engine.font_system.db().is_empty() {
            return;
        }

        let font = Font::sans();
        let style = TextMeasureStyle {
            font_size: LogicalPx(14.0),
            line_height_multiplier: 1.2,
            max_width: None,
            font: &font,
            align: TextAlign::Left,
            bounds_mode: TextBoundsMode::Typographic,
            overflow: TextOverflow::Clip,
            max_lines: None,
            spans: None,
        };
        let first =
            engine.measure_text(&SharedStr::from("Hello WayWi"), style, ScaleFactor::ONE, 1);
        assert!(first.width.0 > 0.0);
        assert!(engine.mapped_font_file_count() > 0);
        assert!(!engine.buffer_cache.is_empty());

        engine.trim();
        assert_eq!(engine.mapped_font_file_count(), 0);
        assert!(engine.buffer_cache.is_empty());
        assert!(!engine.font_system_bloated);

        // Re-shaping after trim works cleanly with identical metrics.
        let second =
            engine.measure_text(&SharedStr::from("Hello WayWi"), style, ScaleFactor::ONE, 2);
        assert_eq!(first, second);
        assert!(engine.mapped_font_file_count() > 0);
    }

    #[test]
    fn fallback_storm_auto_reclaims_at_frame_end_while_preserving_warm_caches() {
        let mut engine = TextEngine::with_config(64, 16, true, 2);
        let unique_system_files = {
            let mut paths = HashSet::new();
            for face in engine.font_system.db().faces() {
                if let fontdb::Source::File(p) | fontdb::Source::SharedFile(p, _) = &face.source {
                    paths.insert(p.clone());
                }
            }
            paths.len()
        };
        if unique_system_files <= 2 {
            return;
        }

        let font = Font::sans();
        let style = TextMeasureStyle {
            font_size: LogicalPx(14.0),
            line_height_multiplier: 1.2,
            max_width: None,
            font: &font,
            align: TextAlign::Left,
            bounds_mode: TextBoundsMode::Typographic,
            overflow: TextOverflow::Clip,
            max_lines: None,
            spans: None,
        };
        // Noncharacter U+10FFFD is guaranteed unassigned in Unicode, triggering exhaustive fallback.
        let content = SharedStr::from("Alert \u{10FFFD}");
        let measured = engine.measure_text(&content, style, ScaleFactor::ONE, 1);
        assert!(measured.width.0 > 0.0);
        assert!(
            engine.mapped_font_file_count() > 2,
            "expected fallback scan to mmap > 2 system font files"
        );
        assert!(engine.font_system_bloated);

        // Rasterize on the same frame so `swash_cache` is populated before reclamation.
        let w = (measured.width.0.ceil() as u32).max(16);
        let h = (measured.height.0.ceil() as u32).max(16);
        let mut pixels = vec![0u8; (w * h * 4) as usize];
        let mut target = TextRasterTarget {
            dst: &mut pixels,
            width: w,
            height: h,
            clip: crate::geometry::BufferRect {
                x: 0,
                y: 0,
                width: w as i32,
                height: h as i32,
            },
        };
        let params = TextDrawParams {
            content: &content,
            style,
            wrap_to_width: false,
            scale: ScaleFactor::ONE,
        };
        engine.draw_text_premultiplied_rgba(&mut target, (0.0, 0.0), params, Color::WHITE, 1.0, 1);

        // End of frame unmaps all fallback fonts while preserving shaped buffer and glyph caches.
        engine.finish_frame();
        assert_eq!(engine.mapped_font_file_count(), 0);
        assert!(!engine.font_system_bloated);
        assert_eq!(engine.buffer_cache.len(), 1);

        // Redrawing the same string on the next frame hits both caches without re-triggering bloat.
        engine.draw_text_premultiplied_rgba(&mut target, (0.0, 0.0), params, Color::WHITE, 1.0, 2);
        assert!(!engine.font_system_bloated);
        assert!(engine.mapped_font_file_count() <= 2);

        // Setting `max_mapped_font_files = 0` disables automatic reclamation (explicit `trim` still works).
        let mut disabled = TextEngine::with_config(64, 16, true, 0);
        let _ = disabled.measure_text(&content, style, ScaleFactor::ONE, 1);
        assert!(!disabled.font_system_bloated);
        disabled.finish_frame();
        assert!(disabled.mapped_font_file_count() > 2);
        disabled.trim();
        assert_eq!(disabled.mapped_font_file_count(), 0);
    }

    #[test]
    fn ellipsis_max_lines_rich_spans_selection_and_hit_test_work() {
        let mut engine = TextEngine::new();
        let font = Font::sans();
        let long =
            SharedStr::from("This is a very long window title that should ellipsize cleanly");
        let full_style = TextMeasureStyle {
            font_size: LogicalPx(14.0),
            line_height_multiplier: 1.25,
            max_width: Some(LogicalPx(90.0)),
            font: &font,
            align: TextAlign::Left,
            bounds_mode: TextBoundsMode::Typographic,
            overflow: TextOverflow::Clip,
            max_lines: None,
            spans: None,
        };
        let wrapped = engine.measure_text(&long, full_style, ScaleFactor::ONE, 1);
        let single_line = engine.measure_text(
            &long,
            TextMeasureStyle {
                overflow: TextOverflow::EllipsisEnd,
                max_lines: Some(1),
                ..full_style
            },
            ScaleFactor::ONE,
            2,
        );
        assert!(
            single_line.height.0 < wrapped.height.0,
            "1-line ellipsized text ({:?}) must be shorter than unwrapped multiline ({:?})",
            single_line,
            wrapped
        );
        assert!(single_line.width.0 <= 92.0);

        // Rich spans render per-span colors into distinct RGBA channels.
        let spans = [
            TextSpan::new("R").color(Color::RED),
            TextSpan::new("B").color(Color::BLUE),
        ];
        let rich_style = TextMeasureStyle {
            max_width: None,
            spans: Some(&spans),
            ..full_style
        };
        let content = SharedStr::from("RB");
        let sz = engine.measure_text(&content, rich_style, ScaleFactor::ONE, 3);
        let w = (sz.width.0.ceil() as u32).max(16) + 8;
        let h = (sz.height.0.ceil() as u32).max(16) + 8;
        let mut pixels = vec![0u8; (w * h * 4) as usize];
        let mut target = TextRasterTarget {
            dst: &mut pixels,
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
            (2.0, 2.0),
            TextDrawParams {
                content: &content,
                style: rich_style,
                wrap_to_width: false,
                scale: ScaleFactor::ONE,
            },
            Color::WHITE,
            1.0,
            3,
        );
        let (rgba, _) = pixels.as_chunks::<4>();
        let has_red = rgba.iter().any(|p| p[0] > 180 && p[2] < 40 && p[3] > 0);
        let has_blue = rgba.iter().any(|p| p[2] > 180 && p[0] < 40 && p[3] > 0);
        assert!(
            has_red && has_blue,
            "expected both red and blue span pixels"
        );

        // Selection metrics & hit-testing roundtrip.
        let sample = SharedStr::from("Hello World");
        let sel_style = TextMeasureStyle {
            max_width: None,
            ..full_style
        };
        let sel = engine.measure_text_selection(&sample, sel_style, 0, 5, 4);
        assert_eq!(sel.rects.len(), 1);
        assert!(sel.rects[0].width.0 > 0.0);
        let hit_end = engine.hit_test_text_byte(
            &sample,
            sel_style,
            Point::new(LogicalPx(500.0), LogicalPx(8.0)),
            4,
        );
        assert_eq!(hit_end, sample.len());
        let hit_start = engine.hit_test_text_byte(
            &sample,
            sel_style,
            Point::new(LogicalPx(0.0), LogicalPx(8.0)),
            4,
        );
        assert_eq!(hit_start, 0);

        // Invalid custom font bytes return an honest error.
        assert!(
            engine
                .load_font_source(Arc::new(&b"not-a-font"[..]))
                .is_err()
        );
    }
}
