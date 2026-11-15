use crate::arena::FrameSharedFn;
use crate::geometry::Point;

use super::keys::{KeyEvent, Modifiers};

/// Pointer button identifiers.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum MouseButton {
    Left,
    Right,
    Middle,
    Side,
    Extra,
    Other(u32),
}

impl MouseButton {
    /// Converts a Linux input event button code (`BTN_LEFT = 0x110`, etc.).
    #[must_use]
    pub const fn from_linux_button(code: u32) -> Self {
        match code {
            0x110 => Self::Left,
            0x111 => Self::Right,
            0x112 => Self::Middle,
            0x113 => Self::Side,
            0x114 => Self::Extra,
            other => Self::Other(other),
        }
    }
}

/// Pointer interaction metadata.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct PointerEvent {
    pub position: Point,
    pub local_position: Point,
    pub button: Option<MouseButton>,
    pub modifiers: Modifiers,
}

/// Pointer scroll/axis event metadata.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ScrollEvent {
    pub position: Point,
    pub local_position: Point,
    pub delta_x: f32,
    pub delta_y: f32,
    pub discrete_x: i32,
    pub discrete_y: i32,
    pub modifiers: Modifiers,
}

/// Touch contact metadata.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct TouchEvent {
    pub id: i32,
    pub position: Point,
    pub local_position: Point,
}

/// Event bindings attached to an element.
#[derive(Clone)]
pub(crate) struct EventHandlers<M> {
    pub(crate) on_click: Option<M>,
    pub(crate) on_pointer_down: Option<FrameSharedFn<PointerEvent, M>>,
    pub(crate) on_pointer_up: Option<FrameSharedFn<PointerEvent, M>>,
    pub(crate) on_pointer_move: Option<FrameSharedFn<PointerEvent, M>>,
    pub(crate) on_hover: Option<M>,
    pub(crate) on_leave: Option<M>,
    pub(crate) on_scroll: Option<FrameSharedFn<ScrollEvent, M>>,
    pub(crate) on_key: Option<FrameSharedFn<KeyEvent, Option<M>>>,
    #[cfg(any(
        feature = "pipewire",
        feature = "pulseaudio",
        feature = "alsa",
        feature = "audio-playback"
    ))]
    pub(crate) on_click_sound: Option<(crate::audio::SoundAsset, f32)>,
    #[cfg(any(
        feature = "pipewire",
        feature = "pulseaudio",
        feature = "alsa",
        feature = "audio-playback"
    ))]
    pub(crate) on_hover_sound: Option<(crate::audio::SoundAsset, f32)>,
}

impl<M> Default for EventHandlers<M> {
    fn default() -> Self {
        Self {
            on_click: None,
            on_pointer_down: None,
            on_pointer_up: None,
            on_pointer_move: None,
            on_hover: None,
            on_leave: None,
            on_scroll: None,
            on_key: None,
            #[cfg(any(
                feature = "pipewire",
                feature = "pulseaudio",
                feature = "alsa",
                feature = "audio-playback"
            ))]
            on_click_sound: None,
            #[cfg(any(
                feature = "pipewire",
                feature = "pulseaudio",
                feature = "alsa",
                feature = "audio-playback"
            ))]
            on_hover_sound: None,
        }
    }
}

impl<M: 'static> EventHandlers<M> {
    pub(crate) fn has_pointer_handlers(&self) -> bool {
        let base = self.on_click.is_some()
            || self.on_pointer_down.is_some()
            || self.on_pointer_up.is_some()
            || self.on_pointer_move.is_some()
            || self.on_hover.is_some()
            || self.on_leave.is_some()
            || self.on_scroll.is_some();
        #[cfg(any(
            feature = "pipewire",
            feature = "pulseaudio",
            feature = "alsa",
            feature = "audio-playback"
        ))]
        if self.on_click_sound.is_some() || self.on_hover_sound.is_some() {
            return true;
        }
        base
    }

    pub(crate) fn set_on_pointer_down<F>(&mut self, handler: F)
    where
        F: Fn(PointerEvent) -> M + 'static,
    {
        self.on_pointer_down = Some(FrameSharedFn::new(handler));
    }

    pub(crate) fn set_on_pointer_up<F>(&mut self, handler: F)
    where
        F: Fn(PointerEvent) -> M + 'static,
    {
        self.on_pointer_up = Some(FrameSharedFn::new(handler));
    }

    pub(crate) fn set_on_pointer_move<F>(&mut self, handler: F)
    where
        F: Fn(PointerEvent) -> M + 'static,
    {
        self.on_pointer_move = Some(FrameSharedFn::new(handler));
    }

    pub(crate) fn set_on_scroll<F>(&mut self, handler: F)
    where
        F: Fn(ScrollEvent) -> M + 'static,
    {
        self.on_scroll = Some(FrameSharedFn::new(handler));
    }

    pub(crate) fn set_on_key<F>(&mut self, handler: F)
    where
        F: Fn(KeyEvent) -> Option<M> + 'static,
    {
        self.on_key = Some(FrameSharedFn::new(handler));
    }

    pub(crate) fn map<N: 'static>(self, mapper: &FrameSharedFn<M, N>) -> EventHandlers<N> {
        fn map_fn<A: 'static, M: 'static, N: 'static>(
            opt: Option<FrameSharedFn<A, M>>,
            mapper: &FrameSharedFn<M, N>,
        ) -> Option<FrameSharedFn<A, N>> {
            opt.map(|prev| {
                let m = mapper.clone();
                FrameSharedFn::new(move |ev| m.call(prev.call(ev)))
            })
        }

        let on_pointer_down = map_fn(self.on_pointer_down, mapper);
        let on_pointer_up = map_fn(self.on_pointer_up, mapper);
        let on_pointer_move = map_fn(self.on_pointer_move, mapper);
        let on_scroll = map_fn(self.on_scroll, mapper);
        let on_key = self.on_key.map(|prev| {
            let m = mapper.clone();
            FrameSharedFn::new(move |ev| prev.call(ev).map(|msg| m.call(msg)))
        });
        EventHandlers {
            on_click: self.on_click.map(|msg| mapper.call(msg)),
            on_pointer_down,
            on_pointer_up,
            on_pointer_move,
            on_hover: self.on_hover.map(|msg| mapper.call(msg)),
            on_leave: self.on_leave.map(|msg| mapper.call(msg)),
            on_scroll,
            on_key,
            #[cfg(any(
                feature = "pipewire",
                feature = "pulseaudio",
                feature = "alsa",
                feature = "audio-playback"
            ))]
            on_click_sound: self.on_click_sound,
            #[cfg(any(
                feature = "pipewire",
                feature = "pulseaudio",
                feature = "alsa",
                feature = "audio-playback"
            ))]
            on_hover_sound: self.on_hover_sound,
        }
    }
}
