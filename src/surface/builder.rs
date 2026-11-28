use crate::accessibility::{AccessibleProps, AccessibleRole};
use crate::arena::{FrameSharedFn, FrameVec};
use crate::display::Output;
use crate::geometry::{Bounds, Edges, Length, LogicalPx, Size};
use crate::input::{
    CursorShape, DndEvent, ElementKey, GestureEvent, ImeConfig, ImeEvent, KeyEvent,
    PointerConstraintMode, PointerEvent, RelativePointerEvent, TabletEvent, TouchEvent,
};
use crate::render::{Color, Element, begin_memo_check, set_memo_surface_hash};
use crate::text::SharedStr;

use super::types::{
    Anchor, BackgroundBlur, BufferRetention, ColorSpace, InputMode, IntoSurfaceKey,
    KeyboardInteractivity, Layer, PopupPositioner, PopupSpec, SurfaceKey, SurfaceKind,
    WindowDecoration, WindowIcon,
};

/// Wayland surface specification returned from `view(state)`.
#[must_use]
#[derive(Clone)]
pub struct Surface<M = ()> {
    pub(crate) key: SurfaceKey,
    pub(crate) kind: SurfaceKind,
    pub(crate) output: Output,
    pub(crate) layer: Layer,
    pub(crate) anchor: Anchor,
    pub(crate) margin: Edges,
    pub(crate) size: Size,
    pub(crate) exclusive_zone: i32,
    pub(crate) keyboard_interactivity: KeyboardInteractivity,
    pub(crate) input_mode: InputMode,
    pub(crate) buffer_retention: Option<BufferRetention>,
    pub(crate) popup: Option<PopupSpec>,
    pub(crate) visible: bool,
    pub(crate) background: Color,
    pub(crate) title: SharedStr,
    pub(crate) namespace: SharedStr,
    pub(crate) cursor: CursorShape,
    pub(crate) decoration: WindowDecoration,
    pub(crate) background_blur: BackgroundBlur,
    pub(crate) window_icon: Option<WindowIcon>,
    pub(crate) color_space: ColorSpace,
    pub(crate) inhibit_idle: bool,
    pub(crate) pointer_constraint: PointerConstraintMode,
    pub(crate) dnd_mime_types: Vec<String>,
    pub(crate) a11y: AccessibleProps,
    pub(crate) children: FrameVec<Element<M>>,
    pub(crate) on_close: Option<M>,
    pub(crate) on_key: Option<FrameSharedFn<KeyEvent, Option<M>>>,
    pub(crate) ime: ImeConfig,
    pub(crate) on_ime: Option<FrameSharedFn<ImeEvent, Option<M>>>,
    pub(crate) on_pointer_move: Option<FrameSharedFn<PointerEvent, Option<M>>>,
    pub(crate) on_pointer_up: Option<FrameSharedFn<PointerEvent, Option<M>>>,
    pub(crate) on_relative_pointer: Option<FrameSharedFn<RelativePointerEvent, Option<M>>>,
    pub(crate) on_dnd: Option<FrameSharedFn<DndEvent, Option<M>>>,
    pub(crate) on_touch: Option<FrameSharedFn<TouchEvent, Option<M>>>,
    pub(crate) on_gesture: Option<FrameSharedFn<GestureEvent, Option<M>>>,
    pub(crate) on_tablet: Option<FrameSharedFn<TabletEvent, Option<M>>>,
    pub(crate) memo_hit: bool,
}

impl<M: 'static> Surface<M> {
    fn new_base(key: SurfaceKey, kind: SurfaceKind, output: Output) -> Self {
        set_memo_surface_hash(key.0);
        let (anchor, size, input_mode, layer, kb) = match kind {
            SurfaceKind::Panel => (
                Anchor::TOP | Anchor::LEFT | Anchor::RIGHT,
                Size::new(Length::Fill, LogicalPx(36.0)),
                InputMode::Full,
                Layer::Top,
                KeyboardInteractivity::None,
            ),
            SurfaceKind::Floating => (
                Anchor::empty(),
                Size::new(LogicalPx(200.0), LogicalPx(64.0)),
                InputMode::Full,
                Layer::Overlay,
                KeyboardInteractivity::None,
            ),
            SurfaceKind::Overlay => (
                Anchor::TOP | Anchor::BOTTOM | Anchor::LEFT | Anchor::RIGHT,
                Size::fill(),
                InputMode::Selective,
                Layer::Overlay,
                KeyboardInteractivity::None,
            ),
            SurfaceKind::Window => (
                Anchor::empty(),
                Size::new(LogicalPx(480.0), LogicalPx(320.0)),
                InputMode::Full,
                Layer::Top,
                KeyboardInteractivity::None,
            ),
            SurfaceKind::Popup => (
                Anchor::empty(),
                Size::new(LogicalPx(200.0), LogicalPx(120.0)),
                InputMode::Full,
                Layer::Overlay,
                KeyboardInteractivity::None,
            ),
            SurfaceKind::SessionLock => (
                Anchor::TOP | Anchor::BOTTOM | Anchor::LEFT | Anchor::RIGHT,
                Size::fill(),
                InputMode::Full,
                Layer::Overlay,
                KeyboardInteractivity::Exclusive,
            ),
        };
        Self {
            key,
            kind,
            output,
            layer,
            anchor,
            margin: Edges::ZERO,
            size,
            exclusive_zone: 0,
            keyboard_interactivity: kb,
            input_mode,
            buffer_retention: None,
            popup: None,
            visible: true,
            background: Color::TRANSPARENT,
            title: SharedStr::EMPTY,
            namespace: SharedStr::EMPTY,
            cursor: CursorShape::Default,
            decoration: WindowDecoration::default(),
            background_blur: BackgroundBlur::None,
            window_icon: None,
            color_space: ColorSpace::Default,
            inhibit_idle: false,
            pointer_constraint: PointerConstraintMode::None,
            dnd_mime_types: Vec::new(),
            a11y: AccessibleProps::default(),
            children: FrameVec::new(),
            on_close: None,
            on_key: None,
            ime: ImeConfig::disabled(),
            on_ime: None,
            on_pointer_move: None,
            on_pointer_up: None,
            on_relative_pointer: None,
            on_dnd: None,
            on_touch: None,
            on_gesture: None,
            on_tablet: None,
            memo_hit: false,
        }
    }

    /// Declares an edge-anchored panel surface (`zwlr_layer_shell_v1`).
    pub fn panel(key: impl IntoSurfaceKey, output: Output) -> Self {
        Self::new_base(key.into_surface_key(), SurfaceKind::Panel, output)
    }

    /// Declares a compact bounded floating layer surface (`zwlr_layer_shell_v1`).
    ///
    /// Recommended default for OSDs, popups, clocks, and corner widgets so buffers allocate only
    /// for the widget bounds instead of the entire monitor.
    pub fn floating(key: impl IntoSurfaceKey, output: Output) -> Self {
        Self::new_base(key.into_surface_key(), SurfaceKind::Floating, output)
    }

    /// Declares a full-output transparent overlay surface (`zwlr_layer_shell_v1`).
    pub fn overlay(key: impl IntoSurfaceKey, output: Output) -> Self {
        Self::new_base(key.into_surface_key(), SurfaceKind::Overlay, output)
    }

    /// Declares an explicit XDG toplevel window (`xdg_wm_base`).
    ///
    /// Intended as an honest fallback when `Capability::LayerShell` is unavailable.
    pub fn window(key: impl IntoSurfaceKey) -> Self {
        Self::new_base(key.into_surface_key(), SurfaceKind::Window, Output::Primary)
    }

    /// Declares a native Wayland session lockscreen surface (`ext_session_lock_v1`).
    ///
    /// Declaring one or more `session_lock` surfaces (typically with [`Output::All`]) acquires
    /// the compositor lock automatically; removing all `session_lock` surfaces from `view(state)`
    /// calls `unlock_and_destroy()`.
    pub fn session_lock(key: impl IntoSurfaceKey, output: Output) -> Self {
        Self::new_base(key.into_surface_key(), SurfaceKind::SessionLock, output)
    }

    /// Declares a compositor-positioned XDG popup (`xdg_popup` + `xdg_positioner`) attached to
    /// `parent_surface` (which may be a layer-shell panel/floating/overlay surface via
    /// `zwlr_layer_surface_v1.get_popup`, an `xdg_toplevel` window, or another `xdg_popup`).
    ///
    /// By default, `popup_grab` is `true` so clicking outside or pressing `Escape` causes the
    /// compositor to send `xdg_popup.popup_done` and trigger [`Self::on_close`].
    pub fn popup(key: impl IntoSurfaceKey, parent_surface: impl IntoSurfaceKey) -> Self {
        let mut surf = Self::new_base(key.into_surface_key(), SurfaceKind::Popup, Output::Primary);
        surf.popup = Some(PopupSpec {
            parent: parent_surface.into_surface_key(),
            positioner: PopupPositioner::default(),
            grab: true,
            desired_size: None,
        });
        surf
    }

    /// Targets a specific output when matching a multi-output parent surface for [`Self::popup`].
    pub fn output(mut self, output: Output) -> Self {
        self.output = output;
        self
    }

    /// Replaces the [`PopupPositioner`] for a [`Self::popup`] surface.
    pub fn positioner(mut self, positioner: PopupPositioner) -> Self {
        if let Some(ref mut spec) = self.popup {
            spec.positioner = positioner;
        }
        self
    }

    /// Controls whether a [`Self::popup`] surface requests an implicit compositor grab
    /// (`xdg_popup.grab`) using the most recent seat input serial (default `true`).
    pub fn popup_grab(mut self, grab: bool) -> Self {
        if let Some(ref mut spec) = self.popup {
            spec.grab = grab;
        }
        self
    }

    /// Keyed surface memoization boundary.
    ///
    /// When `dep` equals the previous frame's value for `key`, `build` is skipped and the
    /// reconciler retains the surface unchanged in $O(1)$.
    pub fn memo<K, D, F>(key: K, dep: &D, build: F) -> Self
    where
        K: IntoSurfaceKey,
        D: PartialEq + Clone + 'static,
        F: FnOnce(&D) -> Surface<M>,
    {
        let surf_key = key.into_surface_key();
        set_memo_surface_hash(surf_key.0);
        // Reserve ElementKey(0) under `surf_key.0` for surface-level memoization.
        if let Some(_build_guard) = begin_memo_check(ElementKey(0), dep) {
            let mut built = build(dep);
            built.key = surf_key;
            built
        } else {
            let mut hit = Self::new_base(surf_key, SurfaceKind::Floating, Output::Primary);
            hit.memo_hit = true;
            hit
        }
    }

    pub fn layer(mut self, layer: Layer) -> Self {
        self.layer = layer;
        self
    }

    pub fn anchor(mut self, anchor: Anchor) -> Self {
        self.anchor = anchor;
        self
    }

    pub fn margin(mut self, margin: Edges) -> Self {
        self.margin = margin;
        self
    }

    pub fn size(mut self, size: impl Into<Size>) -> Self {
        self.size = size.into();
        self
    }

    pub fn exclusive_zone(mut self, zone: LogicalPx) -> Self {
        self.exclusive_zone = zone.round_i32();
        self
    }

    /// Requests `-1` exclusive zone (overlay ignores other surfaces' exclusive zones).
    pub fn ignore_exclusive_zone(mut self) -> Self {
        self.exclusive_zone = -1;
        self
    }

    pub fn keyboard_interactivity(mut self, policy: KeyboardInteractivity) -> Self {
        self.keyboard_interactivity = policy;
        self
    }

    pub fn input_mode(mut self, mode: InputMode) -> Self {
        self.input_mode = mode;
        self
    }

    /// Configures how client-side `wl_shm` buffers are retained after compositor presentation.
    ///
    /// Use [`BufferRetention::ReleaseAfterCommit`] for static wallpapers or large rarely-updated
    /// surfaces so client SHM memory drops back to zero bytes once the compositor releases the
    /// committed buffer.
    pub fn buffer_retention(mut self, retention: BufferRetention) -> Self {
        self.buffer_retention = Some(retention);
        self
    }

    pub fn visible(mut self, visible: bool) -> Self {
        self.visible = visible;
        self
    }

    pub fn background(mut self, color: Color) -> Self {
        self.background = color;
        self
    }

    pub fn title(mut self, title: impl Into<SharedStr>) -> Self {
        self.title = title.into();
        self
    }

    pub fn namespace(mut self, namespace: impl Into<SharedStr>) -> Self {
        self.namespace = namespace.into();
        self
    }

    /// Sets the fallback cursor shape (`wp_cursor_shape_v1` or [`CursorShape::Hidden`]) for this
    /// surface when the pointer is not hovering an element with its own `.cursor(...)`.
    pub fn cursor(mut self, shape: CursorShape) -> Self {
        self.cursor = shape;
        self
    }

    /// Configures `zxdg_decoration_manager_v1` decoration mode for [`Self::window`] surfaces.
    pub fn decoration(mut self, decoration: WindowDecoration) -> Self {
        self.decoration = decoration;
        self
    }

    /// Requests compositor background blur (`ext_background_effect_v1`) covering the full surface
    /// when `enabled` is `true`, or clears blur when `false`.
    pub fn background_blur(mut self, enabled: bool) -> Self {
        self.background_blur = if enabled {
            BackgroundBlur::Full
        } else {
            BackgroundBlur::None
        };
        self
    }

    /// Requests compositor background blur (`ext_background_effect_v1`) for an explicit logical
    /// region (or clears blur when `None`).
    pub fn background_blur_region(mut self, region: Option<Bounds>) -> Self {
        self.background_blur = match region {
            Some(r) => BackgroundBlur::Region(r),
            None => BackgroundBlur::None,
        };
        self
    }

    /// Sets a native toplevel window icon (`xdg_toplevel_icon_v1`) for [`Self::window`] surfaces.
    pub fn window_icon(mut self, icon: WindowIcon) -> Self {
        self.window_icon = Some(icon);
        self
    }

    /// Convenience helper setting a toplevel window icon by XDG icon theme name.
    pub fn window_icon_name(mut self, name: impl Into<SharedStr>) -> Self {
        self.window_icon = Some(WindowIcon::Name(name.into()));
        self
    }

    /// Clears any explicit toplevel window icon.
    pub fn clear_window_icon(mut self) -> Self {
        self.window_icon = None;
        self
    }

    /// Requests a Wayland color-management image description (`wp_color_management_v1`) for this
    /// surface (e.g. `DisplayP3`, `Rec2020Pq`, `Srgb`, or `LinearSrgb`).
    pub fn color_space(mut self, space: ColorSpace) -> Self {
        self.color_space = space;
        self
    }

    /// Requests per-surface idle inhibition (`zwp_idle_inhibit_manager_v1`) while this surface is
    /// mapped and visible.
    pub fn inhibit_idle(mut self, inhibit: bool) -> Self {
        self.inhibit_idle = inhibit;
        self
    }

    /// Requests pointer locking or confinement (`zwp_pointer_constraints_v1`) on this surface.
    pub fn pointer_constraint(mut self, mode: PointerConstraintMode) -> Self {
        self.pointer_constraint = mode;
        self
    }

    /// Configures preferred MIME types accepted by this surface during Drag & Drop
    /// (`wl_data_device`), in priority order (e.g. `["text/uri-list", "text/plain;charset=utf-8"]`).
    pub fn dnd_mime_types<I, S>(mut self, mimes: I) -> Self
    where
        I: IntoIterator<Item = S>,
        S: Into<String>,
    {
        self.dnd_mime_types = mimes.into_iter().map(Into::into).collect();
        self
    }

    pub fn accessible_role(mut self, role: AccessibleRole) -> Self {
        self.a11y.role = Some(role);
        self
    }

    pub fn accessible_label(mut self, label: impl Into<SharedStr>) -> Self {
        self.a11y.label = Some(label.into());
        self
    }

    pub fn accessible_description(mut self, description: impl Into<SharedStr>) -> Self {
        self.a11y.description = Some(description.into());
        self
    }

    pub fn accessible_hidden(mut self, hidden: bool) -> Self {
        self.a11y.hidden = hidden;
        self
    }

    pub fn child(mut self, child: impl Into<Element<M>>) -> Self {
        self.children.push(child.into());
        self
    }

    pub fn children<I, E>(mut self, children: I) -> Self
    where
        I: IntoIterator<Item = E>,
        E: Into<Element<M>>,
    {
        for child in children {
            self.children.push(child.into());
        }
        self
    }

    pub fn on_close(mut self, msg: M) -> Self {
        self.on_close = Some(msg);
        self
    }

    pub fn on_key<F>(mut self, handler: F) -> Self
    where
        F: Fn(KeyEvent) -> Option<M> + 'static,
    {
        self.on_key = Some(FrameSharedFn::new(handler));
        self
    }

    /// Configures Wayland IME (`zwp_text_input_v3`) state and hints for this surface.
    pub fn ime(mut self, config: ImeConfig) -> Self {
        self.ime = config;
        self
    }

    /// Subscribes to Wayland IME events (`Enter`, `Leave`, atomic `Update`) for this surface.
    pub fn on_ime<F>(mut self, handler: F) -> Self
    where
        F: Fn(ImeEvent) -> Option<M> + 'static,
    {
        self.on_ime = Some(FrameSharedFn::new(handler));
        self
    }

    pub fn on_pointer_move<F>(mut self, handler: F) -> Self
    where
        F: Fn(PointerEvent) -> Option<M> + 'static,
    {
        self.on_pointer_move = Some(FrameSharedFn::new(handler));
        self
    }

    pub fn on_pointer_up<F>(mut self, handler: F) -> Self
    where
        F: Fn(PointerEvent) -> Option<M> + 'static,
    {
        self.on_pointer_up = Some(FrameSharedFn::new(handler));
        self
    }

    /// Subscribes to unaccelerated relative pointer deltas (`zwp_relative_pointer_v1`) while
    /// pointer focus is on this surface.
    pub fn on_relative_pointer<F>(mut self, handler: F) -> Self
    where
        F: Fn(RelativePointerEvent) -> Option<M> + 'static,
    {
        self.on_relative_pointer = Some(FrameSharedFn::new(handler));
        self
    }

    /// Subscribes to Wayland Drag & Drop events (`wl_data_device`: `Enter`, `Motion`, `Leave`,
    /// `Drop`, `DropError`) targeting this surface.
    pub fn on_dnd<F>(mut self, handler: F) -> Self
    where
        F: Fn(DndEvent) -> Option<M> + 'static,
    {
        self.on_dnd = Some(FrameSharedFn::new(handler));
        self
    }

    /// Subscribes to surface-level touchscreen events (`Down`, `Move`, `Up`, `Cancel`).
    pub fn on_touch<F>(mut self, handler: F) -> Self
    where
        F: Fn(TouchEvent) -> Option<M> + 'static,
    {
        self.on_touch = Some(FrameSharedFn::new(handler));
        self
    }

    /// Subscribes to multi-finger touchpad gestures (`zwp_pointer_gestures_v1`: swipe, pinch, hold).
    pub fn on_gesture<F>(mut self, handler: F) -> Self
    where
        F: Fn(GestureEvent) -> Option<M> + 'static,
    {
        self.on_gesture = Some(FrameSharedFn::new(handler));
        self
    }

    /// Subscribes to graphics tablet stylus and express-key pad events (`zwp_tablet_manager_v2`).
    pub fn on_tablet<F>(mut self, handler: F) -> Self
    where
        F: Fn(TabletEvent) -> Option<M> + 'static,
    {
        self.on_tablet = Some(FrameSharedFn::new(handler));
        self
    }

    /// Maps emitted surface and child messages into a parent message type `N`.
    pub fn map<N: 'static, F>(self, f: F) -> Surface<N>
    where
        F: Fn(M) -> N + 'static,
    {
        fn map_opt<E: 'static, M: 'static, N: 'static>(
            opt: Option<FrameSharedFn<E, Option<M>>>,
            mapper: &FrameSharedFn<M, N>,
        ) -> Option<FrameSharedFn<E, Option<N>>> {
            opt.map(|prev| {
                let m = mapper.clone();
                FrameSharedFn::new(move |ev| prev.call(ev).map(|msg| m.call(msg)))
            })
        }

        let mapper = FrameSharedFn::new(f);
        let mut mapped_children = FrameVec::with_capacity(self.children.len());
        for child in self.children {
            mapped_children.push(child.map_with(&mapper));
        }
        let on_key = map_opt(self.on_key, &mapper);
        let on_ime = map_opt(self.on_ime, &mapper);
        let on_pointer_move = map_opt(self.on_pointer_move, &mapper);
        let on_pointer_up = map_opt(self.on_pointer_up, &mapper);
        let on_relative_pointer = map_opt(self.on_relative_pointer, &mapper);
        let on_dnd = map_opt(self.on_dnd, &mapper);
        let on_touch = map_opt(self.on_touch, &mapper);
        let on_gesture = map_opt(self.on_gesture, &mapper);
        let on_tablet = map_opt(self.on_tablet, &mapper);
        Surface {
            key: self.key,
            kind: self.kind,
            output: self.output,
            layer: self.layer,
            anchor: self.anchor,
            margin: self.margin,
            size: self.size,
            exclusive_zone: self.exclusive_zone,
            keyboard_interactivity: self.keyboard_interactivity,
            input_mode: self.input_mode,
            buffer_retention: self.buffer_retention,
            popup: self.popup,
            visible: self.visible,
            background: self.background,
            title: self.title,
            namespace: self.namespace,
            cursor: self.cursor,
            decoration: self.decoration,
            background_blur: self.background_blur,
            window_icon: self.window_icon,
            color_space: self.color_space,
            inhibit_idle: self.inhibit_idle,
            pointer_constraint: self.pointer_constraint,
            dnd_mime_types: self.dnd_mime_types,
            a11y: self.a11y,
            children: mapped_children,
            on_close: self.on_close.map(|m| mapper.call(m)),
            on_key,
            ime: self.ime,
            on_ime,
            on_pointer_move,
            on_pointer_up,
            on_relative_pointer,
            on_dnd,
            on_touch,
            on_gesture,
            on_tablet,
            memo_hit: self.memo_hit,
        }
    }
}
