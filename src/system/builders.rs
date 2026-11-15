//! Builders for Linux system telemetry subscriptions and `SnapshotState`.

use std::time::Duration;

use crate::subscription::{IntoSubscriptionKey, Subscription, SubscriptionSpec};

use super::cpu::CpuSnapshot;
use super::disk::DiskSnapshot;
use super::gpu::GpuSnapshot;
use super::hardware::{BatteryInfo, BrightnessSnapshot, ThermalSensor};
use super::memory::MemorySnapshot;
use super::network::NetworkSnapshot;
use super::os::UptimeSnapshot;
use super::privacy::PrivacySnapshot;
use super::time::TimeSnapshot;

/// Represents whether an asynchronous or delta-sampled provider has delivered its initial value.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub enum SnapshotState<T> {
    #[default]
    Pending,
    Ready(T),
}

impl<T> SnapshotState<T> {
    #[inline]
    #[must_use]
    pub const fn is_ready(&self) -> bool {
        matches!(self, Self::Ready(_))
    }

    #[inline]
    #[must_use]
    pub const fn as_ref(&self) -> Option<&T> {
        match self {
            Self::Pending => None,
            Self::Ready(v) => Some(v),
        }
    }

    #[inline]
    #[must_use]
    pub fn into_option(self) -> Option<T> {
        match self {
            Self::Pending => None,
            Self::Ready(v) => Some(v),
        }
    }
}

/// System provider factory (`System::cpu()`, `System::gpu()`, `System::disk()`, etc.).
pub struct System;

impl System {
    #[must_use]
    pub const fn cpu() -> CpuSubBuilder {
        CpuSubBuilder
    }

    #[must_use]
    pub const fn memory() -> MemorySubBuilder {
        MemorySubBuilder
    }

    #[must_use]
    pub const fn time() -> TimeSubBuilder {
        TimeSubBuilder
    }

    #[must_use]
    pub const fn gpu() -> GpuSubBuilder {
        GpuSubBuilder
    }

    #[must_use]
    pub const fn disk() -> DiskSubBuilder {
        DiskSubBuilder
    }

    #[must_use]
    pub const fn network() -> NetworkSubBuilder {
        NetworkSubBuilder
    }

    #[must_use]
    pub const fn battery() -> BatterySubBuilder {
        BatterySubBuilder
    }

    #[must_use]
    pub const fn sensors() -> SensorsSubBuilder {
        SensorsSubBuilder
    }

    #[must_use]
    pub const fn uptime() -> UptimeSubBuilder {
        UptimeSubBuilder
    }

    #[must_use]
    pub const fn brightness() -> BrightnessSubBuilder {
        BrightnessSubBuilder
    }

    #[must_use]
    pub const fn privacy() -> PrivacySubBuilder {
        PrivacySubBuilder
    }
}

macro_rules! define_periodic_builder {
    ($name:ident, $snap:ty, $variant:ident, $default_key:literal, $min_ms:expr, $doc:literal) => {
        pub struct $name;

        impl $name {
            #[doc = $doc]
            pub fn every<M: 'static>(
                self,
                interval: Duration,
                map: impl Fn($snap) -> M + 'static,
            ) -> Subscription<M> {
                self.every_keyed($default_key, interval, map)
            }

            /// Keyed subscription allowing multiple independent handlers to share one sampler.
            pub fn every_keyed<M: 'static>(
                self,
                key: impl IntoSubscriptionKey,
                interval: Duration,
                map: impl Fn($snap) -> M + 'static,
            ) -> Subscription<M> {
                Subscription {
                    spec: SubscriptionSpec::$variant {
                        key: key.into_subscription_key(),
                        interval: interval.max(Duration::from_millis($min_ms)),
                        map: crate::arena::FrameSharedFn::new(map),
                    },
                }
            }
        }
    };
}

define_periodic_builder!(
    CpuSubBuilder,
    CpuSnapshot,
    SystemCpu,
    "system-cpu",
    100,
    "Subscribes to the shared CPU sampler at `interval` (minimum 100ms)."
);

define_periodic_builder!(
    MemorySubBuilder,
    MemorySnapshot,
    SystemMemory,
    "system-memory",
    100,
    "Subscribes to the shared memory sampler at `interval` (minimum 100ms)."
);

define_periodic_builder!(
    TimeSubBuilder,
    TimeSnapshot,
    SystemTime,
    "system-time",
    50,
    "Subscribes to wall-clock and monotonic time ticks at `interval` (minimum 50ms)."
);

define_periodic_builder!(
    GpuSubBuilder,
    GpuSnapshot,
    SystemGpu,
    "system-gpu",
    100,
    "Subscribes to the shared DRM/hwmon GPU sampler at `interval` (minimum 100ms)."
);

define_periodic_builder!(
    DiskSubBuilder,
    DiskSnapshot,
    SystemDisk,
    "system-disk",
    250,
    "Subscribes to disk mounts (`statvfs`) and block I/O rates at `interval` (minimum 250ms)."
);

define_periodic_builder!(
    NetworkSubBuilder,
    NetworkSnapshot,
    SystemNetwork,
    "system-network",
    100,
    "Subscribes to network link status and Rx/Tx bandwidth at `interval` (minimum 100ms)."
);

define_periodic_builder!(
    BatterySubBuilder,
    Vec<BatteryInfo>,
    SystemBattery,
    "system-battery",
    500,
    "Subscribes to `/sys/class/power_supply` batteries at `interval` (minimum 500ms)."
);

define_periodic_builder!(
    SensorsSubBuilder,
    Vec<ThermalSensor>,
    SystemSensors,
    "system-sensors",
    500,
    "Subscribes to `/sys/class/hwmon` thermal sensors at `interval` (minimum 500ms)."
);

define_periodic_builder!(
    UptimeSubBuilder,
    UptimeSnapshot,
    SystemUptime,
    "system-uptime",
    500,
    "Subscribes to `/proc/uptime` and `/proc/loadavg` at `interval` (minimum 500ms)."
);

define_periodic_builder!(
    BrightnessSubBuilder,
    BrightnessSnapshot,
    SystemBrightness,
    "system-brightness",
    250,
    "Subscribes to laptop backlights and DDC/CI external monitor luminance at `interval` (minimum 250ms)."
);

define_periodic_builder!(
    PrivacySubBuilder,
    PrivacySnapshot,
    SystemPrivacy,
    "system-privacy",
    250,
    "Subscribes to active camera, microphone, screen-share, and location access telemetry at `interval` (minimum 250ms)."
);
