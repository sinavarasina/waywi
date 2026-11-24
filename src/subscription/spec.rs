use std::ops::Deref;
#[cfg(feature = "control-ipc")]
use std::path::PathBuf;
#[cfg(any(feature = "pipewire", feature = "pulseaudio", feature = "alsa"))]
use std::sync::Arc;
use std::time::Duration;

use crate::arena::{FrameSharedFn, FrameVec};

use super::key::SubscriptionKey;

/// Internal subscription descriptor kinds reconciled by the runtime.
#[derive(Clone)]
pub(crate) enum SubscriptionSpec<M> {
    None,
    Batch(FrameVec<Subscription<M>>),
    TimerOnce {
        key: SubscriptionKey,
        duration: Duration,
        msg: M,
    },
    TimerEvery {
        key: SubscriptionKey,
        interval: Duration,
        msg: M,
    },
    SystemCpu {
        key: SubscriptionKey,
        interval: Duration,
        map: FrameSharedFn<crate::system::CpuSnapshot, M>,
    },
    SystemMemory {
        key: SubscriptionKey,
        interval: Duration,
        map: FrameSharedFn<crate::system::MemorySnapshot, M>,
    },
    SystemTime {
        key: SubscriptionKey,
        interval: Duration,
        map: FrameSharedFn<crate::system::TimeSnapshot, M>,
    },
    SystemGpu {
        key: SubscriptionKey,
        interval: Duration,
        map: FrameSharedFn<crate::system::GpuSnapshot, M>,
    },
    SystemDisk {
        key: SubscriptionKey,
        interval: Duration,
        map: FrameSharedFn<crate::system::DiskSnapshot, M>,
    },
    SystemNetwork {
        key: SubscriptionKey,
        interval: Duration,
        map: FrameSharedFn<crate::system::NetworkSnapshot, M>,
    },
    SystemBattery {
        key: SubscriptionKey,
        interval: Duration,
        map: FrameSharedFn<crate::system::BatterySnapshot, M>,
    },
    SystemSensors {
        key: SubscriptionKey,
        interval: Duration,
        map: FrameSharedFn<crate::system::ThermalSnapshot, M>,
    },
    SystemUptime {
        key: SubscriptionKey,
        interval: Duration,
        map: FrameSharedFn<crate::system::UptimeSnapshot, M>,
    },
    SystemBrightness {
        key: SubscriptionKey,
        interval: Duration,
        map: FrameSharedFn<crate::system::BrightnessSnapshot, M>,
    },
    SystemPrivacy {
        key: SubscriptionKey,
        interval: Duration,
        map: FrameSharedFn<crate::system::PrivacySnapshot, M>,
    },
    KeyboardLayout {
        map: FrameSharedFn<crate::input::KeyboardLayoutInfo, M>,
    },
    #[cfg(feature = "idle-notify")]
    IdleNotify {
        key: SubscriptionKey,
        timeout: Duration,
        input_only: bool,
        map: FrameSharedFn<crate::idle::IdleEvent, M>,
    },
    #[cfg(feature = "clipboard")]
    Clipboard {
        key: SubscriptionKey,
        selection: Option<crate::clipboard::ClipboardSelection>,
        map: FrameSharedFn<crate::clipboard::ClipboardEvent, M>,
    },
    #[cfg(any(feature = "pipewire", feature = "pulseaudio", feature = "alsa"))]
    AudioSnapshot {
        map: FrameSharedFn<Arc<crate::audio::AudioSnapshot>, M>,
    },
    #[cfg(any(feature = "pipewire", feature = "pulseaudio", feature = "alsa"))]
    AudioEvent {
        map: FrameSharedFn<crate::audio::AudioEvent, M>,
    },
    #[cfg(feature = "hyprland-ipc")]
    HyprlandEvent {
        map: FrameSharedFn<crate::compositor::hyprland::HyprlandEvent, M>,
    },
    #[cfg(feature = "sway-ipc")]
    SwayEvent {
        map: FrameSharedFn<crate::compositor::sway::SwayEvent, M>,
    },
    #[cfg(any(feature = "hyprland-ipc", feature = "sway-ipc"))]
    CompositorEvent {
        map: FrameSharedFn<crate::compositor::CompositorEvent, M>,
    },
    #[cfg(feature = "dbus")]
    DbusSignal {
        key: SubscriptionKey,
        bus: crate::dbus::DbusBus,
        interface: String,
        member: String,
        path: Option<String>,
        map: FrameSharedFn<crate::dbus::DbusSignalEvent, M>,
    },
    #[cfg(feature = "dbus")]
    DbusNotifications {
        capabilities: Option<crate::dbus::NotificationCapabilities>,
        map: FrameSharedFn<crate::dbus::NotificationEvent, M>,
    },
    #[cfg(feature = "dbus")]
    DbusTray {
        map: FrameSharedFn<crate::dbus::TrayEvent, M>,
    },
    #[cfg(feature = "dbus")]
    DbusPowerProfiles {
        map: FrameSharedFn<crate::dbus::PowerProfileSnapshot, M>,
    },
    #[cfg(feature = "dbus")]
    DbusNetworkManager {
        map: FrameSharedFn<crate::dbus::NetworkManagerSnapshot, M>,
    },
    #[cfg(feature = "dbus")]
    DbusUPower {
        map: FrameSharedFn<crate::dbus::UPowerSnapshot, M>,
    },
    #[cfg(feature = "dbus")]
    DbusUDisks {
        map: FrameSharedFn<crate::dbus::UDisksSnapshot, M>,
    },
    #[cfg(feature = "dbus")]
    DbusLogind {
        map: FrameSharedFn<crate::dbus::LogindEvent, M>,
    },
    #[cfg(feature = "dbus")]
    DbusBluetooth {
        map: FrameSharedFn<crate::dbus::BluetoothSnapshot, M>,
    },
    #[cfg(feature = "dbus")]
    DbusBluetoothAgent {
        map: FrameSharedFn<crate::dbus::BluetoothAgentEvent, M>,
    },
    #[cfg(feature = "dbus")]
    DbusModem {
        map: FrameSharedFn<crate::dbus::ModemSnapshot, M>,
    },
    #[cfg(feature = "dbus")]
    DbusScreenSaver {
        map: FrameSharedFn<crate::dbus::ScreenSaverSnapshot, M>,
    },
    #[cfg(feature = "dbus")]
    DbusGeoClue {
        map: FrameSharedFn<crate::dbus::GeoClueSnapshot, M>,
    },
    #[cfg(feature = "dbus")]
    DbusPortalSettings {
        map: FrameSharedFn<crate::dbus::PortalSettingsSnapshot, M>,
    },
    #[cfg(feature = "dbus")]
    DbusMpris {
        map: FrameSharedFn<crate::dbus::MprisEvent, M>,
    },
    #[cfg(feature = "dbus")]
    DbusActionResults {
        map: FrameSharedFn<crate::dbus::DbusActionResult, M>,
    },
    #[cfg(feature = "global-shortcuts")]
    GlobalShortcuts {
        key: SubscriptionKey,
        app_id: String,
        shortcuts: Vec<crate::dbus::GlobalShortcutDescriptor>,
        map: FrameSharedFn<crate::dbus::GlobalShortcutEvent, M>,
    },
    #[cfg(feature = "control-ipc")]
    ControlSignal {
        key: SubscriptionKey,
        signal: crate::control::Signal,
        msg: M,
    },
    #[cfg(feature = "control-ipc")]
    ControlSocket {
        key: SubscriptionKey,
        path: PathBuf,
        map: FrameSharedFn<crate::control::ControlRequest, M>,
    },
}

/// Background stream or timer subscription returned from `subscriptions(state)`.
#[must_use]
#[derive(Clone)]
pub struct Subscription<M> {
    pub(crate) spec: SubscriptionSpec<M>,
}

impl<M: 'static> Subscription<M> {
    /// Empty subscription producing no messages.
    pub fn none() -> Self {
        Self {
            spec: SubscriptionSpec::None,
        }
    }

    /// Combines multiple subscriptions into one value.
    pub fn batch<I>(subs: I) -> Self
    where
        I: IntoIterator<Item = Subscription<M>>,
    {
        let mut items = FrameVec::new();
        for sub in subs {
            items.push(sub);
        }
        Self {
            spec: SubscriptionSpec::Batch(items),
        }
    }

    /// Maps emitted messages into a parent message type `N`.
    pub fn map<N: 'static, F>(self, f: F) -> Subscription<N>
    where
        F: Fn(M) -> N + 'static,
    {
        let mapper = FrameSharedFn::new(f);
        self.map_with(&mapper)
    }

    pub(crate) fn map_with<N: 'static>(self, mapper: &FrameSharedFn<M, N>) -> Subscription<N> {
        fn compose<E: 'static, M: 'static, N: 'static>(
            outer: &FrameSharedFn<M, N>,
            inner: FrameSharedFn<E, M>,
        ) -> FrameSharedFn<E, N> {
            let outer = outer.clone();
            FrameSharedFn::new(move |ev| outer.call(inner.call(ev)))
        }

        macro_rules! map_specs {
            ($($(#[$meta:meta])* $variant:ident { $($field:ident),* }),* $(,)?) => {
                match self.spec {
                    SubscriptionSpec::None => SubscriptionSpec::None,
                    SubscriptionSpec::Batch(items) => {
                        let mut out = FrameVec::with_capacity(items.len());
                        for item in items {
                            out.push(item.map_with(mapper));
                        }
                        SubscriptionSpec::Batch(out)
                    }
                    SubscriptionSpec::TimerOnce { key, duration, msg } => {
                        SubscriptionSpec::TimerOnce {
                            key,
                            duration,
                            msg: mapper.call(msg),
                        }
                    }
                    SubscriptionSpec::TimerEvery { key, interval, msg } => {
                        SubscriptionSpec::TimerEvery {
                            key,
                            interval,
                            msg: mapper.call(msg),
                        }
                    }
                    #[cfg(feature = "control-ipc")]
                    SubscriptionSpec::ControlSignal { key, signal, msg } => {
                        SubscriptionSpec::ControlSignal {
                            key,
                            signal,
                            msg: mapper.call(msg),
                        }
                    }
                    $(
                        $(#[$meta])*
                        SubscriptionSpec::$variant { $($field,)* map } => SubscriptionSpec::$variant {
                            $($field,)*
                            map: compose(mapper, map),
                        },
                    )*
                }
            };
        }

        let spec = map_specs! {
            SystemCpu { key, interval },
            SystemMemory { key, interval },
            SystemTime { key, interval },
            SystemGpu { key, interval },
            SystemDisk { key, interval },
            SystemNetwork { key, interval },
            SystemBattery { key, interval },
            SystemSensors { key, interval },
            SystemUptime { key, interval },
            SystemBrightness { key, interval },
            SystemPrivacy { key, interval },
            KeyboardLayout {},
            #[cfg(any(feature = "pipewire", feature = "pulseaudio", feature = "alsa"))]
            AudioSnapshot {},
            #[cfg(any(feature = "pipewire", feature = "pulseaudio", feature = "alsa"))]
            AudioEvent {},
            #[cfg(feature = "hyprland-ipc")]
            HyprlandEvent {},
            #[cfg(feature = "sway-ipc")]
            SwayEvent {},
            #[cfg(any(feature = "hyprland-ipc", feature = "sway-ipc"))]
            CompositorEvent {},
            #[cfg(feature = "dbus")]
            DbusSignal { key, bus, interface, member, path },
            #[cfg(feature = "dbus")]
            DbusNotifications { capabilities },
            #[cfg(feature = "dbus")]
            DbusTray {},
            #[cfg(feature = "dbus")]
            DbusPowerProfiles {},
            #[cfg(feature = "dbus")]
            DbusNetworkManager {},
            #[cfg(feature = "dbus")]
            DbusUPower {},
            #[cfg(feature = "dbus")]
            DbusUDisks {},
            #[cfg(feature = "dbus")]
            DbusLogind {},
            #[cfg(feature = "dbus")]
            DbusBluetooth {},
            #[cfg(feature = "dbus")]
            DbusBluetoothAgent {},
            #[cfg(feature = "dbus")]
            DbusModem {},
            #[cfg(feature = "dbus")]
            DbusScreenSaver {},
            #[cfg(feature = "dbus")]
            DbusGeoClue {},
            #[cfg(feature = "dbus")]
            DbusPortalSettings {},
            #[cfg(feature = "dbus")]
            DbusMpris {},
            #[cfg(feature = "dbus")]
            DbusActionResults {},
            #[cfg(feature = "idle-notify")]
            IdleNotify { key, timeout, input_only },
            #[cfg(feature = "clipboard")]
            Clipboard { key, selection },
            #[cfg(feature = "global-shortcuts")]
            GlobalShortcuts { key, app_id, shortcuts },
            #[cfg(feature = "control-ipc")]
            ControlSocket { key, path },
        };
        Subscription { spec }
    }
}

/// Collection of active subscriptions returned by `subscriptions(state)`.
#[must_use]
#[derive(Clone)]
pub struct Subscriptions<M> {
    pub(crate) items: FrameVec<Subscription<M>>,
}

impl<M: 'static> Default for Subscriptions<M> {
    fn default() -> Self {
        Self::new()
    }
}

impl<M: 'static> Subscriptions<M> {
    pub fn new() -> Self {
        Self {
            items: FrameVec::new(),
        }
    }

    #[inline]
    pub fn none() -> Self {
        Self::new()
    }

    pub fn push(&mut self, sub: Subscription<M>) {
        self.items.push(sub);
    }

    pub fn extend<I>(&mut self, subs: I)
    where
        I: IntoIterator<Item = Subscription<M>>,
    {
        for s in subs {
            self.items.push(s);
        }
    }

    #[must_use]
    pub fn len(&self) -> usize {
        self.items.len()
    }

    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.items.is_empty()
    }

    /// Maps all contained subscriptions into a parent message type `N`.
    pub fn map<N: 'static, F>(self, f: F) -> Subscriptions<N>
    where
        F: Fn(M) -> N + 'static,
    {
        let mapper = FrameSharedFn::new(f);
        let mut out = FrameVec::with_capacity(self.items.len());
        for sub in self.items {
            out.push(sub.map_with(&mapper));
        }
        Subscriptions { items: out }
    }
}

impl<M: 'static> Deref for Subscriptions<M> {
    type Target = [Subscription<M>];

    fn deref(&self) -> &Self::Target {
        &self.items
    }
}

impl<M: 'static> FromIterator<Subscription<M>> for Subscriptions<M> {
    fn from_iter<T: IntoIterator<Item = Subscription<M>>>(iter: T) -> Self {
        let mut s = Self::new();
        s.extend(iter);
        s
    }
}

impl<M: 'static> IntoIterator for Subscriptions<M> {
    type Item = Subscription<M>;
    type IntoIter = crate::arena::FrameVecIntoIter<Subscription<M>>;

    fn into_iter(self) -> Self::IntoIter {
        self.items.into_iter()
    }
}
