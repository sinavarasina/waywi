//! `/proc/mounts` parser and `statvfs` capacity reader.

use std::collections::HashSet;
use std::path::PathBuf;

use crate::error::Result;
use crate::system::units::ByteSize;

use super::types::DiskMount;

/// Parsed mount record from `/proc/mounts`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProcMountEntry {
    pub device: String,
    pub mount_point: PathBuf,
    pub fs_type: String,
}

/// Parses `/proc/mounts` into local storage mount candidates, filtering virtual/pseudo/network
/// filesystems so polling never blocks on unreachable remote mounts.
#[must_use]
pub fn parse_proc_mounts(contents: &str) -> Vec<ProcMountEntry> {
    let mut seen_mounts = HashSet::new();
    let mut out = Vec::new();
    for line in contents.lines() {
        let mut parts = line.split_whitespace();
        let Some(device) = parts.next() else {
            continue;
        };
        let Some(raw_mount) = parts.next() else {
            continue;
        };
        let Some(fs_type) = parts.next() else {
            continue;
        };
        if !is_local_storage_fs(device, fs_type) {
            continue;
        }
        let mount_point = PathBuf::from(unescape_mount_field(raw_mount));
        if seen_mounts.insert(mount_point.clone()) {
            out.push(ProcMountEntry {
                device: unescape_mount_field(device),
                mount_point,
                fs_type: fs_type.to_owned(),
            });
        }
    }
    out
}

/// Reads local mount capacities using `/proc/mounts` and `rustix::fs::statvfs`.
pub fn read_disk_mounts() -> Result<Vec<DiskMount>> {
    let raw = std::fs::read_to_string("/proc/mounts")?;
    let mut mounts = Vec::new();
    for entry in parse_proc_mounts(&raw) {
        let Ok(st) = rustix::fs::statvfs(&entry.mount_point) else {
            continue;
        };
        let block_size = st.f_frsize.max(1);
        let total_bytes = st.f_blocks.saturating_mul(block_size);
        if total_bytes == 0 {
            continue;
        }
        let free_bytes = st.f_bfree.saturating_mul(block_size);
        let avail_bytes = st.f_bavail.saturating_mul(block_size);
        let total = ByteSize::from_bytes(total_bytes);
        let free = ByteSize::from_bytes(free_bytes);
        let available = ByteSize::from_bytes(avail_bytes);
        let used = total.saturating_sub(free);

        mounts.push(DiskMount {
            device: entry.device,
            mount_point: entry.mount_point,
            fs_type: entry.fs_type,
            total,
            available,
            free,
            used,
        });
    }
    Ok(mounts)
}

fn is_local_storage_fs(device: &str, fs_type: &str) -> bool {
    if !device.starts_with('/') {
        return false;
    }
    !matches!(
        fs_type,
        "proc"
            | "sysfs"
            | "devtmpfs"
            | "devpts"
            | "tmpfs"
            | "cgroup"
            | "cgroup2"
            | "pstore"
            | "bpf"
            | "tracefs"
            | "debugfs"
            | "securityfs"
            | "configfs"
            | "fusectl"
            | "hugetlbfs"
            | "mqueue"
            | "ramfs"
            | "autofs"
            | "rpc_pipefs"
            | "efivarfs"
            | "nsfs"
            | "overlay"
            | "squashfs"
            | "nfs"
            | "nfs4"
            | "cifs"
            | "smb3"
            | "smbfs"
            | "fuse.sshfs"
    )
}

fn unescape_mount_field(input: &str) -> String {
    let mut out = String::with_capacity(input.len());
    let bytes = input.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'\\' && i + 3 < bytes.len() {
            let escaped = match &bytes[i + 1..i + 4] {
                b"040" => Some(' '),
                b"011" => Some('\t'),
                b"012" => Some('\n'),
                b"134" => Some('\\'),
                _ => None,
            };
            if let Some(ch) = escaped {
                out.push(ch);
                i += 4;
                continue;
            }
        }
        out.push(bytes[i] as char);
        i += 1;
    }
    out
}
