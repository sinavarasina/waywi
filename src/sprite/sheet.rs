use std::ops::Range;
use std::sync::Arc;
use std::time::Duration;

use crate::asset::ImageAsset;
use crate::image::SourceRect;
use crate::text::SharedStr;

/// Frame playback mode for [`Sprite`](super::Sprite) animations.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub enum PlaybackMode {
    /// Play from start to end once and hold the final frame.
    Once,
    /// Loop continuously from start to end.
    #[default]
    Loop,
    /// Alternate forward and backward across the frame sequence.
    PingPong,
}

/// Frame rate specification (`frames per second`).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Fps(pub f32);

impl Fps {
    #[inline]
    #[must_use]
    pub const fn new(fps: f32) -> Self {
        Self(fps)
    }

    #[inline]
    #[must_use]
    pub fn get(self) -> f32 {
        self.0.max(0.1)
    }

    #[inline]
    #[must_use]
    pub fn frame_duration(self) -> Duration {
        Duration::from_secs_f64(1.0 / f64::from(self.get()))
    }
}

/// Extension trait providing `.fps()` on `i32` and `f32`.
pub trait FpsExt {
    fn fps(self) -> Fps;
}

impl FpsExt for i32 {
    #[inline]
    fn fps(self) -> Fps {
        Fps(self as f32)
    }
}

impl FpsExt for f32 {
    #[inline]
    fn fps(self) -> Fps {
        Fps(self)
    }
}

/// Single sprite animation frame with an explicit display duration.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct SpriteFrame {
    /// Frame index inside the [`SpriteSheet`].
    pub index: usize,
    /// How long this frame should remain visible before advancing.
    pub duration: Duration,
}

impl SpriteFrame {
    #[inline]
    #[must_use]
    pub const fn new(index: usize, duration: Duration) -> Self {
        Self { index, duration }
    }
}

/// Frame sequence specification inside a [`SpriteSheet`].
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum Frames {
    /// Contiguous frame index range `[start, end)` played at the clip's uniform FPS.
    Range { start: usize, end: usize },
    /// Arbitrary ordered sequence of frame indices (repeats allowed) played at uniform FPS.
    Sequence(Arc<[usize]>),
    /// Ordered sequence of frames with explicit per-frame display durations.
    Timed(Arc<[SpriteFrame]>),
}

impl Frames {
    /// Creates a contiguous frame range `[range.start, range.end)`.
    #[inline]
    #[must_use]
    pub const fn range(range: Range<usize>) -> Self {
        Self::Range {
            start: range.start,
            end: range.end,
        }
    }

    /// Creates an arbitrary ordered sequence of frame indices (e.g. `[0, 1, 2, 1, 0, 4]`).
    #[must_use]
    pub fn sequence(indices: impl IntoIterator<Item = usize>) -> Self {
        let vec: Vec<usize> = indices.into_iter().collect();
        Self::Sequence(Arc::from(vec))
    }

    /// Creates a sequence of frames with explicit per-frame display durations.
    #[must_use]
    pub fn timed(frames: impl IntoIterator<Item = (usize, Duration)>) -> Self {
        let vec: Vec<SpriteFrame> = frames
            .into_iter()
            .map(|(index, duration)| SpriteFrame { index, duration })
            .collect();
        Self::Timed(Arc::from(vec))
    }

    /// Number of logical steps in one forward pass of this sequence.
    #[inline]
    #[must_use]
    pub fn count(&self) -> usize {
        match self {
            Self::Range { start, end } => end.saturating_sub(*start),
            Self::Sequence(seq) => seq.len(),
            Self::Timed(frames) => frames.len(),
        }
    }

    /// First frame index in the sequence (or `0` when empty).
    #[inline]
    #[must_use]
    pub fn first_frame(&self) -> usize {
        match self {
            Self::Range { start, .. } => *start,
            Self::Sequence(seq) => seq.first().copied().unwrap_or(0),
            Self::Timed(frames) => frames.first().map_or(0, |f| f.index),
        }
    }

    #[inline]
    fn frame_index_at_offset(&self, offset: usize) -> usize {
        match self {
            Self::Range { start, .. } => start + offset,
            Self::Sequence(seq) => seq.get(offset).copied().unwrap_or(0),
            Self::Timed(frames) => frames.get(offset).map_or(0, |f| f.index),
        }
    }

    #[inline]
    fn offset_for_step(count: usize, step: usize, mode: PlaybackMode) -> usize {
        if count <= 1 {
            return 0;
        }
        match mode {
            PlaybackMode::Once => step.min(count - 1),
            PlaybackMode::Loop => step % count,
            PlaybackMode::PingPong => {
                let cycle = (count - 1) * 2;
                let pos = step % cycle;
                if pos < count { pos } else { cycle - pos }
            }
        }
    }

    /// Resolves the sheet frame index at discrete step `step` under `mode`.
    #[must_use]
    pub fn frame_at(&self, step: usize, mode: PlaybackMode) -> usize {
        let count = self.count();
        if count == 0 {
            return self.first_frame();
        }
        let offset = Self::offset_for_step(count, step, mode);
        self.frame_index_at_offset(offset)
    }

    /// Resolves `(frame_index, finished, next_change_offset)` for elapsed time since clip start.
    ///
    /// For [`PlaybackMode::Once`], the final frame remains active for its full duration before
    /// `finished` transitions to `true` and `next_change_offset` becomes `None`.
    pub(crate) fn resolve_at_elapsed(
        &self,
        elapsed: Duration,
        mode: PlaybackMode,
        default_fps: Fps,
    ) -> (usize, bool, Option<Duration>) {
        let count = self.count();
        if count == 0 {
            return (self.first_frame(), mode == PlaybackMode::Once, None);
        }

        match self {
            Self::Range { .. } | Self::Sequence(_) => {
                let frame_dur = default_fps.frame_duration();
                let dur_secs = frame_dur.as_secs_f64();
                let step = (elapsed.as_secs_f64() / dur_secs).floor() as usize;
                let frame = self.frame_at(step, mode);
                if mode == PlaybackMode::Once {
                    if step >= count {
                        (frame, true, None)
                    } else {
                        let next_offset = frame_dur.mul_f64((step + 1) as f64);
                        (frame, false, Some(next_offset))
                    }
                } else if count <= 1 {
                    (frame, false, None)
                } else {
                    let next_offset = frame_dur.mul_f64((step + 1) as f64);
                    (frame, false, Some(next_offset))
                }
            }
            Self::Timed(frames) => Self::resolve_timed(frames, elapsed, mode),
        }
    }

    fn resolve_timed(
        frames: &[SpriteFrame],
        elapsed: Duration,
        mode: PlaybackMode,
    ) -> (usize, bool, Option<Duration>) {
        let count = frames.len();
        let step_dur = |idx: usize| frames[idx].duration.max(Duration::from_millis(1));

        if mode == PlaybackMode::Once {
            let mut accum = Duration::ZERO;
            for (idx, frame) in frames.iter().enumerate() {
                let next_accum = accum.saturating_add(step_dur(idx));
                if elapsed < next_accum {
                    return (frame.index, false, Some(next_accum));
                }
                accum = next_accum;
            }
            return (frames[count - 1].index, true, None);
        }

        if count <= 1 {
            return (frames[0].index, false, None);
        }

        let cycle_steps = match mode {
            PlaybackMode::Loop => count,
            PlaybackMode::PingPong => (count - 1) * 2,
            PlaybackMode::Once => unreachable!(),
        };

        let mut cycle_nanos: u128 = 0;
        for s in 0..cycle_steps {
            let offset = Self::offset_for_step(count, s, mode);
            cycle_nanos = cycle_nanos.saturating_add(step_dur(offset).as_nanos());
        }
        let cycle_nanos = cycle_nanos.max(1);
        let elapsed_nanos = elapsed.as_nanos();
        let completed_cycles = elapsed_nanos / cycle_nanos;
        let in_cycle = elapsed_nanos % cycle_nanos;
        let cycle_base_nanos = completed_cycles.saturating_mul(cycle_nanos);

        let mut accum_nanos: u128 = 0;
        for s in 0..cycle_steps {
            let offset = Self::offset_for_step(count, s, mode);
            let next_accum = accum_nanos.saturating_add(step_dur(offset).as_nanos());
            if in_cycle < next_accum {
                let next_total_nanos = cycle_base_nanos.saturating_add(next_accum);
                let next_dur = Duration::from_nanos(next_total_nanos.min(u64::MAX as u128) as u64);
                return (frames[offset].index, false, Some(next_dur));
            }
            accum_nanos = next_accum;
        }

        let last_offset = Self::offset_for_step(count, cycle_steps - 1, mode);
        let next_total_nanos = cycle_base_nanos.saturating_add(cycle_nanos);
        let next_dur = Duration::from_nanos(next_total_nanos.min(u64::MAX as u128) as u64);
        (frames[last_offset].index, false, Some(next_dur))
    }
}

/// Named animation sequence registered on a [`SpriteSheet`].
#[derive(Debug, Clone, PartialEq)]
pub struct AnimationDef {
    pub name: SharedStr,
    pub frames: Frames,
    pub fps: Fps,
    pub mode: PlaybackMode,
}

/// Reusable sprite sheet definition referencing a decoded [`ImageAsset`] and an unbounded set
/// of named animation clips (cheap O(1) `Clone` via `Arc`).
///
/// Supports both uniform grid sheets ([`SpriteSheet::frame_size`]) and irregular texture atlases
/// with per-frame pixel sub-rectangles ([`SpriteSheet::from_regions`] / [`SpriteSheet::regions`]).
#[derive(Debug, Clone, PartialEq)]
pub struct SpriteSheet {
    pub asset: ImageAsset,
    pub frame_width: u32,
    pub frame_height: u32,
    regions: Option<Arc<[SourceRect]>>,
    animations: Arc<Vec<AnimationDef>>,
}

impl From<&SpriteSheet> for SpriteSheet {
    #[inline]
    fn from(value: &SpriteSheet) -> Self {
        value.clone()
    }
}

impl SpriteSheet {
    #[must_use]
    pub fn new(asset: ImageAsset) -> Self {
        Self {
            asset,
            frame_width: asset.width.max(1),
            frame_height: asset.height.max(1),
            regions: None,
            animations: Arc::new(Vec::new()),
        }
    }

    /// Creates a sprite sheet backed by explicit per-frame [`SourceRect`] regions inside an
    /// irregular texture atlas (e.g. visual novel facial expression / body-part strips).
    #[must_use]
    pub fn from_regions(asset: ImageAsset, regions: impl IntoIterator<Item = SourceRect>) -> Self {
        Self::new(asset).regions(regions)
    }

    /// Sets explicit per-frame [`SourceRect`] regions for an irregular texture atlas.
    ///
    /// When set, frame indices index directly into this list (wrapping modulo `regions.len()`)
    /// rather than slicing a uniform grid.
    #[must_use]
    pub fn regions(mut self, regions: impl IntoIterator<Item = SourceRect>) -> Self {
        let list: Arc<[SourceRect]> = Arc::from(regions.into_iter().collect::<Vec<_>>());
        if let Some(first) = list.first() {
            self.frame_width = first.width.max(1);
            self.frame_height = first.height.max(1);
        }
        self.regions = if list.is_empty() { None } else { Some(list) };
        self
    }

    /// Returns the explicit [`SourceRect`] for `frame_index` when using region-based atlas frames.
    #[must_use]
    pub fn region_of(&self, frame_index: usize) -> Option<SourceRect> {
        let list = self.regions.as_ref()?;
        if list.is_empty() {
            return None;
        }
        Some(list[frame_index % list.len()])
    }

    /// Sets the pixel size of each sprite frame cell (switches to uniform grid slicing).
    #[must_use]
    pub fn frame_size(mut self, width: u32, height: u32) -> Self {
        self.frame_width = width.max(1);
        self.frame_height = height.max(1);
        self.regions = None;
        self
    }

    /// Creates a grid-sliced sprite sheet directly from `(cols, rows)` counts.
    #[must_use]
    pub fn grid(asset: ImageAsset, cols: u32, rows: u32) -> Self {
        let fw = (asset.width / cols.max(1)).max(1);
        let fh = (asset.height / rows.max(1)).max(1);
        Self::new(asset).frame_size(fw, fh)
    }

    /// Registers a looping named animation clip (`name`, `frames`, `fps`).
    ///
    /// Accepts static `&str` literals or dynamic runtime strings (`String`, `SharedStr`).
    #[must_use]
    pub fn animation(self, name: impl Into<SharedStr>, frames: Frames, fps: Fps) -> Self {
        self.animation_with_mode(name, frames, fps, PlaybackMode::Loop)
    }

    /// Registers a named animation clip with explicit [`PlaybackMode`].
    #[must_use]
    pub fn animation_with_mode(
        mut self,
        name: impl Into<SharedStr>,
        frames: Frames,
        fps: Fps,
        mode: PlaybackMode,
    ) -> Self {
        let name = name.into();
        let list = Arc::make_mut(&mut self.animations);
        if let Some(existing) = list.iter_mut().find(|slot| slot.name == name) {
            *existing = AnimationDef {
                name,
                frames,
                fps,
                mode,
            };
        } else {
            list.push(AnimationDef {
                name,
                frames,
                fps,
                mode,
            });
        }
        self
    }

    /// Registers a named animation clip with explicit per-frame `(frame_index, duration)` pairs.
    #[must_use]
    pub fn timed_animation(
        self,
        name: impl Into<SharedStr>,
        frames: impl IntoIterator<Item = (usize, Duration)>,
        mode: PlaybackMode,
    ) -> Self {
        self.animation_with_mode(name, Frames::timed(frames), Fps::new(1.0), mode)
    }

    /// Number of named animation clips registered on this sheet.
    #[must_use]
    pub fn animation_count(&self) -> usize {
        self.animations.len()
    }

    /// Slice of all registered animation clips on this sheet.
    #[must_use]
    pub fn animations(&self) -> &[AnimationDef] {
        &self.animations
    }

    #[must_use]
    pub fn get_animation(&self, name: &str) -> Option<&AnimationDef> {
        self.animations
            .iter()
            .find(|anim| anim.name.as_str() == name)
    }

    #[must_use]
    pub fn columns(&self) -> usize {
        (self.asset.width / self.frame_width.max(1)).max(1) as usize
    }

    #[must_use]
    pub fn rows(&self) -> usize {
        (self.asset.height / self.frame_height.max(1)).max(1) as usize
    }

    #[must_use]
    pub fn total_frames(&self) -> usize {
        if let Some(ref list) = self.regions {
            list.len()
        } else {
            self.columns() * self.rows()
        }
    }

    /// Computes source pixel `(x, y, width, height)` inside the underlying sheet asset.
    #[must_use]
    pub fn frame_source_rect(&self, frame_index: usize) -> (u32, u32, u32, u32) {
        if let Some(region) = self.region_of(frame_index) {
            return region.as_tuple();
        }
        let total = self.total_frames().max(1);
        let idx = frame_index % total;
        let cols = self.columns();
        let col = (idx % cols) as u32;
        let row = (idx / cols) as u32;
        (
            col * self.frame_width,
            row * self.frame_height,
            self.frame_width,
            self.frame_height,
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::asset::AssetId;

    #[test]
    fn frames_sequence_and_timed_resolve_across_modes_with_exact_deadlines() {
        let seq = Frames::sequence([3, 1, 4, 1]);
        assert_eq!(seq.count(), 4);
        assert_eq!(seq.first_frame(), 3);
        assert_eq!(seq.frame_at(0, PlaybackMode::Loop), 3);
        assert_eq!(seq.frame_at(2, PlaybackMode::Loop), 4);
        assert_eq!(seq.frame_at(5, PlaybackMode::Loop), 1);
        assert_eq!(seq.frame_at(4, PlaybackMode::PingPong), 4);
        assert_eq!(seq.frame_at(10, PlaybackMode::Once), 1);

        let timed = Frames::timed([
            (0, Duration::from_millis(100)),
            (2, Duration::from_millis(250)),
            (5, Duration::from_millis(50)),
        ]);
        // Once mode: total = 400ms; holds final frame until finished at 400ms.
        assert_eq!(
            timed.resolve_at_elapsed(Duration::from_millis(0), PlaybackMode::Once, 1.fps()),
            (0, false, Some(Duration::from_millis(100)))
        );
        assert_eq!(
            timed.resolve_at_elapsed(Duration::from_millis(150), PlaybackMode::Once, 1.fps()),
            (2, false, Some(Duration::from_millis(350)))
        );
        assert_eq!(
            timed.resolve_at_elapsed(Duration::from_millis(360), PlaybackMode::Once, 1.fps()),
            (5, false, Some(Duration::from_millis(400)))
        );
        assert_eq!(
            timed.resolve_at_elapsed(Duration::from_millis(400), PlaybackMode::Once, 1.fps()),
            (5, true, None)
        );

        // Loop mode wraps across cycles seamlessly (cycle = 400ms).
        assert_eq!(
            timed.resolve_at_elapsed(Duration::from_millis(420), PlaybackMode::Loop, 1.fps()),
            (0, false, Some(Duration::from_millis(500)))
        );

        // PingPong mode: steps are 0(100ms), 2(250ms), 5(50ms), 2(250ms) -> cycle = 650ms.
        assert_eq!(
            timed.resolve_at_elapsed(Duration::from_millis(410), PlaybackMode::PingPong, 1.fps()),
            (2, false, Some(Duration::from_millis(650)))
        );
    }

    #[test]
    fn sprite_sheet_supports_more_than_16_dynamic_animations() {
        let asset = ImageAsset {
            id: AssetId::new(1),
            width: 128,
            height: 128,
        };
        let mut sheet = SpriteSheet::new(asset).frame_size(32, 32);
        for i in 0..32 {
            sheet = sheet.animation(format!("clip_{i}"), Frames::range(0..4), 12.fps());
        }
        assert_eq!(sheet.animation_count(), 32);
        assert_eq!(sheet.animations().len(), 32);
        assert!(sheet.get_animation("clip_0").is_some());
        assert!(sheet.get_animation("clip_31").is_some());
    }
}
