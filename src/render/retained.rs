use crate::accessibility::AccessibleProps;
use crate::arena::FrameSharedFn;
use crate::geometry::{Affine2D, LogicalPx, Rect as GeoRect};
use crate::image::{ImageFilter, ImageFit, SourceRect};
use crate::input::{ElementKey, EventHandlers, HitTestNode, NodeId};
use crate::render::canvas::Painter;
use crate::render::style::{Border, Color, CornerRadius};
use crate::sprite::{SpriteSheet, SpriteSourceState, SpriteTimeline};
use crate::text::{Font, SharedStr, TextAlign, TextBoundsMode};

#[derive(Debug, Clone, PartialEq)]
pub(crate) enum PaintPayload {
    Rect {
        fill: Color,
        corner_radius: CornerRadius,
        border: Option<Border>,
    },
    Text {
        content: SharedStr,
        font_size: LogicalPx,
        line_height_multiplier: f32,
        color: Color,
        font: Font,
        align: TextAlign,
        bounds_mode: TextBoundsMode,
        wrap_to_width: bool,
    },
    Image {
        asset: crate::asset::ImageAsset,
        asset_revision: u64,
        fit: ImageFit,
        filter: ImageFilter,
        source_rect: Option<SourceRect>,
    },
    Sprite {
        sheet: Box<SpriteSheet>,
        asset_revision: u64,
        frame_index: usize,
        filter: ImageFilter,
    },
    Canvas {
        render_key: u64,
        draw: CanvasDrawHandle,
    },
    ClipPush {
        corner_radius: CornerRadius,
    },
    ClipPop,
}

#[derive(Clone)]
pub(crate) struct CanvasDrawHandle {
    pub(crate) func: FrameSharedFn<*mut Painter<'static>, ()>,
}

impl PartialEq for CanvasDrawHandle {
    fn eq(&self, _other: &Self) -> bool {
        true
    }
}

impl std::fmt::Debug for CanvasDrawHandle {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("CanvasDrawHandle")
    }
}

#[derive(Debug, Clone, PartialEq)]
pub(crate) struct DrawCommand {
    pub(crate) node_id: NodeId,
    pub(crate) local_rect: GeoRect,
    pub(crate) affine: Affine2D,
    pub(crate) world_bounds: GeoRect,
    pub(crate) clip_rect: Option<GeoRect>,
    pub(crate) z_index: i32,
    pub(crate) paint_order: usize,
    pub(crate) opacity: f32,
    pub(crate) payload: PaintPayload,
}

/// Retained node tree built by the reconciler for one surface.
pub(crate) struct RetainedNode<M> {
    pub(crate) node_id: NodeId,
    pub(crate) key: Option<ElementKey>,
    pub(crate) kind_tag: u8,
    pub(crate) bounds: GeoRect,
    pub(crate) world_bounds: GeoRect,
    pub(crate) affine: Affine2D,
    pub(crate) clip_rect: Option<GeoRect>,
    pub(crate) z_index: i32,
    pub(crate) visible: bool,
    pub(crate) input_enabled: bool,
    pub(crate) opacity: f32,
    pub(crate) handlers: EventHandlers<M>,
    pub(crate) visual: RetainedVisual,
    #[cfg_attr(not(feature = "accessibility"), allow(dead_code))]
    pub(crate) a11y: AccessibleProps,
    pub(crate) sprite_timeline: Option<SpriteTimeline>,
    pub(crate) on_animation_end: Option<M>,
    pub(crate) children: Vec<RetainedNode<M>>,
}

#[derive(Clone)]
pub(crate) enum RetainedVisual {
    Container,
    Rect {
        fill: Color,
        corner_radius: CornerRadius,
        border: Option<Border>,
    },
    Text {
        content: SharedStr,
        font_size: LogicalPx,
        line_height_multiplier: f32,
        color: Color,
        font: Font,
        align: TextAlign,
        bounds_mode: TextBoundsMode,
        wrap_to_width: bool,
    },
    Image {
        asset: crate::asset::ImageAsset,
        asset_revision: u64,
        fit: ImageFit,
        filter: ImageFilter,
        source_rect: Option<SourceRect>,
    },
    Sprite {
        sheet: Box<SpriteSheet>,
        asset_revision: u64,
        mode: SpriteSourceState,
        resolved_frame: usize,
        filter: ImageFilter,
    },
    Canvas {
        render_key: u64,
        draw: CanvasDrawHandle,
    },
    Clip {
        corner_radius: CornerRadius,
    },
}

impl RetainedVisual {
    pub(crate) fn visual_changed(&self, other: &Self) -> bool {
        match (self, other) {
            (Self::Container, Self::Container) => false,
            (
                Self::Rect {
                    fill: f1,
                    corner_radius: c1,
                    border: b1,
                },
                Self::Rect {
                    fill: f2,
                    corner_radius: c2,
                    border: b2,
                },
            ) => f1 != f2 || c1 != c2 || b1 != b2,
            (
                Self::Text {
                    content: t1,
                    font_size: fs1,
                    line_height_multiplier: lh1,
                    color: c1,
                    font: fn1,
                    align: a1,
                    bounds_mode: bm1,
                    wrap_to_width: w1,
                },
                Self::Text {
                    content: t2,
                    font_size: fs2,
                    line_height_multiplier: lh2,
                    color: c2,
                    font: fn2,
                    align: a2,
                    bounds_mode: bm2,
                    wrap_to_width: w2,
                },
            ) => {
                t1 != t2
                    || fs1 != fs2
                    || (lh1 - lh2).abs() > f32::EPSILON
                    || c1 != c2
                    || fn1 != fn2
                    || a1 != a2
                    || bm1 != bm2
                    || w1 != w2
            }
            (
                Self::Image {
                    asset: a1,
                    asset_revision: r1,
                    fit: f1,
                    filter: fl1,
                    source_rect: sr1,
                },
                Self::Image {
                    asset: a2,
                    asset_revision: r2,
                    fit: f2,
                    filter: fl2,
                    source_rect: sr2,
                },
            ) => a1 != a2 || r1 != r2 || f1 != f2 || fl1 != fl2 || sr1 != sr2,
            (
                Self::Sprite {
                    sheet: s1,
                    asset_revision: r1,
                    resolved_frame: rf1,
                    filter: fl1,
                    ..
                },
                Self::Sprite {
                    sheet: s2,
                    asset_revision: r2,
                    resolved_frame: rf2,
                    filter: fl2,
                    ..
                },
            ) => s1 != s2 || r1 != r2 || rf1 != rf2 || fl1 != fl2,
            (Self::Canvas { render_key: k1, .. }, Self::Canvas { render_key: k2, .. }) => k1 != k2,
            (Self::Clip { corner_radius: c1 }, Self::Clip { corner_radius: c2 }) => c1 != c2,
            _ => true,
        }
    }
}

/// Flattens a retained tree into sorted `DrawCommand`s and `HitTestNode`s.
pub(crate) fn collect_draw_and_hit<M: Clone + 'static>(
    roots: &[RetainedNode<M>],
    commands: &mut Vec<DrawCommand>,
    hit_nodes: &mut Vec<HitTestNode<M>>,
) {
    commands.clear();
    hit_nodes.clear();
    let mut order = 0_usize;
    for node in roots {
        collect_node_recursive(node, commands, hit_nodes, &mut order);
    }
    commands.sort_by_key(|c| (c.z_index, c.paint_order));
}

fn collect_node_recursive<M: Clone + 'static>(
    node: &RetainedNode<M>,
    commands: &mut Vec<DrawCommand>,
    hit_nodes: &mut Vec<HitTestNode<M>>,
    order: &mut usize,
) {
    if !node.visible {
        return;
    }

    let paint_order = *order;
    *order += 1;

    if (node.input_enabled && node.handlers.has_pointer_handlers())
        || node.handlers.on_key.is_some()
    {
        hit_nodes.push(HitTestNode {
            node_id: node.node_id,
            local_rect: node.bounds,
            affine: node.affine,
            inverse_affine: node.affine.inverse(),
            clip_rect: node.clip_rect,
            z_index: node.z_index,
            paint_order,
            visible: node.visible,
            input_enabled: node.input_enabled,
            handlers: node.handlers.clone(),
        });
    }

    let payload = match &node.visual {
        RetainedVisual::Container => None,
        RetainedVisual::Rect {
            fill,
            corner_radius,
            border,
        } => Some(PaintPayload::Rect {
            fill: *fill,
            corner_radius: *corner_radius,
            border: *border,
        }),
        RetainedVisual::Text {
            content,
            font_size,
            line_height_multiplier,
            color,
            font,
            align,
            bounds_mode,
            wrap_to_width,
        } => Some(PaintPayload::Text {
            content: content.clone(),
            font_size: *font_size,
            line_height_multiplier: *line_height_multiplier,
            color: *color,
            font: font.clone(),
            align: *align,
            bounds_mode: *bounds_mode,
            wrap_to_width: *wrap_to_width,
        }),
        RetainedVisual::Image {
            asset,
            asset_revision,
            fit,
            filter,
            source_rect,
        } => Some(PaintPayload::Image {
            asset: *asset,
            asset_revision: *asset_revision,
            fit: *fit,
            filter: *filter,
            source_rect: *source_rect,
        }),
        RetainedVisual::Sprite {
            sheet,
            asset_revision,
            resolved_frame,
            filter,
            ..
        } => Some(PaintPayload::Sprite {
            sheet: sheet.clone(),
            asset_revision: *asset_revision,
            frame_index: *resolved_frame,
            filter: *filter,
        }),
        RetainedVisual::Canvas { render_key, draw } => Some(PaintPayload::Canvas {
            render_key: *render_key,
            draw: draw.clone(),
        }),
        RetainedVisual::Clip { corner_radius } => Some(PaintPayload::ClipPush {
            corner_radius: *corner_radius,
        }),
    };

    if let Some(payload) = payload {
        let is_clip = matches!(payload, PaintPayload::ClipPush { .. });
        commands.push(DrawCommand {
            node_id: node.node_id,
            local_rect: node.bounds,
            affine: node.affine,
            world_bounds: node.world_bounds,
            clip_rect: node.clip_rect,
            z_index: node.z_index,
            paint_order,
            opacity: node.opacity,
            payload,
        });
        for child in &node.children {
            collect_node_recursive(child, commands, hit_nodes, order);
        }
        if is_clip {
            let pop_order = *order;
            *order += 1;
            commands.push(DrawCommand {
                node_id: node.node_id,
                local_rect: node.bounds,
                affine: node.affine,
                world_bounds: node.world_bounds,
                clip_rect: node.clip_rect,
                z_index: node.z_index,
                paint_order: pop_order,
                opacity: node.opacity,
                payload: PaintPayload::ClipPop,
            });
        }
    } else {
        for child in &node.children {
            collect_node_recursive(child, commands, hit_nodes, order);
        }
    }
}
