use std::time::Instant;

use crate::geometry::Bounds;
use crate::image::SourceRect;
use crate::input::NodeId;
use crate::render::{RetainedNode, RetainedVisual};
use crate::sprite::SpriteSourceState;

pub(crate) fn collect_subtree_deadlines<M>(node: &RetainedNode<M>, out: &mut Option<Instant>) {
    if !node.visible {
        return;
    }
    if let (
        Some(tl),
        RetainedVisual::Sprite {
            mode: SpriteSourceState::Playing { .. },
            ..
        },
    ) = (&node.sprite_timeline, &node.visual)
        && let Some(dl) = tl.next_deadline
    {
        *out = Some(out.map_or(dl, |p| p.min(dl)));
    }
    for ch in &node.children {
        collect_subtree_deadlines(ch, out);
    }
}

pub(crate) fn tick_node_animations<M: Clone>(
    node: &mut RetainedNode<M>,
    now: Instant,
    changed_frames: &mut Vec<(NodeId, SourceRect)>,
    dirty_rects: &mut Vec<Bounds>,
    next_deadline: &mut Option<Instant>,
    emitted_messages: &mut Vec<M>,
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
        let adv = tl.advance(
            sheet,
            animation.as_str(),
            *playback_override,
            now,
            true,
            node.on_animation_end.is_some(),
        );
        if adv.frame_changed {
            *resolved_frame = adv.frame;
            changed_frames.push((
                node.node_id,
                SourceRect::from(sheet.frame_source_rect(adv.frame)),
            ));
            dirty_rects.push(node.world_bounds);
        }
        if adv.just_finished
            && let Some(ref msg) = node.on_animation_end
        {
            emitted_messages.push(msg.clone());
        }
        if let Some(d) = adv.next_deadline {
            *next_deadline = Some(next_deadline.map_or(d, |p| p.min(d)));
        }
    }
    for ch in &mut node.children {
        tick_node_animations(
            ch,
            now,
            changed_frames,
            dirty_rects,
            next_deadline,
            emitted_messages,
        );
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

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use super::*;
    use crate::accessibility::AccessibleProps;
    use crate::asset::{AssetId, ImageAsset};
    use crate::geometry::Affine2D;
    use crate::image::ImageFilter;
    use crate::input::{EventHandlers, NodeId};
    use crate::sprite::{FpsExt, Frames, PlaybackMode, SpriteSheet, SpriteTimeline};
    use crate::text::SharedStr;

    #[test]
    fn collect_subtree_deadlines_preserves_future_step_deadline_not_first_frame() {
        let asset = ImageAsset {
            id: AssetId::new(1),
            width: 64,
            height: 16,
        };
        let sheet = SpriteSheet::new(asset)
            .frame_size(16, 16)
            .animation_with_mode("walk", Frames::range(0..4), 10.fps(), PlaybackMode::Loop);

        let t0 = Instant::now();
        let mut tl = SpriteTimeline::new(SharedStr::from_static("walk"), t0, 0);
        // Advance past 2.5 frames (250ms): current frame = 2, next deadline = t0 + 300ms.
        let adv = tl.advance(
            &sheet,
            "walk",
            None,
            t0 + Duration::from_millis(250),
            true,
            false,
        );
        assert_eq!(adv.frame, 2);
        assert_eq!(adv.next_deadline, Some(t0 + Duration::from_millis(300)));

        let node = RetainedNode::<()> {
            node_id: NodeId {
                index: 0,
                generation: 1,
            },
            key: None,
            kind_tag: 3,
            bounds: Bounds::ZERO,
            world_bounds: Bounds::ZERO,
            affine: Affine2D::IDENTITY,
            clip_rect: None,
            z_index: 0,
            visible: true,
            input_enabled: false,
            opacity: 1.0,
            handlers: EventHandlers::default(),
            visual: RetainedVisual::Sprite {
                sheet,
                asset_revision: 1,
                mode: SpriteSourceState::Playing {
                    animation: SharedStr::from_static("walk"),
                    playback_override: None,
                },
                resolved_frame: adv.frame,
                filter: ImageFilter::Nearest,
            },
            a11y: AccessibleProps::default(),
            sprite_timeline: Some(tl),
            on_animation_end: None,
            children: Vec::new(),
        };

        let mut collected = None;
        collect_subtree_deadlines(&node, &mut collected);
        assert_eq!(collected, adv.next_deadline);
        assert_eq!(collected, Some(t0 + Duration::from_millis(300)));
    }
}
