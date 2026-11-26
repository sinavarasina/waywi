//! Cross-compositor Wayland foreign toplevel backend supporting:
//! 1. `zwlr_foreign_toplevel_management_v1` (preferred: listing + state + control actions)
//! 2. `ext_foreign_toplevel_list_v1` (fallback: read-only listing)

use std::collections::{BTreeMap, HashMap};

use wayland_client::backend::ObjectId;
use wayland_client::globals::GlobalList;
use wayland_client::protocol::{wl_output, wl_seat};
use wayland_client::{Connection, Dispatch, Proxy, QueueHandle, event_created_child};
use wayland_protocols::ext::foreign_toplevel_list::v1::client::{
    ext_foreign_toplevel_handle_v1::{self, ExtForeignToplevelHandleV1},
    ext_foreign_toplevel_list_v1::{self, ExtForeignToplevelListV1},
};
use wayland_protocols_wlr::foreign_toplevel::v1::client::{
    zwlr_foreign_toplevel_handle_v1::{self, State as WlrState, ZwlrForeignToplevelHandleV1},
    zwlr_foreign_toplevel_manager_v1::{self, ZwlrForeignToplevelManagerV1},
};

use crate::display::OutputId;
use crate::effect::ForeignToplevelAction;
use crate::wayland::connection::WaylandState;
use crate::wayland_ext::{
    ForeignToplevelId, ForeignToplevelInfo, ForeignToplevelSnapshot, ForeignToplevelState,
};

enum HandleProxy {
    Wlr(ZwlrForeignToplevelHandleV1),
    Ext(ExtForeignToplevelHandleV1),
}

impl HandleProxy {
    fn destroy(self) {
        match self {
            Self::Wlr(h) => h.destroy(),
            Self::Ext(h) => h.destroy(),
        }
    }
}

struct ToplevelRecord {
    proxy: HandleProxy,
    identifier: Option<String>,
    title: String,
    app_id: String,
    state: ForeignToplevelState,
    outputs: Vec<ObjectId>,
    parent_obj: Option<ObjectId>,
    committed: bool,
}

/// Manages active foreign toplevel handles and tracks dirty state for coalesced
/// [`ForeignToplevelSnapshot`] emission.
pub(crate) struct WaylandForeignToplevelState {
    wlr_manager: Option<ZwlrForeignToplevelManagerV1>,
    ext_list: Option<ExtForeignToplevelListV1>,
    next_id: u64,
    records: BTreeMap<u64, ToplevelRecord>,
    id_by_obj: HashMap<ObjectId, u64>,
    dirty: bool,
}

impl Drop for WaylandForeignToplevelState {
    fn drop(&mut self) {
        for (_, rec) in std::mem::take(&mut self.records) {
            rec.proxy.destroy();
        }
        if let Some(m) = self.wlr_manager.take() {
            m.stop();
        }
        if let Some(l) = self.ext_list.take() {
            l.destroy();
        }
    }
}

impl WaylandForeignToplevelState {
    pub(crate) fn bind(globals: &GlobalList, qh: &QueueHandle<WaylandState>) -> Self {
        // Prefer `zwlr_foreign_toplevel_management_v1` because it provides both listing and
        // control actions (`activate`, `close`, `minimize`, `maximize`, `fullscreen`).
        let wlr_manager = globals
            .bind::<ZwlrForeignToplevelManagerV1, _, _>(qh, 1..=3, ())
            .ok();
        let ext_list = if wlr_manager.is_none() {
            globals
                .bind::<ExtForeignToplevelListV1, _, _>(qh, 1..=1, ())
                .ok()
        } else {
            None
        };
        Self {
            wlr_manager,
            ext_list,
            next_id: 1,
            records: BTreeMap::new(),
            id_by_obj: HashMap::new(),
            dirty: false,
        }
    }

    /// Consumes the dirty flag so burst updates coalesce into a single snapshot per loop wakeup
    /// and perform zero allocations when no app subscriber is active.
    pub(crate) fn take_dirty(&mut self) -> bool {
        std::mem::replace(&mut self.dirty, false)
    }

    #[must_use]
    pub(crate) fn is_supported(&self) -> bool {
        self.wlr_manager.is_some() || self.ext_list.is_some()
    }

    #[must_use]
    pub(crate) fn can_control(&self) -> bool {
        self.wlr_manager.is_some()
    }

    fn alloc_id(&mut self) -> u64 {
        let id = self.next_id;
        self.next_id = self.next_id.wrapping_add(1).max(1);
        id
    }

    pub(crate) fn snapshot(
        &self,
        output_handles: &HashMap<OutputId, wl_output::WlOutput>,
    ) -> ForeignToplevelSnapshot {
        let can_control = self.can_control();
        let mut toplevels = Vec::with_capacity(self.records.len());
        for (&id, rec) in &self.records {
            if !rec.committed {
                continue;
            }
            let outputs: Vec<OutputId> = rec
                .outputs
                .iter()
                .filter_map(|oid| {
                    output_handles
                        .iter()
                        .find(|(_, o)| o.id() == *oid)
                        .map(|(&id, _)| id)
                })
                .collect();
            let parent = rec
                .parent_obj
                .as_ref()
                .and_then(|oid| self.id_by_obj.get(oid).copied())
                .map(ForeignToplevelId);
            toplevels.push(ForeignToplevelInfo {
                id: ForeignToplevelId(id),
                identifier: rec.identifier.clone(),
                title: rec.title.clone(),
                app_id: rec.app_id.clone(),
                state: rec.state,
                outputs,
                parent,
                can_control,
            });
        }
        ForeignToplevelSnapshot {
            toplevels,
            can_control,
        }
    }

    pub(crate) fn apply_action(
        &self,
        action: &ForeignToplevelAction,
        seat: Option<&wl_seat::WlSeat>,
        output_handles: &HashMap<OutputId, wl_output::WlOutput>,
    ) -> bool {
        let id = match action {
            ForeignToplevelAction::Activate(id)
            | ForeignToplevelAction::Close(id)
            | ForeignToplevelAction::SetMinimized(id, _)
            | ForeignToplevelAction::SetMaximized(id, _)
            | ForeignToplevelAction::SetFullscreen(id, _, _) => id.0,
        };
        let Some(rec) = self.records.get(&id) else {
            return false;
        };
        let HandleProxy::Wlr(ref handle) = rec.proxy else {
            return false;
        };
        match *action {
            ForeignToplevelAction::Activate(_) => {
                if let Some(s) = seat {
                    handle.activate(s);
                    true
                } else {
                    false
                }
            }
            ForeignToplevelAction::Close(_) => {
                handle.close();
                true
            }
            ForeignToplevelAction::SetMinimized(_, minimized) => {
                if minimized {
                    handle.set_minimized();
                } else {
                    handle.unset_minimized();
                }
                true
            }
            ForeignToplevelAction::SetMaximized(_, maximized) => {
                if maximized {
                    handle.set_maximized();
                } else {
                    handle.unset_maximized();
                }
                true
            }
            ForeignToplevelAction::SetFullscreen(_, fullscreen, out_id) => {
                if fullscreen {
                    let out = out_id.and_then(|oid| output_handles.get(&oid));
                    handle.set_fullscreen(out);
                } else {
                    handle.unset_fullscreen();
                }
                true
            }
        }
    }
}

impl Dispatch<ZwlrForeignToplevelManagerV1, ()> for WaylandState {
    fn event(
        state: &mut Self,
        _proxy: &ZwlrForeignToplevelManagerV1,
        event: zwlr_foreign_toplevel_manager_v1::Event,
        _data: &(),
        _conn: &Connection,
        _qh: &QueueHandle<Self>,
    ) {
        match event {
            zwlr_foreign_toplevel_manager_v1::Event::Toplevel { toplevel } => {
                let id = state.foreign_toplevel.alloc_id();
                let oid = toplevel.id();
                state.foreign_toplevel.id_by_obj.insert(oid, id);
                state.foreign_toplevel.records.insert(
                    id,
                    ToplevelRecord {
                        proxy: HandleProxy::Wlr(toplevel),
                        identifier: None,
                        title: String::new(),
                        app_id: String::new(),
                        state: ForeignToplevelState::empty(),
                        outputs: Vec::new(),
                        parent_obj: None,
                        committed: false,
                    },
                );
            }
            zwlr_foreign_toplevel_manager_v1::Event::Finished => {
                state.foreign_toplevel.wlr_manager = None;
            }
            _ => {}
        }
    }

    event_created_child!(WaylandState, ZwlrForeignToplevelManagerV1, [
        zwlr_foreign_toplevel_manager_v1::EVT_TOPLEVEL_OPCODE => (ZwlrForeignToplevelHandleV1, ()),
    ]);
}

impl Dispatch<ZwlrForeignToplevelHandleV1, ()> for WaylandState {
    fn event(
        state: &mut Self,
        proxy: &ZwlrForeignToplevelHandleV1,
        event: zwlr_foreign_toplevel_handle_v1::Event,
        _data: &(),
        _conn: &Connection,
        _qh: &QueueHandle<Self>,
    ) {
        let oid = proxy.id();
        let Some(&id) = state.foreign_toplevel.id_by_obj.get(&oid) else {
            return;
        };
        match event {
            zwlr_foreign_toplevel_handle_v1::Event::Title { title } => {
                if let Some(rec) = state.foreign_toplevel.records.get_mut(&id) {
                    rec.title = title;
                }
            }
            zwlr_foreign_toplevel_handle_v1::Event::AppId { app_id } => {
                if let Some(rec) = state.foreign_toplevel.records.get_mut(&id) {
                    rec.app_id = app_id;
                }
            }
            zwlr_foreign_toplevel_handle_v1::Event::OutputEnter { output } => {
                if let Some(rec) = state.foreign_toplevel.records.get_mut(&id) {
                    let out_oid = output.id();
                    if !rec.outputs.contains(&out_oid) {
                        rec.outputs.push(out_oid);
                    }
                }
            }
            zwlr_foreign_toplevel_handle_v1::Event::OutputLeave { output } => {
                if let Some(rec) = state.foreign_toplevel.records.get_mut(&id) {
                    let out_oid = output.id();
                    rec.outputs.retain(|o| *o != out_oid);
                }
            }
            zwlr_foreign_toplevel_handle_v1::Event::State { state: raw } => {
                let mut flags = ForeignToplevelState::empty();
                for chunk in raw.as_chunks::<4>().0 {
                    let v = u32::from_ne_bytes(*chunk);
                    if v == WlrState::Maximized as u32 {
                        flags |= ForeignToplevelState::MAXIMIZED;
                    } else if v == WlrState::Minimized as u32 {
                        flags |= ForeignToplevelState::MINIMIZED;
                    } else if v == WlrState::Activated as u32 {
                        flags |= ForeignToplevelState::ACTIVATED;
                    } else if v == WlrState::Fullscreen as u32 {
                        flags |= ForeignToplevelState::FULLSCREEN;
                    }
                }
                if let Some(rec) = state.foreign_toplevel.records.get_mut(&id) {
                    rec.state = flags;
                }
            }
            zwlr_foreign_toplevel_handle_v1::Event::Parent { parent } => {
                if let Some(rec) = state.foreign_toplevel.records.get_mut(&id) {
                    rec.parent_obj = parent.map(|p| p.id());
                }
            }
            zwlr_foreign_toplevel_handle_v1::Event::Done => {
                if let Some(rec) = state.foreign_toplevel.records.get_mut(&id) {
                    rec.committed = true;
                }
                state.foreign_toplevel.dirty = true;
            }
            zwlr_foreign_toplevel_handle_v1::Event::Closed => {
                state.foreign_toplevel.id_by_obj.remove(&oid);
                if let Some(rec) = state.foreign_toplevel.records.remove(&id) {
                    rec.proxy.destroy();
                }
                state.foreign_toplevel.dirty = true;
            }
            _ => {}
        }
    }
}

impl Dispatch<ExtForeignToplevelListV1, ()> for WaylandState {
    fn event(
        state: &mut Self,
        _proxy: &ExtForeignToplevelListV1,
        event: ext_foreign_toplevel_list_v1::Event,
        _data: &(),
        _conn: &Connection,
        _qh: &QueueHandle<Self>,
    ) {
        match event {
            ext_foreign_toplevel_list_v1::Event::Toplevel { toplevel } => {
                let id = state.foreign_toplevel.alloc_id();
                let oid = toplevel.id();
                state.foreign_toplevel.id_by_obj.insert(oid, id);
                state.foreign_toplevel.records.insert(
                    id,
                    ToplevelRecord {
                        proxy: HandleProxy::Ext(toplevel),
                        identifier: None,
                        title: String::new(),
                        app_id: String::new(),
                        state: ForeignToplevelState::empty(),
                        outputs: Vec::new(),
                        parent_obj: None,
                        committed: false,
                    },
                );
            }
            ext_foreign_toplevel_list_v1::Event::Finished => {
                if let Some(l) = state.foreign_toplevel.ext_list.take() {
                    l.destroy();
                }
            }
            _ => {}
        }
    }

    event_created_child!(WaylandState, ExtForeignToplevelListV1, [
        ext_foreign_toplevel_list_v1::EVT_TOPLEVEL_OPCODE => (ExtForeignToplevelHandleV1, ()),
    ]);
}

impl Dispatch<ExtForeignToplevelHandleV1, ()> for WaylandState {
    fn event(
        state: &mut Self,
        proxy: &ExtForeignToplevelHandleV1,
        event: ext_foreign_toplevel_handle_v1::Event,
        _data: &(),
        _conn: &Connection,
        _qh: &QueueHandle<Self>,
    ) {
        let oid = proxy.id();
        let Some(&id) = state.foreign_toplevel.id_by_obj.get(&oid) else {
            return;
        };
        match event {
            ext_foreign_toplevel_handle_v1::Event::Title { title } => {
                if let Some(rec) = state.foreign_toplevel.records.get_mut(&id) {
                    rec.title = title;
                }
            }
            ext_foreign_toplevel_handle_v1::Event::AppId { app_id } => {
                if let Some(rec) = state.foreign_toplevel.records.get_mut(&id) {
                    rec.app_id = app_id;
                }
            }
            ext_foreign_toplevel_handle_v1::Event::Identifier { identifier } => {
                if let Some(rec) = state.foreign_toplevel.records.get_mut(&id) {
                    rec.identifier = Some(identifier);
                }
            }
            ext_foreign_toplevel_handle_v1::Event::Done => {
                if let Some(rec) = state.foreign_toplevel.records.get_mut(&id) {
                    rec.committed = true;
                }
                state.foreign_toplevel.dirty = true;
            }
            ext_foreign_toplevel_handle_v1::Event::Closed => {
                state.foreign_toplevel.id_by_obj.remove(&oid);
                if let Some(rec) = state.foreign_toplevel.records.remove(&id) {
                    rec.proxy.destroy();
                }
                state.foreign_toplevel.dirty = true;
            }
            _ => {}
        }
    }
}
