use crate::accessibility::AccessibleProps;
use crate::animation::TransitionSpec;
use crate::arena::FrameSharedFn;
use crate::geometry::{ElementAnchor, Position, Size, Transform};

use super::cursor::CursorShape;
use super::events::EventHandlers;
use super::keys::ElementKey;

/// Shared layout and interaction fields on visual elements.
#[derive(Clone)]
pub(crate) struct CommonProps<M> {
    pub(crate) key: Option<ElementKey>,
    pub(crate) position: Position,
    pub(crate) size: Size,
    pub(crate) anchor: ElementAnchor,
    pub(crate) visible: bool,
    pub(crate) opacity: f32,
    pub(crate) transform: Transform,
    pub(crate) opacity_transition: Option<TransitionSpec>,
    pub(crate) transform_transition: Option<TransitionSpec>,
    pub(crate) z_index: i32,
    pub(crate) input_enabled: bool,
    pub(crate) cursor: Option<CursorShape>,
    pub(crate) handlers: EventHandlers<M>,
    pub(crate) a11y: AccessibleProps,
}

impl<M> Default for CommonProps<M> {
    fn default() -> Self {
        Self {
            key: None,
            position: Position::Auto,
            size: Size::auto(),
            anchor: ElementAnchor::TopLeft,
            visible: true,
            opacity: 1.0,
            transform: Transform::identity(),
            opacity_transition: None,
            transform_transition: None,
            z_index: 0,
            input_enabled: true,
            cursor: None,
            handlers: EventHandlers::default(),
            a11y: AccessibleProps::default(),
        }
    }
}

impl<M: 'static> CommonProps<M> {
    pub(crate) fn map<N: 'static, F>(self, f: F) -> CommonProps<N>
    where
        F: Fn(M) -> N + 'static,
    {
        let mapper = FrameSharedFn::new(f);
        self.map_with(&mapper)
    }

    pub(crate) fn map_with<N: 'static>(self, mapper: &FrameSharedFn<M, N>) -> CommonProps<N> {
        CommonProps {
            key: self.key,
            position: self.position,
            size: self.size,
            anchor: self.anchor,
            visible: self.visible,
            opacity: self.opacity,
            transform: self.transform,
            opacity_transition: self.opacity_transition,
            transform_transition: self.transform_transition,
            z_index: self.z_index,
            input_enabled: self.input_enabled,
            cursor: self.cursor,
            handlers: self.handlers.map(mapper),
            a11y: self.a11y,
        }
    }
}

macro_rules! impl_pointer_builders {
    () => {
        pub fn on_pointer_down<F>(mut self, handler: F) -> Self
        where
            F: Fn($crate::input::PointerEvent) -> M + 'static,
        {
            self.common.handlers.set_on_pointer_down(handler);
            self
        }

        pub fn on_pointer_up<F>(mut self, handler: F) -> Self
        where
            F: Fn($crate::input::PointerEvent) -> M + 'static,
        {
            self.common.handlers.set_on_pointer_up(handler);
            self
        }

        pub fn on_pointer_move<F>(mut self, handler: F) -> Self
        where
            F: Fn($crate::input::PointerEvent) -> M + 'static,
        {
            self.common.handlers.set_on_pointer_move(handler);
            self
        }
    };
}

macro_rules! impl_accessible_builders {
    () => {
        pub fn accessible_role(mut self, role: $crate::accessibility::AccessibleRole) -> Self {
            self.common.a11y.role = Some(role);
            self
        }

        pub fn accessible_label(mut self, label: impl Into<$crate::text::SharedStr>) -> Self {
            self.common.a11y.label = Some(label.into());
            self
        }

        pub fn accessible_description(
            mut self,
            description: impl Into<$crate::text::SharedStr>,
        ) -> Self {
            self.common.a11y.description = Some(description.into());
            self
        }

        pub fn accessible_value(mut self, value: impl Into<$crate::text::SharedStr>) -> Self {
            self.common.a11y.value = Some(value.into());
            self
        }

        pub fn accessible_hidden(mut self, hidden: bool) -> Self {
            self.common.a11y.hidden = hidden;
            self
        }
    };
}

macro_rules! impl_common_builders {
    () => {
        pub fn key<K: $crate::input::IntoElementKey>(mut self, key: K) -> Self {
            self.common.key = Some(key.into_element_key());
            self
        }

        pub fn position(mut self, position: $crate::geometry::Position) -> Self {
            self.common.position = position;
            self
        }

        pub fn size(mut self, size: impl Into<$crate::geometry::Size>) -> Self {
            self.common.size = size.into();
            self
        }

        pub fn anchor(mut self, anchor: $crate::geometry::ElementAnchor) -> Self {
            self.common.anchor = anchor;
            self
        }

        pub fn visible(mut self, visible: bool) -> Self {
            self.common.visible = visible;
            self
        }

        pub fn opacity(mut self, opacity: f32) -> Self {
            self.common.opacity = opacity.clamp(0.0, 1.0);
            self
        }

        pub fn transform(mut self, transform: $crate::geometry::Transform) -> Self {
            self.common.transform = transform;
            self
        }

        /// Opt-in retained opacity transition evaluated on `wl_surface.frame` VSync without
        /// rebuilding `view(state)`.
        ///
        /// Newly mounted elements start immediately at their initial declared opacity (no implicit
        /// mount animation). Requires a stable `.key(...)` on the element (checked with
        /// `debug_assert!` in debug builds).
        pub fn transition_opacity(mut self, spec: $crate::animation::TransitionSpec) -> Self {
            self.common.opacity_transition = Some(spec);
            self
        }

        /// Opt-in retained transform transition evaluated on `wl_surface.frame` VSync without
        /// rebuilding `view(state)`.
        ///
        /// Newly mounted elements start immediately at their initial declared transform (no
        /// implicit mount animation). Requires a stable `.key(...)` on the element (checked with
        /// `debug_assert!` in debug builds).
        pub fn transition_transform(mut self, spec: $crate::animation::TransitionSpec) -> Self {
            self.common.transform_transition = Some(spec);
            self
        }

        pub fn z_index(mut self, z_index: i32) -> Self {
            self.common.z_index = z_index;
            self
        }

        pub fn input_enabled(mut self, enabled: bool) -> Self {
            self.common.input_enabled = enabled;
            self
        }

        /// Requests a compositor cursor shape (`wp_cursor_shape_v1` or `CursorShape::Hidden`)
        /// while the pointer hovers over this element.
        pub fn cursor(mut self, shape: $crate::input::CursorShape) -> Self {
            self.common.cursor = Some(shape);
            self
        }

        pub fn on_click(mut self, msg: M) -> Self {
            self.common.handlers.on_click = Some(msg);
            self
        }

        $crate::input::impl_pointer_builders!();
        $crate::input::impl_accessible_builders!();

        pub fn on_hover(mut self, msg: M) -> Self {
            self.common.handlers.on_hover = Some(msg);
            self
        }

        #[cfg(any(
            feature = "pipewire",
            feature = "pulseaudio",
            feature = "alsa",
            feature = "audio-playback"
        ))]
        pub fn on_click_sound(mut self, sound: impl Into<$crate::audio::SoundPlay>) -> Self {
            self.common.handlers.on_click_sound = Some(sound.into());
            self
        }

        #[cfg(any(
            feature = "pipewire",
            feature = "pulseaudio",
            feature = "alsa",
            feature = "audio-playback"
        ))]
        pub fn on_hover_sound(mut self, sound: impl Into<$crate::audio::SoundPlay>) -> Self {
            self.common.handlers.on_hover_sound = Some(sound.into());
            self
        }

        pub fn on_leave(mut self, msg: M) -> Self {
            self.common.handlers.on_leave = Some(msg);
            self
        }

        pub fn on_scroll<F>(mut self, handler: F) -> Self
        where
            F: Fn($crate::input::ScrollEvent) -> M + 'static,
        {
            self.common.handlers.set_on_scroll(handler);
            self
        }

        pub fn on_touch<F>(mut self, handler: F) -> Self
        where
            F: Fn($crate::input::TouchEvent) -> M + 'static,
        {
            self.common.handlers.set_on_touch(handler);
            self
        }

        pub fn on_gesture<F>(mut self, handler: F) -> Self
        where
            F: Fn($crate::input::GestureEvent) -> M + 'static,
        {
            self.common.handlers.set_on_gesture(handler);
            self
        }

        pub fn on_tablet<F>(mut self, handler: F) -> Self
        where
            F: Fn($crate::input::TabletEvent) -> M + 'static,
        {
            self.common.handlers.set_on_tablet(handler);
            self
        }
    };
}

pub(crate) use impl_accessible_builders;
pub(crate) use impl_common_builders;
pub(crate) use impl_pointer_builders;
