pub(crate) mod node;

use std::collections::HashMap;

use crate::asset::AssetCache;
use crate::geometry::{Affine2D, Edges, LogicalPx, LogicalSize, Rect as GeoRect};
use crate::input::{ElementKey, NodeId};
use crate::render::{Align, Element, ElementKind, RenderBackend, RetainedNode};

use super::animation::collect_subtree_deadlines;
use super::diff::Reconciler;
use super::layout::{LayoutFlow, element_kind_tag};

#[derive(Clone, Copy)]
pub(super) struct ParentScope {
    pub(super) rect: GeoRect,
    pub(super) affine: Affine2D,
    pub(super) clip_rect: Option<GeoRect>,
}

impl ParentScope {
    pub(super) fn new(rect: GeoRect, affine: Affine2D, clip_rect: Option<GeoRect>) -> Self {
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
    pub(super) padding: Edges,
}

impl FlowLayout {
    pub(super) fn stack() -> Self {
        Self {
            flow: LayoutFlow::Stack,
            gap: LogicalPx::ZERO,
            cross_align: Align::Start,
            padding: Edges::ZERO,
        }
    }
}

pub(super) struct TreePass<'a, R: RenderBackend> {
    pub(super) renderer: &'a mut R,
    pub(super) assets: &'a AssetCache,
    pub(super) now: std::time::Instant,
    pub(super) frame_seq: u64,
    pub(super) surface_size: LogicalSize,
    pub(super) dirty_rects: &'a mut Vec<GeoRect>,
    pub(super) skipped_memo: &'a mut usize,
    pub(super) retired_ids: &'a mut Vec<NodeId>,
    pub(super) anim_deadline: &'a mut Option<std::time::Instant>,
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
        let inner_rect = GeoRect::new(
            parent.rect.x + padding.left,
            parent.rect.y + padding.top,
            (parent.rect.width - padding.left - padding.right).max(LogicalPx::ZERO),
            (parent.rect.height - padding.top - padding.bottom).max(LogicalPx::ZERO),
        );
        let child_scope = ParentScope::new(inner_rect, parent.affine, parent.clip_rect);

        let items: Vec<Element<M>> = declared.into_iter().collect();
        let mut out = Vec::with_capacity(items.len());
        let mut cursor_main = LogicalPx::ZERO;
        let mut unkeyed_cursor = 0usize;

        for el in items {
            // Handle $O(1)$ memo hit first!
            if let ElementKind::MemoHit { key } = el.kind {
                if let Some(&idx) = keyed_map.get(&key)
                    && let Some(retained) = prev_slots[idx].take()
                {
                    *pass.skipped_memo += 1;
                    collect_subtree_deadlines(&retained, pass.anim_deadline);
                    if let LayoutFlow::Row = flow.flow {
                        cursor_main = cursor_main + retained.bounds.width + flow.gap;
                    } else if let LayoutFlow::Column = flow.flow {
                        cursor_main = cursor_main + retained.bounds.height + flow.gap;
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

            let retained = self.build_or_update_node(
                el,
                matched_prev,
                child_scope,
                flow,
                &mut cursor_main,
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
