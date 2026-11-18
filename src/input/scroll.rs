use std::ops::Range;

use super::events::ScrollEvent;

/// Headless scroll offset and viewport range tracker for custom UI containers.
///
/// Tracks `offset`, `viewport`, and `content` extents in logical units without imposing any
/// scrollbar visuals or element structure. Designed to pair with [`Clip`](crate::render::Clip) and
/// [`Transform::translate`](crate::geometry::Transform::translate).
#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub struct ScrollState {
    offset: f32,
    viewport: f32,
    content: f32,
}

impl ScrollState {
    /// Creates a scroll state initialized at offset `0.0` with sanitized extents.
    #[must_use]
    pub fn new(viewport: f32, content: f32) -> Self {
        Self {
            offset: 0.0,
            viewport: sanitize_extent(viewport),
            content: sanitize_extent(content),
        }
    }

    /// Current clamped scroll offset (`0.0..=max_offset()`).
    #[inline]
    #[must_use]
    pub const fn offset(&self) -> f32 {
        self.offset
    }

    /// Viewport extent along the scroll axis.
    #[inline]
    #[must_use]
    pub const fn viewport(&self) -> f32 {
        self.viewport
    }

    /// Total scrollable content extent along the scroll axis.
    #[inline]
    #[must_use]
    pub const fn content(&self) -> f32 {
        self.content
    }

    /// Maximum reachable scroll offset (`(content - viewport).max(0.0)`).
    #[inline]
    #[must_use]
    pub fn max_offset(&self) -> f32 {
        (self.content - self.viewport).max(0.0)
    }

    /// Returns `true` when content exceeds the viewport.
    #[inline]
    #[must_use]
    pub fn can_scroll(&self) -> bool {
        self.max_offset() > 0.0
    }

    /// Normalized scroll progress in `0.0..=1.0` (`0.0` when not scrollable).
    #[must_use]
    pub fn progress(&self) -> f32 {
        let max = self.max_offset();
        if max <= 0.0 {
            0.0
        } else {
            (self.offset / max).clamp(0.0, 1.0)
        }
    }

    /// Updates viewport and content extents and re-clamps `offset` into bounds.
    pub fn set_bounds(&mut self, viewport: f32, content: f32) {
        self.viewport = sanitize_extent(viewport);
        self.content = sanitize_extent(content);
        self.offset = self.clamp_offset(self.offset);
    }

    /// Sets `offset` clamped to `0.0..=max_offset()` and returns `true` if the offset changed.
    pub fn set_offset(&mut self, offset: f32) -> bool {
        let next = if offset.is_finite() {
            self.clamp_offset(offset)
        } else {
            self.offset
        };
        if (next - self.offset).abs() > f32::EPSILON {
            self.offset = next;
            true
        } else {
            false
        }
    }

    /// Shifts the scroll offset by `delta` and clamps to valid bounds.
    pub fn scroll_by(&mut self, delta: f32) -> bool {
        if !delta.is_finite() {
            return false;
        }
        self.set_offset(self.offset + delta)
    }

    /// Applies a vertical [`ScrollEvent`] using `wheel_step_px` per discrete wheel notch (falling
    /// back to `delta_y` for smooth trackpad scrolling). Returns `true` if the offset changed.
    pub fn apply_scroll(&mut self, ev: &ScrollEvent, wheel_step_px: f32) -> bool {
        let delta = resolve_axis_delta(ev.discrete_y, ev.delta_y, wheel_step_px);
        self.scroll_by(delta)
    }

    /// Applies a horizontal [`ScrollEvent`] using `wheel_step_px` per discrete notch.
    pub fn apply_scroll_x(&mut self, ev: &ScrollEvent, wheel_step_px: f32) -> bool {
        let delta = resolve_axis_delta(ev.discrete_x, ev.delta_x, wheel_step_px);
        self.scroll_by(delta)
    }

    /// Adjusts `offset` by the minimal amount needed so `[item_start, item_end]` is inside the
    /// viewport (useful when moving selection with `Up`/`Down` keys).
    pub fn ensure_visible(&mut self, item_start: f32, item_end: f32) -> bool {
        if !item_start.is_finite()
            || !item_end.is_finite()
            || item_end <= item_start
            || self.viewport <= 0.0
        {
            return false;
        }
        let target = if item_start < self.offset {
            item_start
        } else if item_end > self.offset + self.viewport {
            item_end - self.viewport
        } else {
            return false;
        };
        self.set_offset(target)
    }

    /// Returns the `start..end` index slice of uniform-extent items visible in the current
    /// viewport plus `overscan` items on each side (for list virtualization).
    #[must_use]
    pub fn visible_range(
        &self,
        item_count: usize,
        item_extent: f32,
        overscan: usize,
    ) -> Range<usize> {
        if item_count == 0 || self.viewport <= 0.0 || !item_extent.is_finite() || item_extent <= 0.0
        {
            return 0..0;
        }
        let first = (self.offset / item_extent).floor().max(0.0) as usize;
        let last_excl = ((self.offset + self.viewport) / item_extent)
            .ceil()
            .max(0.0) as usize;
        let start = first.saturating_sub(overscan).min(item_count);
        let end = last_excl.saturating_add(overscan).min(item_count);
        start..end.max(start)
    }

    /// Computes `(thumb_offset, thumb_size)` along a scrollbar track of `track_extent`, or `None`
    /// when content fits inside the viewport.
    #[must_use]
    pub fn thumb_metrics(&self, track_extent: f32, min_thumb_extent: f32) -> Option<(f32, f32)> {
        if !self.can_scroll() || !track_extent.is_finite() || track_extent <= 0.0 {
            return None;
        }
        let min_thumb = sanitize_extent(min_thumb_extent).min(track_extent);
        let ratio = (self.viewport / self.content).clamp(0.0, 1.0);
        let thumb_size = (track_extent * ratio).clamp(min_thumb, track_extent);
        let travel = (track_extent - thumb_size).max(0.0);
        let thumb_offset = travel * self.progress();
        Some((thumb_offset, thumb_size))
    }

    #[inline]
    fn clamp_offset(&self, offset: f32) -> f32 {
        offset.clamp(0.0, self.max_offset())
    }
}

fn sanitize_extent(val: f32) -> f32 {
    if val.is_finite() && val > 0.0 {
        val
    } else {
        0.0
    }
}

fn resolve_axis_delta(discrete: i32, smooth: f32, wheel_step_px: f32) -> f32 {
    if discrete != 0 && wheel_step_px.is_finite() {
        discrete as f32 * wheel_step_px
    } else if smooth.is_finite() {
        smooth
    } else {
        0.0
    }
}

#[cfg(test)]
mod tests {
    use crate::geometry::Point;
    use crate::input::Modifiers;

    use super::*;

    fn scroll_ev(delta_y: f32, discrete_y: i32) -> ScrollEvent {
        ScrollEvent {
            position: Point::ZERO,
            local_position: Point::ZERO,
            delta_x: 0.0,
            delta_y,
            discrete_x: 0,
            discrete_y,
            modifiers: Modifiers::empty(),
        }
    }

    #[test]
    fn scroll_clamps_bounds_and_handles_wheel_vs_smooth_events() {
        let mut s = ScrollState::new(200.0, 500.0);
        assert!(s.can_scroll());
        assert_eq!(s.max_offset(), 300.0);

        // Discrete wheel notches advance by wheel_step_px and clamp at bounds.
        assert!(s.apply_scroll(&scroll_ev(15.0, 2), 40.0));
        assert_eq!(s.offset(), 80.0);

        assert!(s.apply_scroll(&scroll_ev(15.0, 10), 40.0));
        assert_eq!(s.offset(), 300.0);
        assert!(!s.apply_scroll(&scroll_ev(15.0, 1), 40.0));

        // Smooth trackpad scroll when discrete_y == 0 uses delta_y directly.
        assert!(s.apply_scroll(&scroll_ev(-25.5, 0), 40.0));
        assert_eq!(s.offset(), 274.5);

        // Shrinking content re-clamps offset.
        s.set_bounds(200.0, 250.0);
        assert_eq!(s.offset(), 50.0);
        s.set_bounds(200.0, 100.0);
        assert_eq!(s.offset(), 0.0);
        assert!(!s.can_scroll());
    }

    #[test]
    fn ensure_visible_visible_range_and_thumb_metrics() {
        let mut s = ScrollState::new(100.0, 500.0);
        // Item already in view -> unchanged.
        assert!(!s.ensure_visible(30.0, 60.0));
        assert_eq!(s.offset(), 0.0);

        // Item below viewport -> minimal downward scroll.
        assert!(s.ensure_visible(120.0, 150.0));
        assert_eq!(s.offset(), 50.0);

        // Item above viewport -> scroll up to item_start.
        assert!(s.ensure_visible(20.0, 50.0));
        assert_eq!(s.offset(), 20.0);

        // Virtualized range with 25px rows at offset 20..120 -> rows 0..5 + overscan 1 = 0..6.
        assert_eq!(s.visible_range(20, 25.0, 1), 0..6);
        s.set_offset(100.0);
        // Visible 100..200 -> rows 4..8, overscan 1 -> 3..9.
        assert_eq!(s.visible_range(20, 25.0, 1), 3..9);

        let (thumb_off, thumb_sz) = s.thumb_metrics(100.0, 12.0).expect("scrollable");
        assert!((thumb_sz - 20.0).abs() < 1e-4);
        // offset 100 / max 400 = 0.25 of remaining 80px track = 20.0.
        assert!((thumb_off - 20.0).abs() < 1e-4);
    }
}
