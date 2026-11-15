//! OS, kernel, uptime, and load-average telemetry (`/etc/os-release`, `/proc/uptime`, `/proc/loadavg`).

pub mod release;
pub mod uptime;

pub use release::{OsInfo, parse_os_release, read_hostname, read_os_info};
pub use uptime::{
    LoadAverage, UptimeSnapshot, parse_loadavg, parse_uptime, read_load_average, read_uptime,
};
