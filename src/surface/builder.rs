use crate::accessibility::{AccessibleProps, AccessibleRole};
use crate::arena::{FrameSharedFn, FrameVec};
use crate::display::Output;
use crate::geometry::{Edges, Length, LogicalPx, Size};
use crate::input::{ElementKey, ImeConfig, ImeEvent, KeyEvent, PointerEvent};
use crate::render::{Color, Element, check_memo, set_memo_surface_hash};
use crate::text::SharedStr;

use super::types::{
    Anchor, BufferRetention, InputMode, IntoSurfaceKey, KeyboardInteractivity, Layer, SurfaceKey,
    SurfaceKind,
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
    pub(crate) buffer_retention: BufferRetention,
    pub(crate) visible: bool,
    pub(crate) background: Color,
    pub(crate) title: SharedStr,
    pub(crate) namespace: SharedStr,
    pub(crate) a11y: AccessibleProps,
    pub(crate) children: FrameVec<Element<M>>,
    pub(crate) on_close: Option<M>,
    pub(crate) on_key: Option<FrameSharedFn<KeyEvent, Option<M>>>,
    pub(crate) ime: ImeConfig,
    pub(crate) on_ime: Option<FrameSharedFn<ImeEvent, Option<M>>>,
    pub(crate) on_pointer_move: Option<FrameSharedFn<PointerEvent, Option<M>>>,
    pub(crate) on_pointer_up: Option<FrameSharedFn<PointerEvent, Option<M>>>,
    pub(crate) memo_hit: bool,
}

impl<M: 'static> Surface<M> {
    fn new_base(key: SurfaceKey, kind: SurfaceKind, output: Output) -> Self {
        set_memo_surface_hash(key.0);
        let (anchor, size, input_mode, layer) = match kind {
            SurfaceKind::Panel => (
                Anchor::Top | Anchor::Left | Anchor::Right,
                Size::new(Length::Fill, LogicalPx(36.0)),
                InputMode::Full,
                Layer::Top,
            ),
            SurfaceKind::Floating => (
                Anchor::empty(),
                Size::px(LogicalPx(200.0), LogicalPx(64.0)),
                InputMode::Full,
                Layer::Overlay,
            ),
            SurfaceKind::Overlay => (
                Anchor::Top | Anchor::Bottom | Anchor::Left | Anchor::Right,
                Size::fill(),
                InputMode::Selective,
                Layer::Overlay,
            ),
            SurfaceKind::Window => (
                Anchor::empty(),
                Size::px(LogicalPx(480.0), LogicalPx(320.0)),
                InputMode::Full,
                Layer::Top,
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
            keyboard_interactivity: KeyboardInteractivity::None,
            input_mode,
            buffer_retention: BufferRetention::Retain,
            visible: true,
            background: Color::TRANSPARENT,
            title: SharedStr::from_static("waywi"),
            namespace: SharedStr::from_static("waywi"),
            a11y: AccessibleProps::default(),
            children: FrameVec::new(),
            on_close: None,
            on_key: None,
            ime: ImeConfig::disabled(),
            on_ime: None,
            on_pointer_move: None,
            on_pointer_up: None,
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
        if check_memo(ElementKey(0), dep) {
            let mut hit = Self::new_base(surf_key, SurfaceKind::Floating, Output::Primary);
            hit.memo_hit = true;
            hit
        } else {
            let mut built = build(dep);
            built.key = surf_key;
            built
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
        self.buffer_retention = retention;
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
            visible: self.visible,
            background: self.background,
            title: self.title,
            namespace: self.namespace,
            a11y: self.a11y,
            children: mapped_children,
            on_close: self.on_close.map(|m| mapper.call(m)),
            on_key,
            ime: self.ime,
            on_ime,
            on_pointer_move,
            on_pointer_up,
            memo_hit: self.memo_hit,
        }
    }
}
