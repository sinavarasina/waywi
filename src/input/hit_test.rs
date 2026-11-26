use crate::geometry::{Affine2D, Bounds, Point};

use super::cursor::CursorShape;
use super::events::EventHandlers;
use super::keys::NodeId;

/// Hit-testable resolved node entry retained for one surface.
pub(crate) struct HitTestNode<M> {
    pub(crate) node_id: NodeId,
    /// Un-transformed local layout bounds `[0..w, 0..h]` in element space (actually surface-space
    /// origin + size before local transform).
    pub(crate) local_rect: Bounds,
    /// Forward transform from surface-local space -> world affine maps surface-local layout rect to
    /// transformed surface-local coordinates.
    pub(crate) affine: Affine2D,
    pub(crate) inverse_affine: Option<Affine2D>,
    pub(crate) clip_rect: Option<Bounds>,
    pub(crate) z_index: i32,
    pub(crate) paint_order: usize,
    pub(crate) visible: bool,
    pub(crate) input_enabled: bool,
    pub(crate) cursor: Option<CursorShape>,
    pub(crate) handlers: EventHandlers<M>,
}

impl<M: 'static> HitTestNode<M> {
    /// Returns the world-space bounding box used for selective `wl_region` input regions.
    #[must_use]
    pub(crate) fn world_bounds(&self) -> Bounds {
        let transformed = self.affine.transform_rect(self.local_rect);
        match self.clip_rect {
            Some(clip) => transformed.intersect(clip).unwrap_or(Bounds::ZERO),
            None => transformed,
        }
    }

    /// Projects a surface-local point into this element's local coordinate space without clamping
    /// to bounds (used for captured touch/stylus drags that leave the element's bounding rect).
    #[must_use]
    pub(crate) fn surface_point_to_local(&self, p: Point) -> Point {
        if let Some(inv) = self.inverse_affine {
            let un_transformed = inv.transform_point(p);
            Point::new(
                un_transformed.x - self.local_rect.x,
                un_transformed.y - self.local_rect.y,
            )
        } else {
            Point::new(p.x - self.local_rect.x, p.y - self.local_rect.y)
        }
    }

    #[must_use]
    pub(crate) fn contains_surface_point(&self, p: Point) -> Option<Point> {
        if !self.visible || !self.input_enabled {
            return None;
        }
        if let Some(clip) = self.clip_rect
            && !clip.contains(p)
        {
            return None;
        }
        let inv = self.inverse_affine?;
        let un_transformed = inv.transform_point(p);
        if self.local_rect.contains(un_transformed) {
            Some(Point::new(
                un_transformed.x - self.local_rect.x,
                un_transformed.y - self.local_rect.y,
            ))
        } else {
            None
        }
    }
}

/// Hit-tests surface-local `point` against retained nodes matching `predicate`, returning the
/// topmost hit node and element-local coordinates.
pub(crate) fn hit_test_where<M: 'static, F>(
    nodes: &[HitTestNode<M>],
    point: Point,
    mut predicate: F,
) -> Option<(&HitTestNode<M>, Point)>
where
    F: FnMut(&HitTestNode<M>) -> bool,
{
    let mut best: Option<((i32, usize), &HitTestNode<M>, Point)> = None;
    for node in nodes {
        if !predicate(node) {
            continue;
        }
        if let Some(local) = node.contains_surface_point(point) {
            let rank = (node.z_index, node.paint_order);
            if best.is_none_or(|(best_rank, _, _)| rank > best_rank) {
                best = Some((rank, node, local));
            }
        }
    }
    best.map(|(_, node, local)| (node, local))
}

/// Hit-tests surface-local `point` against retained interactive nodes, returning the topmost hit
/// node and element-local coordinates.
pub(crate) fn hit_test<M: 'static>(
    nodes: &[HitTestNode<M>],
    point: Point,
) -> Option<(&HitTestNode<M>, Point)> {
    hit_test_where(nodes, point, |n| n.handlers.has_pointer_handlers())
}

/// Hit-tests surface-local `point` against retained nodes that declare an explicit [`CursorShape`],
/// returning the topmost matching cursor shape.
pub(crate) fn hit_test_cursor<M: 'static>(
    nodes: &[HitTestNode<M>],
    point: Point,
) -> Option<CursorShape> {
    hit_test_where(nodes, point, |n| n.cursor.is_some()).and_then(|(n, _)| n.cursor)
}

/// Single-pass hit-test returning both the topmost interactive pointer node (and local coords)
/// and the topmost explicit [`CursorShape`] at `point`, avoiding scanning `hit_nodes` twice per
/// pointer motion event.
pub(crate) fn hit_test_pointer_and_cursor<M: 'static>(
    nodes: &[HitTestNode<M>],
    point: Point,
) -> (Option<(&HitTestNode<M>, Point)>, Option<CursorShape>) {
    let mut best_ptr: Option<((i32, usize), &HitTestNode<M>, Point)> = None;
    let mut best_cur: Option<((i32, usize), CursorShape)> = None;
    for node in nodes {
        let wants_ptr = node.handlers.has_pointer_handlers();
        let cursor = node.cursor;
        if !wants_ptr && cursor.is_none() {
            continue;
        }
        if let Some(local) = node.contains_surface_point(point) {
            let rank = (node.z_index, node.paint_order);
            if wants_ptr && best_ptr.is_none_or(|(best_rank, _, _)| rank > best_rank) {
                best_ptr = Some((rank, node, local));
            }
            if let Some(shape) = cursor
                && best_cur.is_none_or(|(best_rank, _)| rank > best_rank)
            {
                best_cur = Some((rank, shape));
            }
        }
    }
    (
        best_ptr.map(|(_, node, local)| (node, local)),
        best_cur.map(|(_, shape)| shape),
    )
}

/// Hit-tests surface-local `point` against retained nodes that bind `on_scroll`.
pub(crate) fn hit_test_scroll<M: 'static>(
    nodes: &[HitTestNode<M>],
    point: Point,
) -> Option<(&HitTestNode<M>, Point)> {
    hit_test_where(nodes, point, |n| n.handlers.on_scroll.is_some())
}

/// Hit-tests surface-local `point` against retained nodes that bind `on_touch`.
pub(crate) fn hit_test_touch<M: 'static>(
    nodes: &[HitTestNode<M>],
    point: Point,
) -> Option<(&HitTestNode<M>, Point)> {
    hit_test_where(nodes, point, |n| n.handlers.on_touch.is_some())
}

/// Hit-tests surface-local `point` against retained nodes that bind `on_gesture`.
pub(crate) fn hit_test_gesture<M: 'static>(
    nodes: &[HitTestNode<M>],
    point: Point,
) -> Option<(&HitTestNode<M>, Point)> {
    hit_test_where(nodes, point, |n| n.handlers.on_gesture.is_some())
}

/// Hit-tests surface-local `point` against retained nodes that bind `on_tablet`.
pub(crate) fn hit_test_tablet<M: 'static>(
    nodes: &[HitTestNode<M>],
    point: Point,
) -> Option<(&HitTestNode<M>, Point)> {
    hit_test_where(nodes, point, |n| n.handlers.on_tablet.is_some())
}
