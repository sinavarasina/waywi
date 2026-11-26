use std::time::Instant;

use crate::accessibility::AccessibleProps;
use crate::arena::FrameSharedFn;
use crate::display::OutputId;
use crate::geometry::{Bounds, BufferRect, Edges, LogicalPx, LogicalSize, Point, ScaleFactor};
use crate::input::{
    CursorShape, DndEvent, GestureEvent, HitTestNode, ImeConfig, ImeEvent, KeyEvent, Modifiers,
    MouseButton, NodeId, PointerConstraintMode, PointerEvent, RelativePointerEvent, ScrollEvent,
    TabletEvent, TabletToolEventKind, TabletToolId, TouchEvent, TouchPhase, hit_test,
    hit_test_cursor, hit_test_gesture, hit_test_pointer_and_cursor, hit_test_scroll,
    hit_test_tablet, hit_test_touch,
};
use crate::render::{Color, DrawCommand, RetainedNode};
use crate::surface::types::PopupSpec;
use crate::surface::{
    Anchor, BufferRetention, InputMode, KeyboardInteractivity, Layer, SurfaceKey, SurfaceKind,
    WindowDecoration,
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
    pub(crate) cursor: CursorShape,
    pub(crate) decoration: WindowDecoration,
    pub(crate) inhibit_idle: bool,
    pub(crate) pointer_constraint: PointerConstraintMode,
    pub(crate) accepts_dnd: bool,
    pub(crate) dnd_mime_types: Vec<String>,
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
    pub(crate) has_custom_cursors: bool,
    pub(crate) hovered_node: Option<NodeId>,
    pub(crate) on_close: Option<M>,
    pub(crate) on_key: Option<FrameSharedFn<KeyEvent, Option<M>>>,
    pub(crate) on_ime: Option<FrameSharedFn<ImeEvent, Option<M>>>,
    pub(crate) on_pointer_move: Option<FrameSharedFn<PointerEvent, Option<M>>>,
    pub(crate) on_pointer_up: Option<FrameSharedFn<PointerEvent, Option<M>>>,
    pub(crate) on_relative_pointer: Option<FrameSharedFn<RelativePointerEvent, Option<M>>>,
    pub(crate) on_dnd: Option<FrameSharedFn<DndEvent, Option<M>>>,
    pub(crate) on_touch: Option<FrameSharedFn<TouchEvent, Option<M>>>,
    pub(crate) on_gesture: Option<FrameSharedFn<GestureEvent, Option<M>>>,
    pub(crate) on_tablet: Option<FrameSharedFn<TabletEvent, Option<M>>>,
    /// Per-contact touch capture (`touch_id -> NodeId`) preserving delivery across drags.
    pub(crate) touch_captures: Vec<(i32, NodeId)>,
    /// Per-tool tip-down capture (`tool_id -> NodeId`) preserving stylus strokes across drags.
    pub(crate) tablet_captures: Vec<(TabletToolId, NodeId)>,
    pub(crate) dirty_logical_rects: Vec<Bounds>,
    pub(crate) needs_redraw: bool,
    pub(crate) full_redraw: bool,
    pub(crate) input_region_dirty: bool,
    pub(crate) selective_input_rects: Vec<Bounds>,
    pub(crate) next_animation_deadline: Option<Instant>,
    pub(crate) has_active_transitions: bool,
}

impl<M: 'static> RetainedSurface<M> {
    pub(crate) fn effective_logical_size(&self) -> LogicalSize {
        self.spec.logical_size
    }

    /// Resolves the effective [`CursorShape`] at surface-local `pos`, checking the topmost
    /// visible element with `.cursor(...)` before falling back to the surface cursor.
    ///
    /// Runs in $O(1)$ when no retained element on this surface declares a custom cursor.
    #[must_use]
    pub(crate) fn cursor_at(&self, pos: Point) -> CursorShape {
        if !self.has_custom_cursors {
            return self.spec.cursor;
        }
        hit_test_cursor(&self.hit_nodes, pos).unwrap_or(self.spec.cursor)
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

    /// Dispatches a multi-touch contact event with per-contact capture to matching element and
    /// surface `on_touch` handlers.
    pub(crate) fn dispatch_touch(&mut self, mut event: TouchEvent) -> TouchDispatch<M> {
        let target_node_id = match event.phase {
            TouchPhase::Down => {
                self.touch_captures.retain(|(id, _)| *id != event.id);
                if let Some((node, local)) = hit_test_touch(&self.hit_nodes, event.position) {
                    let nid = node.node_id;
                    self.touch_captures.push((event.id, nid));
                    event.local_position = local;
                    Some(nid)
                } else {
                    None
                }
            }
            TouchPhase::Move => self
                .touch_captures
                .iter()
                .find(|(id, _)| *id == event.id)
                .map(|(_, nid)| *nid),
            TouchPhase::Up | TouchPhase::Cancel => self
                .touch_captures
                .iter()
                .position(|(id, _)| *id == event.id)
                .map(|idx| self.touch_captures.swap_remove(idx).1),
        };

        let on_node = target_node_id.and_then(|nid| {
            let node = self.hit_nodes.iter().find(|n| n.node_id == nid)?;
            let handler = node.handlers.on_touch.as_ref()?;
            let mut node_ev = event;
            if event.phase != TouchPhase::Down {
                node_ev.local_position = node.surface_point_to_local(event.position);
            }
            Some(handler.call(node_ev))
        });

        let on_surface = self.on_touch.as_ref().and_then(|h| {
            let mut surf_ev = event;
            surf_ev.local_position = event.position;
            h.call(surf_ev)
        });

        TouchDispatch {
            handled_by_touch: target_node_id.is_some() || on_surface.is_some(),
            on_node,
            on_surface,
        }
    }

    /// Dispatches a multi-finger touchpad gesture to hit-tested element and surface `on_gesture`
    /// handlers.
    pub(crate) fn dispatch_gesture(&self, event: GestureEvent) -> GestureDispatch<M> {
        let on_node =
            hit_test_gesture(&self.hit_nodes, event.position).and_then(|(node, local)| {
                let handler = node.handlers.on_gesture.as_ref()?;
                let mut node_ev = event;
                node_ev.local_position = local;
                Some(handler.call(node_ev))
            });
        let on_surface = self.on_gesture.as_ref().and_then(|h| {
            let mut surf_ev = event;
            surf_ev.local_position = event.position;
            h.call(surf_ev)
        });
        GestureDispatch {
            on_node,
            on_surface,
        }
    }

    /// Dispatches a pen tablet tool or pad event with per-tool tip-down capture to matching
    /// element and surface `on_tablet` handlers.
    pub(crate) fn dispatch_tablet(&mut self, event: TabletEvent) -> TabletDispatch<M> {
        match event {
            TabletEvent::Pad(_) => {
                let on_surface = self.on_tablet.as_ref().and_then(|h| h.call(event));
                TabletDispatch {
                    handled_by_tablet: on_surface.is_some(),
                    on_node: None,
                    on_surface,
                }
            }
            TabletEvent::Tool(tool_ev) => {
                let captured_id = match tool_ev.kind {
                    TabletToolEventKind::Down => {
                        self.tablet_captures
                            .retain(|(id, _)| *id != tool_ev.tool_id);
                        if let Some((node, _)) = hit_test_tablet(&self.hit_nodes, tool_ev.position)
                        {
                            self.tablet_captures.push((tool_ev.tool_id, node.node_id));
                            Some(node.node_id)
                        } else {
                            None
                        }
                    }
                    TabletToolEventKind::Up | TabletToolEventKind::ProximityOut => self
                        .tablet_captures
                        .iter()
                        .position(|(id, _)| *id == tool_ev.tool_id)
                        .map(|idx| self.tablet_captures.swap_remove(idx).1),
                    _ => self
                        .tablet_captures
                        .iter()
                        .find(|(id, _)| *id == tool_ev.tool_id)
                        .map(|(_, nid)| *nid),
                };

                let target = if let Some(nid) = captured_id {
                    self.hit_nodes
                        .iter()
                        .find(|n| n.node_id == nid)
                        .map(|n| (n, n.surface_point_to_local(tool_ev.position)))
                } else {
                    hit_test_tablet(&self.hit_nodes, tool_ev.position)
                };

                let has_node = target.is_some();
                let on_node = target.and_then(|(node, local)| {
                    let handler = node.handlers.on_tablet.as_ref()?;
                    let mut node_ev = tool_ev;
                    node_ev.local_position = local;
                    Some(handler.call(TabletEvent::Tool(node_ev)))
                });

                let on_surface = self.on_tablet.as_ref().and_then(|h| {
                    let mut surf_ev = tool_ev;
                    surf_ev.local_position = tool_ev.position;
                    h.call(TabletEvent::Tool(surf_ev))
                });

                TabletDispatch {
                    handled_by_tablet: has_node || on_surface.is_some(),
                    on_node,
                    on_surface,
                }
            }
        }
    }

    /// Updates hover state at `pos`, resolves the active cursor shape in the same hit-test pass,
    /// and optionally evaluates node/surface `on_pointer_move`.
    pub(crate) fn dispatch_pointer_move_at(
        &mut self,
        pos: Point,
        modifiers: Modifiers,
        emit_move: bool,
    ) -> PointerMoveDispatch<M>
    where
        M: Clone,
    {
        let (hit, cursor_override) = if self.has_custom_cursors {
            hit_test_pointer_and_cursor(&self.hit_nodes, pos)
        } else {
            (hit_test(&self.hit_nodes, pos), None)
        };
        let cursor = cursor_override.unwrap_or(self.spec.cursor);
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
            cursor,
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
    pub(crate) on_hover_sound: Option<crate::audio::SoundPlay>,
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
    pub(crate) click_sound: Option<crate::audio::SoundPlay>,
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
    pub(crate) cursor: CursorShape,
}

/// Output of touchscreen contact dispatch on a [`RetainedSurface`].
pub(crate) struct TouchDispatch<M> {
    /// `true` when an element or surface `on_touch` handler claimed this contact.
    pub(crate) handled_by_touch: bool,
    pub(crate) on_node: Option<M>,
    pub(crate) on_surface: Option<M>,
}

/// Output of touchpad multi-finger gesture dispatch on a [`RetainedSurface`].
pub(crate) struct GestureDispatch<M> {
    pub(crate) on_node: Option<M>,
    pub(crate) on_surface: Option<M>,
}

/// Output of graphics tablet tool/pad dispatch on a [`RetainedSurface`].
pub(crate) struct TabletDispatch<M> {
    /// `true` when an element or surface `on_tablet` handler claimed this event.
    pub(crate) handled_by_tablet: bool,
    pub(crate) on_node: Option<M>,
    pub(crate) on_surface: Option<M>,
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
