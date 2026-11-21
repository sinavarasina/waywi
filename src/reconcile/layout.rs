use crate::asset::AssetCache;
use crate::geometry::{Edges, Length, LogicalPx, LogicalSize, Rect as GeoRect, Size};
use crate::input::HitTestNode;
use crate::render::{Element, ElementKind, RenderBackend};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum LayoutFlow {
    Stack,
    Row,
    Column,
}

pub(crate) fn element_kind_tag<M>(el: &Element<M>) -> u8 {
    match &el.kind {
        ElementKind::Text(_) => 1,
        ElementKind::Image(_) => 2,
        ElementKind::Sprite(_) => 3,
        ElementKind::Rect(_) => 4,
        ElementKind::Canvas(_) => 5,
        ElementKind::Group(_) => 6,
        ElementKind::Row(_) => 7,
        ElementKind::Column(_) => 8,
        ElementKind::Clip(_) => 9,
        ElementKind::MemoHit { .. } => 10,
    }
}

pub(crate) fn resolve_length(len: Length, available: LogicalPx, intrinsic: LogicalPx) -> LogicalPx {
    match len {
        Length::Px(px) => px.max(LogicalPx::ZERO),
        Length::Percent(pct) => pct.of(available).max(LogicalPx::ZERO),
        Length::Fill => available.max(LogicalPx::ZERO),
        Length::Auto => intrinsic.max(LogicalPx::ZERO),
    }
}

fn resolve_intrinsic_child(len: Length, intrinsic: LogicalPx) -> LogicalPx {
    // Extrinsic constraints (`Fill`/`Percent`) depend on the parent container's final size and
    // must not expand an `Auto` container to its grandparent's bounds during intrinsic sizing.
    match len {
        Length::Px(px) => px.max(LogicalPx::ZERO),
        Length::Auto => intrinsic.max(LogicalPx::ZERO),
        Length::Percent(_) | Length::Fill => LogicalPx::ZERO,
    }
}

pub(crate) fn resolve_surface_size(size: Size, output: LogicalSize, margin: Edges) -> LogicalSize {
    let avail_w = (output.width - margin.left - margin.right).max(LogicalPx(1.0));
    let avail_h = (output.height - margin.top - margin.bottom).max(LogicalPx(1.0));
    let w = match size.width {
        Length::Px(px) => px.max(LogicalPx(1.0)),
        Length::Percent(pct) => pct.of(output.width).max(LogicalPx(1.0)),
        Length::Fill | Length::Auto => avail_w,
    };
    let h = match size.height {
        Length::Px(px) => px.max(LogicalPx(1.0)),
        Length::Percent(pct) => pct.of(output.height).max(LogicalPx(1.0)),
        Length::Fill | Length::Auto => avail_h,
    };
    LogicalSize::new(w, h)
}

pub(crate) fn resolve_surface_size_with_children<M: 'static>(
    size: Size,
    children: &[Element<M>],
    output: LogicalSize,
    margin: Edges,
    renderer: &mut impl RenderBackend,
    assets: &AssetCache,
    frame_seq: u64,
) -> LogicalSize {
    let base = resolve_surface_size(size, output, margin);
    if !matches!(size.width, Length::Auto) && !matches!(size.height, Length::Auto) {
        return base;
    }
    let avail_w = (output.width - margin.left - margin.right).max(LogicalPx(1.0));
    let avail_h = (output.height - margin.top - margin.bottom).max(LogicalPx(1.0));
    let parent_rect = GeoRect::new(LogicalPx::ZERO, LogicalPx::ZERO, avail_w, avail_h);
    let mut max_w = LogicalPx::ZERO;
    let mut max_h = LogicalPx::ZERO;
    for ch in children {
        let s = measure_element_intrinsic(ch, parent_rect, renderer, assets, frame_seq);
        if let Some(c) = ch.common() {
            let cw = match c.size.width {
                Length::Px(px) => px.max(LogicalPx::ZERO),
                Length::Auto | Length::Fill => s.width.max(LogicalPx::ZERO),
                Length::Percent(p) => p.of(avail_w).max(LogicalPx::ZERO),
            };
            let ch_h = match c.size.height {
                Length::Px(px) => px.max(LogicalPx::ZERO),
                Length::Auto | Length::Fill => s.height.max(LogicalPx::ZERO),
                Length::Percent(p) => p.of(avail_h).max(LogicalPx::ZERO),
            };
            max_w = max_w.max(cw);
            max_h = max_h.max(ch_h);
        } else {
            max_w = max_w.max(s.width);
            max_h = max_h.max(s.height);
        }
    }
    LogicalSize::new(
        if matches!(size.width, Length::Auto) {
            max_w.min(avail_w).max(LogicalPx(1.0))
        } else {
            base.width
        },
        if matches!(size.height, Length::Auto) {
            max_h.min(avail_h).max(LogicalPx(1.0))
        } else {
            base.height
        },
    )
}

pub(crate) fn measure_element_intrinsic<M: 'static>(
    el: &Element<M>,
    parent_rect: GeoRect,
    renderer: &mut impl RenderBackend,
    assets: &AssetCache,
    frame_seq: u64,
) -> LogicalSize {
    match &el.kind {
        ElementKind::Text(t) => {
            let max_w = match t.common.size.width {
                Length::Px(w) => Some(w),
                Length::Percent(p) => Some(p.of(parent_rect.width)),
                Length::Fill => Some(parent_rect.width),
                Length::Auto => None,
            };
            renderer.measure_text(
                &t.content,
                crate::text::TextMeasureStyle {
                    font_size: t.font_size,
                    line_height_multiplier: t.line_height_multiplier,
                    max_width: max_w,
                    font: &t.font,
                    align: t.align,
                },
                frame_seq,
            )
        }
        ElementKind::Image(img) => {
            if let Some(sr) = img.source_rect {
                LogicalSize::new(LogicalPx(sr.width as f32), LogicalPx(sr.height as f32))
            } else if let Some(d) = assets.get(img.asset.id) {
                LogicalSize::new(LogicalPx(d.width as f32), LogicalPx(d.height as f32))
            } else {
                LogicalSize::new(
                    LogicalPx(img.asset.width as f32),
                    LogicalPx(img.asset.height as f32),
                )
            }
        }
        ElementKind::Sprite(sp) => {
            let active_idx = match &sp.mode {
                crate::sprite::SpriteSourceState::FixedFrame(idx) => *idx,
                crate::sprite::SpriteSourceState::Playing { animation, .. } => sp
                    .sheet
                    .get_animation(animation.as_str())
                    .map_or(0, |a| a.frames.first_frame()),
            };
            let (_, _, fw, fh) = sp.sheet.frame_source_rect(active_idx);
            LogicalSize::new(LogicalPx(fw as f32), LogicalPx(fh as f32))
        }
        ElementKind::Rect(_) | ElementKind::Canvas(_) => LogicalSize::ZERO,
        ElementKind::Group(g) => {
            let mut max_w = LogicalPx::ZERO;
            let mut max_h = LogicalPx::ZERO;
            for ch in g.children.iter() {
                let s = measure_element_intrinsic(ch, parent_rect, renderer, assets, frame_seq);
                if let Some(c) = ch.common() {
                    let cw = resolve_intrinsic_child(c.size.width, s.width);
                    let ch_h = resolve_intrinsic_child(c.size.height, s.height);
                    max_w = max_w.max(cw);
                    max_h = max_h.max(ch_h);
                }
            }
            LogicalSize::new(max_w, max_h)
        }
        ElementKind::Row(r) => {
            let mut sum_w = LogicalPx::ZERO;
            let mut max_h = LogicalPx::ZERO;
            let mut count = 0usize;
            for ch in r.children.iter() {
                let s = measure_element_intrinsic(ch, parent_rect, renderer, assets, frame_seq);
                if let Some(c) = ch.common() {
                    let cw = resolve_intrinsic_child(c.size.width, s.width);
                    let ch_h = resolve_intrinsic_child(c.size.height, s.height);
                    sum_w = sum_w + cw;
                    max_h = max_h.max(ch_h);
                    count += 1;
                }
            }
            if count > 1 {
                sum_w = sum_w + r.gap * ((count - 1) as f32);
            }
            LogicalSize::new(
                sum_w + r.padding.left + r.padding.right,
                max_h + r.padding.top + r.padding.bottom,
            )
        }
        ElementKind::Column(c) => {
            let mut max_w = LogicalPx::ZERO;
            let mut sum_h = LogicalPx::ZERO;
            let mut count = 0usize;
            for ch in c.children.iter() {
                let s = measure_element_intrinsic(ch, parent_rect, renderer, assets, frame_seq);
                if let Some(cm) = ch.common() {
                    let cw = resolve_intrinsic_child(cm.size.width, s.width);
                    let ch_h = resolve_intrinsic_child(cm.size.height, s.height);
                    max_w = max_w.max(cw);
                    sum_h = sum_h + ch_h;
                    count += 1;
                }
            }
            if count > 1 {
                sum_h = sum_h + c.gap * ((count - 1) as f32);
            }
            LogicalSize::new(
                max_w + c.padding.left + c.padding.right,
                sum_h + c.padding.top + c.padding.bottom,
            )
        }
        ElementKind::Clip(c) => c.child.first().map_or(LogicalSize::ZERO, |ch| {
            measure_element_intrinsic(ch, parent_rect, renderer, assets, frame_seq)
        }),
        ElementKind::MemoHit { .. } => LogicalSize::ZERO,
    }
}

pub(crate) fn compute_selective_input_rects<M: 'static>(
    hit_nodes: &[HitTestNode<M>],
) -> Vec<GeoRect> {
    hit_nodes
        .iter()
        .filter(|n| n.visible && n.input_enabled && n.handlers.has_pointer_handlers())
        .map(|n| n.world_bounds())
        .filter(|r| !r.is_empty())
        .collect()
}
