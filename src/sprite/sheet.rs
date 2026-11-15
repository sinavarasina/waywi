use std::ops::Range;
use std::time::Duration;

use crate::asset::ImageAsset;
use crate::geometry::LogicalPx;

const MAX_SHEET_ANIMATIONS: usize = 16;

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
        Duration::from_secs_f32(1.0 / self.get())
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

/// Contiguous frame index range inside a [`SpriteSheet`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct Frames {
    pub start: usize,
    pub end: usize,
}

impl Frames {
    #[inline]
    #[must_use]
    pub const fn range(range: Range<usize>) -> Self {
        Self {
            start: range.start,
            end: range.end,
        }
    }

    #[inline]
    #[must_use]
    pub const fn count(&self) -> usize {
        self.end.saturating_sub(self.start)
    }

    #[must_use]
    pub fn frame_at(&self, step: usize, mode: PlaybackMode) -> usize {
        let count = self.count();
        if count <= 1 {
            return self.start;
        }
        let offset = match mode {
            PlaybackMode::Once => step.min(count - 1),
            PlaybackMode::Loop => step % count,
            PlaybackMode::PingPong => {
                let cycle = (count - 1) * 2;
                let pos = step % cycle;
                if pos < count { pos } else { cycle - pos }
            }
        };
        self.start + offset
    }
}

/// Named animation sequence registered on a [`SpriteSheet`].
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct AnimationDef {
    pub name: &'static str,
    pub frames: Frames,
    pub fps: Fps,
    pub mode: PlaybackMode,
}

/// Reusable sprite sheet definition referencing a decoded [`ImageAsset`] and optional named
/// animation clips.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct SpriteSheet {
    pub asset: ImageAsset,
    pub frame_width: u32,
    pub frame_height: u32,
    animations: [Option<AnimationDef>; MAX_SHEET_ANIMATIONS],
    anim_count: usize,
}

impl From<&SpriteSheet> for SpriteSheet {
    #[inline]
    fn from(value: &SpriteSheet) -> Self {
        *value
    }
}

impl SpriteSheet {
    #[must_use]
    pub fn new(asset: ImageAsset) -> Self {
        Self {
            asset,
            frame_width: asset.width.max(1),
            frame_height: asset.height.max(1),
            animations: [None; MAX_SHEET_ANIMATIONS],
            anim_count: 0,
        }
    }

    /// Sets the logical/pixel size of each sprite frame cell.
    #[must_use]
    pub fn frame_size(mut self, width: LogicalPx, height: LogicalPx) -> Self {
        self.frame_width = width.ceil_u32().max(1);
        self.frame_height = height.ceil_u32().max(1);
        self
    }

    /// Registers a looping named animation clip (`name`, `frames`, `fps`).
    #[must_use]
    pub fn animation(self, name: &'static str, frames: Frames, fps: Fps) -> Self {
        self.animation_with_mode(name, frames, fps, PlaybackMode::Loop)
    }

    /// Registers a named animation clip with explicit [`PlaybackMode`].
    #[must_use]
    pub fn animation_with_mode(
        mut self,
        name: &'static str,
        frames: Frames,
        fps: Fps,
        mode: PlaybackMode,
    ) -> Self {
        for slot in self.animations[..self.anim_count].iter_mut().flatten() {
            if slot.name == name {
                *slot = AnimationDef {
                    name,
                    frames,
                    fps,
                    mode,
                };
                return self;
            }
        }
        if self.anim_count < MAX_SHEET_ANIMATIONS {
            self.animations[self.anim_count] = Some(AnimationDef {
                name,
                frames,
                fps,
                mode,
            });
            self.anim_count += 1;
        }
        self
    }

    #[must_use]
    pub fn get_animation(&self, name: &str) -> Option<&AnimationDef> {
        self.animations[..self.anim_count]
            .iter()
            .flatten()
            .find(|anim| anim.name == name)
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
        self.columns() * self.rows()
    }

    /// Computes source pixel `(x, y, width, height)` inside the underlying sheet asset.
    #[must_use]
    pub fn frame_source_rect(&self, frame_index: usize) -> (u32, u32, u32, u32) {
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
