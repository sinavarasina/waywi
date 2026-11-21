use crate::image::ImageFilter;
use crate::input::CommonProps;
use crate::text::SharedStr;

use super::sheet::{PlaybackMode, SpriteSheet};

/// Sprite selection mode.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum SpriteSourceState {
    FixedFrame(usize),
    Playing {
        animation: SharedStr,
        playback_override: Option<PlaybackMode>,
    },
}

/// Animated sprite element backed by a [`SpriteSheet`].
#[must_use]
#[derive(Clone)]
pub struct Sprite<M = ()> {
    pub(crate) common: CommonProps<M>,
    pub(crate) sheet: SpriteSheet,
    pub(crate) mode: SpriteSourceState,
    pub(crate) filter: ImageFilter,
    pub(crate) flip_x: bool,
    pub(crate) flip_y: bool,
    pub(crate) on_animation_end: Option<M>,
}

impl<M: 'static> Sprite<M> {
    pub fn new(sheet: impl Into<SpriteSheet>) -> Self {
        Self {
            common: CommonProps::default(),
            sheet: sheet.into(),
            mode: SpriteSourceState::FixedFrame(0),
            filter: ImageFilter::Bilinear,
            flip_x: false,
            flip_y: false,
            on_animation_end: None,
        }
    }

    /// Selects a state-derived frame index explicitly.
    pub fn frame(mut self, index: usize) -> Self {
        self.mode = SpriteSourceState::FixedFrame(index);
        self
    }

    /// Plays a named animation registered on the [`SpriteSheet`].
    ///
    /// When the `Sprite` is keyed, the runtime's internal animation scheduler advances frames
    /// smoothly without invoking user `update`/`view` per frame.
    pub fn playing(mut self, animation_name: impl Into<SharedStr>) -> Self {
        self.mode = SpriteSourceState::Playing {
            animation: animation_name.into(),
            playback_override: None,
        };
        self
    }

    pub fn playback(mut self, mode: PlaybackMode) -> Self {
        if let SpriteSourceState::Playing {
            ref mut playback_override,
            ..
        } = self.mode
        {
            *playback_override = Some(mode);
        }
        self
    }

    /// Sets the resampling filter used when scaling or transforming this sprite
    /// (use [`ImageFilter::Nearest`] for crisp pixel art).
    pub fn filter(mut self, filter: ImageFilter) -> Self {
        self.filter = filter;
        self
    }

    /// Mirrors the sprite horizontally around its center (composes with `.transform(...)`).
    pub fn flip_x(mut self, flip: bool) -> Self {
        self.flip_x = flip;
        self
    }

    /// Mirrors the sprite vertically around its center (composes with `.transform(...)`).
    pub fn flip_y(mut self, flip: bool) -> Self {
        self.flip_y = flip;
        self
    }

    /// Emits `msg` once when a [`PlaybackMode::Once`] animation finishes playing its final frame.
    pub fn on_animation_end(mut self, msg: M) -> Self {
        self.on_animation_end = Some(msg);
        self
    }

    crate::input::impl_common_builders!();

    pub fn map<N: 'static, F>(self, f: F) -> Sprite<N>
    where
        F: Fn(M) -> N + 'static,
    {
        let on_animation_end = self.on_animation_end.map(&f);
        Sprite {
            common: self.common.map(f),
            sheet: self.sheet,
            mode: self.mode,
            filter: self.filter,
            flip_x: self.flip_x,
            flip_y: self.flip_y,
            on_animation_end,
        }
    }
}
