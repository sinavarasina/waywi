use std::time::Instant;

use crate::accessibility::AccessibleProps;
use crate::arena::FrameSharedFn;
use crate::display::OutputId;
use crate::geometry::{
    BufferRect, Edges, LogicalPx, LogicalSize, Point, Rect as GeoRect, ScaleFactor,
};
use crate::input::{
    HitTestNode, ImeConfig, ImeEvent, KeyEvent, Modifiers, MouseButton, NodeId, PointerEvent,
    ScrollEvent, hit_test, hit_test_scroll,
};
use crate::render::{Color, DrawCommand, RetainedNode};
use crate::surface::types::PopupSpec;
use crate::surface::{
    Anchor, BufferRetention, InputMode, KeyboardInteractivity, Layer, SurfaceKey, SurfaceKind,
};
use crate::text::SharedStr;

/// Identity of a live retained Wayland surface instance.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub(crate) struct RetainedSurfaceKey {
    pub(crate) surface_key: SurfaceKey,
    pub(crate) output_id: Option<OutputId>,
}

/// Desired compositor surface configuration derived from `Surface<M>`.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct SurfaceSpec {
    pub(crate) kind: SurfaceKind,
    pub(crate) output_id: Option<OutputId>,
    pub(crate) layer: Layer,
    pub(crate) anchor: Anchor,
    pub(crate) margin: Edges,
    pub(crate) logical_size: LogicalSize,
    pub(crate) exclusive_zone: i32,
    pub(crate) keyboard_interactivity: KeyboardInteractivity,
    pub(crate) input_mode: InputMode,
    pub(crate) buffer_retention: BufferRetention,
    pub(crate) popup: Option<PopupSpec>,
    pub(crate) background: Color,
    pub(crate) title: SharedStr,
    pub(crate) namespace: SharedStr,
    pub(crate) a11y: AccessibleProps,
    pub(crate) ime: ImeConfig,
}

/// Retained state for one live visible surface.
pub(crate) struct RetainedSurface<M> {
    pub(crate) spec: SurfaceSpec,
    pub(crate) scale: ScaleFactor,
    pub(crate) roots: Vec<RetainedNode<M>>,
    pub(crate) commands: Vec<DrawCommand>,
    pub(crate) hit_nodes: Vec<HitTestNode<M>>,
    pub(crate) hovered_node: Option<NodeId>,
    pub(crate) on_close: Option<M>,
    pub(crate) on_key: Option<FrameSharedFn<KeyEvent, Option<M>>>,
    pub(crate) on_ime: Option<FrameSharedFn<ImeEvent, Option<M>>>,
    pub(crate) on_pointer_move: Option<FrameSharedFn<PointerEvent, Option<M>>>,
    pub(crate) on_pointer_up: Option<FrameSharedFn<PointerEvent, Option<M>>>,
    pub(crate) dirty_logical_rects: Vec<GeoRect>,
    pub(crate) needs_redraw: bool,
    pub(crate) full_redraw: bool,
    pub(crate) input_region_dirty: bool,
    pub(crate) selective_input_rects: Vec<GeoRect>,
    pub(crate) next_animation_deadline: Option<Instant>,
}

impl<M: 'static> RetainedSurface<M> {
    pub(crate) fn effective_logical_size(&self) -> LogicalSize {
        self.spec.logical_size
    }

    /// Dispatches a logical click/tap at `pos` against hit-tested nodes cleanly.
    pub(crate) fn dispatch_click_at(&self, pos: Point) -> PointerDispatch<M>
    where
        M: Clone,
    {
        let Some((node, _)) = hit_test(&self.hit_nodes, pos) else {
            return PointerDispatch::default();
        };
        PointerDispatch {
            on_click: node.handlers.on_click.clone(),
            on_pointer: None,
            on_surface: None,
            #[cfg(any(
                feature = "pipewire",
                feature = "pulseaudio",
                feature = "alsa",
                feature = "audio-playback"
            ))]
            click_sound: node.handlers.on_click_sound,
        }
    }

    /// Dispatches a pointer button press at `pos` against hit-tested nodes.
    pub(crate) fn dispatch_pointer_down_at(
        &self,
        pos: Point,
        button: MouseButton,
        modifiers: Modifiers,
    ) -> PointerDispatch<M>
    where
        M: Clone,
    {
        let Some((node, local)) = hit_test(&self.hit_nodes, pos) else {
            return PointerDispatch::default();
        };
        let on_pointer = node.handlers.on_pointer_down.as_ref().map(|h| {
            h.call(PointerEvent {
                position: pos,
                local_position: local,
                button: Some(button),
                modifiers,
            })
        });
        PointerDispatch {
            on_click: node.handlers.on_click.clone(),
            on_pointer,
            on_surface: None,
            #[cfg(any(
                feature = "pipewire",
                feature = "pulseaudio",
                feature = "alsa",
                feature = "audio-playback"
            ))]
            click_sound: node.handlers.on_click_sound,
        }
    }

    /// Dispatches a pointer button release at `pos` against hit-tested nodes and surface fallback.
    pub(crate) fn dispatch_pointer_up_at(
        &self,
        pos: Point,
        button: MouseButton,
        modifiers: Modifiers,
    ) -> PointerDispatch<M> {
        let on_pointer = hit_test(&self.hit_nodes, pos).and_then(|(node, local)| {
            node.handlers.on_pointer_up.as_ref().map(|h| {
                h.call(PointerEvent {
                    position: pos,
                    local_position: local,
                    button: Some(button),
                    modifiers,
                })
            })
        });
        let on_surface = self.on_pointer_up.as_ref().and_then(|h| {
            h.call(PointerEvent {
                position: pos,
                local_position: pos,
                button: Some(button),
                modifiers,
            })
        });
        PointerDispatch {
            on_click: None,
            on_pointer,
            on_surface,
            #[cfg(any(
                feature = "pipewire",
                feature = "pulseaudio",
                feature = "alsa",
                feature = "audio-playback"
            ))]
            click_sound: None,
        }
    }

    /// Dispatches a pointer scroll event against the topmost matching `on_scroll` handler.
    pub(crate) fn dispatch_scroll_at(&self, mut event: ScrollEvent) -> Option<M> {
        let (node, local) = hit_test_scroll(&self.hit_nodes, event.position)?;
        let on_scroll = node.handlers.on_scroll.as_ref()?;
        event.local_position = local;
        Some(on_scroll.call(event))
    }

    /// Updates hover state at `pos` and optionally evaluates node/surface `on_pointer_move`.
    pub(crate) fn dispatch_pointer_move_at(
        &mut self,
        pos: Point,
        modifiers: Modifiers,
        emit_move: bool,
    ) -> PointerMoveDispatch<M>
    where
        M: Clone,
    {
        let hit = hit_test(&self.hit_nodes, pos);
        let hovered_id = hit.map(|(n, _)| n.node_id);
        let on_node_move = if emit_move {
            hit.and_then(|(node, local)| {
                node.handlers.on_pointer_move.as_ref().map(|h| {
                    h.call(PointerEvent {
                        position: pos,
                        local_position: local,
                        button: None,
                        modifiers,
                    })
                })
            })
        } else {
            None
        };
        let on_surface_move = if emit_move {
            self.on_pointer_move.as_ref().and_then(|h| {
                h.call(PointerEvent {
                    position: pos,
                    local_position: pos,
                    button: None,
                    modifiers,
                })
            })
        } else {
            None
        };
        let hover = self.update_hover(hovered_id);
        PointerMoveDispatch {
            hover,
            on_node_move,
            on_surface_move,
        }
    }

    /// Updates `hovered_node` to `new_id` and returns any leave/hover messages and sound to emit.
    pub(crate) fn update_hover(&mut self, new_id: Option<NodeId>) -> Option<HoverTransition<M>>
    where
        M: Clone,
    {
        let prev_id = self.hovered_node;
        if new_id == prev_id {
            return None;
        }
        let on_leave = prev_id.and_then(|old| {
            self.hit_nodes
                .iter()
                .find(|n| n.node_id == old)
                .and_then(|n| n.handlers.on_leave.clone())
        });
        let hovered = new_id.and_then(|nid| self.hit_nodes.iter().find(|n| n.node_id == nid));
        let on_hover = hovered.and_then(|n| n.handlers.on_hover.clone());
        #[cfg(any(
            feature = "pipewire",
            feature = "pulseaudio",
            feature = "alsa",
            feature = "audio-playback"
        ))]
        let on_hover_sound = hovered.and_then(|n| n.handlers.on_hover_sound);
        self.hovered_node = new_id;
        Some(HoverTransition {
            on_leave,
            on_hover,
            #[cfg(any(
                feature = "pipewire",
                feature = "pulseaudio",
                feature = "alsa",
                feature = "audio-playback"
            ))]
            on_hover_sound,
        })
    }

    pub(crate) fn dirty_buffer_rects(&self) -> Vec<BufferRect> {
        let eff = self.effective_logical_size();
        let (max_w, max_h) = eff.to_buffer_ceil(self.scale);
        if self.full_redraw {
            return vec![BufferRect {
                x: 0,
                y: 0,
                width: max_w.0,
                height: max_h.0,
            }];
        }
        self.dirty_logical_rects
            .iter()
            .filter_map(|r| {
                let br = r
                    .outset(LogicalPx(1.0))
                    .to_buffer_rect(self.scale, max_w, max_h);
                if br.is_empty() { None } else { Some(br) }
            })
            .collect()
    }
}

/// Output of [`RetainedSurface::update_hover`] when the hovered node changes.
pub(crate) struct HoverTransition<M> {
    pub(crate) on_leave: Option<M>,
    pub(crate) on_hover: Option<M>,
    #[cfg(any(
        feature = "pipewire",
        feature = "pulseaudio",
        feature = "alsa",
        feature = "audio-playback"
    ))]
    pub(crate) on_hover_sound: Option<(crate::audio::SoundAsset, f32)>,
}

/// Output of pointer click, press, or release dispatch on a [`RetainedSurface`].
pub(crate) struct PointerDispatch<M> {
    pub(crate) on_click: Option<M>,
    pub(crate) on_pointer: Option<M>,
    pub(crate) on_surface: Option<M>,
    #[cfg(any(
        feature = "pipewire",
        feature = "pulseaudio",
        feature = "alsa",
        feature = "audio-playback"
    ))]
    pub(crate) click_sound: Option<(crate::audio::SoundAsset, f32)>,
}

impl<M> Default for PointerDispatch<M> {
    fn default() -> Self {
        Self {
            on_click: None,
            on_pointer: None,
            on_surface: None,
            #[cfg(any(
                feature = "pipewire",
                feature = "pulseaudio",
                feature = "alsa",
                feature = "audio-playback"
            ))]
            click_sound: None,
        }
    }
}

/// Output of pointer motion + hover dispatch on a [`RetainedSurface`].
pub(crate) struct PointerMoveDispatch<M> {
    pub(crate) hover: Option<HoverTransition<M>>,
    pub(crate) on_node_move: Option<M>,
    pub(crate) on_surface_move: Option<M>,
}

/// Output of one reconciliation pass across all declared surfaces.
#[derive(Debug, Default)]
pub(crate) struct ReconcileReport {
    pub(crate) removed_surfaces: Vec<RetainedSurfaceKey>,
    pub(crate) created_surfaces: Vec<RetainedSurfaceKey>,
    pub(crate) updated_surfaces: Vec<RetainedSurfaceKey>,
    pub(crate) skipped_memo_surfaces: usize,
    pub(crate) skipped_memo_elements: usize,
    pub(crate) retired_node_ids: Vec<NodeId>,
    pub(crate) next_animation_deadline: Option<Instant>,
}
