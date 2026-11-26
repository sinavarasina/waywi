use crate::arena::FrameSharedFn;
use crate::geometry::Point;

use super::keys::Modifiers;
use super::tablet::TabletEvent;

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

/// Physical hardware source of a pointer scroll/axis event (`wl_pointer::AxisSource`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub enum ScrollSource {
    #[default]
    Unknown,
    /// Physical mouse scroll wheel.
    Wheel,
    /// Touchpad or touchscreen finger scroll (smooth, typically followed by `stop_x`/`stop_y`).
    Finger,
    /// Continuous scroll device (e.g. TrackPoint or joystick).
    Continuous,
    /// Side-tilting mouse wheel.
    WheelTilt,
}

/// Pointer scroll/axis event metadata.
#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub struct ScrollEvent {
    pub position: Point,
    pub local_position: Point,
    pub delta_x: f32,
    pub delta_y: f32,
    pub discrete_x: i32,
    pub discrete_y: i32,
    pub source: ScrollSource,
    /// `true` when the compositor signaled `wl_pointer.axis_stop` on the horizontal axis
    /// (e.g. fingers lifted off a touchpad, enabling kinetic scrolling).
    pub stop_x: bool,
    /// `true` when the compositor signaled `wl_pointer.axis_stop` on the vertical axis.
    pub stop_y: bool,
    pub modifiers: Modifiers,
}

/// Lifecycle phase of a touchscreen contact point (`wl_touch`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum TouchPhase {
    Down,
    Move,
    Up,
    Cancel,
}

/// Touch contact metadata delivered to element and surface `on_touch` handlers.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct TouchEvent {
    pub id: i32,
    pub phase: TouchPhase,
    pub position: Point,
    pub local_position: Point,
    /// Contact ellipse `(major, minor)` in surface-local coordinates, if reported by hardware.
    pub shape: Option<(f32, f32)>,
    /// Contact ellipse orientation in degrees clockwise from the Y axis, if reported.
    pub orientation: Option<f32>,
    pub modifiers: Modifiers,
}

/// Lifecycle phase of a multi-finger touchpad gesture (`zwp_pointer_gestures_v1`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum GesturePhase {
    Begin,
    Update,
    End,
    Cancelled,
}

/// Kind and motion payload of a touchpad gesture (`zwp_pointer_gestures_v1`).
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum GestureKind {
    /// Multi-finger swipe (`dx`, `dy` delta in surface-local logical coordinates on `Update`).
    Swipe { dx: f32, dy: f32 },
    /// Multi-finger pinch/rotate (`dx`, `dy` center delta, `scale` relative to `Begin`,
    /// `rotation` delta in degrees clockwise since the previous event).
    Pinch {
        dx: f32,
        dy: f32,
        scale: f32,
        rotation: f32,
    },
    /// Single- or multi-finger hold (`Begin` + `End`/`Cancelled`, e.g. to stop kinetic scroll).
    Hold,
}

/// Multi-finger touchpad gesture event delivered to element and surface `on_gesture` handlers.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct GestureEvent {
    pub kind: GestureKind,
    pub phase: GesturePhase,
    pub fingers: u32,
    /// Last known pointer position on the focused surface (used for hit-testing).
    pub position: Point,
    /// Position relative to the hit-tested element (`position` for surface-level handlers).
    pub local_position: Point,
    pub modifiers: Modifiers,
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
    pub(crate) on_touch: Option<FrameSharedFn<TouchEvent, M>>,
    pub(crate) on_gesture: Option<FrameSharedFn<GestureEvent, M>>,
    pub(crate) on_tablet: Option<FrameSharedFn<TabletEvent, M>>,
    #[cfg(any(
        feature = "pipewire",
        feature = "pulseaudio",
        feature = "alsa",
        feature = "audio-playback"
    ))]
    pub(crate) on_click_sound: Option<crate::audio::SoundPlay>,
    #[cfg(any(
        feature = "pipewire",
        feature = "pulseaudio",
        feature = "alsa",
        feature = "audio-playback"
    ))]
    pub(crate) on_hover_sound: Option<crate::audio::SoundPlay>,
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
            on_touch: None,
            on_gesture: None,
            on_tablet: None,
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
            || self.on_scroll.is_some()
            || self.on_touch.is_some()
            || self.on_gesture.is_some()
            || self.on_tablet.is_some();
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

    pub(crate) fn set_on_touch<F>(&mut self, handler: F)
    where
        F: Fn(TouchEvent) -> M + 'static,
    {
        self.on_touch = Some(FrameSharedFn::new(handler));
    }

    pub(crate) fn set_on_gesture<F>(&mut self, handler: F)
    where
        F: Fn(GestureEvent) -> M + 'static,
    {
        self.on_gesture = Some(FrameSharedFn::new(handler));
    }

    pub(crate) fn set_on_tablet<F>(&mut self, handler: F)
    where
        F: Fn(TabletEvent) -> M + 'static,
    {
        self.on_tablet = Some(FrameSharedFn::new(handler));
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
        let on_touch = map_fn(self.on_touch, mapper);
        let on_gesture = map_fn(self.on_gesture, mapper);
        let on_tablet = map_fn(self.on_tablet, mapper);
        EventHandlers {
            on_click: self.on_click.map(|msg| mapper.call(msg)),
            on_pointer_down,
            on_pointer_up,
            on_pointer_move,
            on_hover: self.on_hover.map(|msg| mapper.call(msg)),
            on_leave: self.on_leave.map(|msg| mapper.call(msg)),
            on_scroll,
            on_touch,
            on_gesture,
            on_tablet,
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
