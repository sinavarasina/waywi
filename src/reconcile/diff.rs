use std::collections::{HashMap, HashSet};
use std::time::Instant;

use crate::asset::AssetCache;
use crate::display::{Output, OutputInfo, resolve_outputs};
use crate::geometry::{Affine2D, Bounds, Length, LogicalPx, LogicalSize, ScaleFactor};
use crate::image::SourceRect;
use crate::input::NodeId;
use crate::render::retained::PaintPayload;
use crate::render::{RenderBackend, RetainedNode, collect_draw_and_hit};
use crate::surface::{BufferRetention, InputMode, SurfaceKind, Surfaces};
use crate::text::SharedStr;

use super::animation::{NodeTickCtx, tick_node_animations, update_asset_rev_in_tree};
use super::layout::{
    MeasureCtx, compute_selective_input_rects, resolve_surface_size_with_children,
};
use super::node_id::NodeIdAllocator;
use super::surface::{ReconcileReport, RetainedSurface, RetainedSurfaceKey, SurfaceSpec};

/// Borrowed Wayland output and surface target state passed into [`Reconciler::reconcile`].
pub(crate) struct ReconcileTargets<'a> {
    pub(crate) outputs: &'a [OutputInfo],
    pub(crate) configured_sizes: &'a HashMap<RetainedSurfaceKey, LogicalSize>,
    pub(crate) surface_scales: &'a HashMap<RetainedSurfaceKey, ScaleFactor>,
    pub(crate) default_buffer_retention: BufferRetention,
    pub(crate) app_name: &'a SharedStr,
}

struct SurfacePass<'a, R> {
    targets: &'a ReconcileTargets<'a>,
    renderer: &'a mut R,
    assets: &'a AssetCache,
    now: Instant,
    seen_keys: HashSet<RetainedSurfaceKey>,
    report: ReconcileReport,
}

/// Retained UI reconciler managing keyed surfaces, elements, dirty rects, and sprite timelines.
pub(crate) struct Reconciler<M> {
    pub(crate) surfaces: HashMap<RetainedSurfaceKey, RetainedSurface<M>>,
    pub(crate) node_ids: NodeIdAllocator,
    pub(crate) frame_seq: u64,
}

impl<M: Clone + 'static> Default for Reconciler<M> {
    fn default() -> Self {
        Self::new()
    }
}

impl<M: Clone + 'static> Reconciler<M> {
    pub(crate) fn new() -> Self {
        Self {
            surfaces: HashMap::new(),
            node_ids: NodeIdAllocator::new(),
            frame_seq: 0,
        }
    }

    /// Reconciles declared `Surfaces<M>` against the retained surface table.
    pub(crate) fn reconcile(
        &mut self,
        declared: Surfaces<M>,
        targets: &ReconcileTargets<'_>,
        renderer: &mut impl RenderBackend,
        assets: &AssetCache,
        now: Instant,
    ) -> ReconcileReport {
        self.frame_seq += 1;
        let mut pass = SurfacePass {
            targets,
            renderer,
            assets,
            now,
            seen_keys: HashSet::with_capacity(self.surfaces.len()),
            report: ReconcileReport::default(),
        };

        for surface in declared {
            // Fast O(1) surface-level memoization skip before expanding output targets
            // (a memo hit uses a placeholder `kind`/`output` so inspecting those would evict
            // `Window` or multi-monitor `Output::All` surfaces).
            if surface.memo_hit {
                for (&rk, existing) in &mut self.surfaces {
                    if rk.surface_key != surface.key {
                        continue;
                    }
                    pass.seen_keys.insert(rk);
                    pass.report.skipped_memo_surfaces += 1;
                    if let Some(dl) = existing.next_animation_deadline {
                        pass.report.next_animation_deadline = Some(
                            pass.report
                                .next_animation_deadline
                                .map_or(dl, |prev| prev.min(dl)),
                        );
                    }
                }
                continue;
            }

            let expanded_targets: Vec<Option<&OutputInfo>> = match surface.kind {
                SurfaceKind::Window => vec![None],
                _ => {
                    let matched = resolve_outputs(&surface.output, pass.targets.outputs);
                    if matched.is_empty() {
                        if matches!(surface.output, Output::All) {
                            Vec::new()
                        } else if matches!(surface.output, Output::Primary)
                            && pass.targets.outputs.is_empty()
                        {
                            vec![None]
                        } else {
                            Vec::new()
                        }
                    } else {
                        matched.into_iter().map(Some).collect()
                    }
                }
            };

            let target_count = expanded_targets.len();
            if target_count == 0 {
                continue;
            }

            if !surface.visible {
                for opt_out in expanded_targets {
                    let rk = RetainedSurfaceKey {
                        surface_key: surface.key,
                        output_id: opt_out.map(|o| o.id),
                    };
                    if let Some(removed) = self.surfaces.remove(&rk) {
                        self.retire_tree(&removed.roots, &mut pass.report.retired_node_ids);
                        pass.report.removed_surfaces.push(rk);
                    }
                }
                continue;
            }

            if target_count == 1 {
                self.reconcile_surface_instance(expanded_targets[0], surface, &mut pass);
            } else {
                let mut surf_opt = Some(surface);
                for (idx, opt_out) in expanded_targets.into_iter().enumerate() {
                    let surf = if idx + 1 == target_count {
                        surf_opt.take().expect("last expanded target owns surface")
                    } else {
                        surf_opt.as_ref().expect("surface present").clone()
                    };
                    self.reconcile_surface_instance(opt_out, surf, &mut pass);
                }
            }
        }

        // Clean up surfaces omitted from `view(state)`.
        if self.surfaces.len() > pass.seen_keys.len() {
            let stale_keys: Vec<RetainedSurfaceKey> = self
                .surfaces
                .keys()
                .filter(|k| !pass.seen_keys.contains(k))
                .copied()
                .collect();
            for rk in stale_keys {
                if let Some(removed) = self.surfaces.remove(&rk) {
                    self.retire_tree(&removed.roots, &mut pass.report.retired_node_ids);
                    pass.report.removed_surfaces.push(rk);
                }
            }
        }

        pass.renderer
            .drop_canvas_cache_for(&pass.report.retired_node_ids);
        pass.report
    }

    fn reconcile_surface_instance(
        &mut self,
        opt_out: Option<&OutputInfo>,
        surf: crate::surface::Surface<M>,
        pass: &mut SurfacePass<'_, impl RenderBackend>,
    ) {
        let output_id = opt_out.map(|o| o.id);
        let rk = RetainedSurfaceKey {
            surface_key: surf.key,
            output_id,
        };
        pass.seen_keys.insert(rk);

        let output_size = opt_out
            .map(|o| o.logical_size)
            .unwrap_or(LogicalSize::new(LogicalPx(1920.0), LogicalPx(1080.0)));
        let scale = pass
            .targets
            .surface_scales
            .get(&rk)
            .copied()
            .or_else(|| opt_out.map(|o| o.scale_factor))
            .unwrap_or(ScaleFactor::ONE);
        let declared_size = resolve_surface_size_with_children(
            surf.size,
            &surf.children,
            output_size,
            surf.margin,
            &mut MeasureCtx {
                scale,
                renderer: pass.renderer,
                assets: pass.assets,
                frame_seq: self.frame_seq,
            },
        );
        let configured =
            pass.targets
                .configured_sizes
                .get(&rk)
                .copied()
                .map(|cfg| match surf.kind {
                    SurfaceKind::Window | SurfaceKind::Popup | SurfaceKind::SessionLock => cfg,
                    SurfaceKind::Panel | SurfaceKind::Floating | SurfaceKind::Overlay => {
                        LogicalSize::new(
                            if matches!(surf.size.width, Length::Fill) {
                                cfg.width
                            } else {
                                declared_size.width
                            },
                            if matches!(surf.size.height, Length::Fill) {
                                cfg.height
                            } else {
                                declared_size.height
                            },
                        )
                    }
                });
        let target_size = configured.unwrap_or(declared_size);

        let popup = surf.popup.map(|mut p| {
            p.desired_size = Some(declared_size);
            p
        });

        let title = if surf.title.is_empty() {
            pass.targets.app_name.clone()
        } else {
            surf.title
        };
        let namespace = if surf.namespace.is_empty() {
            pass.targets.app_name.clone()
        } else {
            surf.namespace
        };

        let spec = SurfaceSpec {
            kind: surf.kind,
            output_id,
            layer: surf.layer,
            anchor: surf.anchor,
            margin: surf.margin,
            logical_size: target_size,
            exclusive_zone: surf.exclusive_zone,
            keyboard_interactivity: surf.keyboard_interactivity,
            input_mode: surf.input_mode,
            buffer_retention: surf
                .buffer_retention
                .unwrap_or(pass.targets.default_buffer_retention),
            popup,
            background: surf.background,
            title,
            namespace,
            cursor: surf.cursor,
            decoration: surf.decoration,
            background_blur: surf.background_blur,
            window_icon: surf.window_icon,
            color_space: surf.color_space,
            inhibit_idle: surf.inhibit_idle,
            pointer_constraint: surf.pointer_constraint,
            accepts_dnd: surf.on_dnd.is_some(),
            dnd_mime_types: surf.dnd_mime_types,
            a11y: surf.a11y,
            ime: surf.ime,
        };

        let prev_opt = self.surfaces.remove(&rk);
        let is_new = prev_opt.is_none();
        let mut prev_roots = Vec::new();
        let mut prev_hovered = None;
        let mut prev_touch_captures = Vec::new();
        let mut prev_tablet_captures = Vec::new();
        let mut config_dirty = true;
        let mut full_redraw = true;
        let mut prev_needs_redraw = false;
        let mut dirty_rects = Vec::new();
        let mut prev_spec_input_mode = None;
        let mut prev_selective_rects = Vec::new();

        if let Some(prev) = prev_opt {
            config_dirty = prev.spec.kind != spec.kind
                || prev.spec.layer != spec.layer
                || prev.spec.anchor != spec.anchor
                || prev.spec.margin != spec.margin
                || prev.spec.exclusive_zone != spec.exclusive_zone
                || prev.spec.keyboard_interactivity != spec.keyboard_interactivity
                || prev.spec.title != spec.title
                || prev.spec.logical_size != spec.logical_size
                || prev.spec.buffer_retention != spec.buffer_retention
                || prev.spec.popup != spec.popup
                || prev.spec.decoration != spec.decoration
                || prev.spec.background_blur != spec.background_blur
                || prev.spec.window_icon != spec.window_icon
                || prev.spec.color_space != spec.color_space
                || prev.spec.inhibit_idle != spec.inhibit_idle
                || prev.spec.pointer_constraint != spec.pointer_constraint
                || prev.spec.accepts_dnd != spec.accepts_dnd
                || prev.spec.dnd_mime_types != spec.dnd_mime_types
                || prev.spec.ime != spec.ime
                || prev.scale != scale;
            full_redraw = prev.full_redraw
                || prev.spec.background != spec.background
                || prev.spec.logical_size != spec.logical_size
                || prev.scale != scale;
            prev_needs_redraw = prev.needs_redraw;
            dirty_rects = prev.dirty_logical_rects;
            prev_spec_input_mode = Some(prev.spec.input_mode);
            prev_selective_rects = prev.selective_input_rects;
            prev_roots = prev.roots;
            prev_hovered = prev.hovered_node;
            prev_touch_captures = prev.touch_captures;
            prev_tablet_captures = prev.tablet_captures;
        }

        let mut anim_deadline: Option<Instant> = None;
        let mut has_active_transitions = false;
        let mut tree_pass = super::tree::TreePass {
            renderer: pass.renderer,
            assets: pass.assets,
            now: pass.now,
            frame_seq: self.frame_seq,
            scale,
            surface_size: target_size,
            dirty_rects: &mut dirty_rects,
            skipped_memo: &mut pass.report.skipped_memo_elements,
            retired_ids: &mut pass.report.retired_node_ids,
            anim_deadline: &mut anim_deadline,
            has_active_transitions: &mut has_active_transitions,
        };
        let new_roots = self.reconcile_children_positioned(
            surf.children,
            prev_roots,
            super::tree::ParentScope::new(
                Bounds::new(
                    LogicalPx::ZERO,
                    LogicalPx::ZERO,
                    target_size.width,
                    target_size.height,
                ),
                Affine2D::IDENTITY,
                None,
            ),
            super::tree::FlowLayout::stack(),
            &mut tree_pass,
        );

        let mut commands = Vec::new();
        let mut hit_nodes = Vec::new();
        let has_custom_cursors = collect_draw_and_hit(&new_roots, &mut commands, &mut hit_nodes);

        let selective_input_rects = if spec.input_mode == InputMode::Selective {
            compute_selective_input_rects(&hit_nodes)
        } else {
            Vec::new()
        };

        let input_region_dirty = prev_spec_input_mode != Some(spec.input_mode)
            || prev_selective_rects != selective_input_rects;
        let needs_redraw = prev_needs_redraw || full_redraw || !dirty_rects.is_empty();

        let hovered_node = prev_hovered.filter(|id| hit_nodes.iter().any(|n| n.node_id == *id));
        prev_touch_captures.retain(|(_, id)| hit_nodes.iter().any(|n| n.node_id == *id));
        prev_tablet_captures.retain(|(_, id)| hit_nodes.iter().any(|n| n.node_id == *id));

        let retained = RetainedSurface {
            spec,
            scale,
            roots: new_roots,
            commands,
            hit_nodes,
            has_custom_cursors,
            hovered_node,
            on_close: surf.on_close,
            on_key: surf.on_key,
            on_ime: surf.on_ime,
            on_pointer_move: surf.on_pointer_move,
            on_pointer_up: surf.on_pointer_up,
            on_relative_pointer: surf.on_relative_pointer,
            on_dnd: surf.on_dnd,
            on_touch: surf.on_touch,
            on_gesture: surf.on_gesture,
            on_tablet: surf.on_tablet,
            touch_captures: prev_touch_captures,
            tablet_captures: prev_tablet_captures,
            dirty_logical_rects: dirty_rects,
            needs_redraw,
            full_redraw,
            input_region_dirty,
            selective_input_rects,
            next_animation_deadline: anim_deadline,
            has_active_transitions,
        };

        self.surfaces.insert(rk, retained);
        if is_new {
            pass.report.created_surfaces.push(rk);
        } else if needs_redraw || config_dirty || input_region_dirty {
            pass.report.updated_surfaces.push(rk);
        }
        if let Some(dl) = anim_deadline {
            pass.report.next_animation_deadline = Some(
                pass.report
                    .next_animation_deadline
                    .map_or(dl, |prev| prev.min(dl)),
            );
        }
    }

    /// Earliest scheduled sprite animation deadline across retained surfaces, without mutating.
    #[must_use]
    pub(crate) fn next_animation_deadline(&self) -> Option<Instant> {
        self.surfaces
            .values()
            .filter_map(|s| s.next_animation_deadline)
            .min()
    }

    /// Advances active keyed sprite animations and retained element transitions up to `now` across
    /// all retained surfaces.
    pub(crate) fn tick_animations(
        &mut self,
        now: Instant,
    ) -> (Vec<RetainedSurfaceKey>, Option<Instant>, Vec<M>) {
        self.tick_animations_for(None, now)
    }

    /// Advances active keyed sprite animations and retained element transitions up to `now`,
    /// optionally scoped to a single surface (e.g. its own `wl_surface.frame` VSync callback) so
    /// mixed-refresh multi-monitor surfaces never trigger redundant work on sibling outputs.
    pub(crate) fn tick_animations_for(
        &mut self,
        surface_filter: Option<RetainedSurfaceKey>,
        now: Instant,
    ) -> (Vec<RetainedSurfaceKey>, Option<Instant>, Vec<M>) {
        let mut dirty_surfaces = Vec::new();
        let mut next_deadline: Option<Instant> = None;
        let mut emitted_messages = Vec::new();

        for (&rk, surface) in &mut self.surfaces {
            if let Some(target) = surface_filter
                && rk != target
            {
                if let Some(dl) = surface.next_animation_deadline {
                    next_deadline = Some(next_deadline.map_or(dl, |p| p.min(dl)));
                }
                continue;
            }
            if surface.next_animation_deadline.is_none() && !surface.has_active_transitions {
                continue;
            }
            let mut changed_frames: Vec<(NodeId, SourceRect)> = Vec::new();
            let mut surf_deadline: Option<Instant> = None;
            let mut surf_dirty_rects = Vec::new();
            let mut transitions_changed = false;
            let mut still_active_transitions = false;

            {
                let mut tick_ctx = NodeTickCtx {
                    now,
                    changed_frames: &mut changed_frames,
                    dirty_rects: &mut surf_dirty_rects,
                    next_deadline: &mut surf_deadline,
                    emitted_messages: &mut emitted_messages,
                    transitions_changed: &mut transitions_changed,
                    has_active_transitions: &mut still_active_transitions,
                };
                for node in &mut surface.roots {
                    tick_node_animations(node, Affine2D::IDENTITY, false, None, &mut tick_ctx);
                }
            }

            surface.next_animation_deadline = surf_deadline;
            surface.has_active_transitions = still_active_transitions;
            if let Some(dl) = surf_deadline {
                next_deadline = Some(next_deadline.map_or(dl, |p| p.min(dl)));
            }

            if transitions_changed {
                surface.commands.clear();
                surface.hit_nodes.clear();
                surface.has_custom_cursors = collect_draw_and_hit(
                    &surface.roots,
                    &mut surface.commands,
                    &mut surface.hit_nodes,
                );
                if surface.spec.input_mode == InputMode::Selective {
                    let next_rects = compute_selective_input_rects(&surface.hit_nodes);
                    if next_rects != surface.selective_input_rects {
                        surface.selective_input_rects = next_rects;
                        surface.input_region_dirty = true;
                    }
                }
                surface.dirty_logical_rects.extend(surf_dirty_rects);
                surface.needs_redraw = true;
                dirty_surfaces.push(rk);
            } else if !changed_frames.is_empty() {
                for cmd in &mut surface.commands {
                    if let PaintPayload::Image {
                        ref mut source_rect,
                        ..
                    } = cmd.payload
                        && let Some(&(_, next_rect)) =
                            changed_frames.iter().find(|&&(nid, _)| nid == cmd.node_id)
                    {
                        *source_rect = Some(next_rect);
                    }
                }
                surface.dirty_logical_rects.extend(surf_dirty_rects);
                surface.needs_redraw = true;
                dirty_surfaces.push(rk);
            }
        }

        (dirty_surfaces, next_deadline, emitted_messages)
    }

    /// Marks every surface referencing `asset_id` for a full redraw (used after `ctx.reload_asset`).
    pub(crate) fn invalidate_asset(&mut self, asset_id: crate::asset::AssetId, new_rev: u64) {
        for surface in self.surfaces.values_mut() {
            if update_asset_rev_in_tree(&mut surface.roots, asset_id, new_rev) {
                surface.commands.clear();
                surface.hit_nodes.clear();
                surface.has_custom_cursors = collect_draw_and_hit(
                    &surface.roots,
                    &mut surface.commands,
                    &mut surface.hit_nodes,
                );
                surface.full_redraw = true;
                surface.needs_redraw = true;
            }
        }
    }

    /// Removes a retained surface directly (e.g. compositor-initiated layer close or popup
    /// dismiss), retiring its node IDs and returning its `on_close` message (if any) plus retired
    /// node IDs for renderer canvas cache cleanup.
    pub(crate) fn remove_surface(
        &mut self,
        rk: RetainedSurfaceKey,
    ) -> Option<(Option<M>, Vec<NodeId>)> {
        let removed = self.surfaces.remove(&rk)?;
        let mut retired = Vec::new();
        self.retire_tree(&removed.roots, &mut retired);
        Some((removed.on_close, retired))
    }

    pub(super) fn retire_tree(&mut self, nodes: &[RetainedNode<M>], out: &mut Vec<NodeId>) {
        for n in nodes {
            out.push(n.node_id);
            self.node_ids.free(n.node_id);
            self.retire_tree(&n.children, out);
        }
    }
}
