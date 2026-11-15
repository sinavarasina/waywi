use std::collections::{HashMap, VecDeque};
use std::sync::{Arc, Mutex};

use accesskit::{ActionHandler, ActionRequest, ActivationHandler, DeactivationHandler, TreeUpdate};
use accesskit_unix::Adapter;

use crate::reconcile::{RetainedSurface, RetainedSurfaceKey};

use super::tree::{AccessibilityTreeState, apply_accessibility_action, build_tree_update};

/// Probes whether AT-SPI2 / session D-Bus accessibility transport appears available in this
/// environment.
pub(crate) fn atspi_runtime_available() -> bool {
    if let Ok(addr) = std::env::var("AT_SPI_BUS_ADDRESS")
        && !addr.is_empty()
    {
        return true;
    }
    if let Ok(addr) = std::env::var("DBUS_SESSION_BUS_ADDRESS")
        && !addr.is_empty()
    {
        return true;
    }
    if let Ok(runtime_dir) = std::env::var("XDG_RUNTIME_DIR") {
        let rt_path = std::path::Path::new(&runtime_dir);
        if rt_path.join("at-spi").exists() || rt_path.join("bus").exists() {
            return true;
        }
    }
    false
}

struct WaywiActivation {
    latest_tree: Arc<Mutex<Option<TreeUpdate>>>,
    wakeup: Arc<dyn Fn() + Send + Sync>,
}

impl ActivationHandler for WaywiActivation {
    fn request_initial_tree(&mut self) -> Option<TreeUpdate> {
        let initial = self.latest_tree.lock().ok().and_then(|g| g.clone());
        if initial.is_none() {
            (self.wakeup)();
        }
        initial
    }
}

struct WaywiActionHandler {
    pending: Arc<Mutex<VecDeque<ActionRequest>>>,
    wakeup: Arc<dyn Fn() + Send + Sync>,
}

impl ActionHandler for WaywiActionHandler {
    fn do_action(&mut self, request: ActionRequest) {
        if let Ok(mut queue) = self.pending.lock() {
            queue.push_back(request);
        }
        (self.wakeup)();
    }
}

struct WaywiDeactivation;

impl DeactivationHandler for WaywiDeactivation {
    fn deactivate_accessibility(&mut self) {}
}

/// Runtime AT-SPI2 bridge publishing AccessKit trees and forwarding screen-reader actions.
pub(crate) struct AccessibilityBridge {
    adapter: Adapter,
    pub(crate) tree_state: AccessibilityTreeState,
    latest_tree: Arc<Mutex<Option<TreeUpdate>>>,
    pending_actions: Arc<Mutex<VecDeque<ActionRequest>>>,
    last_focus_state: Option<bool>,
    app_name: String,
}

impl AccessibilityBridge {
    pub(crate) fn new(app_name: String, wakeup: impl Fn() + Send + Sync + 'static) -> Self {
        let wakeup_arc: Arc<dyn Fn() + Send + Sync> = Arc::new(wakeup);
        let latest_tree = Arc::new(Mutex::new(None));
        let pending_actions = Arc::new(Mutex::new(VecDeque::new()));

        let activation = WaywiActivation {
            latest_tree: Arc::clone(&latest_tree),
            wakeup: Arc::clone(&wakeup_arc),
        };
        let actions = WaywiActionHandler {
            pending: Arc::clone(&pending_actions),
            wakeup: wakeup_arc,
        };
        let adapter = Adapter::new(activation, actions, WaywiDeactivation);

        Self {
            adapter,
            tree_state: AccessibilityTreeState::default(),
            latest_tree,
            pending_actions,
            last_focus_state: None,
            app_name,
        }
    }

    /// Synchronizes retained WayWi surfaces into AT-SPI2 / AccessKit and updates focus state.
    pub(crate) fn sync<M: Clone + 'static>(
        &mut self,
        surfaces: &HashMap<RetainedSurfaceKey, RetainedSurface<M>>,
        focused_surface: Option<RetainedSurfaceKey>,
    ) {
        let update = build_tree_update(
            &mut self.tree_state,
            surfaces,
            focused_surface,
            &self.app_name,
        );
        if let Ok(mut guard) = self.latest_tree.lock() {
            *guard = Some(update.clone());
        }
        self.adapter.update_if_active(|| update);
        let is_focused = focused_surface.is_some();
        if self.last_focus_state != Some(is_focused) {
            self.adapter.update_window_focus_state(is_focused);
            self.last_focus_state = Some(is_focused);
        }
    }

    /// Drains pending AT-SPI2 action requests and dispatches them into the application inbox.
    ///
    /// Returns `true` if accessible focus state changed and the tree should be re-synced.
    pub(crate) fn drain_and_dispatch<M: Clone + 'static>(
        &mut self,
        surfaces: &HashMap<RetainedSurfaceKey, RetainedSurface<M>>,
        inbox: &mut VecDeque<M>,
    ) -> bool {
        let drained: Vec<ActionRequest> = match self.pending_actions.lock() {
            Ok(mut guard) => guard.drain(..).collect(),
            Err(_) => return false,
        };
        let mut focus_changed = false;
        for req in drained {
            let is_focus = req.action == accesskit::Action::Focus;
            if apply_accessibility_action(
                &mut self.tree_state,
                surfaces,
                req.action,
                req.target_node,
                inbox,
            ) && is_focus
            {
                focus_changed = true;
            }
        }
        focus_changed
    }
}
