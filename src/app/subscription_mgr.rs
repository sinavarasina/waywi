use std::cell::RefCell;
use std::collections::VecDeque;
#[cfg(feature = "dbus")]
use std::collections::{HashMap, HashSet};
#[cfg(any(
    feature = "hyprland-ipc",
    feature = "sway-ipc",
    feature = "control-ipc"
))]
use std::path::PathBuf;
use std::rc::Rc;

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
    pub(crate) hyprland: Option<&'a crate::hyprland::HyprlandClient>,
    #[cfg(feature = "sway-ipc")]
    pub(crate) sway: Option<&'a crate::sway::SwayClient>,
    #[cfg(feature = "dbus")]
    pub(crate) dbus: &'a crate::dbus::DbusClient,
    #[cfg(feature = "control-ipc")]
    pub(crate) control_socket_override: Option<&'a std::path::Path>,
    pub(crate) _marker: std::marker::PhantomData<&'a ()>,
}

pub(crate) struct LoopInbox<M> {
    pub(crate) messages: VecDeque<M>,
    #[cfg(feature = "hyprland-ipc")]
    pub(crate) hyprland_events: VecDeque<crate::hyprland::HyprlandEvent>,
    #[cfg(feature = "sway-ipc")]
    pub(crate) sway_events: VecDeque<crate::sway::SwayEvent>,
    #[cfg(feature = "dbus")]
    pub(crate) dbus_signal_events: VecDeque<(DbusSignalKey, crate::dbus::DbusSignalEvent)>,
    #[cfg(feature = "dbus")]
    pub(crate) dbus_notifications: VecDeque<crate::dbus::NotificationEvent>,
    #[cfg(feature = "dbus")]
    pub(crate) dbus_tray: VecDeque<crate::dbus::TrayEvent>,
    #[cfg(feature = "dbus")]
    pub(crate) dbus_power_profiles: VecDeque<crate::dbus::PowerProfileSnapshot>,
    #[cfg(feature = "dbus")]
    pub(crate) dbus_network_manager: VecDeque<crate::dbus::NetworkManagerSnapshot>,
    #[cfg(feature = "dbus")]
    pub(crate) dbus_upower: VecDeque<crate::dbus::UPowerSnapshot>,
    #[cfg(feature = "dbus")]
    pub(crate) dbus_logind: VecDeque<crate::dbus::LogindEvent>,
    #[cfg(feature = "dbus")]
    pub(crate) dbus_bluetooth: VecDeque<crate::dbus::BluetoothSnapshot>,
    #[cfg(feature = "dbus")]
    pub(crate) dbus_bluetooth_agent: VecDeque<crate::dbus::BluetoothAgentEvent>,
    #[cfg(feature = "dbus")]
    pub(crate) dbus_modem: VecDeque<crate::dbus::ModemSnapshot>,
    #[cfg(feature = "dbus")]
    pub(crate) dbus_screensaver: VecDeque<crate::dbus::ScreenSaverSnapshot>,
    #[cfg(feature = "dbus")]
    pub(crate) dbus_geoclue: VecDeque<crate::dbus::GeoClueSnapshot>,
    #[cfg(feature = "dbus")]
    pub(crate) dbus_mpris: VecDeque<crate::dbus::MprisEvent>,
    #[cfg(feature = "dbus")]
    pub(crate) dbus_action_results: VecDeque<crate::dbus::DbusActionResult>,
    #[cfg(feature = "global-shortcuts")]
    pub(crate) global_shortcut_events:
        VecDeque<(SubscriptionKey, crate::dbus::GlobalShortcutEvent)>,
    #[cfg(feature = "control-ipc")]
    pub(crate) control_signals: VecDeque<crate::control::Signal>,
    #[cfg(feature = "control-ipc")]
    pub(crate) control_commands: VecDeque<crate::control::ControlRequest>,
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
            #[cfg(feature = "dbus")]
            dbus_notifications: VecDeque::new(),
            #[cfg(feature = "dbus")]
            dbus_tray: VecDeque::new(),
            #[cfg(feature = "dbus")]
            dbus_power_profiles: VecDeque::new(),
            #[cfg(feature = "dbus")]
            dbus_network_manager: VecDeque::new(),
            #[cfg(feature = "dbus")]
            dbus_upower: VecDeque::new(),
            #[cfg(feature = "dbus")]
            dbus_logind: VecDeque::new(),
            #[cfg(feature = "dbus")]
            dbus_bluetooth: VecDeque::new(),
            #[cfg(feature = "dbus")]
            dbus_bluetooth_agent: VecDeque::new(),
            #[cfg(feature = "dbus")]
            dbus_modem: VecDeque::new(),
            #[cfg(feature = "dbus")]
            dbus_screensaver: VecDeque::new(),
            #[cfg(feature = "dbus")]
            dbus_geoclue: VecDeque::new(),
            #[cfg(feature = "dbus")]
            dbus_mpris: VecDeque::new(),
            #[cfg(feature = "dbus")]
            dbus_action_results: VecDeque::new(),
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
    pub(crate) audio_snapshot_subs: Vec<FrameSharedFn<crate::audio::AudioSnapshot, M>>,
    #[cfg(any(feature = "pipewire", feature = "pulseaudio", feature = "alsa"))]
    pub(crate) audio_event_subs: Vec<FrameSharedFn<crate::audio::AudioEvent, M>>,

    #[cfg(feature = "hyprland-ipc")]
    pub(crate) hyprland_subs: Vec<FrameSharedFn<crate::hyprland::HyprlandEvent, M>>,
    #[cfg(feature = "hyprland-ipc")]
    hyprland_listener: Option<(
        PathBuf,
        crate::hyprland::HyprlandListenerHandle,
        calloop::RegistrationToken,
    )>,

    #[cfg(feature = "sway-ipc")]
    pub(crate) sway_subs: Vec<FrameSharedFn<crate::sway::SwayEvent, M>>,
    #[cfg(feature = "sway-ipc")]
    sway_listener: Option<(
        PathBuf,
        crate::sway::SwayListenerHandle,
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
    pub(crate) dbus_notification_subs: Vec<FrameSharedFn<crate::dbus::NotificationEvent, M>>,
    #[cfg(feature = "dbus")]
    dbus_notification_worker: Option<(
        crate::dbus::NotificationServerWorker,
        calloop::RegistrationToken,
    )>,
    #[cfg(feature = "dbus")]
    pub(crate) dbus_tray_subs: Vec<FrameSharedFn<crate::dbus::TrayEvent, M>>,
    #[cfg(feature = "dbus")]
    dbus_tray_worker: Option<(crate::dbus::TrayWorker, calloop::RegistrationToken)>,
    #[cfg(feature = "dbus")]
    pub(crate) dbus_power_profile_subs: Vec<FrameSharedFn<crate::dbus::PowerProfileSnapshot, M>>,
    #[cfg(feature = "dbus")]
    dbus_power_profile_worker:
        Option<(crate::dbus::PowerProfilesWorker, calloop::RegistrationToken)>,
    #[cfg(feature = "dbus")]
    pub(crate) dbus_network_manager_subs:
        Vec<FrameSharedFn<crate::dbus::NetworkManagerSnapshot, M>>,
    #[cfg(feature = "dbus")]
    dbus_network_manager_worker: Option<(
        crate::dbus::NetworkManagerWorker,
        calloop::RegistrationToken,
    )>,
    #[cfg(feature = "dbus")]
    pub(crate) dbus_upower_subs: Vec<FrameSharedFn<crate::dbus::UPowerSnapshot, M>>,
    #[cfg(feature = "dbus")]
    dbus_upower_worker: Option<(crate::dbus::ServiceWatchWorker, calloop::RegistrationToken)>,
    #[cfg(feature = "dbus")]
    pub(crate) dbus_logind_subs: Vec<FrameSharedFn<crate::dbus::LogindEvent, M>>,
    #[cfg(feature = "dbus")]
    dbus_logind_worker: Option<(crate::dbus::LogindWorker, calloop::RegistrationToken)>,
    #[cfg(feature = "dbus")]
    pub(crate) dbus_bluetooth_subs: Vec<FrameSharedFn<crate::dbus::BluetoothSnapshot, M>>,
    #[cfg(feature = "dbus")]
    dbus_bluetooth_worker: Option<(crate::dbus::ServiceWatchWorker, calloop::RegistrationToken)>,
    #[cfg(feature = "dbus")]
    pub(crate) dbus_bluetooth_agent_subs: Vec<FrameSharedFn<crate::dbus::BluetoothAgentEvent, M>>,
    #[cfg(feature = "dbus")]
    dbus_bluetooth_agent_worker: Option<(
        crate::dbus::BluetoothAgentWorker,
        calloop::RegistrationToken,
    )>,
    #[cfg(feature = "dbus")]
    pub(crate) dbus_modem_subs: Vec<FrameSharedFn<crate::dbus::ModemSnapshot, M>>,
    #[cfg(feature = "dbus")]
    dbus_modem_worker: Option<(crate::dbus::ServiceWatchWorker, calloop::RegistrationToken)>,
    #[cfg(feature = "dbus")]
    pub(crate) dbus_screensaver_subs: Vec<FrameSharedFn<crate::dbus::ScreenSaverSnapshot, M>>,
    #[cfg(feature = "dbus")]
    dbus_screensaver_worker: Option<(crate::dbus::ServiceWatchWorker, calloop::RegistrationToken)>,
    #[cfg(feature = "dbus")]
    pub(crate) dbus_geoclue_subs: Vec<FrameSharedFn<crate::dbus::GeoClueSnapshot, M>>,
    #[cfg(feature = "dbus")]
    dbus_geoclue_worker: Option<(crate::dbus::ServiceWatchWorker, calloop::RegistrationToken)>,
    #[cfg(feature = "dbus")]
    pub(crate) dbus_mpris_subs: Vec<FrameSharedFn<crate::dbus::MprisEvent, M>>,
    #[cfg(feature = "dbus")]
    dbus_mpris_worker: Option<(crate::dbus::MprisWorker, calloop::RegistrationToken)>,
    #[cfg(feature = "dbus")]
    pub(crate) dbus_action_result_subs: Vec<FrameSharedFn<crate::dbus::DbusActionResult, M>>,
    #[cfg(feature = "dbus")]
    dbus_action_result_worker: Option<(
        crate::dbus::DbusActionResultWorker,
        calloop::RegistrationToken,
    )>,
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
    pub(crate) control_socket_subs: Vec<(PathBuf, FrameSharedFn<String, Option<M>>)>,
    #[cfg(feature = "control-ipc")]
    pub(crate) control_request_subs: Vec<(
        PathBuf,
        FrameSharedFn<crate::control::ControlRequest, Option<M>>,
    )>,
    #[cfg(feature = "control-ipc")]
    control_socket: Option<crate::control::ControlSocketRegistration>,
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
            dbus_notification_subs: Vec::new(),
            #[cfg(feature = "dbus")]
            dbus_notification_worker: None,
            #[cfg(feature = "dbus")]
            dbus_tray_subs: Vec::new(),
            #[cfg(feature = "dbus")]
            dbus_tray_worker: None,
            #[cfg(feature = "dbus")]
            dbus_power_profile_subs: Vec::new(),
            #[cfg(feature = "dbus")]
            dbus_power_profile_worker: None,
            #[cfg(feature = "dbus")]
            dbus_network_manager_subs: Vec::new(),
            #[cfg(feature = "dbus")]
            dbus_network_manager_worker: None,
            #[cfg(feature = "dbus")]
            dbus_upower_subs: Vec::new(),
            #[cfg(feature = "dbus")]
            dbus_upower_worker: None,
            #[cfg(feature = "dbus")]
            dbus_logind_subs: Vec::new(),
            #[cfg(feature = "dbus")]
            dbus_logind_worker: None,
            #[cfg(feature = "dbus")]
            dbus_bluetooth_subs: Vec::new(),
            #[cfg(feature = "dbus")]
            dbus_bluetooth_worker: None,
            #[cfg(feature = "dbus")]
            dbus_bluetooth_agent_subs: Vec::new(),
            #[cfg(feature = "dbus")]
            dbus_bluetooth_agent_worker: None,
            #[cfg(feature = "dbus")]
            dbus_modem_subs: Vec::new(),
            #[cfg(feature = "dbus")]
            dbus_modem_worker: None,
            #[cfg(feature = "dbus")]
            dbus_screensaver_subs: Vec::new(),
            #[cfg(feature = "dbus")]
            dbus_screensaver_worker: None,
            #[cfg(feature = "dbus")]
            dbus_geoclue_subs: Vec::new(),
            #[cfg(feature = "dbus")]
            dbus_geoclue_worker: None,
            #[cfg(feature = "dbus")]
            dbus_mpris_subs: Vec::new(),
            #[cfg(feature = "dbus")]
            dbus_mpris_worker: None,
            #[cfg(feature = "dbus")]
            dbus_action_result_subs: Vec::new(),
            #[cfg(feature = "dbus")]
            dbus_action_result_worker: None,
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
            control_request_subs: Vec::new(),
            #[cfg(feature = "control-ipc")]
            control_socket: None,
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
        #[cfg(feature = "hyprland-ipc")]
        self.hyprland_subs.clear();
        #[cfg(feature = "sway-ipc")]
        self.sway_subs.clear();
        #[cfg(feature = "dbus")]
        {
            self.dbus_signal_subs.clear();
            self.dbus_notification_subs.clear();
            self.dbus_tray_subs.clear();
            self.dbus_power_profile_subs.clear();
            self.dbus_network_manager_subs.clear();
            self.dbus_upower_subs.clear();
            self.dbus_logind_subs.clear();
            self.dbus_bluetooth_subs.clear();
            self.dbus_bluetooth_agent_subs.clear();
            self.dbus_modem_subs.clear();
            self.dbus_screensaver_subs.clear();
            self.dbus_geoclue_subs.clear();
            self.dbus_mpris_subs.clear();
            self.dbus_action_result_subs.clear();
        }
        #[cfg(feature = "global-shortcuts")]
        self.global_shortcut_subs.clear();
        #[cfg(feature = "control-ipc")]
        {
            self.control_signal_subs.clear();
            self.control_socket_subs.clear();
            self.control_request_subs.clear();
        }

        for spec in specs {
            match spec {
                SubscriptionSpec::KeyboardLayout { map, .. } => {
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
                SubscriptionSpec::AudioSnapshot { map, .. } => {
                    self.audio_snapshot_subs.push(map.clone());
                }
                #[cfg(any(feature = "pipewire", feature = "pulseaudio", feature = "alsa"))]
                SubscriptionSpec::AudioEvent { map, .. } => {
                    self.audio_event_subs.push(map.clone());
                }
                #[cfg(feature = "hyprland-ipc")]
                SubscriptionSpec::HyprlandEvent { map, .. } => {
                    self.hyprland_subs.push(map.clone());
                }
                #[cfg(feature = "sway-ipc")]
                SubscriptionSpec::SwayEvent { map, .. } => {
                    self.sway_subs.push(map.clone());
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
                SubscriptionSpec::DbusNotifications { map, .. } => {
                    self.dbus_notification_subs.push(map.clone());
                }
                #[cfg(feature = "dbus")]
                SubscriptionSpec::DbusTray { map, .. } => {
                    self.dbus_tray_subs.push(map.clone());
                }
                #[cfg(feature = "dbus")]
                SubscriptionSpec::DbusPowerProfiles { map, .. } => {
                    self.dbus_power_profile_subs.push(map.clone());
                }
                #[cfg(feature = "dbus")]
                SubscriptionSpec::DbusNetworkManager { map, .. } => {
                    self.dbus_network_manager_subs.push(map.clone());
                }
                #[cfg(feature = "dbus")]
                SubscriptionSpec::DbusUPower { map, .. } => {
                    self.dbus_upower_subs.push(map.clone());
                }
                #[cfg(feature = "dbus")]
                SubscriptionSpec::DbusLogind { map, .. } => {
                    self.dbus_logind_subs.push(map.clone());
                }
                #[cfg(feature = "dbus")]
                SubscriptionSpec::DbusBluetooth { map, .. } => {
                    self.dbus_bluetooth_subs.push(map.clone());
                }
                #[cfg(feature = "dbus")]
                SubscriptionSpec::DbusBluetoothAgent { map, .. } => {
                    self.dbus_bluetooth_agent_subs.push(map.clone());
                }
                #[cfg(feature = "dbus")]
                SubscriptionSpec::DbusModem { map, .. } => {
                    self.dbus_modem_subs.push(map.clone());
                }
                #[cfg(feature = "dbus")]
                SubscriptionSpec::DbusScreenSaver { map, .. } => {
                    self.dbus_screensaver_subs.push(map.clone());
                }
                #[cfg(feature = "dbus")]
                SubscriptionSpec::DbusGeoClue { map, .. } => {
                    self.dbus_geoclue_subs.push(map.clone());
                }
                #[cfg(feature = "dbus")]
                SubscriptionSpec::DbusMpris { map, .. } => {
                    self.dbus_mpris_subs.push(map.clone());
                }
                #[cfg(feature = "dbus")]
                SubscriptionSpec::DbusActionResults { map, .. } => {
                    self.dbus_action_result_subs.push(map.clone());
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
                SubscriptionSpec::ControlSocket {
                    socket_path, map, ..
                } => {
                    self.control_socket_subs
                        .push((socket_path.clone(), map.clone()));
                }
                #[cfg(feature = "control-ipc")]
                SubscriptionSpec::ControlSocketRequest {
                    socket_path, map, ..
                } => {
                    self.control_request_subs
                        .push((socket_path.clone(), map.clone()));
                }
                _ => {}
            }
        }

        #[cfg(any(feature = "pipewire", feature = "pulseaudio", feature = "alsa"))]
        if !self.audio_snapshot_subs.is_empty() || !self.audio_event_subs.is_empty() {
            audio_client.ensure_started();
        }

        #[cfg(feature = "hyprland-ipc")]
        {
            if !self.hyprland_subs.is_empty() {
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
                        let handle =
                            crate::hyprland::HyprlandListenerHandle::spawn(sock.clone(), tx);
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
            if !self.sway_subs.is_empty() {
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
                        let handle = crate::sway::SwayListenerHandle::spawn(sock.clone(), tx);
                        self.sway_listener = Some((sock, handle, token));
                    }
                }
            } else if let Some((_, handle, token)) = self.sway_listener.take() {
                drop(handle);
                loop_handle.remove(token);
            }
        }

        #[cfg(feature = "dbus")]
        {
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

            {
                let inbox_cl = Rc::clone(inbox);
                let notif_conn = std::sync::Arc::clone(&dbus_client.notifications_conn);
                sync_singleton_dbus_worker(
                    !self.dbus_notification_subs.is_empty(),
                    &mut self.dbus_notification_worker,
                    loop_handle,
                    move |ev| inbox_cl.borrow_mut().dbus_notifications.push_back(ev),
                    move |tx| crate::dbus::NotificationServerWorker::spawn(tx, notif_conn),
                );
            }
            {
                let inbox_cl = Rc::clone(inbox);
                sync_singleton_dbus_worker(
                    !self.dbus_tray_subs.is_empty(),
                    &mut self.dbus_tray_worker,
                    loop_handle,
                    move |ev| inbox_cl.borrow_mut().dbus_tray.push_back(ev),
                    crate::dbus::TrayWorker::spawn,
                );
            }
            {
                let active = !self.dbus_power_profile_subs.is_empty();
                if !active && let Ok(mut g) = dbus_client.cache.power_profiles.write() {
                    *g = None;
                }
                let inbox_cl = Rc::clone(inbox);
                let cache = std::sync::Arc::clone(&dbus_client.cache);
                sync_shared_dbus_worker(
                    active,
                    &mut self.dbus_power_profile_worker,
                    loop_handle,
                    &sys_bus,
                    move |ev| {
                        if let Ok(mut g) = cache.power_profiles.write() {
                            *g = Some(ev.clone());
                        }
                        inbox_cl.borrow_mut().dbus_power_profiles.push_back(ev);
                    },
                    crate::dbus::PowerProfilesWorker::spawn,
                );
            }
            {
                let active = !self.dbus_network_manager_subs.is_empty();
                if !active && let Ok(mut g) = dbus_client.cache.network_manager.write() {
                    *g = None;
                }
                let inbox_cl = Rc::clone(inbox);
                let cache = std::sync::Arc::clone(&dbus_client.cache);
                sync_shared_dbus_worker(
                    active,
                    &mut self.dbus_network_manager_worker,
                    loop_handle,
                    &sys_bus,
                    move |ev| {
                        if let Ok(mut g) = cache.network_manager.write() {
                            *g = Some(ev.clone());
                        }
                        inbox_cl.borrow_mut().dbus_network_manager.push_back(ev);
                    },
                    crate::dbus::NetworkManagerWorker::spawn,
                );
            }
            {
                let active = !self.dbus_upower_subs.is_empty();
                if !active && let Ok(mut g) = dbus_client.cache.upower.write() {
                    *g = None;
                }
                let inbox_cl = Rc::clone(inbox);
                let cache = std::sync::Arc::clone(&dbus_client.cache);
                sync_shared_dbus_worker(
                    active,
                    &mut self.dbus_upower_worker,
                    loop_handle,
                    &sys_bus,
                    move |ev| {
                        if let Ok(mut g) = cache.upower.write() {
                            *g = Some(ev.clone());
                        }
                        inbox_cl.borrow_mut().dbus_upower.push_back(ev);
                    },
                    crate::dbus::spawn_upower_worker,
                );
            }
            {
                let active = !self.dbus_logind_subs.is_empty();
                if !active && let Ok(mut g) = dbus_client.cache.logind.write() {
                    *g = None;
                }
                let inbox_cl = Rc::clone(inbox);
                let cache = std::sync::Arc::clone(&dbus_client.cache);
                sync_shared_dbus_worker(
                    active,
                    &mut self.dbus_logind_worker,
                    loop_handle,
                    &sys_bus,
                    move |ev| {
                        if let crate::dbus::LogindEvent::Snapshot(ref snap) = ev
                            && let Ok(mut g) = cache.logind.write()
                        {
                            *g = Some(snap.clone());
                        }
                        inbox_cl.borrow_mut().dbus_logind.push_back(ev);
                    },
                    crate::dbus::LogindWorker::spawn,
                );
            }
            {
                let active = !self.dbus_bluetooth_subs.is_empty();
                if !active && let Ok(mut g) = dbus_client.cache.bluetooth.write() {
                    *g = None;
                }
                let inbox_cl = Rc::clone(inbox);
                let cache = std::sync::Arc::clone(&dbus_client.cache);
                sync_shared_dbus_worker(
                    active,
                    &mut self.dbus_bluetooth_worker,
                    loop_handle,
                    &sys_bus,
                    move |ev| {
                        if let Ok(mut g) = cache.bluetooth.write() {
                            *g = Some(ev.clone());
                        }
                        inbox_cl.borrow_mut().dbus_bluetooth.push_back(ev);
                    },
                    crate::dbus::spawn_bluetooth_worker,
                );
            }
            {
                let inbox_cl = Rc::clone(inbox);
                let bt_pending = std::sync::Arc::clone(&dbus_client.bluetooth_agent_pending);
                sync_singleton_dbus_worker(
                    !self.dbus_bluetooth_agent_subs.is_empty(),
                    &mut self.dbus_bluetooth_agent_worker,
                    loop_handle,
                    move |ev| inbox_cl.borrow_mut().dbus_bluetooth_agent.push_back(ev),
                    move |tx| crate::dbus::BluetoothAgentWorker::spawn(tx, bt_pending),
                );
            }
            {
                let active = !self.dbus_modem_subs.is_empty();
                if !active && let Ok(mut g) = dbus_client.cache.modem.write() {
                    *g = None;
                }
                let inbox_cl = Rc::clone(inbox);
                let cache = std::sync::Arc::clone(&dbus_client.cache);
                sync_shared_dbus_worker(
                    active,
                    &mut self.dbus_modem_worker,
                    loop_handle,
                    &sys_bus,
                    move |ev| {
                        if let Ok(mut g) = cache.modem.write() {
                            *g = Some(ev.clone());
                        }
                        inbox_cl.borrow_mut().dbus_modem.push_back(ev);
                    },
                    crate::dbus::spawn_modem_worker,
                );
            }
            {
                let inbox_cl = Rc::clone(inbox);
                sync_shared_dbus_worker(
                    !self.dbus_screensaver_subs.is_empty(),
                    &mut self.dbus_screensaver_worker,
                    loop_handle,
                    &sess_bus,
                    move |ev| inbox_cl.borrow_mut().dbus_screensaver.push_back(ev),
                    crate::dbus::spawn_screensaver_worker,
                );
            }
            {
                let inbox_cl = Rc::clone(inbox);
                sync_shared_dbus_worker(
                    !self.dbus_geoclue_subs.is_empty(),
                    &mut self.dbus_geoclue_worker,
                    loop_handle,
                    &sys_bus,
                    move |ev| inbox_cl.borrow_mut().dbus_geoclue.push_back(ev),
                    crate::dbus::spawn_geoclue_worker,
                );
            }
            {
                let active = !self.dbus_mpris_subs.is_empty();
                if !active && let Ok(mut g) = dbus_client.cache.mpris.write() {
                    *g = None;
                }
                let inbox_cl = Rc::clone(inbox);
                let cache = std::sync::Arc::clone(&dbus_client.cache);
                sync_shared_dbus_worker(
                    active,
                    &mut self.dbus_mpris_worker,
                    loop_handle,
                    &sess_bus,
                    move |ev| {
                        if let crate::dbus::MprisEvent::PlayersUpdated(ref list) = ev
                            && let Ok(mut g) = cache.mpris.write()
                        {
                            *g = Some(list.clone());
                        }
                        inbox_cl.borrow_mut().dbus_mpris.push_back(ev);
                    },
                    crate::dbus::MprisWorker::spawn,
                );
            }
            {
                let inbox_cl = Rc::clone(inbox);
                let action_outbox = std::sync::Arc::clone(&dbus_client.action_outbox);
                sync_singleton_dbus_worker(
                    !self.dbus_action_result_subs.is_empty(),
                    &mut self.dbus_action_result_worker,
                    loop_handle,
                    move |ev| inbox_cl.borrow_mut().dbus_action_results.push_back(ev),
                    move |tx| crate::dbus::DbusActionResultWorker::spawn(tx, action_outbox),
                );
            }
        }

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
        {
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

            let target_socket = clients
                .control_socket_override
                .map(PathBuf::from)
                .or_else(|| self.control_socket_subs.first().map(|(p, _)| p.clone()))
                .or_else(|| self.control_request_subs.first().map(|(p, _)| p.clone()));
            let reply_enabled = !self.control_request_subs.is_empty();
            match target_socket {
                Some(path) => {
                    let needs_rebind = self
                        .control_socket
                        .as_ref()
                        .is_none_or(|reg| reg.path() != path);
                    if needs_rebind {
                        if let Some(old) = self.control_socket.take() {
                            old.remove(loop_handle);
                        }
                        let inbox_cl = Rc::clone(inbox);
                        if let Ok(reg) = crate::control::ControlSocketRegistration::bind_and_insert(
                            path,
                            loop_handle,
                            reply_enabled,
                            move |reqs| {
                                let mut g = inbox_cl.borrow_mut();
                                for req in reqs {
                                    g.control_commands.push_back(req);
                                }
                            },
                        ) {
                            self.control_socket = Some(reg);
                        }
                    } else if let Some(ref reg) = self.control_socket {
                        reg.set_reply_enabled(reply_enabled);
                    }
                }
                None => {
                    if let Some(old) = self.control_socket.take() {
                        old.remove(loop_handle);
                    }
                }
            }
        }
    }

    pub(crate) fn cleanup(&mut self, loop_handle: &LoopHandle<'static, WaylandState>) {
        let _ = loop_handle;
        #[cfg(feature = "dbus")]
        macro_rules! stop_worker {
            ($($slot:expr),+ $(,)?) => {
                $(
                    if let Some((worker, token)) = $slot.take() {
                        drop(worker);
                        loop_handle.remove(token);
                    }
                )+
            };
        }
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
            stop_worker!(
                self.dbus_notification_worker,
                self.dbus_tray_worker,
                self.dbus_power_profile_worker,
                self.dbus_network_manager_worker,
                self.dbus_upower_worker,
                self.dbus_logind_worker,
                self.dbus_bluetooth_worker,
                self.dbus_bluetooth_agent_worker,
                self.dbus_modem_worker,
                self.dbus_screensaver_worker,
                self.dbus_geoclue_worker,
                self.dbus_mpris_worker,
                self.dbus_action_result_worker,
            );
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
            if let Some(reg) = self.control_socket.take() {
                reg.remove(loop_handle);
            }
        }
    }
}

#[cfg(feature = "dbus")]
fn sync_shared_dbus_worker<Ev: 'static, W>(
    active: bool,
    slot: &mut Option<(W, calloop::RegistrationToken)>,
    loop_handle: &LoopHandle<'static, WaylandState>,
    shared: &crate::dbus::DbusSharedBus,
    on_event: impl Fn(Ev) + 'static,
    spawn: impl FnOnce(calloop_channel::Sender<Ev>, crate::dbus::DbusSharedBus) -> W,
) {
    let shared = std::sync::Arc::clone(shared);
    sync_singleton_dbus_worker(active, slot, loop_handle, on_event, move |tx| {
        spawn(tx, shared)
    });
}

#[cfg(feature = "dbus")]
fn sync_singleton_dbus_worker<Ev: 'static, W>(
    active: bool,
    slot: &mut Option<(W, calloop::RegistrationToken)>,
    loop_handle: &LoopHandle<'static, WaylandState>,
    on_event: impl Fn(Ev) + 'static,
    spawn: impl FnOnce(calloop_channel::Sender<Ev>) -> W,
) {
    if active {
        if slot.is_none() {
            let (tx, rx) = calloop_channel::channel();
            if let Ok(token) = loop_handle.insert_source(rx, move |evt, _, _| {
                if let calloop_channel::Event::Msg(ev) = evt {
                    on_event(ev);
                }
            }) {
                *slot = Some((spawn(tx), token));
            }
        }
    } else if let Some((worker, token)) = slot.take() {
        drop(worker);
        loop_handle.remove(token);
    }
}
