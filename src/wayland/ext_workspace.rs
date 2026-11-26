//! Cross-compositor Wayland `ext_workspace_v1` backend.

use std::collections::{BTreeMap, HashMap};

use wayland_client::backend::ObjectId;
use wayland_client::globals::GlobalList;
use wayland_client::protocol::wl_output;
use wayland_client::{Connection, Dispatch, Proxy, QueueHandle, WEnum, event_created_child};
use wayland_protocols::ext::workspace::v1::client::{
    ext_workspace_group_handle_v1::{self, ExtWorkspaceGroupHandleV1, GroupCapabilities},
    ext_workspace_handle_v1::{
        self, ExtWorkspaceHandleV1, State as ProtoWsState, WorkspaceCapabilities,
    },
    ext_workspace_manager_v1::{self, ExtWorkspaceManagerV1},
};

use crate::display::OutputId;
use crate::effect::ExtWorkspaceAction;
use crate::wayland::connection::WaylandState;
use crate::wayland_ext::{
    ExtWorkspaceCapabilities, ExtWorkspaceGroupId, ExtWorkspaceGroupInfo, ExtWorkspaceId,
    ExtWorkspaceInfo, ExtWorkspaceSnapshot, ExtWorkspaceState,
};

struct GroupRecord {
    proxy: ExtWorkspaceGroupHandleV1,
    can_create_workspace: bool,
    outputs: Vec<ObjectId>,
    workspaces: Vec<ObjectId>,
}

struct WorkspaceRecord {
    proxy: ExtWorkspaceHandleV1,
    protocol_id: Option<String>,
    name: String,
    coordinates: Vec<u32>,
    state: ExtWorkspaceState,
    capabilities: ExtWorkspaceCapabilities,
}

/// Manages `ext_workspace_manager_v1` groups, workspaces, coalesced atomic `done` snapshots, and
/// batched mutations.
pub(crate) struct WaylandExtWorkspaceState {
    manager: Option<ExtWorkspaceManagerV1>,
    next_group_id: u64,
    next_ws_id: u64,
    groups: BTreeMap<u64, GroupRecord>,
    group_by_obj: HashMap<ObjectId, u64>,
    workspaces: BTreeMap<u64, WorkspaceRecord>,
    ws_by_obj: HashMap<ObjectId, u64>,
    dirty: bool,
}

impl Drop for WaylandExtWorkspaceState {
    fn drop(&mut self) {
        for (_, ws) in std::mem::take(&mut self.workspaces) {
            ws.proxy.destroy();
        }
        for (_, g) in std::mem::take(&mut self.groups) {
            g.proxy.destroy();
        }
        if let Some(mgr) = self.manager.take() {
            mgr.stop();
        }
    }
}

impl WaylandExtWorkspaceState {
    pub(crate) fn bind(globals: &GlobalList, qh: &QueueHandle<WaylandState>) -> Self {
        let manager = globals
            .bind::<ExtWorkspaceManagerV1, _, _>(qh, 1..=1, ())
            .ok();
        Self {
            manager,
            next_group_id: 1,
            next_ws_id: 1,
            groups: BTreeMap::new(),
            group_by_obj: HashMap::new(),
            workspaces: BTreeMap::new(),
            ws_by_obj: HashMap::new(),
            dirty: false,
        }
    }

    #[must_use]
    pub(crate) fn is_supported(&self) -> bool {
        self.manager.is_some()
    }

    /// Consumes the dirty flag so burst `done` events coalesce into one snapshot per wakeup and
    /// perform zero allocations when no subscriber is active.
    pub(crate) fn take_dirty(&mut self) -> bool {
        std::mem::replace(&mut self.dirty, false)
    }

    pub(crate) fn snapshot(
        &self,
        output_handles: &HashMap<OutputId, wl_output::WlOutput>,
    ) -> ExtWorkspaceSnapshot {
        let mut ws_to_group: HashMap<u64, ExtWorkspaceGroupId> = HashMap::new();
        let mut groups_out = Vec::with_capacity(self.groups.len());
        for (&gid, g) in &self.groups {
            let outputs: Vec<OutputId> = g
                .outputs
                .iter()
                .filter_map(|oid| {
                    output_handles
                        .iter()
                        .find(|(_, o)| o.id() == *oid)
                        .map(|(&id, _)| id)
                })
                .collect();
            let mut ws_ids = Vec::with_capacity(g.workspaces.len());
            for ws_oid in &g.workspaces {
                if let Some(&wid) = self.ws_by_obj.get(ws_oid) {
                    ws_ids.push(ExtWorkspaceId(wid));
                    ws_to_group.insert(wid, ExtWorkspaceGroupId(gid));
                }
            }
            groups_out.push(ExtWorkspaceGroupInfo {
                id: ExtWorkspaceGroupId(gid),
                can_create_workspace: g.can_create_workspace,
                outputs,
                workspaces: ws_ids,
            });
        }

        let mut ws_out = Vec::with_capacity(self.workspaces.len());
        for (&wid, ws) in &self.workspaces {
            ws_out.push(ExtWorkspaceInfo {
                id: ExtWorkspaceId(wid),
                protocol_id: ws.protocol_id.clone(),
                name: ws.name.clone(),
                coordinates: ws.coordinates.clone(),
                state: ws.state,
                capabilities: ws.capabilities,
                group: ws_to_group.get(&wid).copied(),
            });
        }

        ExtWorkspaceSnapshot {
            groups: groups_out,
            workspaces: ws_out,
        }
    }

    /// Enqueues one `ext_workspace_v1` mutation without committing; call [`Self::commit`] once
    /// after applying all actions in a batch so the compositor receives a single atomic transaction.
    pub(crate) fn apply_action(&self, action: &ExtWorkspaceAction) -> bool {
        if self.manager.is_none() {
            return false;
        }
        match action {
            ExtWorkspaceAction::Activate(id) => {
                if let Some(ws) = self.workspaces.get(&id.0) {
                    ws.proxy.activate();
                    true
                } else {
                    false
                }
            }
            ExtWorkspaceAction::Deactivate(id) => {
                if let Some(ws) = self.workspaces.get(&id.0) {
                    ws.proxy.deactivate();
                    true
                } else {
                    false
                }
            }
            ExtWorkspaceAction::Create { group, name } => {
                if let Some(g) = self.groups.get(&group.0) {
                    g.proxy.create_workspace(name.clone());
                    true
                } else {
                    false
                }
            }
            ExtWorkspaceAction::Remove(id) => {
                if let Some(ws) = self.workspaces.get(&id.0) {
                    ws.proxy.remove();
                    true
                } else {
                    false
                }
            }
        }
    }

    /// Commits pending `ext_workspace_v1` requests as a single atomic manager transaction.
    pub(crate) fn commit(&self) {
        if let Some(ref mgr) = self.manager {
            mgr.commit();
        }
    }
}

impl Dispatch<ExtWorkspaceManagerV1, ()> for WaylandState {
    fn event(
        state: &mut Self,
        _proxy: &ExtWorkspaceManagerV1,
        event: ext_workspace_manager_v1::Event,
        _data: &(),
        _conn: &Connection,
        _qh: &QueueHandle<Self>,
    ) {
        match event {
            ext_workspace_manager_v1::Event::WorkspaceGroup { workspace_group } => {
                let gid = state.ext_workspace.next_group_id;
                state.ext_workspace.next_group_id = gid.wrapping_add(1).max(1);
                let oid = workspace_group.id();
                state.ext_workspace.group_by_obj.insert(oid, gid);
                state.ext_workspace.groups.insert(
                    gid,
                    GroupRecord {
                        proxy: workspace_group,
                        can_create_workspace: false,
                        outputs: Vec::new(),
                        workspaces: Vec::new(),
                    },
                );
            }
            ext_workspace_manager_v1::Event::Workspace { workspace } => {
                let wid = state.ext_workspace.next_ws_id;
                state.ext_workspace.next_ws_id = wid.wrapping_add(1).max(1);
                let oid = workspace.id();
                state.ext_workspace.ws_by_obj.insert(oid, wid);
                state.ext_workspace.workspaces.insert(
                    wid,
                    WorkspaceRecord {
                        proxy: workspace,
                        protocol_id: None,
                        name: String::new(),
                        coordinates: Vec::new(),
                        state: ExtWorkspaceState::empty(),
                        capabilities: ExtWorkspaceCapabilities::empty(),
                    },
                );
            }
            ext_workspace_manager_v1::Event::Done => {
                state.ext_workspace.dirty = true;
            }
            ext_workspace_manager_v1::Event::Finished => {
                state.ext_workspace.manager = None;
            }
            _ => {}
        }
    }

    event_created_child!(WaylandState, ExtWorkspaceManagerV1, [
        ext_workspace_manager_v1::EVT_WORKSPACE_GROUP_OPCODE => (ExtWorkspaceGroupHandleV1, ()),
        ext_workspace_manager_v1::EVT_WORKSPACE_OPCODE => (ExtWorkspaceHandleV1, ()),
    ]);
}

impl Dispatch<ExtWorkspaceGroupHandleV1, ()> for WaylandState {
    fn event(
        state: &mut Self,
        proxy: &ExtWorkspaceGroupHandleV1,
        event: ext_workspace_group_handle_v1::Event,
        _data: &(),
        _conn: &Connection,
        _qh: &QueueHandle<Self>,
    ) {
        let oid = proxy.id();
        let Some(&gid) = state.ext_workspace.group_by_obj.get(&oid) else {
            return;
        };
        match event {
            ext_workspace_group_handle_v1::Event::Capabilities { capabilities } => {
                if let WEnum::Value(caps) = capabilities
                    && let Some(g) = state.ext_workspace.groups.get_mut(&gid)
                {
                    g.can_create_workspace = caps.contains(GroupCapabilities::CreateWorkspace);
                }
            }
            ext_workspace_group_handle_v1::Event::OutputEnter { output } => {
                if let Some(g) = state.ext_workspace.groups.get_mut(&gid) {
                    let o = output.id();
                    if !g.outputs.contains(&o) {
                        g.outputs.push(o);
                    }
                }
            }
            ext_workspace_group_handle_v1::Event::OutputLeave { output } => {
                if let Some(g) = state.ext_workspace.groups.get_mut(&gid) {
                    let o = output.id();
                    g.outputs.retain(|x| *x != o);
                }
            }
            ext_workspace_group_handle_v1::Event::WorkspaceEnter { workspace } => {
                if let Some(g) = state.ext_workspace.groups.get_mut(&gid) {
                    let w = workspace.id();
                    if !g.workspaces.contains(&w) {
                        g.workspaces.push(w);
                    }
                }
            }
            ext_workspace_group_handle_v1::Event::WorkspaceLeave { workspace } => {
                if let Some(g) = state.ext_workspace.groups.get_mut(&gid) {
                    let w = workspace.id();
                    g.workspaces.retain(|x| *x != w);
                }
            }
            ext_workspace_group_handle_v1::Event::Removed => {
                state.ext_workspace.group_by_obj.remove(&oid);
                if let Some(g) = state.ext_workspace.groups.remove(&gid) {
                    g.proxy.destroy();
                }
            }
            _ => {}
        }
    }
}

impl Dispatch<ExtWorkspaceHandleV1, ()> for WaylandState {
    fn event(
        state: &mut Self,
        proxy: &ExtWorkspaceHandleV1,
        event: ext_workspace_handle_v1::Event,
        _data: &(),
        _conn: &Connection,
        _qh: &QueueHandle<Self>,
    ) {
        let oid = proxy.id();
        let Some(&wid) = state.ext_workspace.ws_by_obj.get(&oid) else {
            return;
        };
        match event {
            ext_workspace_handle_v1::Event::Id { id } => {
                if let Some(ws) = state.ext_workspace.workspaces.get_mut(&wid) {
                    ws.protocol_id = Some(id);
                }
            }
            ext_workspace_handle_v1::Event::Name { name } => {
                if let Some(ws) = state.ext_workspace.workspaces.get_mut(&wid) {
                    ws.name = name;
                }
            }
            ext_workspace_handle_v1::Event::Coordinates { coordinates } => {
                let coords: Vec<u32> = coordinates
                    .as_chunks::<4>()
                    .0
                    .iter()
                    .map(|c| u32::from_ne_bytes(*c))
                    .collect();
                if let Some(ws) = state.ext_workspace.workspaces.get_mut(&wid) {
                    ws.coordinates = coords;
                }
            }
            ext_workspace_handle_v1::Event::State { state: raw } => {
                if let WEnum::Value(st) = raw
                    && let Some(ws) = state.ext_workspace.workspaces.get_mut(&wid)
                {
                    let mut flags = ExtWorkspaceState::empty();
                    if st.contains(ProtoWsState::Active) {
                        flags |= ExtWorkspaceState::ACTIVE;
                    }
                    if st.contains(ProtoWsState::Urgent) {
                        flags |= ExtWorkspaceState::URGENT;
                    }
                    if st.contains(ProtoWsState::Hidden) {
                        flags |= ExtWorkspaceState::HIDDEN;
                    }
                    ws.state = flags;
                }
            }
            ext_workspace_handle_v1::Event::Capabilities { capabilities } => {
                if let WEnum::Value(caps) = capabilities
                    && let Some(ws) = state.ext_workspace.workspaces.get_mut(&wid)
                {
                    let mut flags = ExtWorkspaceCapabilities::empty();
                    if caps.contains(WorkspaceCapabilities::Activate) {
                        flags |= ExtWorkspaceCapabilities::ACTIVATE;
                    }
                    if caps.contains(WorkspaceCapabilities::Deactivate) {
                        flags |= ExtWorkspaceCapabilities::DEACTIVATE;
                    }
                    if caps.contains(WorkspaceCapabilities::Remove) {
                        flags |= ExtWorkspaceCapabilities::REMOVE;
                    }
                    if caps.contains(WorkspaceCapabilities::Assign) {
                        flags |= ExtWorkspaceCapabilities::ASSIGN;
                    }
                    ws.capabilities = flags;
                }
            }
            ext_workspace_handle_v1::Event::Removed => {
                state.ext_workspace.ws_by_obj.remove(&oid);
                for g in state.ext_workspace.groups.values_mut() {
                    g.workspaces.retain(|w| *w != oid);
                }
                if let Some(ws) = state.ext_workspace.workspaces.remove(&wid) {
                    ws.proxy.destroy();
                }
            }
            _ => {}
        }
    }
}
