use crate::geometry::{Affine2D, LogicalPx, LogicalSize, Position, Rect as GeoRect};
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
    world_bounds: GeoRect,
    opacity: f32,
    z_index: i32,
    visual: RetainedVisual,
}

fn resolve_auto_position(
    flow: LayoutFlow,
    cross_align: Align,
    gap: LogicalPx,
    parent_rect: GeoRect,
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

fn resolve_node_geometry<M: 'static, R: RenderBackend>(
    el: &Element<M>,
    parent_rect: GeoRect,
    parent_affine: Affine2D,
    surface_size: LogicalSize,
    flow: FlowLayout,
    cursor_main: &mut LogicalPx,
    pass: &mut TreePass<'_, R>,
) -> (GeoRect, Affine2D, GeoRect) {
    let intrinsic =
        measure_element_intrinsic(el, parent_rect, pass.renderer, pass.assets, pass.frame_seq);
    let common = el.common().expect("non-memo element has common props");
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
    let bounds = GeoRect::new(
        raw_x - anchor_offset.x,
        raw_y - anchor_offset.y,
        resolved_w,
        resolved_h,
    );
    let local_affine = if common.transform.is_identity() {
        Affine2D::IDENTITY
    } else {
        common.transform.to_affine_around(bounds.center())
    };
    let affine = local_affine.then(&parent_affine);
    let world_bounds = affine.transform_rect(bounds);
    (bounds, affine, world_bounds)
}

fn resolve_sprite_timeline<R: RenderBackend>(
    sheet: &SpriteSheet,
    mode: &SpriteSourceState,
    prev_timeline: Option<SpriteTimeline>,
    visible: bool,
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
                        .map_or(0, |a| a.frames.start);
                    SpriteTimeline::new(animation.clone(), pass.now, start_idx)
                }
            };
            let (frame, _, next_dl) = if cfg!(feature = "animation") && visible {
                tl.advance(sheet, animation.as_str(), *playback_override, pass.now)
            } else {
                (tl.current_frame, false, None)
            };
            if let Some(dl) = next_dl {
                *pass.anim_deadline = Some(pass.anim_deadline.map_or(dl, |p| p.min(dl)));
            }
            (frame, Some(tl))
        }
    }
}

fn record_dirty_bounds(
    prev_snapshot: Option<PrevSnapshot>,
    visible: bool,
    world_bounds: GeoRect,
    opacity: f32,
    z_index: i32,
    visual: &RetainedVisual,
    dirty_rects: &mut Vec<GeoRect>,
) {
    match prev_snapshot {
        None => {
            if visible && !matches!(visual, RetainedVisual::Container) {
                dirty_rects.push(world_bounds);
            }
        }
        Some(prev) => {
            let changed = prev.visible != visible
                || prev.world_bounds != world_bounds
                || (prev.opacity - opacity).abs() > f32::EPSILON
                || prev.z_index != z_index
                || prev.visual.visual_changed(visual);
            if changed && !matches!(visual, RetainedVisual::Container) {
                if prev.visible {
                    dirty_rects.push(prev.world_bounds);
                }
                if visible && world_bounds != prev.world_bounds {
                    dirty_rects.push(world_bounds);
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
        let (bounds, affine, world_bounds) = resolve_node_geometry(
            &el,
            parent.rect,
            parent.affine,
            pass.surface_size,
            flow,
            cursor_main,
            pass,
        );

        let common = el.common().expect("non-memo element has common props");
        let key = common.key;
        let visible = common.visible;
        let input_enabled = common.input_enabled;
        let opacity = common.opacity;
        let z_index = common.z_index;
        let a11y = common.a11y.clone();

        let (prev_id, prev_children, prev_timeline, prev_snapshot) = match prev {
            Some(p) => {
                let snap = Some(PrevSnapshot {
                    visible: p.visible,
                    world_bounds: p.world_bounds,
                    opacity: p.opacity,
                    z_index: p.z_index,
                    visual: p.visual,
                });
                (Some(p.node_id), p.children, p.sprite_timeline, snap)
            }
            None => (None, Vec::new(), None, None),
        };
        let node_id = prev_id.unwrap_or_else(|| self.node_ids.alloc());

        let mut sprite_timeline = None;
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
                }
            }
            ElementKind::Sprite(sp) => {
                let sp = *sp;
                handlers = sp.common.handlers;
                let rev = pass.assets.revision_of(sp.sheet.asset.id);
                let (resolved_frame, tl) =
                    resolve_sprite_timeline(&sp.sheet, &sp.mode, prev_timeline, visible, pass);
                sprite_timeline = tl;
                RetainedVisual::Sprite {
                    sheet: Box::new(sp.sheet),
                    asset_revision: rev,
                    mode: sp.mode,
                    resolved_frame,
                }
            }
            ElementKind::Canvas(c) => {
                handlers = c.common.handlers;
                RetainedVisual::Canvas {
                    render_key: c.render_key,
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
            visible,
            world_bounds,
            opacity,
            z_index,
            &visual,
            pass.dirty_rects,
        );

        RetainedNode {
            node_id,
            key,
            kind_tag,
            bounds,
            world_bounds,
            affine,
            clip_rect: parent.clip_rect,
            z_index,
            visible,
            input_enabled,
            opacity,
            handlers,
            visual,
            a11y,
            sprite_timeline,
            children: children_out,
        }
    }
}
