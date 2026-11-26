use std::time::Instant;

use crate::geometry::{Affine2D, Bounds};
use crate::image::SourceRect;
use crate::input::NodeId;
use crate::render::{RetainedNode, RetainedVisual};
use crate::sprite::SpriteSourceState;

pub(crate) fn collect_subtree_animation_state<M>(
    node: &RetainedNode<M>,
    out_deadline: &mut Option<Instant>,
    has_active_transitions: &mut bool,
) {
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
        *out_deadline = Some(out_deadline.map_or(dl, |p| p.min(dl)));
    }
    if node.transitions.as_ref().is_some_and(|t| t.is_active()) {
        *has_active_transitions = true;
    }
    for ch in &node.children {
        collect_subtree_animation_state(ch, out_deadline, has_active_transitions);
    }
}

pub(crate) struct NodeTickCtx<'a, M> {
    pub(crate) now: Instant,
    pub(crate) changed_frames: &'a mut Vec<(NodeId, SourceRect)>,
    pub(crate) dirty_rects: &'a mut Vec<Bounds>,
    pub(crate) next_deadline: &'a mut Option<Instant>,
    pub(crate) emitted_messages: &'a mut Vec<M>,
    pub(crate) transitions_changed: &'a mut bool,
    pub(crate) has_active_transitions: &'a mut bool,
}

/// Single-pass top-down animation ticker for sprite timelines and retained element transitions.
///
/// Recomputes world transforms directly from `node.local_affine.then(&parent_affine)` whenever a
/// node or any ancestor changes transform, avoiding `Affine2D::inverse()` singularities (such as
/// animating out of `Transform::scale(0.0)`) and visiting each descendant exactly once.
pub(crate) fn tick_node_animations<M: Clone>(
    node: &mut RetainedNode<M>,
    parent_affine: Affine2D,
    parent_affine_changed: bool,
    parent_clip: Option<Bounds>,
    ctx: &mut NodeTickCtx<'_, M>,
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
            ctx.now,
            true,
            node.on_animation_end.is_some(),
        );
        if adv.frame_changed {
            *resolved_frame = adv.frame;
            ctx.changed_frames.push((
                node.node_id,
                SourceRect::from(sheet.frame_source_rect(adv.frame)),
            ));
            ctx.dirty_rects.push(node.world_bounds);
        }
        if adv.just_finished
            && let Some(ref msg) = node.on_animation_end
        {
            ctx.emitted_messages.push(msg.clone());
        }
        if let Some(d) = adv.next_deadline {
            *ctx.next_deadline = Some(ctx.next_deadline.map_or(d, |p| p.min(d)));
        }
    }

    let old_bounds = node.world_bounds;
    let mut opacity_changed = false;
    let mut local_transform_changed = false;

    if cfg!(feature = "animation")
        && let Some(tr) = node.transitions.as_mut()
    {
        if let Some(op) = tr.opacity.as_mut()
            && op.advance(ctx.now)
        {
            let clamped = op.value().clamp(0.0, 1.0);
            if (node.opacity - clamped).abs() > f32::EPSILON {
                node.opacity = clamped;
                opacity_changed = true;
            }
        }

        if let Some(tf) = tr.transform.as_mut()
            && tf.advance(ctx.now)
        {
            let t = tf.value();
            node.local_affine = if t.is_identity() {
                Affine2D::IDENTITY
            } else {
                t.to_affine_around(t.pivot_point(node.bounds))
            };
            local_transform_changed = true;
        }

        if tr.is_active() {
            *ctx.has_active_transitions = true;
        }
    }

    let affine_changed = local_transform_changed || parent_affine_changed;
    if affine_changed {
        node.affine = node.local_affine.then(&parent_affine);
        node.world_bounds = node.affine.transform_rect(node.bounds);
        node.clip_rect = parent_clip;
    }

    if opacity_changed || affine_changed {
        *ctx.transitions_changed = true;
        if !matches!(node.visual, RetainedVisual::Container) {
            ctx.dirty_rects.push(old_bounds);
            if node.world_bounds != old_bounds {
                ctx.dirty_rects.push(node.world_bounds);
            }
        }
    }

    let child_clip = if matches!(node.visual, RetainedVisual::Clip { .. }) {
        match parent_clip {
            Some(prev_c) => prev_c.intersect(node.world_bounds),
            None => Some(node.world_bounds),
        }
    } else {
        parent_clip
    };
    let cur_affine = node.affine;
    for ch in &mut node.children {
        tick_node_animations(ch, cur_affine, affine_changed, child_clip, ctx);
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
            local_affine: Affine2D::IDENTITY,
            affine: Affine2D::IDENTITY,
            clip_rect: None,
            z_index: 0,
            visible: true,
            input_enabled: false,
            opacity: 1.0,
            cursor: None,
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
            transitions: None,
            children: Vec::new(),
        };

        let mut collected = None;
        let mut active_tr = false;
        collect_subtree_animation_state(&node, &mut collected, &mut active_tr);
        assert_eq!(collected, adv.next_deadline);
        assert_eq!(collected, Some(t0 + Duration::from_millis(300)));
        assert!(!active_tr);
    }
}
