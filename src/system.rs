//! Linux system telemetry domain: CPU, Memory, Time, OS/Host, Physical Hardware, GPU, Disk,
//! Network, Brightness, and Privacy/Hardware-Access Indicators.

pub mod builders;
pub mod cpu;
pub mod disk;
pub mod gpu;
pub mod hardware;
pub mod memory;
pub mod network;
pub mod os;
pub mod privacy;
pub mod providers;
pub(crate) mod sysfs;
pub mod time;
pub mod units;

pub use builders::{
    BatterySubBuilder, BrightnessSubBuilder, CpuSubBuilder, DiskSubBuilder, GpuSubBuilder,
    MemorySubBuilder, NetworkSubBuilder, PrivacySubBuilder, SensorsSubBuilder, SnapshotState,
    System, TimeSubBuilder, UptimeSubBuilder,
};
pub use cpu::{CpuCounters, CpuSnapshot};
pub use disk::{DiskDeviceIo, DiskMount, DiskSampler, DiskSnapshot};
pub use gpu::{GpuDeviceSnapshot, GpuSnapshot, GpuVendor};
pub use hardware::{
    BacklightDevice, BatteryInfo, BatterySnapshot, BrightnessSnapshot, DdcMonitorInfo, PowerStatus,
    ThermalSensor, ThermalSnapshot, UsbDevice, read_usb_devices, read_usb_devices_from,
    set_backlight_percent, set_backlight_raw,
};
pub use memory::{MemoryPsi, MemorySnapshot};
pub use network::{
    NetInterfaceKind, NetInterfaceSnapshot, NetLinkState, NetworkSampler, NetworkSnapshot,
};
pub use os::{
    LoadAverage, OsInfo, ProcessCgroupInfo, UptimeSnapshot, parse_process_cgroup,
    read_process_cgroup,
};
pub use privacy::{
    PrivacyAccessEntry, PrivacyAccessKind, PrivacyAccessSource, PrivacyMonitor, PrivacySnapshot,
    sample_privacy_oneshot,
};
pub use providers::SystemProviders;
pub(crate) use providers::SystemSamplers;
pub use time::{
    CalendarDayCell, CalendarMonth, CalendarRow, LocalDateTime, TimeFormat, TimeSnapshot,
    days_in_month, iso_week_number, month_name, shift_year_month,
};
pub use units::{ByteSize, DataRate, FreqUnit, RateUnit};
