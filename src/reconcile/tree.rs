pub(crate) mod node;

use std::collections::HashMap;

use crate::asset::AssetCache;
use crate::geometry::{
    Affine2D, Bounds, Edges, Length, LogicalPx, LogicalSize, Position, ScaleFactor,
};
use crate::input::{ElementKey, NodeId};
use crate::render::{Align, Element, ElementKind, Justify, RenderBackend, RetainedNode};

use super::animation::collect_subtree_animation_state;
use super::diff::Reconciler;
use super::layout::{
    LayoutFlow, element_kind_tag, measure_element_intrinsic, resolve_clamped_length,
};

#[derive(Clone, Copy)]
pub(super) struct ParentScope {
    pub(super) rect: Bounds,
    pub(super) affine: Affine2D,
    pub(super) clip_rect: Option<Bounds>,
}

impl ParentScope {
    pub(super) fn new(rect: Bounds, affine: Affine2D, clip_rect: Option<Bounds>) -> Self {
        Self {
            rect,
            affine,
            clip_rect,
        }
    }
}

#[derive(Clone, Copy)]
pub(super) struct FlowLayout {
    pub(super) flow: LayoutFlow,
    pub(super) gap: LogicalPx,
    pub(super) cross_align: Align,
    pub(super) justify: Justify,
    pub(super) padding: Edges,
}

impl FlowLayout {
    pub(super) fn stack() -> Self {
        Self {
            flow: LayoutFlow::Stack,
            gap: LogicalPx::ZERO,
            cross_align: Align::Start,
            justify: Justify::Start,
            padding: Edges::ZERO,
        }
    }
}

pub(super) struct TreePass<'a, R: RenderBackend> {
    pub(super) renderer: &'a mut R,
    pub(super) assets: &'a AssetCache,
    pub(super) now: std::time::Instant,
    pub(super) frame_seq: u64,
    pub(super) scale: ScaleFactor,
    pub(super) surface_size: LogicalSize,
    pub(super) dirty_rects: &'a mut Vec<Bounds>,
    pub(super) skipped_memo: &'a mut usize,
    pub(super) retired_ids: &'a mut Vec<NodeId>,
    pub(super) anim_deadline: &'a mut Option<std::time::Instant>,
    pub(super) has_active_transitions: &'a mut bool,
}

impl<M: Clone + 'static> Reconciler<M> {
    pub(super) fn reconcile_children_positioned<R: RenderBackend>(
        &mut self,
        declared: impl IntoIterator<Item = Element<M>>,
        mut prev_nodes: Vec<RetainedNode<M>>,
        parent: ParentScope,
        flow: FlowLayout,
        pass: &mut TreePass<'_, R>,
    ) -> Vec<RetainedNode<M>> {
        let mut prev_slots: Vec<Option<RetainedNode<M>>> = prev_nodes.drain(..).map(Some).collect();
        let mut keyed_map: HashMap<ElementKey, usize> = HashMap::new();
        for (i, opt) in prev_slots.iter().enumerate() {
            if let Some(n) = opt
                && let Some(k) = n.key
            {
                keyed_map.insert(k, i);
            }
        }

        let padding = flow.padding;
        let inner_rect = Bounds::new(
            parent.rect.x + padding.left,
            parent.rect.y + padding.top,
            (parent.rect.width - padding.left - padding.right).max(LogicalPx::ZERO),
            (parent.rect.height - padding.top - padding.bottom).max(LogicalPx::ZERO),
        );
        let child_scope = ParentScope::new(inner_rect, parent.affine, parent.clip_rect);

        let declared_vec: Vec<Element<M>> = declared.into_iter().collect();
        let needs_two_pass = flow.flow != LayoutFlow::Stack
            && (flow.justify != Justify::Start
                || declared_vec
                    .iter()
                    .any(|el| el.common().is_some_and(|c| c.flex > 0.0)));

        let mut main_overrides: Vec<Option<LogicalPx>> = Vec::new();
        let mut cursor_main = LogicalPx::ZERO;
        let mut effective_flow = flow;

        if needs_two_pass {
            let is_row = flow.flow == LayoutFlow::Row;
            let avail_main = if is_row {
                inner_rect.width
            } else {
                inner_rect.height
            };
            main_overrides.resize(declared_vec.len(), None);
            let mut base_mains = vec![LogicalPx::ZERO; declared_vec.len()];
            let mut is_flow_item = vec![false; declared_vec.len()];
            let mut flex_weights = vec![0.0_f32; declared_vec.len()];
            let mut total_fixed = LogicalPx::ZERO;
            let mut total_flex = 0.0_f32;
            let mut flow_count = 0usize;

            for (i, el) in declared_vec.iter().enumerate() {
                if let ElementKind::MemoHit { key } = el.kind {
                    if let Some(&idx) = keyed_map.get(&key)
                        && let Some(retained) = prev_slots[idx].as_ref()
                    {
                        let m = if is_row {
                            retained.bounds.width
                        } else {
                            retained.bounds.height
                        };
                        base_mains[i] = m;
                        is_flow_item[i] = true;
                        total_fixed = total_fixed + m;
                        flow_count += 1;
                    }
                    continue;
                }
                let Some(c) = el.common() else {
                    continue;
                };
                if !matches!(c.position, Position::Auto) {
                    continue;
                }
                is_flow_item[i] = true;
                flow_count += 1;
                if c.flex > 0.0 {
                    flex_weights[i] = c.flex;
                    total_flex += c.flex;
                } else {
                    let intrinsic = if matches!(c.size.width, Length::Auto)
                        || matches!(c.size.height, Length::Auto)
                    {
                        measure_element_intrinsic(
                            el,
                            inner_rect,
                            pass.scale,
                            pass.renderer,
                            pass.assets,
                            pass.frame_seq,
                        )
                    } else {
                        LogicalSize::ZERO
                    };
                    let m = if is_row {
                        resolve_clamped_length(
                            c.size.width,
                            inner_rect.width,
                            intrinsic.width,
                            c.min_width,
                            c.max_width,
                        )
                    } else {
                        resolve_clamped_length(
                            c.size.height,
                            inner_rect.height,
                            intrinsic.height,
                            c.min_height,
                            c.max_height,
                        )
                    };
                    base_mains[i] = m;
                    total_fixed = total_fixed + m;
                }
            }

            let total_gaps = if flow_count > 1 {
                flow.gap * ((flow_count - 1) as f32)
            } else {
                LogicalPx::ZERO
            };

            let mut used_main = total_fixed + total_gaps;
            if total_flex > 0.0 {
                let flex_pool = (avail_main - total_fixed - total_gaps).max(LogicalPx::ZERO);
                for (i, el) in declared_vec.iter().enumerate() {
                    if flex_weights[i] <= 0.0 {
                        continue;
                    }
                    let share = LogicalPx(flex_pool.0 * (flex_weights[i] / total_flex));
                    let clamped = if let Some(c) = el.common() {
                        if is_row {
                            super::layout::clamp_dimension(share, c.min_width, c.max_width)
                        } else {
                            super::layout::clamp_dimension(share, c.min_height, c.max_height)
                        }
                    } else {
                        share
                    };
                    main_overrides[i] = Some(clamped);
                    used_main = used_main + clamped;
                }
            }

            let free_space = (avail_main - used_main).max(LogicalPx::ZERO);
            let (start_offset, extra_gap) = match flow.justify {
                Justify::Start => (LogicalPx::ZERO, LogicalPx::ZERO),
                Justify::Center => (LogicalPx(free_space.0 * 0.5), LogicalPx::ZERO),
                Justify::End => (free_space, LogicalPx::ZERO),
                Justify::SpaceBetween => {
                    if flow_count > 1 {
                        (
                            LogicalPx::ZERO,
                            LogicalPx(free_space.0 / (flow_count - 1) as f32),
                        )
                    } else {
                        (LogicalPx::ZERO, LogicalPx::ZERO)
                    }
                }
                Justify::SpaceAround => {
                    if flow_count > 0 {
                        let step = free_space.0 / flow_count as f32;
                        (LogicalPx(step * 0.5), LogicalPx(step))
                    } else {
                        (LogicalPx::ZERO, LogicalPx::ZERO)
                    }
                }
                Justify::SpaceEvenly => {
                    if flow_count > 0 {
                        let step = free_space.0 / (flow_count + 1) as f32;
                        (LogicalPx(step), LogicalPx(step))
                    } else {
                        (LogicalPx::ZERO, LogicalPx::ZERO)
                    }
                }
            };
            cursor_main = start_offset;
            effective_flow.gap = flow.gap + extra_gap;
        }

        let mut out = Vec::with_capacity(declared_vec.len());
        let mut unkeyed_cursor = 0usize;

        for (i, el) in declared_vec.into_iter().enumerate() {
            // Handle O(1) memo hit first!
            if let ElementKind::MemoHit { key } = el.kind {
                if let Some(&idx) = keyed_map.get(&key)
                    && let Some(retained) = prev_slots[idx].take()
                {
                    *pass.skipped_memo += 1;
                    collect_subtree_animation_state(
                        &retained,
                        pass.anim_deadline,
                        pass.has_active_transitions,
                    );
                    if let LayoutFlow::Row = effective_flow.flow {
                        cursor_main = cursor_main + retained.bounds.width + effective_flow.gap;
                    } else if let LayoutFlow::Column = effective_flow.flow {
                        cursor_main = cursor_main + retained.bounds.height + effective_flow.gap;
                    }
                    out.push(retained);
                }
                continue;
            }

            let kind_tag = element_kind_tag(&el);
            let el_key = el.common().and_then(|c| c.key);

            // Match retained node either by explicit key or by positional kind_tag.
            let matched_prev = if let Some(k) = el_key {
                keyed_map.get(&k).and_then(|&idx| prev_slots[idx].take())
            } else {
                let mut found = None;
                while unkeyed_cursor < prev_slots.len() {
                    let take = prev_slots[unkeyed_cursor]
                        .as_ref()
                        .is_some_and(|n| n.key.is_none() && n.kind_tag == kind_tag);
                    if take {
                        found = prev_slots[unkeyed_cursor].take();
                        unkeyed_cursor += 1;
                        break;
                    }
                    unkeyed_cursor += 1;
                }
                found
            };

            let main_override = main_overrides.get(i).copied().flatten();
            let retained = self.build_or_update_node(
                el,
                matched_prev,
                child_scope,
                effective_flow,
                &mut cursor_main,
                main_override,
                pass,
            );
            out.push(retained);
        }

        // Any unmatched previous siblings were removed -> mark old bounds dirty & free IDs.
        for leftover in prev_slots.into_iter().flatten() {
            if leftover.visible {
                pass.dirty_rects.push(leftover.world_bounds);
            }
            self.retire_node_recursive(leftover, pass.retired_ids);
        }

        out
    }

    fn retire_node_recursive(&mut self, node: RetainedNode<M>, out: &mut Vec<NodeId>) {
        out.push(node.node_id);
        self.node_ids.free(node.node_id);
        for child in node.children {
            self.retire_node_recursive(child, out);
        }
    }
}
