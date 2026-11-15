//! Linux `/proc/uptime` and `/proc/loadavg` readers.

use std::time::Duration;

use crate::error::{Result, WaywiError};

/// System load averages and runnable task counters from `/proc/loadavg`.
#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub struct LoadAverage {
    pub one: f64,
    pub five: f64,
    pub fifteen: f64,
    pub running_tasks: u32,
    pub total_tasks: u32,
}

/// System uptime and optional load average snapshot.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct UptimeSnapshot {
    pub uptime: Duration,
    pub idle_total: Duration,
    pub load: Option<LoadAverage>,
}

/// Reads `/proc/uptime` and optional `/proc/loadavg`.
pub fn read_uptime() -> Result<UptimeSnapshot> {
    let raw_uptime = std::fs::read_to_string("/proc/uptime")?;
    let (uptime, idle_total) = parse_uptime(&raw_uptime)?;
    let load = std::fs::read_to_string("/proc/loadavg")
        .ok()
        .and_then(|s| parse_loadavg(&s).ok());
    Ok(UptimeSnapshot {
        uptime,
        idle_total,
        load,
    })
}

/// Reads `/proc/loadavg`.
pub fn read_load_average() -> Result<LoadAverage> {
    let raw = std::fs::read_to_string("/proc/loadavg")?;
    parse_loadavg(&raw)
}

/// Parses two-column `/proc/uptime` text into `(uptime, idle_total)`.
pub fn parse_uptime(contents: &str) -> Result<(Duration, Duration)> {
    let mut parts = contents.split_whitespace();
    let up_secs: f64 = parts
        .next()
        .and_then(|s| s.parse().ok())
        .filter(|v: &f64| v.is_finite() && *v >= 0.0)
        .ok_or_else(|| {
            WaywiError::ProviderUnavailable("invalid /proc/uptime duration".to_owned())
        })?;
    let idle_secs: f64 = parts
        .next()
        .and_then(|s| s.parse().ok())
        .filter(|v: &f64| v.is_finite() && *v >= 0.0)
        .unwrap_or(0.0);
    Ok((
        Duration::from_secs_f64(up_secs),
        Duration::from_secs_f64(idle_secs),
    ))
}

/// Parses five-column `/proc/loadavg` text.
pub fn parse_loadavg(contents: &str) -> Result<LoadAverage> {
    let mut parts = contents.split_whitespace();
    let one: f64 = parts.next().and_then(|s| s.parse().ok()).ok_or_else(|| {
        WaywiError::ProviderUnavailable("invalid /proc/loadavg 1m load".to_owned())
    })?;
    let five: f64 = parts.next().and_then(|s| s.parse().ok()).ok_or_else(|| {
        WaywiError::ProviderUnavailable("invalid /proc/loadavg 5m load".to_owned())
    })?;
    let fifteen: f64 = parts.next().and_then(|s| s.parse().ok()).ok_or_else(|| {
        WaywiError::ProviderUnavailable("invalid /proc/loadavg 15m load".to_owned())
    })?;
    let (running_tasks, total_tasks) = parts
        .next()
        .and_then(|s| s.split_once('/'))
        .and_then(|(r, t)| Some((r.parse::<u32>().ok()?, t.parse::<u32>().ok()?)))
        .ok_or_else(|| {
            WaywiError::ProviderUnavailable("invalid /proc/loadavg task counts".to_owned())
        })?;

    Ok(LoadAverage {
        one,
        five,
        fifteen,
        running_tasks,
        total_tasks,
    })
}
