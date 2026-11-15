//! Internal lightweight sysfs scalar readers shared across hardware, GPU, and network probes.

use std::path::Path;

pub(crate) fn read_sysfs_string(path: &Path) -> Option<String> {
    let raw = std::fs::read_to_string(path).ok()?;
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        None
    } else {
        Some(trimmed.to_owned())
    }
}

pub(crate) fn read_sysfs_parse<T: std::str::FromStr>(path: &Path) -> Option<T> {
    std::fs::read_to_string(path).ok()?.trim().parse::<T>().ok()
}

pub(crate) fn read_sysfs_u64(path: &Path) -> Option<u64> {
    read_sysfs_parse(path)
}

pub(crate) fn read_sysfs_u32(path: &Path) -> Option<u32> {
    read_sysfs_parse(path)
}

pub(crate) fn read_sysfs_i64(path: &Path) -> Option<i64> {
    read_sysfs_parse(path)
}

pub(crate) fn parse_hex_u16(raw: &str) -> Option<u16> {
    let clean = raw
        .trim()
        .strip_prefix("0x")
        .or_else(|| raw.trim().strip_prefix("0X"))
        .unwrap_or_else(|| raw.trim());
    u16::from_str_radix(clean, 16).ok()
}
