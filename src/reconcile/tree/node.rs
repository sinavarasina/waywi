use crate::animation::{
    RetainedScalarTransition, RetainedTransformTransition, RetainedTransitions,
};
use crate::geometry::{Affine2D, Bounds, Length, LogicalPx, LogicalSize, Position, Transform};
use crate::input::EventHandlers;
use crate::render::{
    Align, CanvasDrawHandle, Element, ElementKind, RenderBackend, RetainedNode, RetainedVisual,
};
use crate::sprite::{SpriteSheet, SpriteSourceState, SpriteTimeline};

use super::super::diff::Reconciler;
use super::super::layout::{
    LayoutFlow, element_kind_tag, measure_element_intrinsic, resolve_length,
};
use super::{FlowLayout, ParentScope, TreePass};

struct PrevSnapshot {
    visible: bool,
    world_bounds: Bounds,
    affine: Affine2D,
    opacity: f32,
    z_index: i32,
    visual: RetainedVisual,
}

fn resolve_auto_position(
    flow: LayoutFlow,
    cross_align: Align,
    gap: LogicalPx,
    parent_rect: Bounds,
    resolved_w: LogicalPx,
    resolved_h: LogicalPx,
    cursor_main: &mut LogicalPx,
) -> (LogicalPx, LogicalPx) {
    match flow {
        LayoutFlow::Stack => (parent_rect.x, parent_rect.y),
        LayoutFlow::Row => {
            let x = parent_rect.x + *cursor_main;
            let y = match cross_align {
                Align::Start => parent_rect.y,
                Align::Center => parent_rect.y + (parent_rect.height - resolved_h) / 2.0,
                Align::End => parent_rect.y + parent_rect.height - resolved_h,
            };
            *cursor_main = *cursor_main + resolved_w + gap;
            (x, y)
        }
        LayoutFlow::Column => {
            let y = parent_rect.y + *cursor_main;
            let x = match cross_align {
                Align::Start => parent_rect.x,
                Align::Center => parent_rect.x + (parent_rect.width - resolved_w) / 2.0,
                Align::End => parent_rect.x + parent_rect.width - resolved_w,
            };
            *cursor_main = *cursor_main + resolved_h + gap;
            (x, y)
        }
    }
}

fn compute_node_bounds_and_transform<M: 'static, R: RenderBackend>(
    el: &Element<M>,
    parent_rect: Bounds,
    surface_size: LogicalSize,
    flow: FlowLayout,
    cursor_main: &mut LogicalPx,
    pass: &mut TreePass<'_, R>,
) -> (Bounds, Transform) {
    let common = el.common().expect("non-memo element has common props");
    let intrinsic = if matches!(common.size.width, Length::Auto)
        || matches!(common.size.height, Length::Auto)
    {
        measure_element_intrinsic(
            el,
            parent_rect,
            pass.scale,
            pass.renderer,
            pass.assets,
            pass.frame_seq,
        )
    } else {
        LogicalSize::ZERO
    };
    let resolved_w = resolve_length(common.size.width, parent_rect.width, intrinsic.width);
    let resolved_h = resolve_length(common.size.height, parent_rect.height, intrinsic.height);

    let (raw_x, raw_y) = match common.position {
        Position::Auto => resolve_auto_position(
            flow.flow,
            flow.cross_align,
            flow.gap,
            parent_rect,
            resolved_w,
            resolved_h,
            cursor_main,
        ),
        Position::Absolute { x, y } => (parent_rect.x + x, parent_rect.y + y),
        Position::RelativeToSurface {
            x,
            y,
            offset_x,
            offset_y,
        } => (
            x.of(surface_size.width) + offset_x,
            y.of(surface_size.height) + offset_y,
        ),
        Position::RelativeToParent {
            x,
            y,
            offset_x,
            offset_y,
        } => (
            parent_rect.x + x.of(parent_rect.width) + offset_x,
            parent_rect.y + y.of(parent_rect.height) + offset_y,
        ),
    };

    let anchor_offset = common.anchor.top_left_offset(resolved_w, resolved_h);
    let bounds = Bounds::new(
        raw_x - anchor_offset.x,
        raw_y - anchor_offset.y,
        resolved_w,
        resolved_h,
    );
    let mut local_transform = common.transform;
    match &el.kind {
        ElementKind::Sprite(sp) => {
            if sp.flip_x {
                local_transform.scale_x = -local_transform.scale_x;
            }
            if sp.flip_y {
                local_transform.scale_y = -local_transform.scale_y;
            }
        }
        ElementKind::Image(img) => {
            if img.flip_x {
                local_transform.scale_x = -local_transform.scale_x;
            }
            if img.flip_y {
                local_transform.scale_y = -local_transform.scale_y;
            }
        }
        _ => {}
    }
    (bounds, local_transform)
}

#[inline]
pub(crate) fn compose_node_affine(
    bounds: Bounds,
    local_transform: Transform,
    parent_affine: Affine2D,
) -> (Affine2D, Affine2D, Bounds) {
    let local_affine = if local_transform.is_identity() {
        Affine2D::IDENTITY
    } else {
        local_transform.to_affine_around(local_transform.pivot_point(bounds))
    };
    let affine = local_affine.then(&parent_affine);
    let world_bounds = affine.transform_rect(bounds);
    (local_affine, affine, world_bounds)
}

fn resolve_sprite_timeline<R: RenderBackend>(
    sheet: &SpriteSheet,
    mode: &SpriteSourceState,
    prev_timeline: Option<SpriteTimeline>,
    visible: bool,
    has_end_handler: bool,
    pass: &mut TreePass<'_, R>,
) -> (usize, Option<SpriteTimeline>) {
    match mode {
        SpriteSourceState::FixedFrame(idx) => (*idx, None),
        SpriteSourceState::Playing {
            animation,
            playback_override,
        } => {
            let mut tl = match prev_timeline {
                Some(old) if old.animation == *animation => old,
                _ => {
                    let start_idx = sheet
                        .get_animation(animation.as_str())
                        .map_or(0, |a| a.frames.first_frame());
                    SpriteTimeline::new(animation.clone(), pass.now, start_idx)
                }
            };
            let (frame, next_dl) = if cfg!(feature = "animation") && visible {
                let adv = tl.advance(
                    sheet,
                    animation.as_str(),
                    *playback_override,
                    pass.now,
                    false,
                    has_end_handler,
                );
                (adv.frame, adv.next_deadline)
            } else {
                tl.next_deadline = None;
                (tl.current_frame, None)
            };
            if let Some(dl) = next_dl {
                *pass.anim_deadline = Some(pass.anim_deadline.map_or(dl, |p| p.min(dl)));
            }
            (frame, Some(tl))
        }
    }
}

struct NodeSnapshotRef<'a> {
    visible: bool,
    world_bounds: Bounds,
    affine: Affine2D,
    opacity: f32,
    z_index: i32,
    visual: &'a RetainedVisual,
}

fn record_dirty_bounds(
    prev_snapshot: Option<PrevSnapshot>,
    next: &NodeSnapshotRef<'_>,
    dirty_rects: &mut Vec<Bounds>,
) {
    match prev_snapshot {
        None => {
            if next.visible && !matches!(next.visual, RetainedVisual::Container) {
                dirty_rects.push(next.world_bounds);
            }
        }
        Some(prev) => {
            let changed = prev.visible != next.visible
                || prev.world_bounds != next.world_bounds
                || prev.affine != next.affine
                || (prev.opacity - next.opacity).abs() > f32::EPSILON
                || prev.z_index != next.z_index
                || prev.visual.visual_changed(next.visual);
            if changed && !matches!(next.visual, RetainedVisual::Container) {
                if prev.visible {
                    dirty_rects.push(prev.world_bounds);
                }
                if next.visible && next.world_bounds != prev.world_bounds {
                    dirty_rects.push(next.world_bounds);
                }
            }
        }
    }
}

impl<M: Clone + 'static> Reconciler<M> {
    pub(super) fn build_or_update_node<R: RenderBackend>(
        &mut self,
        el: Element<M>,
        prev: Option<RetainedNode<M>>,
        parent: ParentScope,
        flow: FlowLayout,
        cursor_main: &mut LogicalPx,
        pass: &mut TreePass<'_, R>,
    ) -> RetainedNode<M> {
        let kind_tag = element_kind_tag(&el);
        let (bounds, target_transform) = compute_node_bounds_and_transform(
            &el,
            parent.rect,
            pass.surface_size,
            flow,
            cursor_main,
            pass,
        );

        let common = el.common().expect("non-memo element has common props");
        let key = common.key;
        let visible = common.visible;
        let input_enabled = common.input_enabled;
        let target_opacity = common.opacity;
        let z_index = common.z_index;
        let cursor = common.cursor;
        let a11y = common.a11y.clone();
        let opacity_tr_spec = common.opacity_transition;
        let transform_tr_spec = common.transform_transition;

        let (prev_id, prev_children, prev_timeline, prev_transitions, prev_snapshot) = match prev {
            Some(p) => {
                let snap = Some(PrevSnapshot {
                    visible: p.visible,
                    world_bounds: p.world_bounds,
                    affine: p.affine,
                    opacity: p.opacity,
                    z_index: p.z_index,
                    visual: p.visual,
                });
                (
                    Some(p.node_id),
                    p.children,
                    p.sprite_timeline,
                    p.transitions,
                    snap,
                )
            }
            None => (None, Vec::new(), None, None, None),
        };
        let node_id = prev_id.unwrap_or_else(|| self.node_ids.alloc());

        let (opacity, effective_transform, transitions) = if cfg!(feature = "animation")
            && (opacity_tr_spec.is_some() || transform_tr_spec.is_some())
        {
            debug_assert!(
                key.is_some(),
                "elements with .transition_opacity() or .transition_transform() must set .key(...) for stable identity across sibling reorders"
            );
            let mut tr = prev_transitions.unwrap_or_else(|| {
                Box::new(RetainedTransitions {
                    opacity: None,
                    transform: None,
                })
            });

            let eff_opacity = if let Some(spec) = opacity_tr_spec {
                let state = match tr.opacity.as_mut() {
                    Some(existing) => {
                        existing.retarget(target_opacity, spec, pass.now);
                        existing.advance(pass.now);
                        existing
                    }
                    None => {
                        let initial = prev_snapshot.as_ref().map_or(target_opacity, |s| s.opacity);
                        let mut st = RetainedScalarTransition::new(initial, spec);
                        st.retarget(target_opacity, spec, pass.now);
                        st.advance(pass.now);
                        tr.opacity.insert(st)
                    }
                };
                state.value().clamp(0.0, 1.0)
            } else {
                tr.opacity = None;
                target_opacity
            };

            let eff_transform = if let Some(spec) = transform_tr_spec {
                let state = match tr.transform.as_mut() {
                    Some(existing) => {
                        existing.retarget(target_transform, spec, pass.now);
                        existing.advance(pass.now);
                        existing
                    }
                    None => {
                        let st = RetainedTransformTransition::new(target_transform, spec);
                        tr.transform.insert(st)
                    }
                };
                state.value()
            } else {
                tr.transform = None;
                target_transform
            };

            if visible && tr.is_active() {
                *pass.has_active_transitions = true;
            }
            (eff_opacity, eff_transform, Some(tr))
        } else {
            (target_opacity, target_transform, None)
        };

        let (local_affine, affine, world_bounds) =
            compose_node_affine(bounds, effective_transform, parent.affine);

        let mut sprite_timeline = None;
        let mut on_animation_end = None;
        let mut children_out = Vec::new();
        let handlers: EventHandlers<M>;

        let visual = match el.kind {
            ElementKind::Rect(r) => {
                handlers = r.common.handlers;
                RetainedVisual::Rect {
                    fill: r.fill,
                    corner_radius: r.corner_radius,
                    border: r.border,
                }
            }
            ElementKind::Text(t) => {
                handlers = t.common.handlers;
                RetainedVisual::Text {
                    content: t.content,
                    font_size: t.font_size,
                    line_height_multiplier: t.line_height_multiplier,
                    color: t.color,
                    font: t.font,
                    align: t.align,
                    bounds_mode: t.bounds_mode,
                    wrap_to_width: !matches!(t.common.size.width, crate::geometry::Length::Auto),
                }
            }
            ElementKind::Image(img) => {
                handlers = img.common.handlers;
                let rev = pass.assets.revision_of(img.asset.id);
                RetainedVisual::Image {
                    asset: img.asset,
                    asset_revision: rev,
                    fit: img.fit,
                    filter: img.filter,
                    source_rect: img.source_rect,
                }
            }
            ElementKind::Sprite(sp) => {
                handlers = sp.common.handlers;
                on_animation_end = sp.on_animation_end;
                let rev = pass.assets.revision_of(sp.sheet.asset.id);
                let (resolved_frame, tl) = resolve_sprite_timeline(
                    &sp.sheet,
                    &sp.mode,
                    prev_timeline,
                    visible,
                    on_animation_end.is_some(),
                    pass,
                );
                sprite_timeline = tl;
                RetainedVisual::Sprite {
                    sheet: sp.sheet,
                    asset_revision: rev,
                    mode: sp.mode,
                    resolved_frame,
                    filter: sp.filter,
                }
            }
            ElementKind::Canvas(c) => {
                handlers = c.common.handlers;
                RetainedVisual::Canvas {
                    cache_key: c.cache_key,
                    draw: CanvasDrawHandle { func: c.draw },
                }
            }
            ElementKind::Group(g) => {
                handlers = g.common.handlers;
                children_out = self.reconcile_children_positioned(
                    g.children,
                    prev_children,
                    ParentScope::new(bounds, affine, parent.clip_rect),
                    FlowLayout::stack(),
                    pass,
                );
                RetainedVisual::Container
            }
            ElementKind::Row(r) => {
                handlers = r.common.handlers;
                children_out = self.reconcile_children_positioned(
                    r.children,
                    prev_children,
                    ParentScope::new(bounds, affine, parent.clip_rect),
                    FlowLayout {
                        flow: LayoutFlow::Row,
                        gap: r.gap,
                        cross_align: r.align,
                        padding: r.padding,
                    },
                    pass,
                );
                RetainedVisual::Container
            }
            ElementKind::Column(c) => {
                handlers = c.common.handlers;
                children_out = self.reconcile_children_positioned(
                    c.children,
                    prev_children,
                    ParentScope::new(bounds, affine, parent.clip_rect),
                    FlowLayout {
                        flow: LayoutFlow::Column,
                        gap: c.gap,
                        cross_align: c.align,
                        padding: c.padding,
                    },
                    pass,
                );
                RetainedVisual::Container
            }
            ElementKind::Clip(c) => {
                handlers = c.common.handlers;
                let next_clip = match parent.clip_rect {
                    Some(prev_c) => prev_c.intersect(world_bounds),
                    None => Some(world_bounds),
                };
                children_out = self.reconcile_children_positioned(
                    c.child,
                    prev_children,
                    ParentScope::new(bounds, affine, next_clip),
                    FlowLayout::stack(),
                    pass,
                );
                RetainedVisual::Clip {
                    corner_radius: c.corner_radius,
                }
            }
            ElementKind::MemoHit { .. } => unreachable!("handled by sibling reconciler"),
        };

        record_dirty_bounds(
            prev_snapshot,
            &NodeSnapshotRef {
                visible,
                world_bounds,
                affine,
                opacity,
                z_index,
                visual: &visual,
            },
            pass.dirty_rects,
        );

        RetainedNode {
            node_id,
            key,
            kind_tag,
            bounds,
            world_bounds,
            local_affine,
            affine,
            clip_rect: parent.clip_rect,
            z_index,
            visible,
            input_enabled,
            opacity,
            cursor,
            handlers,
            visual,
            a11y,
            sprite_timeline,
            on_animation_end,
            transitions,
            children: children_out,
        }
    }
}
