use std::collections::{HashMap, HashSet, VecDeque};

use accesskit::{
    Action, Node, NodeId as AkNodeId, Rect as AkRect, Role, TreeId, TreeInfo, TreeUpdate,
};

use crate::geometry::{LogicalPx, Point};
use crate::input::{Modifiers, MouseButton, NodeId, PointerEvent};
use crate::reconcile::{RetainedSurface, RetainedSurfaceKey};
use crate::render::{RetainedNode, RetainedVisual};
use crate::surface::SurfaceKind;

use super::props::AccessibleRole;

/// Fixed AccessKit identifier for the top-level WayWi application node.
pub(crate) const ROOT_NODE_ID: AkNodeId = AkNodeId(1);

/// Stable per-process mapping between retained WayWi IDs and `accesskit::NodeId`s.
pub(crate) struct AccessibilityTreeState {
    surface_ids: HashMap<RetainedSurfaceKey, u32>,
    next_surface_id: u32,
    pub(crate) focused_node: Option<AkNodeId>,
}

impl Default for AccessibilityTreeState {
    fn default() -> Self {
        Self {
            surface_ids: HashMap::new(),
            next_surface_id: 2,
            focused_node: None,
        }
    }
}

impl AccessibilityTreeState {
    fn surface_node_id(&mut self, key: RetainedSurfaceKey) -> AkNodeId {
        if let Some(&id) = self.surface_ids.get(&key) {
            return AkNodeId(u64::from(id));
        }
        let id = self.next_surface_id;
        self.next_surface_id = self.next_surface_id.checked_add(1).unwrap_or(2);
        while self.surface_ids.values().any(|&v| v == id) {
            self.next_surface_id = self.next_surface_id.checked_add(1).unwrap_or(2);
        }
        self.surface_ids.insert(key, id);
        AkNodeId(u64::from(id))
    }

    fn prune_removed_surfaces<M>(
        &mut self,
        surfaces: &HashMap<RetainedSurfaceKey, RetainedSurface<M>>,
    ) {
        self.surface_ids.retain(|k, _| surfaces.contains_key(k));
    }

    fn contains_surface_node(&self, id: AkNodeId) -> bool {
        if id == ROOT_NODE_ID || (id.0 >> 32) != 0 {
            return false;
        }
        let low = id.0 as u32;
        self.surface_ids.values().any(|&v| v == low)
    }
}

#[inline]
pub(crate) fn element_to_accesskit_id(id: NodeId) -> AkNodeId {
    // `NodeIdAllocator` keeps `generation >= 1`, so bit range 32..64 is always non-zero
    // and never collides with `ROOT_NODE_ID` (1) or surface node IDs (2..u32::MAX).
    AkNodeId((u64::from(id.generation) << 32) | u64::from(id.index))
}

#[inline]
pub(crate) fn decode_element_node_id(id: AkNodeId) -> Option<NodeId> {
    let generation = (id.0 >> 32) as u32;
    if generation == 0 {
        return None;
    }
    Some(NodeId {
        index: id.0 as u32,
        generation,
    })
}

fn map_explicit_role(role: AccessibleRole) -> Role {
    match role {
        AccessibleRole::Generic => Role::GenericContainer,
        AccessibleRole::Window => Role::Window,
        AccessibleRole::Panel => Role::Pane,
        AccessibleRole::Group | AccessibleRole::Row | AccessibleRole::Column => Role::Group,
        AccessibleRole::Button => Role::Button,
        AccessibleRole::Label => Role::Label,
        AccessibleRole::Image => Role::Image,
        AccessibleRole::Canvas => Role::Canvas,
        AccessibleRole::List => Role::List,
        AccessibleRole::ListItem => Role::ListItem,
        AccessibleRole::Switch => Role::Switch,
        AccessibleRole::CheckBox => Role::CheckBox,
        AccessibleRole::Slider => Role::Slider,
        AccessibleRole::ProgressIndicator => Role::ProgressIndicator,
        AccessibleRole::Status => Role::Status,
    }
}

fn infer_node_role<M>(node: &RetainedNode<M>) -> Role {
    if let Some(explicit) = node.a11y.role {
        return map_explicit_role(explicit);
    }
    if node.handlers.on_click.is_some() || node.handlers.on_pointer_down.is_some() {
        return Role::Button;
    }
    match node.visual {
        RetainedVisual::Text { .. } => Role::Label,
        RetainedVisual::Image { .. } | RetainedVisual::Sprite { .. } => Role::Image,
        RetainedVisual::Canvas { .. } => Role::Canvas,
        RetainedVisual::Rect { .. } | RetainedVisual::Container | RetainedVisual::Clip { .. } => {
            Role::GenericContainer
        }
    }
}

fn push_retained_node<M>(
    node: &RetainedNode<M>,
    out: &mut Vec<(AkNodeId, Node)>,
    live_ids: &mut HashSet<AkNodeId>,
) -> Option<AkNodeId> {
    if !node.visible || node.a11y.hidden {
        return None;
    }

    let ak_id = element_to_accesskit_id(node.node_id);
    live_ids.insert(ak_id);

    let mut ak_node = Node::new(infer_node_role(node));
    if let Some(ref label) = node.a11y.label {
        ak_node.set_label(label.as_str());
    } else if let RetainedVisual::Text { ref content, .. } = node.visual
        && !content.is_empty()
    {
        ak_node.set_label(content.as_str());
    }

    if let Some(ref desc) = node.a11y.description {
        ak_node.set_description(desc.as_str());
    }
    if let Some(ref val) = node.a11y.value {
        ak_node.set_value(val.as_str());
    }

    let r = node.world_bounds;
    if r.width.0 >= 0.0 && r.height.0 >= 0.0 {
        ak_node.set_bounds(AkRect {
            x0: f64::from(r.x.0),
            y0: f64::from(r.y.0),
            x1: f64::from(r.x.0 + r.width.0),
            y1: f64::from(r.y.0 + r.height.0),
        });
    }

    let clickable = node.handlers.on_click.is_some() || node.handlers.on_pointer_down.is_some();
    if clickable {
        ak_node.add_action(Action::Click);
        ak_node.add_action(Action::Focus);
    } else if node.handlers.on_key.is_some()
        || matches!(
            node.a11y.role,
            Some(
                AccessibleRole::Button
                    | AccessibleRole::Switch
                    | AccessibleRole::CheckBox
                    | AccessibleRole::Slider
            )
        )
    {
        ak_node.add_action(Action::Focus);
    }

    let mut child_ids = Vec::with_capacity(node.children.len());
    for child in &node.children {
        if let Some(cid) = push_retained_node(child, out, live_ids) {
            child_ids.push(cid);
        }
    }
    if !child_ids.is_empty() {
        ak_node.set_children(child_ids);
    }

    out.push((ak_id, ak_node));
    Some(ak_id)
}

/// Builds a full deterministic `TreeUpdate` from all retained surfaces.
pub(crate) fn build_tree_update<M: 'static>(
    state: &mut AccessibilityTreeState,
    surfaces: &HashMap<RetainedSurfaceKey, RetainedSurface<M>>,
    focused_surface: Option<RetainedSurfaceKey>,
    app_name: &str,
) -> TreeUpdate {
    state.prune_removed_surfaces(surfaces);

    let mut sorted_keys: Vec<RetainedSurfaceKey> = surfaces.keys().copied().collect();
    sorted_keys.sort_by_key(|k| (k.surface_key.0, k.output_id.map_or(0, |o| o.0)));

    let mut nodes: Vec<(AkNodeId, Node)> = Vec::new();
    let mut live_ids: HashSet<AkNodeId> = HashSet::new();
    live_ids.insert(ROOT_NODE_ID);

    let mut root_children = Vec::with_capacity(sorted_keys.len());
    let mut focused_surface_ak_id = None;

    for rk in sorted_keys {
        let Some(surf) = surfaces.get(&rk) else {
            continue;
        };
        if surf.spec.a11y.hidden {
            continue;
        }
        let surf_ak_id = state.surface_node_id(rk);
        live_ids.insert(surf_ak_id);
        root_children.push(surf_ak_id);

        if focused_surface == Some(rk) {
            focused_surface_ak_id = Some(surf_ak_id);
        }

        let default_role = match surf.spec.kind {
            SurfaceKind::Window => Role::Window,
            SurfaceKind::Panel | SurfaceKind::Floating | SurfaceKind::Overlay => Role::Pane,
        };
        let mut s_node = Node::new(surf.spec.a11y.role.map_or(default_role, map_explicit_role));
        if let Some(ref label) = surf.spec.a11y.label {
            s_node.set_label(label.as_str());
        } else if !surf.spec.title.is_empty() {
            s_node.set_label(surf.spec.title.as_str());
        }
        if let Some(ref desc) = surf.spec.a11y.description {
            s_node.set_description(desc.as_str());
        }

        let eff = surf.effective_logical_size();
        s_node.set_bounds(AkRect {
            x0: 0.0,
            y0: 0.0,
            x1: f64::from(eff.width.0),
            y1: f64::from(eff.height.0),
        });
        s_node.add_action(Action::Focus);

        let mut surf_children = Vec::with_capacity(surf.roots.len());
        for root in &surf.roots {
            if let Some(cid) = push_retained_node(root, &mut nodes, &mut live_ids) {
                surf_children.push(cid);
            }
        }
        if !surf_children.is_empty() {
            s_node.set_children(surf_children);
        }
        nodes.push((surf_ak_id, s_node));
    }

    let mut root_node = Node::new(Role::Application);
    root_node.set_label(if app_name.is_empty() {
        "waywi"
    } else {
        app_name
    });
    if !root_children.is_empty() {
        root_node.set_children(root_children);
    }
    nodes.push((ROOT_NODE_ID, root_node));

    if let Some(prev_focus) = state.focused_node
        && !live_ids.contains(&prev_focus)
    {
        state.focused_node = None;
    }

    let focus = state
        .focused_node
        .or(focused_surface_ak_id)
        .unwrap_or(ROOT_NODE_ID);

    let mut info = TreeInfo::new(ROOT_NODE_ID);
    info.toolkit_name = Some("waywi".to_string());
    info.toolkit_version = Some(env!("CARGO_PKG_VERSION").to_string());

    TreeUpdate {
        nodes,
        tree: Some(info),
        tree_id: TreeId::ROOT,
        focus,
    }
}

fn find_visible_node<M>(nodes: &[RetainedNode<M>], target: NodeId) -> Option<&RetainedNode<M>> {
    for node in nodes {
        if !node.visible || node.a11y.hidden {
            continue;
        }
        if node.node_id == target {
            return Some(node);
        }
        if let Some(found) = find_visible_node(&node.children, target) {
            return Some(found);
        }
    }
    None
}

/// Applies a single AccessKit `ActionRequest` into WayWi's message inbox cleanly.
pub(crate) fn apply_accessibility_action<M: Clone + 'static>(
    state: &mut AccessibilityTreeState,
    surfaces: &HashMap<RetainedSurfaceKey, RetainedSurface<M>>,
    action: Action,
    target: AkNodeId,
    inbox: &mut VecDeque<M>,
) -> bool {
    match action {
        Action::Focus => {
            if target == ROOT_NODE_ID || state.contains_surface_node(target) {
                state.focused_node = Some(target);
                return true;
            }
            if let Some(nid) = decode_element_node_id(target)
                && surfaces
                    .values()
                    .any(|s| !s.spec.a11y.hidden && find_visible_node(&s.roots, nid).is_some())
            {
                state.focused_node = Some(target);
                return true;
            }
            false
        }
        Action::Click => {
            let Some(nid) = decode_element_node_id(target) else {
                return false;
            };
            for surf in surfaces.values() {
                if surf.spec.a11y.hidden {
                    continue;
                }
                if let Some(node) = find_visible_node(&surf.roots, nid) {
                    if let Some(ref on_click) = node.handlers.on_click {
                        inbox.push_back(on_click.clone());
                        return true;
                    }
                    if let Some(ref on_down) = node.handlers.on_pointer_down {
                        let cx = node.world_bounds.x.0 + node.world_bounds.width.0 * 0.5;
                        let cy = node.world_bounds.y.0 + node.world_bounds.height.0 * 0.5;
                        let lx = node.world_bounds.width.0 * 0.5;
                        let ly = node.world_bounds.height.0 * 0.5;
                        inbox.push_back(on_down.call(PointerEvent {
                            position: Point::new(LogicalPx(cx), LogicalPx(cy)),
                            local_position: Point::new(LogicalPx(lx), LogicalPx(ly)),
                            button: Some(MouseButton::Left),
                            modifiers: Modifiers::empty(),
                        }));
                        return true;
                    }
                    return false;
                }
            }
            false
        }
        _ => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::accessibility::props::AccessibleProps;
    use crate::display::OutputId;
    use crate::geometry::{Affine2D, Edges, LogicalSize, Rect as GeoRect, ScaleFactor};
    use crate::input::EventHandlers;
    use crate::reconcile::SurfaceSpec;
    use crate::surface::{Anchor, InputMode, KeyboardInteractivity, Layer, SurfaceKey};
    use crate::text::{Font, SharedStr, TextAlign};

    #[test]
    fn encodes_element_ids_without_colliding_with_root_or_surfaces() {
        let mut tree_state = AccessibilityTreeState::default();
        let rk = RetainedSurfaceKey {
            surface_key: SurfaceKey::from_hashable(&"bar"),
            output_id: Some(OutputId(1)),
        };
        let sid = tree_state.surface_node_id(rk);
        let eid = element_to_accesskit_id(NodeId {
            index: 0,
            generation: 1,
        });
        assert_ne!(sid, ROOT_NODE_ID);
        assert_ne!(eid, ROOT_NODE_ID);
        assert_ne!(eid, sid);
        assert_eq!(decode_element_node_id(ROOT_NODE_ID), None);
        assert_eq!(decode_element_node_id(sid), None);
        assert_eq!(
            decode_element_node_id(eid),
            Some(NodeId {
                index: 0,
                generation: 1
            })
        );
    }

    #[test]
    fn builds_tree_and_dispatches_click_action_into_inbox() {
        let mut tree_state = AccessibilityTreeState::default();
        let rk = RetainedSurfaceKey {
            surface_key: SurfaceKey::from_hashable(&"panel"),
            output_id: Some(OutputId(1)),
        };
        let btn_id = NodeId {
            index: 0,
            generation: 1,
        };
        let btn_handlers = EventHandlers {
            on_click: Some("activated"),
            ..Default::default()
        };

        let btn_node = RetainedNode {
            node_id: btn_id,
            key: None,
            kind_tag: 0,
            bounds: GeoRect::new(
                LogicalPx(10.0),
                LogicalPx(4.0),
                LogicalPx(80.0),
                LogicalPx(28.0),
            ),
            world_bounds: GeoRect::new(
                LogicalPx(10.0),
                LogicalPx(4.0),
                LogicalPx(80.0),
                LogicalPx(28.0),
            ),
            affine: Affine2D::IDENTITY,
            clip_rect: None,
            z_index: 0,
            visible: true,
            input_enabled: true,
            opacity: 1.0,
            handlers: btn_handlers,
            visual: RetainedVisual::Text {
                content: SharedStr::from_static("Launch"),
                font_size: LogicalPx(14.0),
                line_height_multiplier: 1.25,
                wrap_to_width: false,
                color: crate::render::Color::WHITE,
                font: Font::default(),
                align: TextAlign::Left,
            },
            a11y: AccessibleProps {
                role: Some(AccessibleRole::Button),
                label: Some(SharedStr::from_static("Launcher")),
                description: Some(SharedStr::from_static("Open app drawer")),
                value: None,
                hidden: false,
            },
            sprite_timeline: None,
            children: Vec::new(),
        };

        let surf = RetainedSurface {
            spec: SurfaceSpec {
                kind: SurfaceKind::Panel,
                output_id: Some(OutputId(1)),
                layer: Layer::Top,
                anchor: Anchor::Top,
                margin: Edges::ZERO,
                logical_size: LogicalSize::new(LogicalPx(800.0), LogicalPx(36.0)),
                exclusive_zone: 36,
                keyboard_interactivity: KeyboardInteractivity::None,
                input_mode: InputMode::Full,
                background: crate::render::Color::BLACK,
                title: SharedStr::from_static("Top Panel"),
                namespace: SharedStr::from_static("waywi"),
                a11y: AccessibleProps::default(),
                ime: crate::input::ImeConfig::disabled(),
            },
            scale: ScaleFactor::ONE,
            roots: vec![btn_node],
            commands: Vec::new(),
            hit_nodes: Vec::new(),
            hovered_node: None,
            on_close: None,
            on_key: None,
            on_ime: None,
            on_pointer_move: None,
            on_pointer_up: None,
            dirty_logical_rects: Vec::new(),
            needs_redraw: false,
            full_redraw: false,
            input_region_dirty: false,
            selective_input_rects: Vec::new(),
            next_animation_deadline: None,
        };

        let mut surfaces = HashMap::new();
        surfaces.insert(rk, surf);

        let update = build_tree_update(&mut tree_state, &surfaces, Some(rk), "waywi-test");
        assert!(update.nodes.iter().any(|(id, n)| {
            *id == element_to_accesskit_id(btn_id)
                && n.role() == Role::Button
                && n.label() == Some("Launcher")
                && n.description() == Some("Open app drawer")
                && n.supports_action(Action::Click)
        }));

        let mut inbox = VecDeque::new();
        assert!(apply_accessibility_action(
            &mut tree_state,
            &surfaces,
            Action::Focus,
            element_to_accesskit_id(btn_id),
            &mut inbox
        ));
        assert!(apply_accessibility_action(
            &mut tree_state,
            &surfaces,
            Action::Click,
            element_to_accesskit_id(btn_id),
            &mut inbox
        ));
        assert_eq!(inbox.pop_front(), Some("activated"));
        assert_eq!(
            tree_state.focused_node,
            Some(element_to_accesskit_id(btn_id))
        );
    }
}
