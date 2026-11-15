use crate::error::Result;

pub use super::units::ByteSize;

/// Optional Linux Pressure Stall Information (`/proc/pressure/memory`).
#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub struct MemoryPsi {
    pub some_avg10: f32,
    pub some_avg60: f32,
    pub some_avg300: f32,
    pub full_avg10: f32,
    pub full_avg60: f32,
    pub full_avg300: f32,
}

/// System RAM and swap snapshot.
///
/// Used memory is defined consistently with Linux `free`: `used = total.saturating_sub(available)`.
#[derive(Debug, Clone, PartialEq)]
pub struct MemorySnapshot {
    pub total: ByteSize,
    pub available: ByteSize,
    /// `total.saturating_sub(available)`.
    pub used: ByteSize,
    pub swap_total: ByteSize,
    pub swap_used: ByteSize,
    pub swap_available: ByteSize,
    pub psi: Option<MemoryPsi>,
}

/// Reads `/proc/meminfo` and optional `/proc/pressure/memory`.
pub fn read_proc_meminfo() -> Result<MemorySnapshot> {
    let meminfo = std::fs::read_to_string("/proc/meminfo")?;
    let psi = std::fs::read_to_string("/proc/pressure/memory")
        .ok()
        .and_then(|s| parse_memory_psi(&s));
    parse_meminfo(&meminfo, psi)
}

/// Pure `/proc/meminfo` parser.
pub fn parse_meminfo(contents: &str, psi: Option<MemoryPsi>) -> Result<MemorySnapshot> {
    let mut total_kib = None;
    let mut available_kib = None;
    let mut free_kib = 0u64;
    let mut buffers_kib = 0u64;
    let mut cached_kib = 0u64;
    let mut swap_total_kib = 0u64;
    let mut swap_free_kib = 0u64;

    for line in contents.lines() {
        let Some((key, rest)) = line.split_once(':') else {
            continue;
        };
        let value = rest
            .split_whitespace()
            .next()
            .and_then(|v| v.parse::<u64>().ok())
            .unwrap_or(0);
        match key {
            "MemTotal" => total_kib = Some(value),
            "MemAvailable" => available_kib = Some(value),
            "MemFree" => free_kib = value,
            "Buffers" => buffers_kib = value,
            "Cached" => cached_kib = value,
            "SwapTotal" => swap_total_kib = value,
            "SwapFree" => swap_free_kib = value,
            _ => {}
        }
    }

    let total = ByteSize::from_kib(total_kib.ok_or_else(|| {
        crate::error::WaywiError::ProviderUnavailable(
            "MemTotal missing from /proc/meminfo".to_owned(),
        )
    })?);
    let avail_kib = available_kib.unwrap_or(free_kib + buffers_kib + cached_kib);
    let available = ByteSize::from_kib(avail_kib).min(total);
    let used = total.saturating_sub(available);

    let swap_total = ByteSize::from_kib(swap_total_kib);
    let swap_available = ByteSize::from_kib(swap_free_kib).min(swap_total);
    let swap_used = swap_total.saturating_sub(swap_available);

    Ok(MemorySnapshot {
        total,
        available,
        used,
        swap_total,
        swap_used,
        swap_available,
        psi,
    })
}

/// Parses `/proc/pressure/memory` (`some avg10=...` and `full avg10=...`).
#[must_use]
pub fn parse_memory_psi(contents: &str) -> Option<MemoryPsi> {
    let mut psi = MemoryPsi::default();
    let mut parsed_any = false;
    for line in contents.lines() {
        let mut parts = line.split_whitespace();
        let Some(kind) = parts.next() else {
            continue;
        };
        let parse_val =
            |token: &str, prefix: &str| -> Option<f32> { token.strip_prefix(prefix)?.parse().ok() };
        match kind {
            "some" => {
                for tok in parts {
                    if let Some(v) = parse_val(tok, "avg10=") {
                        psi.some_avg10 = v;
                        parsed_any = true;
                    } else if let Some(v) = parse_val(tok, "avg60=") {
                        psi.some_avg60 = v;
                    } else if let Some(v) = parse_val(tok, "avg300=") {
                        psi.some_avg300 = v;
                    }
                }
            }
            "full" => {
                for tok in parts {
                    if let Some(v) = parse_val(tok, "avg10=") {
                        psi.full_avg10 = v;
                        parsed_any = true;
                    } else if let Some(v) = parse_val(tok, "avg60=") {
                        psi.full_avg60 = v;
                    } else if let Some(v) = parse_val(tok, "avg300=") {
                        psi.full_avg300 = v;
                    }
                }
            }
            _ => {}
        }
    }
    parsed_any.then_some(psi)
}
