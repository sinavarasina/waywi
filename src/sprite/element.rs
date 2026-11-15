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
}

impl<M: 'static> Sprite<M> {
    pub fn new(sheet: impl Into<SpriteSheet>) -> Self {
        Self {
            common: CommonProps::default(),
            sheet: sheet.into(),
            mode: SpriteSourceState::FixedFrame(0),
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
    /// smoothly on `wl_surface.frame` ticks without invoking user `update`/`view` per frame.
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

    crate::input::impl_common_builders!();

    pub fn map<N: 'static, F>(self, f: F) -> Sprite<N>
    where
        F: Fn(M) -> N + 'static,
    {
        Sprite {
            common: self.common.map(f),
            sheet: self.sheet,
            mode: self.mode,
        }
    }
}
