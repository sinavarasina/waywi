use std::cell::RefCell;
#[cfg(any(
    feature = "dbus",
    feature = "control-ipc",
    feature = "global-shortcuts"
))]
use std::collections::HashMap;
#[cfg(any(
    feature = "dbus",
    feature = "global-shortcuts",
    feature = "control-ipc"
))]
use std::collections::HashSet;
use std::collections::VecDeque;
#[cfg(any(
    feature = "hyprland-ipc",
    feature = "sway-ipc",
    feature = "control-ipc"
))]
use std::path::PathBuf;
use std::rc::Rc;
#[cfg(any(feature = "pipewire", feature = "pulseaudio", feature = "alsa"))]
use std::sync::Arc;

use calloop::LoopHandle;
#[cfg(any(feature = "hyprland-ipc", feature = "sway-ipc", feature = "dbus"))]
use calloop::channel as calloop_channel;

use crate::arena::FrameSharedFn;
#[cfg(feature = "global-shortcuts")]
use crate::subscription::SubscriptionKey;
use crate::subscription::{SubscriptionSpec, Subscriptions};
use crate::wayland::WaylandState;

pub(crate) fn flatten_specs<'a, M: 'static>(
    subs: &'a Subscriptions<M>,
    out: &mut Vec<&'a SubscriptionSpec<M>>,
) {
    for sub in subs.iter() {
        collect_spec(&sub.spec, out);
    }
}

fn collect_spec<'a, M: 'static>(
    spec: &'a SubscriptionSpec<M>,
    out: &mut Vec<&'a SubscriptionSpec<M>>,
) {
    match spec {
        SubscriptionSpec::None => {}
        SubscriptionSpec::Batch(children) => {
            for c in children.iter() {
                collect_spec(&c.spec, out);
            }
        }
        other => out.push(other),
    }
}

/// Fans out a single event across `subs`, moving without cloning when there is 1 subscriber
/// (and moving into the final subscriber when there are multiple).
pub(crate) fn fanout_event<E: Clone, M: 'static>(
    ev: E,
    subs: &[FrameSharedFn<E, M>],
    out: &mut VecDeque<M>,
) {
    let len = subs.len();
    if len == 0 {
        return;
    }
    if len == 1 {
        out.push_back(subs[0].call(ev));
        return;
    }
    for map in &subs[..len - 1] {
        out.push_back(map.call(ev.clone()));
    }
    out.push_back(subs[len - 1].call(ev));
}

/// Generic singleton broadcast subscription slot owning its subscribers, event queue, and
/// background worker + `calloop` registration token.
#[cfg(feature = "dbus")]
pub(crate) struct BroadcastSlot<Ev, W, M> {
    pub(crate) subs: Vec<FrameSharedFn<Ev, M>>,
    queue: Rc<RefCell<VecDeque<Ev>>>,
    worker: Option<(W, calloop::RegistrationToken)>,
}

#[cfg(feature = "dbus")]
impl<Ev, W, M> BroadcastSlot<Ev, W, M> {
    pub(crate) fn new() -> Self {
        Self {
            subs: Vec::new(),
            queue: Rc::new(RefCell::new(VecDeque::new())),
            worker: None,
        }
    }

    pub(crate) fn clear_subs(&mut self) {
        self.subs.clear();
    }

    pub(crate) fn is_active(&self) -> bool {
        !self.subs.is_empty()
    }

    pub(crate) fn sync_worker(
        &mut self,
        loop_handle: &LoopHandle<'static, WaylandState>,
        on_event: impl Fn(&Ev) + 'static,
        spawn: impl FnOnce(calloop_channel::Sender<Ev>) -> W,
    ) where
        Ev: 'static,
    {
        if !self.subs.is_empty() {
            if self.worker.is_none() {
                let (tx, rx) = calloop_channel::channel();
                let queue_cl = Rc::clone(&self.queue);
                if let Ok(token) = loop_handle.insert_source(rx, move |evt, _, _| {
                    if let calloop_channel::Event::Msg(ev) = evt {
                        on_event(&ev);
                        queue_cl.borrow_mut().push_back(ev);
                    }
                }) {
                    self.worker = Some((spawn(tx), token));
                }
            }
        } else if let Some((worker, token)) = self.worker.take() {
            drop(worker);
            loop_handle.remove(token);
            self.queue.borrow_mut().clear();
        }
    }

    pub(crate) fn sync_shared_worker(
        &mut self,
        loop_handle: &LoopHandle<'static, WaylandState>,
        shared: &crate::dbus::DbusSharedBus,
        on_event: impl Fn(&Ev) + 'static,
        spawn: impl FnOnce(calloop_channel::Sender<Ev>, crate::dbus::DbusSharedBus) -> W,
    ) where
        Ev: 'static,
    {
        let shared = std::sync::Arc::clone(shared);
        self.sync_worker(loop_handle, on_event, move |tx| spawn(tx, shared));
    }

    pub(crate) fn drain_into(&self, out: &mut VecDeque<M>)
    where
        Ev: Clone,
        M: 'static,
    {
        let mut q = self.queue.borrow_mut();
        if q.is_empty() {
            return;
        }
        if self.subs.is_empty() {
            q.clear();
            return;
        }
        for ev in q.drain(..) {
            fanout_event(ev, &self.subs, out);
        }
    }

    pub(crate) fn cleanup(&mut self, loop_handle: &LoopHandle<'static, WaylandState>) {
        if let Some((worker, token)) = self.worker.take() {
            drop(worker);
            loop_handle.remove(token);
        }
        self.queue.borrow_mut().clear();
    }
}

#[cfg(feature = "dbus")]
pub(crate) type DbusSignalKey = (crate::dbus::DbusBus, String, String, Option<String>);
#[cfg(feature = "dbus")]
type DbusSignalWorkerEntry = (crate::dbus::DbusSignalWorker, calloop::RegistrationToken);
#[cfg(feature = "global-shortcuts")]
type GlobalShortcutSub<M> = (
    SubscriptionKey,
    String,
    Vec<crate::dbus::GlobalShortcutDescriptor>,
    FrameSharedFn<crate::dbus::GlobalShortcutEvent, M>,
);

pub(crate) struct SubscriptionClients<'a> {
    #[cfg(any(feature = "pipewire", feature = "pulseaudio", feature = "alsa"))]
    pub(crate) audio: &'a crate::audio::AudioClient,
    #[cfg(feature = "hyprland-ipc")]
    pub(crate) hyprland: Option<&'a crate::compositor::hyprland::HyprlandClient>,
    #[cfg(feature = "sway-ipc")]
    pub(crate) sway: Option<&'a crate::compositor::sway::SwayClient>,
    #[cfg(feature = "dbus")]
    pub(crate) dbus: &'a crate::dbus::DbusClient,
    #[cfg(feature = "control-ipc")]
    pub(crate) control_socket_override: Option<&'a std::path::Path>,
    #[cfg(feature = "control-ipc")]
    pub(crate) app_name: &'a str,
    pub(crate) _marker: std::marker::PhantomData<&'a ()>,
}

pub(crate) struct LoopInbox<M> {
    pub(crate) messages: VecDeque<M>,
    #[cfg(feature = "hyprland-ipc")]
    pub(crate) hyprland_events: VecDeque<crate::compositor::hyprland::HyprlandEvent>,
    #[cfg(feature = "sway-ipc")]
    pub(crate) sway_events: VecDeque<crate::compositor::sway::SwayEvent>,
    #[cfg(feature = "dbus")]
    pub(crate) dbus_signal_events: VecDeque<(DbusSignalKey, crate::dbus::DbusSignalEvent)>,
    #[cfg(feature = "global-shortcuts")]
    pub(crate) global_shortcut_events:
        VecDeque<(SubscriptionKey, crate::dbus::GlobalShortcutEvent)>,
    #[cfg(feature = "control-ipc")]
    pub(crate) control_signals: VecDeque<crate::control::Signal>,
    #[cfg(feature = "control-ipc")]
    pub(crate) control_commands: VecDeque<(PathBuf, crate::control::ControlRequest)>,
}

impl<M> Default for LoopInbox<M> {
    fn default() -> Self {
        Self {
            messages: VecDeque::new(),
            #[cfg(feature = "hyprland-ipc")]
            hyprland_events: VecDeque::new(),
            #[cfg(feature = "sway-ipc")]
            sway_events: VecDeque::new(),
            #[cfg(feature = "dbus")]
            dbus_signal_events: VecDeque::new(),
            #[cfg(feature = "global-shortcuts")]
            global_shortcut_events: VecDeque::new(),
            #[cfg(feature = "control-ipc")]
            control_signals: VecDeque::new(),
            #[cfg(feature = "control-ipc")]
            control_commands: VecDeque::new(),
        }
    }
}

pub(crate) struct SubscriptionManager<M> {
    pub(crate) keyboard_layout_subs: Vec<FrameSharedFn<crate::input::KeyboardLayoutInfo, M>>,
    #[cfg(feature = "idle-notify")]
    pub(crate) idle_subs: Vec<(
        std::time::Duration,
        bool,
        FrameSharedFn<crate::idle::IdleEvent, M>,
    )>,
    #[cfg(feature = "idle-notify")]
    pub(crate) idle_watches: Vec<(std::time::Duration, bool)>,
    #[cfg(feature = "clipboard")]
    pub(crate) clipboard_subs: Vec<(
        Option<crate::clipboard::ClipboardSelection>,
        FrameSharedFn<crate::clipboard::ClipboardEvent, M>,
    )>,
    #[cfg(any(feature = "pipewire", feature = "pulseaudio", feature = "alsa"))]
    pub(crate) audio_snapshot_subs: Vec<FrameSharedFn<Arc<crate::audio::AudioSnapshot>, M>>,
    #[cfg(any(feature = "pipewire", feature = "pulseaudio", feature = "alsa"))]
    pub(crate) audio_event_subs: Vec<FrameSharedFn<crate::audio::AudioEvent, M>>,

    #[cfg(any(feature = "hyprland-ipc", feature = "sway-ipc"))]
    pub(crate) compositor_subs: Vec<FrameSharedFn<crate::compositor::CompositorEvent, M>>,
    #[cfg(feature = "hyprland-ipc")]
    pub(crate) hyprland_subs: Vec<FrameSharedFn<crate::compositor::hyprland::HyprlandEvent, M>>,
    #[cfg(feature = "hyprland-ipc")]
    hyprland_listener: Option<(
        PathBuf,
        crate::compositor::hyprland::HyprlandListenerHandle,
        calloop::RegistrationToken,
    )>,

    #[cfg(feature = "sway-ipc")]
    pub(crate) sway_subs: Vec<FrameSharedFn<crate::compositor::sway::SwayEvent, M>>,
    #[cfg(feature = "sway-ipc")]
    sway_listener: Option<(
        PathBuf,
        crate::compositor::sway::SwayListenerHandle,
        calloop::RegistrationToken,
    )>,

    #[cfg(feature = "dbus")]
    pub(crate) dbus_signal_subs: Vec<(
        DbusSignalKey,
        FrameSharedFn<crate::dbus::DbusSignalEvent, M>,
    )>,
    #[cfg(feature = "dbus")]
    dbus_signal_workers: HashMap<DbusSignalKey, DbusSignalWorkerEntry>,
    #[cfg(feature = "dbus")]
    pub(crate) dbus_notifications:
        BroadcastSlot<crate::dbus::NotificationEvent, crate::dbus::NotificationServerWorker, M>,
    #[cfg(feature = "dbus")]
    pub(crate) dbus_tray: BroadcastSlot<crate::dbus::TrayEvent, crate::dbus::TrayWorker, M>,
    #[cfg(feature = "dbus")]
    pub(crate) dbus_power_profiles:
        BroadcastSlot<crate::dbus::PowerProfileSnapshot, crate::dbus::PowerProfilesWorker, M>,
    #[cfg(feature = "dbus")]
    pub(crate) dbus_network_manager:
        BroadcastSlot<crate::dbus::NetworkManagerSnapshot, crate::dbus::NetworkManagerWorker, M>,
    #[cfg(feature = "dbus")]
    pub(crate) dbus_upower:
        BroadcastSlot<crate::dbus::UPowerSnapshot, crate::dbus::ServiceWatchWorker, M>,
    #[cfg(feature = "dbus")]
    pub(crate) dbus_udisks:
        BroadcastSlot<crate::dbus::UDisksSnapshot, crate::dbus::ServiceWatchWorker, M>,
    #[cfg(feature = "dbus")]
    pub(crate) dbus_logind: BroadcastSlot<crate::dbus::LogindEvent, crate::dbus::LogindWorker, M>,
    #[cfg(feature = "dbus")]
    pub(crate) dbus_bluetooth:
        BroadcastSlot<crate::dbus::BluetoothSnapshot, crate::dbus::ServiceWatchWorker, M>,
    #[cfg(feature = "dbus")]
    pub(crate) dbus_bluetooth_agent:
        BroadcastSlot<crate::dbus::BluetoothAgentEvent, crate::dbus::BluetoothAgentWorker, M>,
    #[cfg(feature = "dbus")]
    pub(crate) dbus_modem:
        BroadcastSlot<crate::dbus::ModemSnapshot, crate::dbus::ServiceWatchWorker, M>,
    #[cfg(feature = "dbus")]
    pub(crate) dbus_screensaver:
        BroadcastSlot<crate::dbus::ScreenSaverSnapshot, crate::dbus::ServiceWatchWorker, M>,
    #[cfg(feature = "dbus")]
    pub(crate) dbus_geoclue:
        BroadcastSlot<crate::dbus::GeoClueSnapshot, crate::dbus::ServiceWatchWorker, M>,
    #[cfg(feature = "dbus")]
    pub(crate) dbus_portal_settings:
        BroadcastSlot<crate::dbus::PortalSettingsSnapshot, crate::dbus::ServiceWatchWorker, M>,
    #[cfg(feature = "dbus")]
    pub(crate) dbus_mpris: BroadcastSlot<crate::dbus::MprisEvent, crate::dbus::MprisWorker, M>,
    #[cfg(feature = "dbus")]
    pub(crate) dbus_action_results:
        BroadcastSlot<crate::dbus::DbusActionResult, crate::dbus::DbusActionResultWorker, M>,
    #[cfg(feature = "global-shortcuts")]
    pub(crate) global_shortcut_subs: Vec<GlobalShortcutSub<M>>,
    #[cfg(feature = "global-shortcuts")]
    global_shortcut_workers: HashMap<
        SubscriptionKey,
        (
            crate::dbus::GlobalShortcutsWorker,
            calloop::RegistrationToken,
        ),
    >,

    #[cfg(feature = "control-ipc")]
    pub(crate) control_signal_subs: Vec<(crate::control::Signal, M)>,
    #[cfg(feature = "control-ipc")]
    signal_source_token: Option<calloop::RegistrationToken>,
    #[cfg(feature = "control-ipc")]
    pub(crate) control_socket_subs:
        Vec<(PathBuf, FrameSharedFn<crate::control::ControlRequest, M>)>,
    #[cfg(feature = "control-ipc")]
    control_sockets: HashMap<PathBuf, crate::control::ControlSocketRegistration>,
}

impl<M: Clone + 'static> SubscriptionManager<M> {
    pub(crate) fn new() -> Self {
        Self {
            keyboard_layout_subs: Vec::new(),
            #[cfg(feature = "idle-notify")]
            idle_subs: Vec::new(),
            #[cfg(feature = "idle-notify")]
            idle_watches: Vec::new(),
            #[cfg(feature = "clipboard")]
            clipboard_subs: Vec::new(),
            #[cfg(any(feature = "pipewire", feature = "pulseaudio", feature = "alsa"))]
            audio_snapshot_subs: Vec::new(),
            #[cfg(any(feature = "pipewire", feature = "pulseaudio", feature = "alsa"))]
            audio_event_subs: Vec::new(),
            #[cfg(any(feature = "hyprland-ipc", feature = "sway-ipc"))]
            compositor_subs: Vec::new(),
            #[cfg(feature = "hyprland-ipc")]
            hyprland_subs: Vec::new(),
            #[cfg(feature = "hyprland-ipc")]
            hyprland_listener: None,
            #[cfg(feature = "sway-ipc")]
            sway_subs: Vec::new(),
            #[cfg(feature = "sway-ipc")]
            sway_listener: None,
            #[cfg(feature = "dbus")]
            dbus_signal_subs: Vec::new(),
            #[cfg(feature = "dbus")]
            dbus_signal_workers: HashMap::new(),
            #[cfg(feature = "dbus")]
            dbus_notifications: BroadcastSlot::new(),
            #[cfg(feature = "dbus")]
            dbus_tray: BroadcastSlot::new(),
            #[cfg(feature = "dbus")]
            dbus_power_profiles: BroadcastSlot::new(),
            #[cfg(feature = "dbus")]
            dbus_network_manager: BroadcastSlot::new(),
            #[cfg(feature = "dbus")]
            dbus_upower: BroadcastSlot::new(),
            #[cfg(feature = "dbus")]
            dbus_udisks: BroadcastSlot::new(),
            #[cfg(feature = "dbus")]
            dbus_logind: BroadcastSlot::new(),
            #[cfg(feature = "dbus")]
            dbus_bluetooth: BroadcastSlot::new(),
            #[cfg(feature = "dbus")]
            dbus_bluetooth_agent: BroadcastSlot::new(),
            #[cfg(feature = "dbus")]
            dbus_modem: BroadcastSlot::new(),
            #[cfg(feature = "dbus")]
            dbus_screensaver: BroadcastSlot::new(),
            #[cfg(feature = "dbus")]
            dbus_geoclue: BroadcastSlot::new(),
            #[cfg(feature = "dbus")]
            dbus_portal_settings: BroadcastSlot::new(),
            #[cfg(feature = "dbus")]
            dbus_mpris: BroadcastSlot::new(),
            #[cfg(feature = "dbus")]
            dbus_action_results: BroadcastSlot::new(),
            #[cfg(feature = "global-shortcuts")]
            global_shortcut_subs: Vec::new(),
            #[cfg(feature = "global-shortcuts")]
            global_shortcut_workers: HashMap::new(),
            #[cfg(feature = "control-ipc")]
            control_signal_subs: Vec::new(),
            #[cfg(feature = "control-ipc")]
            signal_source_token: None,
            #[cfg(feature = "control-ipc")]
            control_socket_subs: Vec::new(),
            #[cfg(feature = "control-ipc")]
            control_sockets: HashMap::new(),
        }
    }

    pub(crate) fn sync(
        &mut self,
        specs: &[&SubscriptionSpec<M>],
        loop_handle: &LoopHandle<'static, WaylandState>,
        inbox: &Rc<RefCell<LoopInbox<M>>>,
        clients: SubscriptionClients<'_>,
    ) {
        let _ = (loop_handle, inbox, &clients);
        #[cfg(any(feature = "pipewire", feature = "pulseaudio", feature = "alsa"))]
        let audio_client = clients.audio;
        #[cfg(feature = "hyprland-ipc")]
        let hyprland_client = clients.hyprland;
        #[cfg(feature = "sway-ipc")]
        let sway_client = clients.sway;
        #[cfg(feature = "dbus")]
        let dbus_client = clients.dbus;

        self.keyboard_layout_subs.clear();
        #[cfg(feature = "idle-notify")]
        {
            self.idle_subs.clear();
            self.idle_watches.clear();
        }
        #[cfg(feature = "clipboard")]
        self.clipboard_subs.clear();
        #[cfg(any(feature = "pipewire", feature = "pulseaudio", feature = "alsa"))]
        {
            self.audio_snapshot_subs.clear();
            self.audio_event_subs.clear();
        }
        #[cfg(any(feature = "hyprland-ipc", feature = "sway-ipc"))]
        self.compositor_subs.clear();
        #[cfg(feature = "hyprland-ipc")]
        self.hyprland_subs.clear();
        #[cfg(feature = "sway-ipc")]
        self.sway_subs.clear();
        #[cfg(feature = "dbus")]
        {
            self.dbus_signal_subs.clear();
            self.dbus_notifications.clear_subs();
            self.dbus_tray.clear_subs();
            self.dbus_power_profiles.clear_subs();
            self.dbus_network_manager.clear_subs();
            self.dbus_upower.clear_subs();
            self.dbus_udisks.clear_subs();
            self.dbus_logind.clear_subs();
            self.dbus_bluetooth.clear_subs();
            self.dbus_bluetooth_agent.clear_subs();
            self.dbus_modem.clear_subs();
            self.dbus_screensaver.clear_subs();
            self.dbus_geoclue.clear_subs();
            self.dbus_portal_settings.clear_subs();
            self.dbus_mpris.clear_subs();
            self.dbus_action_results.clear_subs();
        }
        #[cfg(feature = "global-shortcuts")]
        self.global_shortcut_subs.clear();
        #[cfg(feature = "control-ipc")]
        {
            self.control_signal_subs.clear();
            self.control_socket_subs.clear();
        }

        for spec in specs {
            match spec {
                SubscriptionSpec::KeyboardLayout { map } => {
                    self.keyboard_layout_subs.push(map.clone());
                }
                #[cfg(feature = "idle-notify")]
                SubscriptionSpec::IdleNotify {
                    timeout,
                    input_only,
                    map,
                    ..
                } => {
                    self.idle_subs.push((*timeout, *input_only, map.clone()));
                    if !self.idle_watches.contains(&(*timeout, *input_only)) {
                        self.idle_watches.push((*timeout, *input_only));
                    }
                }
                #[cfg(feature = "clipboard")]
                SubscriptionSpec::Clipboard { selection, map, .. } => {
                    self.clipboard_subs.push((*selection, map.clone()));
                }
                #[cfg(any(feature = "pipewire", feature = "pulseaudio", feature = "alsa"))]
                SubscriptionSpec::AudioSnapshot { map } => {
                    self.audio_snapshot_subs.push(map.clone());
                }
                #[cfg(any(feature = "pipewire", feature = "pulseaudio", feature = "alsa"))]
                SubscriptionSpec::AudioEvent { map } => {
                    self.audio_event_subs.push(map.clone());
                }
                #[cfg(feature = "hyprland-ipc")]
                SubscriptionSpec::HyprlandEvent { map } => {
                    self.hyprland_subs.push(map.clone());
                }
                #[cfg(feature = "sway-ipc")]
                SubscriptionSpec::SwayEvent { map } => {
                    self.sway_subs.push(map.clone());
                }
                #[cfg(any(feature = "hyprland-ipc", feature = "sway-ipc"))]
                SubscriptionSpec::CompositorEvent { map } => {
                    self.compositor_subs.push(map.clone());
                }
                #[cfg(feature = "dbus")]
                SubscriptionSpec::DbusSignal {
                    bus,
                    interface,
                    member,
                    path,
                    map,
                    ..
                } => {
                    self.dbus_signal_subs.push((
                        (*bus, interface.clone(), member.clone(), path.clone()),
                        map.clone(),
                    ));
                }
                #[cfg(feature = "dbus")]
                SubscriptionSpec::DbusNotifications { capabilities, map } => {
                    if let Some(caps) = capabilities {
                        dbus_client.notifications().set_capabilities(*caps);
                    }
                    self.dbus_notifications.subs.push(map.clone());
                }
                #[cfg(feature = "dbus")]
                SubscriptionSpec::DbusTray { map } => {
                    self.dbus_tray.subs.push(map.clone());
                }
                #[cfg(feature = "dbus")]
                SubscriptionSpec::DbusPowerProfiles { map } => {
                    self.dbus_power_profiles.subs.push(map.clone());
                }
                #[cfg(feature = "dbus")]
                SubscriptionSpec::DbusNetworkManager { map } => {
                    self.dbus_network_manager.subs.push(map.clone());
                }
                #[cfg(feature = "dbus")]
                SubscriptionSpec::DbusUPower { map } => {
                    self.dbus_upower.subs.push(map.clone());
                }
                #[cfg(feature = "dbus")]
                SubscriptionSpec::DbusUDisks { map } => {
                    self.dbus_udisks.subs.push(map.clone());
                }
                #[cfg(feature = "dbus")]
                SubscriptionSpec::DbusLogind { map } => {
                    self.dbus_logind.subs.push(map.clone());
                }
                #[cfg(feature = "dbus")]
                SubscriptionSpec::DbusBluetooth { map } => {
                    self.dbus_bluetooth.subs.push(map.clone());
                }
                #[cfg(feature = "dbus")]
                SubscriptionSpec::DbusBluetoothAgent { map } => {
                    self.dbus_bluetooth_agent.subs.push(map.clone());
                }
                #[cfg(feature = "dbus")]
                SubscriptionSpec::DbusModem { map } => {
                    self.dbus_modem.subs.push(map.clone());
                }
                #[cfg(feature = "dbus")]
                SubscriptionSpec::DbusScreenSaver { map } => {
                    self.dbus_screensaver.subs.push(map.clone());
                }
                #[cfg(feature = "dbus")]
                SubscriptionSpec::DbusGeoClue { map } => {
                    self.dbus_geoclue.subs.push(map.clone());
                }
                #[cfg(feature = "dbus")]
                SubscriptionSpec::DbusPortalSettings { map } => {
                    self.dbus_portal_settings.subs.push(map.clone());
                }
                #[cfg(feature = "dbus")]
                SubscriptionSpec::DbusMpris { map } => {
                    self.dbus_mpris.subs.push(map.clone());
                }
                #[cfg(feature = "dbus")]
                SubscriptionSpec::DbusActionResults { map } => {
                    self.dbus_action_results.subs.push(map.clone());
                }
                #[cfg(feature = "global-shortcuts")]
                SubscriptionSpec::GlobalShortcuts {
                    key,
                    app_id,
                    shortcuts,
                    map,
                } => {
                    self.global_shortcut_subs.push((
                        *key,
                        app_id.clone(),
                        shortcuts.clone(),
                        map.clone(),
                    ));
                }
                #[cfg(feature = "control-ipc")]
                SubscriptionSpec::ControlSignal { signal, msg, .. } => {
                    self.control_signal_subs.push((*signal, msg.clone()));
                }
                #[cfg(feature = "control-ipc")]
                SubscriptionSpec::ControlSocket { path, map, .. } => {
                    let effective = if let Some(ov) = clients.control_socket_override {
                        ov.to_path_buf()
                    } else if path.as_os_str().is_empty() {
                        crate::control::resolve_socket_path(std::path::Path::new(clients.app_name))
                    } else {
                        path.clone()
                    };
                    self.control_socket_subs.push((effective, map.clone()));
                }
                _ => {}
            }
        }

        #[cfg(any(feature = "pipewire", feature = "pulseaudio", feature = "alsa"))]
        {
            if !self.audio_snapshot_subs.is_empty() || !self.audio_event_subs.is_empty() {
                let _ = audio_client.ensure_started();
            } else {
                audio_client.stop_if_idle();
            }
        }

        #[cfg(feature = "hyprland-ipc")]
        let hyprland_active_for_compositor = hyprland_client.is_some();
        #[cfg(not(feature = "hyprland-ipc"))]
        let hyprland_active_for_compositor = false;
        let _ = hyprland_active_for_compositor;

        #[cfg(feature = "hyprland-ipc")]
        {
            let want_hyprland = !self.hyprland_subs.is_empty()
                || (hyprland_active_for_compositor && !self.compositor_subs.is_empty());
            if want_hyprland {
                if self.hyprland_listener.is_none()
                    && let Some(client) = hyprland_client
                {
                    let sock = client.sockets().event_socket.clone();
                    let (tx, rx) = calloop_channel::channel();
                    let inbox_cl = Rc::clone(inbox);
                    if let Ok(token) = loop_handle.insert_source(rx, move |evt, _, _| {
                        if let calloop_channel::Event::Msg(e) = evt {
                            inbox_cl.borrow_mut().hyprland_events.push_back(e);
                        }
                    }) {
                        let handle = crate::compositor::hyprland::HyprlandListenerHandle::spawn(
                            sock.clone(),
                            tx,
                        );
                        self.hyprland_listener = Some((sock, handle, token));
                    }
                }
            } else if let Some((_, handle, token)) = self.hyprland_listener.take() {
                drop(handle);
                loop_handle.remove(token);
            }
        }

        #[cfg(feature = "sway-ipc")]
        {
            let want_sway = !self.sway_subs.is_empty()
                || (!hyprland_active_for_compositor
                    && sway_client.is_some()
                    && !self.compositor_subs.is_empty());
            if want_sway {
                if self.sway_listener.is_none()
                    && let Some(client) = sway_client
                {
                    let sock = client.socket_path().to_path_buf();
                    let (tx, rx) = calloop_channel::channel();
                    let inbox_cl = Rc::clone(inbox);
                    if let Ok(token) = loop_handle.insert_source(rx, move |evt, _, _| {
                        if let calloop_channel::Event::Msg(e) = evt {
                            inbox_cl.borrow_mut().sway_events.push_back(e);
                        }
                    }) {
                        let handle =
                            crate::compositor::sway::SwayListenerHandle::spawn(sock.clone(), tx);
                        self.sway_listener = Some((sock, handle, token));
                    }
                }
            } else if let Some((_, handle, token)) = self.sway_listener.take() {
                drop(handle);
                loop_handle.remove(token);
            }
        }

        #[cfg(feature = "dbus")]
        self.sync_dbus(loop_handle, inbox, dbus_client);

        #[cfg(feature = "global-shortcuts")]
        {
            let desired: HashSet<SubscriptionKey> = self
                .global_shortcut_subs
                .iter()
                .map(|(k, _, _, _)| *k)
                .collect();
            let existing: Vec<SubscriptionKey> =
                self.global_shortcut_workers.keys().copied().collect();
            for key in existing {
                if !desired.contains(&key)
                    && let Some((worker, token)) = self.global_shortcut_workers.remove(&key)
                {
                    drop(worker);
                    loop_handle.remove(token);
                }
            }
            for (key, app_id, shortcuts, _) in &self.global_shortcut_subs {
                if !self.global_shortcut_workers.contains_key(key) {
                    let (tx, rx) = calloop_channel::channel();
                    let inbox_cl = Rc::clone(inbox);
                    let k_for_cb = *key;
                    if let Ok(token) = loop_handle.insert_source(rx, move |evt, _, _| {
                        if let calloop_channel::Event::Msg(ev) = evt {
                            inbox_cl
                                .borrow_mut()
                                .global_shortcut_events
                                .push_back((k_for_cb, ev));
                        }
                    }) {
                        let worker = crate::dbus::GlobalShortcutsWorker::spawn(
                            app_id.clone(),
                            shortcuts.clone(),
                            tx,
                        );
                        self.global_shortcut_workers.insert(*key, (worker, token));
                    }
                }
            }
        }

        #[cfg(feature = "control-ipc")]
        self.sync_control(loop_handle, inbox);
    }

    #[cfg(feature = "dbus")]
    fn sync_dbus(
        &mut self,
        loop_handle: &LoopHandle<'static, WaylandState>,
        inbox: &Rc<RefCell<LoopInbox<M>>>,
        dbus_client: &crate::dbus::DbusClient,
    ) {
        let desired: HashSet<_> = self
            .dbus_signal_subs
            .iter()
            .map(|(k, _)| k.clone())
            .collect();
        let existing: Vec<_> = self.dbus_signal_workers.keys().cloned().collect();
        for key in existing {
            if !desired.contains(&key)
                && let Some((worker, token)) = self.dbus_signal_workers.remove(&key)
            {
                drop(worker);
                loop_handle.remove(token);
            }
        }
        for key in desired {
            if let std::collections::hash_map::Entry::Vacant(entry) =
                self.dbus_signal_workers.entry(key)
            {
                let (bus, interface, member, path) = entry.key().clone();
                let (tx, rx) = calloop_channel::channel();
                let inbox_cl = Rc::clone(inbox);
                let k_for_cb = entry.key().clone();
                if let Ok(token) = loop_handle.insert_source(rx, move |evt, _, _| {
                    if let calloop_channel::Event::Msg(ev) = evt {
                        inbox_cl
                            .borrow_mut()
                            .dbus_signal_events
                            .push_back((k_for_cb.clone(), ev));
                    }
                }) {
                    let worker = crate::dbus::DbusSignalWorker::spawn(
                        bus,
                        interface,
                        member,
                        path,
                        tx,
                        dbus_client.shared_bus(bus),
                    );
                    entry.insert((worker, token));
                }
            }
        }

        let sys_bus = dbus_client.shared_bus(crate::dbus::DbusBus::System);
        let sess_bus = dbus_client.shared_bus(crate::dbus::DbusBus::Session);
        let cache_enabled = dbus_client.cache_enabled();

        {
            let notif_state = std::sync::Arc::clone(&dbus_client.notifications_state);
            self.dbus_notifications.sync_worker(
                loop_handle,
                |_| {},
                move |tx| crate::dbus::NotificationServerWorker::spawn(tx, notif_state),
            );
        }
        {
            if !self.dbus_tray.is_active()
                && let Ok(mut g) = dbus_client.cache.tray.write()
            {
                *g = None;
            }
            let cache = std::sync::Arc::clone(&dbus_client.cache);
            self.dbus_tray.sync_worker(
                loop_handle,
                move |ev| {
                    if cache_enabled {
                        cache.apply_tray_event(ev);
                    }
                },
                crate::dbus::TrayWorker::spawn,
            );
        }
        {
            if !self.dbus_power_profiles.is_active()
                && let Ok(mut g) = dbus_client.cache.power_profiles.write()
            {
                *g = None;
            }
            let cache = std::sync::Arc::clone(&dbus_client.cache);
            self.dbus_power_profiles.sync_shared_worker(
                loop_handle,
                &sys_bus,
                move |ev| {
                    if cache_enabled && let Ok(mut g) = cache.power_profiles.write() {
                        *g = Some(ev.clone());
                    }
                },
                crate::dbus::PowerProfilesWorker::spawn,
            );
        }
        {
            if !self.dbus_network_manager.is_active()
                && let Ok(mut g) = dbus_client.cache.network_manager.write()
            {
                *g = None;
            }
            let cache = std::sync::Arc::clone(&dbus_client.cache);
            self.dbus_network_manager.sync_shared_worker(
                loop_handle,
                &sys_bus,
                move |ev| {
                    if cache_enabled && let Ok(mut g) = cache.network_manager.write() {
                        *g = Some(ev.clone());
                    }
                },
                crate::dbus::NetworkManagerWorker::spawn,
            );
        }
        {
            if !self.dbus_upower.is_active()
                && let Ok(mut g) = dbus_client.cache.upower.write()
            {
                *g = None;
            }
            let cache = std::sync::Arc::clone(&dbus_client.cache);
            self.dbus_upower.sync_shared_worker(
                loop_handle,
                &sys_bus,
                move |ev| {
                    if cache_enabled && let Ok(mut g) = cache.upower.write() {
                        *g = Some(ev.clone());
                    }
                },
                crate::dbus::spawn_upower_worker,
            );
        }
        {
            if !self.dbus_udisks.is_active()
                && let Ok(mut g) = dbus_client.cache.udisks.write()
            {
                *g = None;
            }
            let cache = std::sync::Arc::clone(&dbus_client.cache);
            self.dbus_udisks.sync_shared_worker(
                loop_handle,
                &sys_bus,
                move |ev| {
                    if cache_enabled && let Ok(mut g) = cache.udisks.write() {
                        *g = Some(ev.clone());
                    }
                },
                crate::dbus::spawn_udisks_worker,
            );
        }
        {
            if !self.dbus_logind.is_active()
                && let Ok(mut g) = dbus_client.cache.logind.write()
            {
                *g = None;
            }
            let cache = std::sync::Arc::clone(&dbus_client.cache);
            self.dbus_logind.sync_shared_worker(
                loop_handle,
                &sys_bus,
                move |ev| {
                    if cache_enabled
                        && let crate::dbus::LogindEvent::Snapshot(ref snap) = *ev
                        && let Ok(mut g) = cache.logind.write()
                    {
                        *g = Some(snap.clone());
                    }
                },
                crate::dbus::LogindWorker::spawn,
            );
        }
        {
            if !self.dbus_bluetooth.is_active()
                && let Ok(mut g) = dbus_client.cache.bluetooth.write()
            {
                *g = None;
            }
            let cache = std::sync::Arc::clone(&dbus_client.cache);
            self.dbus_bluetooth.sync_shared_worker(
                loop_handle,
                &sys_bus,
                move |ev| {
                    if cache_enabled && let Ok(mut g) = cache.bluetooth.write() {
                        *g = Some(ev.clone());
                    }
                },
                crate::dbus::spawn_bluetooth_worker,
            );
        }
        {
            let bt_pending = std::sync::Arc::clone(&dbus_client.bluetooth_agent_pending);
            self.dbus_bluetooth_agent.sync_worker(
                loop_handle,
                |_| {},
                move |tx| crate::dbus::BluetoothAgentWorker::spawn(tx, bt_pending),
            );
        }
        {
            if !self.dbus_modem.is_active()
                && let Ok(mut g) = dbus_client.cache.modem.write()
            {
                *g = None;
            }
            let cache = std::sync::Arc::clone(&dbus_client.cache);
            self.dbus_modem.sync_shared_worker(
                loop_handle,
                &sys_bus,
                move |ev| {
                    if cache_enabled && let Ok(mut g) = cache.modem.write() {
                        *g = Some(ev.clone());
                    }
                },
                crate::dbus::spawn_modem_worker,
            );
        }
        self.dbus_screensaver.sync_shared_worker(
            loop_handle,
            &sess_bus,
            |_| {},
            crate::dbus::spawn_screensaver_worker,
        );
        self.dbus_geoclue.sync_shared_worker(
            loop_handle,
            &sys_bus,
            |_| {},
            crate::dbus::spawn_geoclue_worker,
        );
        self.dbus_portal_settings.sync_shared_worker(
            loop_handle,
            &sess_bus,
            |_| {},
            crate::dbus::spawn_portal_settings_worker,
        );
        {
            if !self.dbus_mpris.is_active()
                && let Ok(mut g) = dbus_client.cache.mpris.write()
            {
                *g = None;
            }
            let cache = std::sync::Arc::clone(&dbus_client.cache);
            self.dbus_mpris.sync_shared_worker(
                loop_handle,
                &sess_bus,
                move |ev| {
                    if cache_enabled
                        && let crate::dbus::MprisEvent::PlayersUpdated(ref list) = *ev
                        && let Ok(mut g) = cache.mpris.write()
                    {
                        *g = Some(list.clone());
                    }
                },
                crate::dbus::MprisWorker::spawn,
            );
        }
        {
            let action_outbox = std::sync::Arc::clone(&dbus_client.action_outbox);
            self.dbus_action_results.sync_worker(
                loop_handle,
                |_| {},
                move |tx| crate::dbus::DbusActionResultWorker::spawn(tx, action_outbox),
            );
        }
    }

    #[cfg(feature = "control-ipc")]
    fn sync_control(
        &mut self,
        loop_handle: &LoopHandle<'static, WaylandState>,
        inbox: &Rc<RefCell<LoopInbox<M>>>,
    ) {
        if !self.control_signal_subs.is_empty() {
            if self.signal_source_token.is_none() {
                use calloop::signals::{Signal as CalloopSig, Signals};
                if let Ok(src) = Signals::new(&[CalloopSig::SIGUSR1, CalloopSig::SIGUSR2]) {
                    let inbox_cl = Rc::clone(inbox);
                    if let Ok(token) = loop_handle.insert_source(src, move |evt, _, _| {
                        let sig = match evt.signal() {
                            CalloopSig::SIGUSR1 => Some(crate::control::Signal::Usr1),
                            CalloopSig::SIGUSR2 => Some(crate::control::Signal::Usr2),
                            _ => None,
                        };
                        if let Some(s) = sig {
                            inbox_cl.borrow_mut().control_signals.push_back(s);
                        }
                    }) {
                        self.signal_source_token = Some(token);
                    }
                }
            }
        } else if let Some(token) = self.signal_source_token.take() {
            loop_handle.remove(token);
        }

        let desired_paths: HashSet<PathBuf> = self
            .control_socket_subs
            .iter()
            .map(|(p, _)| p.clone())
            .collect();
        let existing_paths: Vec<PathBuf> = self.control_sockets.keys().cloned().collect();
        for path in existing_paths {
            if !desired_paths.contains(&path)
                && let Some(reg) = self.control_sockets.remove(&path)
            {
                reg.remove(loop_handle);
            }
        }
        for path in desired_paths {
            if let std::collections::hash_map::Entry::Vacant(entry) =
                self.control_sockets.entry(path)
            {
                let inbox_cl = Rc::clone(inbox);
                let path_for_cb = entry.key().clone();
                if let Ok(reg) = crate::control::ControlSocketRegistration::bind_and_insert(
                    entry.key().clone(),
                    loop_handle,
                    move |reqs| {
                        let mut g = inbox_cl.borrow_mut();
                        for req in reqs {
                            g.control_commands.push_back((path_for_cb.clone(), req));
                        }
                    },
                ) {
                    entry.insert(reg);
                }
            }
        }
    }

    pub(crate) fn cleanup(&mut self, loop_handle: &LoopHandle<'static, WaylandState>) {
        let _ = loop_handle;
        #[cfg(feature = "hyprland-ipc")]
        if let Some((_, handle, token)) = self.hyprland_listener.take() {
            drop(handle);
            loop_handle.remove(token);
        }
        #[cfg(feature = "sway-ipc")]
        if let Some((_, handle, token)) = self.sway_listener.take() {
            drop(handle);
            loop_handle.remove(token);
        }
        #[cfg(feature = "dbus")]
        {
            for (_, (worker, token)) in self.dbus_signal_workers.drain() {
                drop(worker);
                loop_handle.remove(token);
            }
            self.dbus_notifications.cleanup(loop_handle);
            self.dbus_tray.cleanup(loop_handle);
            self.dbus_power_profiles.cleanup(loop_handle);
            self.dbus_network_manager.cleanup(loop_handle);
            self.dbus_upower.cleanup(loop_handle);
            self.dbus_udisks.cleanup(loop_handle);
            self.dbus_logind.cleanup(loop_handle);
            self.dbus_bluetooth.cleanup(loop_handle);
            self.dbus_bluetooth_agent.cleanup(loop_handle);
            self.dbus_modem.cleanup(loop_handle);
            self.dbus_screensaver.cleanup(loop_handle);
            self.dbus_geoclue.cleanup(loop_handle);
            self.dbus_portal_settings.cleanup(loop_handle);
            self.dbus_mpris.cleanup(loop_handle);
            self.dbus_action_results.cleanup(loop_handle);
        }
        #[cfg(feature = "global-shortcuts")]
        {
            for (_, (worker, token)) in self.global_shortcut_workers.drain() {
                drop(worker);
                loop_handle.remove(token);
            }
        }
        #[cfg(feature = "control-ipc")]
        {
            if let Some(token) = self.signal_source_token.take() {
                loop_handle.remove(token);
            }
            for (_, reg) in self.control_sockets.drain() {
                reg.remove(loop_handle);
            }
        }
    }
}
