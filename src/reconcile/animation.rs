use std::time::Instant;

use crate::geometry::Rect as GeoRect;
use crate::render::{RetainedNode, RetainedVisual};
use crate::sprite::SpriteSourceState;

pub(crate) fn collect_subtree_deadlines<M>(node: &RetainedNode<M>, out: &mut Option<Instant>) {
    if !node.visible {
        return;
    }
    if let (
        Some(tl),
        RetainedVisual::Sprite {
            sheet,
            mode:
                SpriteSourceState::Playing {
                    animation,
                    playback_override,
                },
            ..
        },
    ) = (&node.sprite_timeline, &node.visual)
        && !tl.finished
        && let Some(def) = sheet.get_animation(animation.as_str())
    {
        let _ = playback_override;
        let dl = tl.started_at + def.fps.frame_duration();
        *out = Some(out.map_or(dl, |p| p.min(dl)));
    }
    for ch in &node.children {
        collect_subtree_deadlines(ch, out);
    }
}

pub(crate) fn tick_node_animations<M>(
    node: &mut RetainedNode<M>,
    now: Instant,
    changed: &mut bool,
    dirty_rects: &mut Vec<GeoRect>,
    next_deadline: &mut Option<Instant>,
) {
    if !node.visible {
        return;
    }
    if let (
        Some(tl),
        RetainedVisual::Sprite {
            sheet,
            mode:
                SpriteSourceState::Playing {
                    animation,
                    playback_override,
                },
            resolved_frame,
            ..
        },
    ) = (&mut node.sprite_timeline, &mut node.visual)
        && cfg!(feature = "animation")
    {
        let (frame, frame_changed, dl) =
            tl.advance(sheet, animation.as_str(), *playback_override, now);
        if frame_changed {
            *resolved_frame = frame;
            *changed = true;
            dirty_rects.push(node.world_bounds);
        }
        if let Some(d) = dl {
            *next_deadline = Some(next_deadline.map_or(d, |p| p.min(d)));
        }
    }
    for ch in &mut node.children {
        tick_node_animations(ch, now, changed, dirty_rects, next_deadline);
    }
}

pub(crate) fn update_asset_rev_in_tree<M>(
    nodes: &mut [RetainedNode<M>],
    asset_id: crate::asset::AssetId,
    new_rev: u64,
) -> bool {
    let mut hit = false;
    for node in nodes {
        match &mut node.visual {
            RetainedVisual::Image {
                asset,
                asset_revision,
                ..
            } if asset.id == asset_id => {
                *asset_revision = new_rev;
                hit = true;
            }
            RetainedVisual::Sprite {
                sheet,
                asset_revision,
                ..
            } if sheet.asset.id == asset_id => {
                *asset_revision = new_rev;
                hit = true;
            }
            _ => {}
        }
        if update_asset_rev_in_tree(&mut node.children, asset_id, new_rev) {
            hit = true;
        }
    }
    hit
}
