//! Linux `/proc/<pid>/cgroup` reader and systemd unit segment extractor.

use crate::error::{Result, WaywiError};

/// Process cgroup membership parsed from `/proc/<pid>/cgroup`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProcessCgroupInfo {
    /// Target process ID.
    pub pid: u32,
    /// Relative cgroup path (preferring the unified cgroup v2 `0::` hierarchy).
    pub cgroup_path: String,
    /// Innermost systemd `.scope` or `.service` unit segment in [`Self::cgroup_path`], if present.
    pub systemd_unit: Option<String>,
    /// `true` when parsed from the unified cgroup v2 (`0::`) hierarchy.
    pub unified_v2: bool,
}

/// Reads and parses `/proc/<pid>/cgroup` on demand.
pub fn read_process_cgroup(pid: u32) -> Result<ProcessCgroupInfo> {
    if pid == 0 {
        return Err(WaywiError::ProviderUnavailable(
            "invalid process ID 0".to_owned(),
        ));
    }
    let path = format!("/proc/{pid}/cgroup");
    let raw = std::fs::read_to_string(path)?;
    parse_process_cgroup(pid, &raw)
}

/// Parses `/proc/<pid>/cgroup` contents into [`ProcessCgroupInfo`].
///
/// Prefers the unified cgroup v2 line (`0::/...`). On legacy/hybrid systems without a unified v2
/// line, falls back to the `name=systemd` controller entry if present.
pub fn parse_process_cgroup(pid: u32, contents: &str) -> Result<ProcessCgroupInfo> {
    if pid == 0 {
        return Err(WaywiError::ProviderUnavailable(
            "invalid process ID 0".to_owned(),
        ));
    }

    let mut fallback_systemd: Option<&str> = None;

    for line in contents.lines() {
        let trimmed = line.trim();
        if trimmed.is_empty() {
            continue;
        }
        let mut parts = trimmed.splitn(3, ':');
        let Some(hierarchy_id) = parts.next() else {
            continue;
        };
        let Some(controllers) = parts.next() else {
            continue;
        };
        let Some(cgroup_path) = parts.next() else {
            continue;
        };
        if cgroup_path.is_empty() {
            continue;
        }
        if hierarchy_id == "0" && controllers.is_empty() {
            return Ok(ProcessCgroupInfo {
                pid,
                cgroup_path: cgroup_path.to_owned(),
                systemd_unit: extract_systemd_unit(cgroup_path),
                unified_v2: true,
            });
        }
        if fallback_systemd.is_none() && controllers.split(',').any(|ctrl| ctrl == "name=systemd") {
            fallback_systemd = Some(cgroup_path);
        }
    }

    if let Some(cgroup_path) = fallback_systemd {
        return Ok(ProcessCgroupInfo {
            pid,
            cgroup_path: cgroup_path.to_owned(),
            systemd_unit: extract_systemd_unit(cgroup_path),
            unified_v2: false,
        });
    }

    Err(WaywiError::ProviderUnavailable(format!(
        "no unified cgroup v2 or systemd entry found for pid {pid}"
    )))
}

fn extract_systemd_unit(cgroup_path: &str) -> Option<String> {
    cgroup_path
        .rsplit('/')
        .find(|seg| !seg.is_empty() && (seg.ends_with(".scope") || seg.ends_with(".service")))
        .map(ToOwned::to_owned)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_unified_v2_cgroup_and_systemd_scope() {
        let raw =
            "0::/user.slice/user-1000.slice/user@1000.service/app.slice/app-example-42195.scope\n";
        let info = parse_process_cgroup(42195, raw).expect("valid v2 cgroup");
        assert_eq!(info.pid, 42195);
        assert!(info.unified_v2);
        assert_eq!(
            info.cgroup_path,
            "/user.slice/user-1000.slice/user@1000.service/app.slice/app-example-42195.scope"
        );
        assert_eq!(
            info.systemd_unit.as_deref(),
            Some("app-example-42195.scope")
        );
    }

    #[test]
    fn finds_owning_systemd_unit_from_nested_subcgroup_and_prefers_v2() {
        let hybrid = "\
12:memory:/user.slice/user-1000.slice
1:name=systemd:/user.slice/user-1000.slice/legacy.scope
0::/user.slice/user-1000.slice/user@1000.service/app.slice/app-flatpak-org.app-99.scope/sandbox
";
        let info = parse_process_cgroup(99, hybrid).expect("prefers unified v2");
        assert!(info.unified_v2);
        assert_eq!(
            info.systemd_unit.as_deref(),
            Some("app-flatpak-org.app-99.scope")
        );
    }

    #[test]
    fn falls_back_to_legacy_systemd_and_handles_root_and_invalid_inputs() {
        let legacy = "1:name=systemd:/system.slice/gamemoded.service\n";
        let info = parse_process_cgroup(777, legacy).expect("legacy fallback");
        assert!(!info.unified_v2);
        assert_eq!(info.systemd_unit.as_deref(), Some("gamemoded.service"));

        let root = "0::/\n";
        let root_info = parse_process_cgroup(1, root).expect("root cgroup");
        assert!(root_info.unified_v2);
        assert_eq!(root_info.cgroup_path, "/");
        assert_eq!(root_info.systemd_unit, None);

        assert!(parse_process_cgroup(0, root).is_err());
        assert!(parse_process_cgroup(10, "2:cpu,cpuacct:/foo\n").is_err());
    }

    #[test]
    fn reads_current_process_cgroup_on_linux() {
        let pid = std::process::id();
        let info = read_process_cgroup(pid).expect("current process /proc/<pid>/cgroup readable");
        assert_eq!(info.pid, pid);
        assert!(info.cgroup_path.starts_with('/'));
    }
}
