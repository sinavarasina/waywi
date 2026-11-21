//! Shared `SystemProviders` registry coalescing multiple subscriptions onto single Linux probes.

use std::any::Any;
use std::time::{Duration, Instant};

use crate::arena::FrameSharedFn;
use crate::error::Result;
use crate::subscription::{SubscriptionKey, SubscriptionSpec};

use super::builders::SnapshotState;
use super::cpu::{self, CpuSnapshot};
use super::disk::{self, DiskSnapshot};
use super::gpu::{self, GpuSnapshot};
use super::hardware::{self, BatteryInfo, BrightnessSnapshot, DdcWorker, ThermalSensor, UsbDevice};
use super::memory::{self, MemorySnapshot};
use super::network::{self, NetworkSnapshot};
use super::os::{self, LoadAverage, OsInfo, UptimeSnapshot};
use super::privacy::{PrivacyMonitor, PrivacySnapshot};
use super::time::{self, TimeSnapshot};

#[derive(Clone)]
struct SharedSubscriber<T> {
    key: SubscriptionKey,
    interval: Duration,
    last_sent: Option<Instant>,
    map: FrameSharedFn<T, Box<dyn Any>>,
}

struct PeriodicStream<T> {
    subs: Vec<SharedSubscriber<T>>,
    last_sample_at: Option<Instant>,
    cached: Option<T>,
}

impl<T> Default for PeriodicStream<T> {
    fn default() -> Self {
        Self {
            subs: Vec::new(),
            last_sample_at: None,
            cached: None,
        }
    }
}

impl<T: Clone + 'static> PeriodicStream<T> {
    fn push_next<M: 'static>(
        &self,
        next: &mut Vec<SharedSubscriber<T>>,
        key: SubscriptionKey,
        interval: Duration,
        map: &FrameSharedFn<T, M>,
    ) {
        let prev_sent = self
            .subs
            .iter()
            .find(|s| s.key == key)
            .and_then(|s| s.last_sent);
        let m = map.clone();
        next.push(SharedSubscriber {
            key,
            interval,
            last_sent: prev_sent,
            map: FrameSharedFn::new(move |v| Box::new(m.call(v)) as Box<dyn Any>),
        });
    }

    fn commit(&mut self, next: Vec<SharedSubscriber<T>>, on_empty: impl FnOnce()) {
        if next.is_empty() {
            self.last_sample_at = None;
            on_empty();
        }
        self.subs = next;
    }

    fn collect_due<M: 'static>(
        &mut self,
        now: Instant,
        sample: impl FnOnce() -> Option<T>,
        out: &mut Vec<M>,
    ) {
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
                    if let Ok(msg) = sub.map.call(snap.clone()).downcast::<M>() {
                        out.push(*msg);
                    }
                }
            }
        }
    }

    fn earliest_deadline(&self, now: Instant, earliest: &mut Option<Instant>) {
        for s in &self.subs {
            let dl = s.last_sent.map_or(now, |last| last + s.interval);
            *earliest = Some(earliest.map_or(dl, |e| e.min(dl)));
        }
    }
}

/// Shared `App`-owned registry ensuring multiple subscribers share one underlying sampler
/// per hardware/procfs domain and stop polling immediately when unsubscribed.
pub struct SystemProviders {
    cpu_sampler: cpu::CpuSampler,
    disk_sampler: disk::DiskSampler,
    network_sampler: network::NetworkSampler,
    ddc_worker: DdcWorker,
    privacy_monitor: PrivacyMonitor,
    last_cpu: SnapshotState<CpuSnapshot>,
    cpu: PeriodicStream<CpuSnapshot>,
    memory: PeriodicStream<MemorySnapshot>,
    time: PeriodicStream<TimeSnapshot>,
    gpu: PeriodicStream<GpuSnapshot>,
    disk: PeriodicStream<DiskSnapshot>,
    network: PeriodicStream<NetworkSnapshot>,
    battery: PeriodicStream<Vec<BatteryInfo>>,
    sensors: PeriodicStream<Vec<ThermalSensor>>,
    uptime: PeriodicStream<UptimeSnapshot>,
    brightness: PeriodicStream<BrightnessSnapshot>,
    privacy: PeriodicStream<PrivacySnapshot>,
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
            privacy_monitor: PrivacyMonitor::new(),
            last_cpu: SnapshotState::Pending,
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

    /// Non-blocking read of the latest cached CPU snapshot (`Pending` until two samples exist).
    #[must_use]
    pub fn cpu_snapshot(&self) -> &SnapshotState<CpuSnapshot> {
        &self.last_cpu
    }

    /// Samples `/proc/stat` immediately once (first call sets baseline and returns `Pending`,
    /// subsequent calls compute utilization).
    pub fn poll_cpu_now(&mut self) -> SnapshotState<CpuSnapshot> {
        if let Ok(Some(snap)) = self.cpu_sampler.sample_proc_stat() {
            self.cpu.cached = Some(snap.clone());
            self.last_cpu = SnapshotState::Ready(snap);
        }
        self.last_cpu.clone()
    }

    /// Reads current Linux memory metrics from `/proc/meminfo` (plus optional PSI).
    pub fn memory_snapshot(&mut self) -> Result<MemorySnapshot> {
        let snap = memory::read_proc_meminfo()?;
        self.memory.cached = Some(snap.clone());
        Ok(snap)
    }

    /// Reads wall-clock and monotonic timestamps.
    #[must_use]
    pub fn time_snapshot(&self) -> TimeSnapshot {
        time::now_snapshot()
    }

    /// Reads current Linux DRM/hwmon GPU metrics without spawning subprocesses.
    pub fn gpu_snapshot(&mut self) -> Result<GpuSnapshot> {
        let snap = gpu::read_gpus()?;
        self.gpu.cached = Some(snap.clone());
        Ok(snap)
    }

    /// Samples disk mounts (`statvfs`) and block I/O throughput immediately once.
    pub fn poll_disk_now(&mut self) -> Result<DiskSnapshot> {
        let snap = self.disk_sampler.sample()?;
        self.disk.cached = Some(snap.clone());
        Ok(snap)
    }

    /// Samples network link status and `/proc/net/dev` throughput immediately once.
    pub fn poll_network_now(&mut self) -> Result<NetworkSnapshot> {
        let snap = self.network_sampler.sample()?;
        self.network.cached = Some(snap.clone());
        Ok(snap)
    }

    /// Reads OS release metadata from `/etc/os-release` and `uname`.
    pub fn os_info(&self) -> Result<OsInfo> {
        os::read_os_info()
    }

    /// Reads current hostname when explicitly requested.
    pub fn hostname(&self) -> Result<String> {
        os::read_hostname()
    }

    /// Enumerates battery and power-supply devices from `/sys/class/power_supply`.
    pub fn batteries(&self) -> Result<Vec<BatteryInfo>> {
        hardware::read_batteries()
    }

    /// Enumerates thermal sensors exposed under `/sys/class/hwmon`.
    pub fn sensors(&self) -> Result<Vec<ThermalSensor>> {
        hardware::read_sensors()
    }

    /// Enumerates attached USB hardware devices from `/sys/bus/usb/devices`.
    pub fn usb_devices(&self) -> Result<Vec<UsbDevice>> {
        hardware::read_usb_devices()
    }

    /// Reads `/proc/uptime` and `/proc/loadavg`.
    pub fn uptime(&self) -> Result<UptimeSnapshot> {
        os::read_uptime()
    }

    /// Reads `/proc/loadavg`.
    pub fn load_average(&self) -> Result<LoadAverage> {
        os::read_load_average()
    }

    /// Reads internal panel backlights and the latest cached DDC/CI external monitor luminance,
    /// scheduling an async background refresh.
    pub fn brightness_snapshot(&mut self) -> Result<BrightnessSnapshot> {
        let backlights = hardware::read_backlights()?;
        let external_monitors = self.ddc_worker.snapshot_and_request_refresh();
        let snap = BrightnessSnapshot {
            backlights,
            external_monitors,
        };
        self.brightness.cached = Some(snap.clone());
        Ok(snap)
    }

    /// Sets laptop backlight brightness (`0.0..=100.0` %) via `logind` or sysfs.
    pub fn set_backlight_percent(&self, device_name: &str, percent: f32) -> Result<()> {
        hardware::set_backlight_percent(device_name, percent)
    }

    /// Schedules an asynchronous DDC/CI VCP `0x10` Luminance write (`0.0..=100.0` %) for `i2c_bus`.
    pub fn set_ddc_brightness(&mut self, i2c_bus: u32, percent: f32) {
        self.ddc_worker.set_luminance(i2c_bus, percent);
    }

    /// Collects current camera, microphone, audio capture, screen-share, and location consumers.
    pub fn privacy_snapshot(&mut self) -> PrivacySnapshot {
        let snap = self.privacy_monitor.sample();
        self.privacy.cached = Some(snap.clone());
        snap
    }

    pub(crate) fn reconcile<'a, M: 'static>(
        &mut self,
        specs: impl Iterator<Item = &'a SubscriptionSpec<M>>,
    ) {
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

        let cpu_sampler = &mut self.cpu_sampler;
        self.cpu.commit(next_cpu, || cpu_sampler.reset());
        self.memory.commit(next_mem, || {});
        self.time.commit(next_time, || {});
        self.gpu.commit(next_gpu, || {});
        let disk_sampler = &mut self.disk_sampler;
        self.disk.commit(next_disk, || disk_sampler.reset());
        let network_sampler = &mut self.network_sampler;
        self.network.commit(next_net, || network_sampler.reset());
        self.battery.commit(next_bat, || {});
        self.sensors.commit(next_sens, || {});
        self.uptime.commit(next_up, || {});
        self.brightness.commit(next_brightness, || {});
        let privacy_monitor = &mut self.privacy_monitor;
        self.privacy
            .commit(next_privacy, || privacy_monitor.stop_background());
    }

    pub(crate) fn collect_due<M: 'static>(&mut self, now: Instant) -> Vec<M> {
        let mut out = Vec::new();

        let cpu_sampler = &mut self.cpu_sampler;
        let last_cpu = &mut self.last_cpu;
        self.cpu.collect_due(
            now,
            || {
                let snap = cpu_sampler.sample_proc_stat().ok().flatten()?;
                *last_cpu = SnapshotState::Ready(snap.clone());
                Some(snap)
            },
            &mut out,
        );
        self.memory
            .collect_due(now, || memory::read_proc_meminfo().ok(), &mut out);
        self.time
            .collect_due(now, || Some(time::now_snapshot()), &mut out);
        self.gpu
            .collect_due(now, || gpu::read_gpus().ok(), &mut out);
        let disk_sampler = &mut self.disk_sampler;
        self.disk
            .collect_due(now, || disk_sampler.sample().ok(), &mut out);
        let network_sampler = &mut self.network_sampler;
        self.network
            .collect_due(now, || network_sampler.sample().ok(), &mut out);
        self.battery
            .collect_due(now, || hardware::read_batteries().ok(), &mut out);
        self.sensors
            .collect_due(now, || hardware::read_sensors().ok(), &mut out);
        self.uptime
            .collect_due(now, || os::read_uptime().ok(), &mut out);
        let ddc_worker = &mut self.ddc_worker;
        self.brightness.collect_due(
            now,
            || {
                let backlights = hardware::read_backlights().ok()?;
                let external_monitors = ddc_worker.snapshot_and_request_refresh();
                Some(BrightnessSnapshot {
                    backlights,
                    external_monitors,
                })
            },
            &mut out,
        );
        let privacy_monitor = &mut self.privacy_monitor;
        self.privacy
            .collect_due(now, || Some(privacy_monitor.sample()), &mut out);

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

    #[must_use]
    pub fn cpu_subscriber_count(&self) -> usize {
        self.cpu.subs.len()
    }

    #[must_use]
    pub fn cpu_sample_count(&self) -> u64 {
        self.cpu_sampler.sample_reads()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::system::System;

    #[test]
    fn shared_periodic_streams_coalesce_and_stop_when_unsubscribed() {
        let mut sys = SystemProviders::new();
        let t0 = Instant::now();

        let subs = [
            System::uptime().every(Duration::from_secs(1), |u| ("a", u.uptime)),
            System::uptime().every_keyed("uptime-2", Duration::from_secs(2), |u| ("b", u.uptime)),
            System::sensors().every(Duration::from_secs(1), |s| {
                ("s", Duration::from_secs(s.len() as u64))
            }),
        ];
        let specs: Vec<_> = subs.iter().map(|s| &s.spec).collect();
        sys.reconcile(specs.into_iter());

        assert!(sys.next_deadline(t0).is_some());
        let batch0: Vec<(&'static str, Duration)> = sys.collect_due(t0);
        assert_eq!(batch0.len(), 3);

        // Next deadline aligns with earliest active subscription interval (t0 + 1s).
        assert_eq!(sys.next_deadline(t0), Some(t0 + Duration::from_secs(1)));

        // Unsubscribing clears active deadlines immediately.
        let empty: [SubscriptionSpec<(&'static str, Duration)>; 0] = [];
        sys.reconcile(empty.iter());
        assert_eq!(sys.next_deadline(t0 + Duration::from_secs(5)), None);
    }
}
