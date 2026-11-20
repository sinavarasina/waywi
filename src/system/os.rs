//! OS, kernel, uptime, load-average, and process cgroup telemetry
//! (`/etc/os-release`, `/proc/uptime`, `/proc/loadavg`, `/proc/<pid>/cgroup`).

pub mod cgroup;
pub mod release;
pub mod uptime;

pub use cgroup::{ProcessCgroupInfo, parse_process_cgroup, read_process_cgroup};
pub use release::{OsInfo, parse_os_release, read_hostname, read_os_info};
pub use uptime::{
    LoadAverage, UptimeSnapshot, parse_loadavg, parse_uptime, read_load_average, read_uptime,
};
