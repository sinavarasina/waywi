//! Linux OS release and hostname readers.

use crate::error::Result;

/// Read-only Linux OS and kernel metadata.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OsInfo {
    pub name: Option<String>,
    pub pretty_name: Option<String>,
    pub id: Option<String>,
    pub version_id: Option<String>,
    pub kernel_release: Option<String>,
    pub arch: &'static str,
}

/// Reads `/etc/os-release` and `/proc/sys/kernel/osrelease`.
pub fn read_os_info() -> Result<OsInfo> {
    let os_release = std::fs::read_to_string("/etc/os-release").unwrap_or_default();
    let kernel_release = std::fs::read_to_string("/proc/sys/kernel/osrelease")
        .ok()
        .map(|s| s.trim().to_owned());

    let mut info = parse_os_release(&os_release);
    info.kernel_release = kernel_release;
    Ok(info)
}

#[must_use]
pub fn parse_os_release(contents: &str) -> OsInfo {
    let mut name = None;
    let mut pretty_name = None;
    let mut id = None;
    let mut version_id = None;

    for line in contents.lines() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let Some((k, v)) = line.split_once('=') else {
            continue;
        };
        let clean = v.trim().trim_matches('"').to_owned();
        match k {
            "NAME" => name = Some(clean),
            "PRETTY_NAME" => pretty_name = Some(clean),
            "ID" => id = Some(clean),
            "VERSION_ID" => version_id = Some(clean),
            _ => {}
        }
    }

    OsInfo {
        name,
        pretty_name,
        id,
        version_id,
        kernel_release: None,
        arch: std::env::consts::ARCH,
    }
}

/// Reads system hostname from `/proc/sys/kernel/hostname`.
pub fn read_hostname() -> Result<String> {
    let s = std::fs::read_to_string("/proc/sys/kernel/hostname")?;
    Ok(s.trim().to_owned())
}
