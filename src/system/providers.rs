//! Pure hardware/procfs probe container (`SystemProviders`) and typed periodic subscription
//! streams (`SystemSamplers<M>`).

use std::time::{Duration, Instant};

use crate::arena::FrameSharedFn;
use crate::error::Result;
use crate::subscription::{SubscriptionKey, SubscriptionSpec};

use super::cpu::{self, CpuSnapshot};
use super::disk::{self, DiskSnapshot};
use super::gpu::{self, GpuSnapshot};
use super::hardware::{
    self, BatteryInfo, BatterySnapshot, BrightnessSnapshot, DdcWorker, ThermalSensor,
    ThermalSnapshot, UsbDevice,
};
use super::memory::{self, MemorySnapshot};
use super::network::{self, NetworkSnapshot};
use super::os::{self, LoadAverage, OsInfo, UptimeSnapshot};
use super::privacy::{self, PrivacyMonitor, PrivacySnapshot};
use super::time::{self, TimeSnapshot};
use super::units::DataRate;

struct SharedSubscriber<T, M> {
    key: SubscriptionKey,
    interval: Duration,
    last_sent: Option<Instant>,
    map: FrameSharedFn<T, M>,
}

struct PeriodicStream<T, M> {
    subs: Vec<SharedSubscriber<T, M>>,
    last_sample_at: Option<Instant>,
    cached: Option<T>,
}

impl<T, M> Default for PeriodicStream<T, M> {
    fn default() -> Self {
        Self {
            subs: Vec::new(),
            last_sample_at: None,
            cached: None,
        }
    }
}

impl<T: Clone, M> PeriodicStream<T, M> {
    fn push_next(
        &self,
        next: &mut Vec<SharedSubscriber<T, M>>,
        key: SubscriptionKey,
        interval: Duration,
        map: &FrameSharedFn<T, M>,
    ) {
        let prev_sent = self
            .subs
            .iter()
            .find(|s| s.key == key)
            .and_then(|s| s.last_sent);
        next.push(SharedSubscriber {
            key,
            interval,
            last_sent: prev_sent,
            map: map.clone(),
        });
    }

    fn commit(&mut self, next: Vec<SharedSubscriber<T, M>>, on_empty: impl FnOnce()) {
        if next.is_empty() {
            self.last_sample_at = None;
            self.cached = None;
            on_empty();
        }
        self.subs = next;
    }

    fn collect_due(&mut self, now: Instant, sample: impl FnOnce() -> Option<T>, out: &mut Vec<M>) {
        if self.subs.is_empty() {
            return;
        }
        let min_iv = self
            .subs
            .iter()
            .map(|s| s.interval)
            .min()
            .unwrap_or(Duration::from_secs(1));
        let should_sample = self
            .last_sample_at
            .is_none_or(|last| now.saturating_duration_since(last) >= min_iv);
        if should_sample {
            self.last_sample_at = Some(now);
            if let Some(snap) = sample() {
                self.cached = Some(snap);
            }
        }
        if let Some(ref snap) = self.cached {
            for sub in &mut self.subs {
                let due = sub
                    .last_sent
                    .is_none_or(|last| now.saturating_duration_since(last) >= sub.interval);
                if due {
                    sub.last_sent = Some(now);
                    out.push(sub.map.call(snap.clone()));
                }
            }
        }
    }

    fn earliest_deadline(&self, now: Instant, earliest: &mut Option<Instant>) {
        if self.subs.is_empty() {
            return;
        }
        let next_sample_dl = if self.cached.is_none() {
            let min_iv = self
                .subs
                .iter()
                .map(|s| s.interval)
                .min()
                .unwrap_or(Duration::from_secs(1));
            self.last_sample_at.map(|last| last + min_iv)
        } else {
            None
        };
        for s in &self.subs {
            let dl = match (s.last_sent, next_sample_dl) {
                (Some(last), _) => last + s.interval,
                (None, Some(sample_dl)) => sample_dl,
                (None, None) => now,
            };
            *earliest = Some(earliest.map_or(dl, |e| e.min(dl)));
        }
    }

    fn trim(&mut self) {
        if self.subs.is_empty() {
            self.subs.shrink_to_fit();
            self.cached = None;
            self.last_sample_at = None;
        }
    }
}

/// Pure Linux hardware and procfs/sysfs probe registry exposed via [`EffectCtx::system`](crate::effect::EffectCtx::system).
///
/// Holds only delta baselines (`cpu`, `disk`, `network`) and lazy hardware workers (`DdcWorker`,
/// `PrivacyMonitor`); periodic subscription state lives separately in `SystemSamplers`.
pub struct SystemProviders {
    cpu_sampler: cpu::CpuSampler,
    disk_sampler: disk::DiskSampler,
    network_sampler: network::NetworkSampler,
    ddc_worker: DdcWorker,
    ddc_subscribed: bool,
    privacy_monitor: PrivacyMonitor,
}

impl Default for SystemProviders {
    fn default() -> Self {
        Self::new()
    }
}

impl SystemProviders {
    #[must_use]
    pub fn new() -> Self {
        Self {
            cpu_sampler: cpu::CpuSampler::new(),
            disk_sampler: disk::DiskSampler::new(),
            network_sampler: network::NetworkSampler::new(),
            ddc_worker: DdcWorker::new(),
            ddc_subscribed: false,
            privacy_monitor: PrivacyMonitor::new(),
        }
    }

    /// Samples `/proc/stat`, `/proc/loadavg`, and `cpufreq` sysfs once.
    ///
    /// The first call records the counter baseline (`0.0%` utilization); subsequent calls report
    /// utilization deltas since the previous read.
    pub fn sample_cpu(&mut self) -> CpuSnapshot {
        self.cpu_sampler.sample_snapshot()
    }

    /// Reads current Linux RAM, swap, and optional memory PSI metrics from `/proc/meminfo`.
    #[must_use]
    pub fn sample_memory(&self) -> MemorySnapshot {
        memory::read_proc_meminfo().unwrap_or_default()
    }

    /// Enumerates thermal sensors exposed under `/sys/class/hwmon`.
    #[must_use]
    pub fn sample_temperatures(&self) -> ThermalSnapshot {
        hardware::read_sensors().unwrap_or_default()
    }

    /// Enumerates battery and power-supply devices from `/sys/class/power_supply`.
    #[must_use]
    pub fn sample_battery(&self) -> BatterySnapshot {
        hardware::read_batteries().unwrap_or_default()
    }

    /// Samples local disk mounts (`statvfs`) and `/proc/diskstats` block I/O throughput.
    pub fn sample_disk(&mut self) -> DiskSnapshot {
        self.disk_sampler.sample().unwrap_or_else(|_| DiskSnapshot {
            mounts: Vec::new(),
            io: Vec::new(),
            total_read_rate: DataRate::ZERO,
            total_write_rate: DataRate::ZERO,
            timestamp: Instant::now(),
        })
    }

    /// Samples network link status and `/proc/net/dev` Rx/Tx throughput.
    pub fn sample_network(&mut self) -> NetworkSnapshot {
        self.network_sampler
            .sample()
            .unwrap_or_else(|_| NetworkSnapshot {
                interfaces: Vec::new(),
                total_rx_bytes_per_sec: 0.0,
                total_tx_bytes_per_sec: 0.0,
                timestamp: Instant::now(),
            })
    }

    /// Reads internal panel backlights and the latest cached DDC/CI external monitor luminance,
    /// scheduling an async background refresh.
    pub fn sample_brightness(&mut self) -> BrightnessSnapshot {
        let backlights = hardware::read_backlights().unwrap_or_default();
        let external_monitors = self.ddc_worker.snapshot_and_request_refresh();
        BrightnessSnapshot {
            backlights,
            external_monitors,
        }
    }

    /// Reads current Linux DRM/hwmon GPU metrics without spawning subprocesses.
    #[must_use]
    pub fn sample_gpu(&self) -> GpuSnapshot {
        gpu::read_gpus().unwrap_or_else(|_| GpuSnapshot {
            gpus: Vec::new(),
            timestamp: Instant::now(),
        })
    }

    /// Performs a stateless one-shot privacy scan of `/proc/<pid>/fd` without spawning or joining
    /// any PipeWire or D-Bus background threads.
    #[must_use]
    pub fn sample_privacy(&self) -> PrivacySnapshot {
        privacy::sample_privacy_oneshot()
    }

    /// Enumerates attached USB hardware devices from `/sys/bus/usb/devices`.
    #[must_use]
    pub fn sample_usb(&self) -> Vec<UsbDevice> {
        hardware::read_usb_devices().unwrap_or_default()
    }

    /// Sets laptop backlight brightness (`0.0..=100.0` %) directly via sysfs.
    pub fn set_backlight_percent(&self, device_name: &str, percent: f32) -> Result<()> {
        hardware::set_backlight_percent(device_name, percent)
    }

    /// Schedules an asynchronous DDC/CI VCP `0x10` Luminance write (`0.0..=100.0` %) for `i2c_bus`.
    pub fn set_ddc_brightness(&mut self, i2c_bus: u32, percent: f32) {
        self.ddc_worker.set_luminance(i2c_bus, percent);
    }

    /// Reads wall-clock and monotonic timestamps.
    #[must_use]
    pub fn sample_time(&self) -> TimeSnapshot {
        time::now_snapshot()
    }

    /// Reads OS release metadata from `/etc/os-release` and `uname`.
    pub fn os_info(&self) -> Result<OsInfo> {
        os::read_os_info()
    }

    /// Reads current hostname when explicitly requested.
    pub fn hostname(&self) -> Result<String> {
        os::read_hostname()
    }

    /// Reads `/proc/uptime` and `/proc/loadavg`.
    pub fn uptime(&self) -> Result<UptimeSnapshot> {
        os::read_uptime()
    }

    /// Reads `/proc/loadavg`.
    pub fn load_average(&self) -> Result<LoadAverage> {
        os::read_load_average()
    }

    /// Total number of `/proc/stat` reads performed by the shared CPU sampler.
    #[must_use]
    pub fn cpu_sample_count(&self) -> u64 {
        self.cpu_sampler.sample_reads()
    }

    /// Stops background workers (`DdcWorker`, `PrivacyMonitor`) that have no active subscription.
    pub(crate) fn stop_idle_workers(&mut self) {
        if !self.ddc_subscribed {
            self.ddc_worker.stop();
        }
        if !self.privacy_monitor.is_subscribed() {
            self.privacy_monitor.stop_background();
        }
    }

    /// Stops idle workers and clears delta baselines to reclaim heap allocations.
    pub(crate) fn trim_memory(&mut self) {
        self.stop_idle_workers();
        self.cpu_sampler.reset();
        self.disk_sampler.reset();
        self.network_sampler.reset();
    }

    pub(crate) fn set_privacy_subscribed(&mut self, active: bool) {
        self.privacy_monitor.set_subscribed(active);
    }

    pub(crate) fn sample_privacy_subscribed(&mut self) -> PrivacySnapshot {
        self.privacy_monitor.sample()
    }

    pub(crate) fn set_ddc_subscribed(&mut self, active: bool) {
        if self.ddc_subscribed && !active {
            self.ddc_worker.stop();
        }
        self.ddc_subscribed = active;
    }

    pub(crate) fn poll_cpu_stream(&mut self) -> Option<CpuSnapshot> {
        self.cpu_sampler.sample_proc_stat().ok().flatten()
    }

    pub(crate) fn reset_cpu_baseline(&mut self) {
        self.cpu_sampler.reset();
    }

    pub(crate) fn reset_disk_baseline(&mut self) {
        self.disk_sampler.reset();
    }

    pub(crate) fn reset_network_baseline(&mut self) {
        self.network_sampler.reset();
    }
}

/// Typed periodic subscription streams coalescing multiple `System::*` subscriptions onto
/// shared [`SystemProviders`] probes without type erasure (`Box<dyn Any>`).
pub(crate) struct SystemSamplers<M> {
    cpu: PeriodicStream<CpuSnapshot, M>,
    memory: PeriodicStream<MemorySnapshot, M>,
    time: PeriodicStream<TimeSnapshot, M>,
    gpu: PeriodicStream<GpuSnapshot, M>,
    disk: PeriodicStream<DiskSnapshot, M>,
    network: PeriodicStream<NetworkSnapshot, M>,
    battery: PeriodicStream<Vec<BatteryInfo>, M>,
    sensors: PeriodicStream<Vec<ThermalSensor>, M>,
    uptime: PeriodicStream<UptimeSnapshot, M>,
    brightness: PeriodicStream<BrightnessSnapshot, M>,
    privacy: PeriodicStream<PrivacySnapshot, M>,
}

impl<M> Default for SystemSamplers<M> {
    fn default() -> Self {
        Self::new()
    }
}

impl<M> SystemSamplers<M> {
    #[must_use]
    pub(crate) fn new() -> Self {
        Self {
            cpu: PeriodicStream::default(),
            memory: PeriodicStream::default(),
            time: PeriodicStream::default(),
            gpu: PeriodicStream::default(),
            disk: PeriodicStream::default(),
            network: PeriodicStream::default(),
            battery: PeriodicStream::default(),
            sensors: PeriodicStream::default(),
            uptime: PeriodicStream::default(),
            brightness: PeriodicStream::default(),
            privacy: PeriodicStream::default(),
        }
    }

    pub(crate) fn reconcile<'a>(
        &mut self,
        specs: impl Iterator<Item = &'a SubscriptionSpec<M>>,
        providers: &mut SystemProviders,
    ) where
        M: 'a,
    {
        let mut next_cpu = Vec::new();
        let mut next_mem = Vec::new();
        let mut next_time = Vec::new();
        let mut next_gpu = Vec::new();
        let mut next_disk = Vec::new();
        let mut next_net = Vec::new();
        let mut next_bat = Vec::new();
        let mut next_sens = Vec::new();
        let mut next_up = Vec::new();
        let mut next_brightness = Vec::new();
        let mut next_privacy = Vec::new();

        for spec in specs {
            match spec {
                SubscriptionSpec::SystemCpu { key, interval, map } => {
                    self.cpu.push_next(&mut next_cpu, *key, *interval, map);
                }
                SubscriptionSpec::SystemMemory { key, interval, map } => {
                    self.memory.push_next(&mut next_mem, *key, *interval, map);
                }
                SubscriptionSpec::SystemTime { key, interval, map } => {
                    self.time.push_next(&mut next_time, *key, *interval, map);
                }
                SubscriptionSpec::SystemGpu { key, interval, map } => {
                    self.gpu.push_next(&mut next_gpu, *key, *interval, map);
                }
                SubscriptionSpec::SystemDisk { key, interval, map } => {
                    self.disk.push_next(&mut next_disk, *key, *interval, map);
                }
                SubscriptionSpec::SystemNetwork { key, interval, map } => {
                    self.network.push_next(&mut next_net, *key, *interval, map);
                }
                SubscriptionSpec::SystemBattery { key, interval, map } => {
                    self.battery.push_next(&mut next_bat, *key, *interval, map);
                }
                SubscriptionSpec::SystemSensors { key, interval, map } => {
                    self.sensors.push_next(&mut next_sens, *key, *interval, map);
                }
                SubscriptionSpec::SystemUptime { key, interval, map } => {
                    self.uptime.push_next(&mut next_up, *key, *interval, map);
                }
                SubscriptionSpec::SystemBrightness { key, interval, map } => {
                    self.brightness
                        .push_next(&mut next_brightness, *key, *interval, map);
                }
                SubscriptionSpec::SystemPrivacy { key, interval, map } => {
                    self.privacy
                        .push_next(&mut next_privacy, *key, *interval, map);
                }
                _ => {}
            }
        }

        let brightness_active = !next_brightness.is_empty();
        let privacy_active = !next_privacy.is_empty();

        self.cpu.commit(next_cpu, || providers.reset_cpu_baseline());
        self.memory.commit(next_mem, || {});
        self.time.commit(next_time, || {});
        self.gpu.commit(next_gpu, || {});
        self.disk
            .commit(next_disk, || providers.reset_disk_baseline());
        self.network
            .commit(next_net, || providers.reset_network_baseline());
        self.battery.commit(next_bat, || {});
        self.sensors.commit(next_sens, || {});
        self.uptime.commit(next_up, || {});
        self.brightness.commit(next_brightness, || {});
        self.privacy.commit(next_privacy, || {});

        providers.set_ddc_subscribed(brightness_active);
        providers.set_privacy_subscribed(privacy_active);
    }

    pub(crate) fn collect_due(&mut self, now: Instant, providers: &mut SystemProviders) -> Vec<M> {
        let mut out = Vec::new();

        self.cpu
            .collect_due(now, || providers.poll_cpu_stream(), &mut out);
        self.memory
            .collect_due(now, || memory::read_proc_meminfo().ok(), &mut out);
        self.time
            .collect_due(now, || Some(time::now_snapshot()), &mut out);
        self.gpu
            .collect_due(now, || gpu::read_gpus().ok(), &mut out);
        self.disk
            .collect_due(now, || providers.disk_sampler.sample().ok(), &mut out);
        self.network
            .collect_due(now, || providers.network_sampler.sample().ok(), &mut out);
        self.battery
            .collect_due(now, || hardware::read_batteries().ok(), &mut out);
        self.sensors
            .collect_due(now, || hardware::read_sensors().ok(), &mut out);
        self.uptime
            .collect_due(now, || os::read_uptime().ok(), &mut out);
        self.brightness
            .collect_due(now, || Some(providers.sample_brightness()), &mut out);
        self.privacy.collect_due(
            now,
            || Some(providers.sample_privacy_subscribed()),
            &mut out,
        );

        out
    }

    #[must_use]
    pub(crate) fn next_deadline(&self, now: Instant) -> Option<Instant> {
        let mut earliest: Option<Instant> = None;
        self.cpu.earliest_deadline(now, &mut earliest);
        self.memory.earliest_deadline(now, &mut earliest);
        self.time.earliest_deadline(now, &mut earliest);
        self.gpu.earliest_deadline(now, &mut earliest);
        self.disk.earliest_deadline(now, &mut earliest);
        self.network.earliest_deadline(now, &mut earliest);
        self.battery.earliest_deadline(now, &mut earliest);
        self.sensors.earliest_deadline(now, &mut earliest);
        self.uptime.earliest_deadline(now, &mut earliest);
        self.brightness.earliest_deadline(now, &mut earliest);
        self.privacy.earliest_deadline(now, &mut earliest);
        earliest
    }

    pub(crate) fn trim(&mut self) {
        self.cpu.trim();
        self.memory.trim();
        self.time.trim();
        self.gpu.trim();
        self.disk.trim();
        self.network.trim();
        self.battery.trim();
        self.sensors.trim();
        self.uptime.trim();
        self.brightness.trim();
        self.privacy.trim();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::system::System;

    #[test]
    fn shared_periodic_streams_coalesce_clear_cache_and_stop_when_unsubscribed() {
        let mut providers = SystemProviders::new();
        let mut samplers: SystemSamplers<(&'static str, Duration)> = SystemSamplers::new();
        let t0 = Instant::now();

        let subs = [
            System::uptime().every(Duration::from_secs(1), |u| ("a", u.uptime)),
            System::uptime().every_keyed("uptime-2", Duration::from_secs(2), |u| ("b", u.uptime)),
            System::sensors().every(Duration::from_secs(1), |s| {
                ("s", Duration::from_secs(s.len() as u64))
            }),
        ];
        let specs: Vec<_> = subs.iter().map(|s| &s.spec).collect();
        samplers.reconcile(specs.into_iter(), &mut providers);

        assert!(samplers.next_deadline(t0).is_some());
        let batch0 = samplers.collect_due(t0, &mut providers);
        assert_eq!(batch0.len(), 3);
        assert!(samplers.uptime.cached.is_some());

        // Next deadline aligns with earliest active subscription interval (t0 + 1s).
        assert_eq!(
            samplers.next_deadline(t0),
            Some(t0 + Duration::from_secs(1))
        );

        // Unsubscribing clears deadlines and cached snapshots immediately.
        let empty: [SubscriptionSpec<(&'static str, Duration)>; 0] = [];
        samplers.reconcile(empty.iter(), &mut providers);
        assert_eq!(samplers.next_deadline(t0 + Duration::from_secs(5)), None);
        assert!(samplers.uptime.cached.is_none());
        assert!(samplers.sensors.cached.is_none());

        samplers.trim();
        providers.trim_memory();
    }

    #[test]
    fn cpu_stream_waits_for_next_interval_after_baseline_without_busy_loop() {
        let mut providers = SystemProviders::new();
        let mut samplers: SystemSamplers<f32> = SystemSamplers::new();
        let t0 = Instant::now();

        let sub = System::cpu().every(Duration::from_secs(1), |c| c.total_percent);
        samplers.reconcile(std::iter::once(&sub.spec), &mut providers);

        // First tick records baseline only (no message emitted yet), and next deadline advances
        // by the subscription interval rather than staying pinned to `now`.
        let first = samplers.collect_due(t0, &mut providers);
        assert!(first.is_empty());
        assert_eq!(
            samplers.next_deadline(t0),
            Some(t0 + Duration::from_secs(1))
        );

        // Second tick at t0 + 1s emits the utilization delta.
        let second = samplers.collect_due(t0 + Duration::from_secs(1), &mut providers);
        assert_eq!(second.len(), 1);
    }

    #[test]
    fn imperative_system_providers_sample_cleanly_without_spawning_privacy_threads() {
        let mut providers = SystemProviders::new();
        let _cpu0 = providers.sample_cpu();
        let _cpu1 = providers.sample_cpu();
        let mem = providers.sample_memory();
        assert!(mem.used <= mem.total);
        let _temps = providers.sample_temperatures();
        let _bat = providers.sample_battery();
        let _disk = providers.sample_disk();
        let _net = providers.sample_network();
        let _gpu = providers.sample_gpu();
        let _usb = providers.sample_usb();
        let _priv = providers.sample_privacy();
        assert!(!providers.privacy_monitor.is_subscribed());
        providers.stop_idle_workers();
        providers.trim_memory();
    }
}
