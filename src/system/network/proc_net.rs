//! `/proc/net/dev` delta bandwidth sampler + `/sys/class/net/*` link status inspection.

use std::collections::HashMap;
use std::path::Path;
use std::time::Instant;

use crate::error::Result;
use crate::system::sysfs::{read_sysfs_string, read_sysfs_u32};
use crate::system::units::ByteSize;

use super::types::{NetInterfaceKind, NetInterfaceSnapshot, NetLinkState, NetworkSnapshot};

/// Cumulative byte and packet counters parsed from `/proc/net/dev`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NetDevCounters {
    pub name: String,
    pub rx_bytes: u64,
    pub rx_packets: u64,
    pub tx_bytes: u64,
    pub tx_packets: u64,
}

/// Parses `/proc/net/dev` rows into interface byte and packet counters.
#[must_use]
pub fn parse_proc_net_dev(contents: &str) -> Vec<NetDevCounters> {
    let mut out = Vec::new();
    for line in contents.lines().skip(2) {
        let Some((iface, rest)) = line.split_once(':') else {
            continue;
        };
        let cols: Vec<&str> = rest.split_whitespace().collect();
        if cols.len() < 10 {
            continue;
        }
        let Ok(rx_bytes) = cols[0].parse::<u64>() else {
            continue;
        };
        let Ok(rx_packets) = cols[1].parse::<u64>() else {
            continue;
        };
        let Ok(tx_bytes) = cols[8].parse::<u64>() else {
            continue;
        };
        let Ok(tx_packets) = cols[9].parse::<u64>() else {
            continue;
        };
        out.push(NetDevCounters {
            name: iface.trim().to_owned(),
            rx_bytes,
            rx_packets,
            tx_bytes,
            tx_packets,
        });
    }
    out
}

fn read_uevent_devtype(iface_dir: &Path) -> Option<String> {
    let raw = std::fs::read_to_string(iface_dir.join("uevent")).ok()?;
    for line in raw.lines() {
        if let Some(val) = line.strip_prefix("DEVTYPE=") {
            let trimmed = val.trim();
            if !trimmed.is_empty() {
                return Some(trimmed.to_owned());
            }
        }
    }
    None
}

/// Classifies an interface using authoritative Linux `sysfs` facts (`type`, `wireless/`,
/// `uevent` `DEVTYPE`, `bridge/`, `tun_flags`, backing `device/`) first, falling back to Linux
/// naming conventions only when `sysfs` is unavailable.
#[must_use]
pub fn classify_interface(name: &str, iface_sys_dir: Option<&Path>) -> NetInterfaceKind {
    if name == "lo" {
        return NetInterfaceKind::Loopback;
    }
    if let Some(dir) = iface_sys_dir {
        let arp_type = read_sysfs_u32(&dir.join("type"));
        if arp_type == Some(772) {
            return NetInterfaceKind::Loopback;
        }
        if dir.join("wireless").is_dir() {
            return NetInterfaceKind::Wifi;
        }
        if let Some(devtype) = read_uevent_devtype(dir) {
            match devtype.as_str() {
                "wlan" => return NetInterfaceKind::Wifi,
                "wwan" => return NetInterfaceKind::Modem,
                "bridge" | "vlan" | "bond" | "veth" | "tun" | "tap" | "wireguard" | "vxlan"
                | "macvlan" | "ipvlan" | "dummy" | "ifb" | "geneve" | "gre" => {
                    return NetInterfaceKind::Virtual;
                }
                _ => {}
            }
        }
        if dir.join("bridge").is_dir() || dir.join("tun_flags").is_file() {
            return NetInterfaceKind::Virtual;
        }
        if arp_type == Some(512) {
            // ARPHRD_PPP
            return NetInterfaceKind::Modem;
        }
        if arp_type == Some(65534) {
            // ARPHRD_NONE (WireGuard, TUN, Tailscale, etc.)
            return NetInterfaceKind::Virtual;
        }
        if arp_type == Some(1) {
            // ARPHRD_ETHER: distinguish physical NICs (which expose a hardware `device` symlink)
            // from software/virtual Ethernet devices (bridges, veth, docker, custom-named taps).
            return if dir.join("device").exists() {
                NetInterfaceKind::Ethernet
            } else {
                NetInterfaceKind::Virtual
            };
        }
    }

    // Fallback naming conventions when sysfs metadata is absent.
    if name.starts_with("wl") || name.starts_with("wlan") {
        return NetInterfaceKind::Wifi;
    }
    if name.starts_with("ww")
        || name.starts_with("rmnet")
        || name.starts_with("cdc-wdm")
        || name.starts_with("ppp")
    {
        return NetInterfaceKind::Modem;
    }
    if name.starts_with("veth")
        || name.starts_with("docker")
        || name.starts_with("br-")
        || name.starts_with("virbr")
        || name.starts_with("tun")
        || name.starts_with("tap")
        || name.starts_with("wg")
        || name.starts_with("tailscale")
        || name.starts_with("podman")
        || name.starts_with("cni")
    {
        return NetInterfaceKind::Virtual;
    }
    if name.starts_with("en") || name.starts_with("eth") {
        return NetInterfaceKind::Ethernet;
    }
    NetInterfaceKind::Other
}

/// Stateful sampler computing per-interface Rx/Tx rates from `/proc/net/dev`.
#[derive(Debug, Default)]
pub struct NetworkSampler {
    prev: HashMap<String, NetDevCounters>,
    prev_time: Option<Instant>,
}

impl NetworkSampler {
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    pub fn reset(&mut self) {
        self.prev.clear();
        self.prev.shrink_to_fit();
        self.prev_time = None;
    }

    /// Reads live `/proc/net/dev`, `/sys/class/net/*`, `/proc/net/route`, `/proc/net/wireless`,
    /// and interface IP addresses.
    pub fn sample(&mut self) -> Result<NetworkSnapshot> {
        let raw = std::fs::read_to_string("/proc/net/dev")?;
        let mut snap = self.feed_proc_net_dev(&raw, Path::new("/sys/class/net"), Instant::now());
        enrich_live_interfaces(&mut snap.interfaces);
        Ok(snap)
    }

    /// Pure deterministic step computing bandwidth deltas from provided `/proc/net/dev` text.
    pub fn feed_proc_net_dev(
        &mut self,
        contents: &str,
        sys_class_net: &Path,
        now: Instant,
    ) -> NetworkSnapshot {
        let counters = parse_proc_net_dev(contents);
        let dt_secs = self
            .prev_time
            .map(|t| now.saturating_duration_since(t).as_secs_f64())
            .filter(|dt| *dt > 0.0);

        let mut interfaces = Vec::with_capacity(counters.len());
        let mut total_rx_bytes_per_sec = 0.0;
        let mut total_tx_bytes_per_sec = 0.0;
        let mut next_prev = HashMap::with_capacity(counters.len());

        for cur in counters {
            let sys_dir = sys_class_net.join(&cur.name);
            let sys_opt = sys_dir.is_dir().then_some(sys_dir.as_path());
            let kind = classify_interface(&cur.name, sys_opt);

            let link_state = sys_opt
                .and_then(|d| read_sysfs_string(&d.join("operstate")))
                .map_or(NetLinkState::Unknown, |s| match s.as_str() {
                    "up" => NetLinkState::Up,
                    "down" => NetLinkState::Down,
                    _ => NetLinkState::Unknown,
                });
            let carrier = sys_opt
                .and_then(|d| read_sysfs_u32(&d.join("carrier")))
                .map(|v| v != 0);
            let link_speed_mbps = sys_opt
                .and_then(|d| read_sysfs_string(&d.join("speed")))
                .and_then(|s| s.parse::<i32>().ok())
                .filter(|&mbps| mbps > 0)
                .map(|mbps| mbps as u32);

            let (rx_bps, tx_bps, rx_pps, tx_pps) = match (dt_secs, self.prev.get(&cur.name)) {
                (Some(dt), Some(prev)) => (
                    cur.rx_bytes.saturating_sub(prev.rx_bytes) as f64 / dt,
                    cur.tx_bytes.saturating_sub(prev.tx_bytes) as f64 / dt,
                    cur.rx_packets.saturating_sub(prev.rx_packets) as f64 / dt,
                    cur.tx_packets.saturating_sub(prev.tx_packets) as f64 / dt,
                ),
                _ => (0.0, 0.0, 0.0, 0.0),
            };

            if kind != NetInterfaceKind::Loopback {
                total_rx_bytes_per_sec += rx_bps;
                total_tx_bytes_per_sec += tx_bps;
            }

            interfaces.push(NetInterfaceSnapshot {
                name: cur.name.clone(),
                kind,
                link_state,
                carrier,
                link_speed_mbps,
                ipv4: None,
                ipv6: None,
                gateway: None,
                wireless_signal_dbm: None,
                wireless_freq_mhz: None,
                rx_total: ByteSize::from_bytes(cur.rx_bytes),
                tx_total: ByteSize::from_bytes(cur.tx_bytes),
                rx_bytes_per_sec: rx_bps,
                tx_bytes_per_sec: tx_bps,
                rx_packets_per_sec: rx_pps,
                tx_packets_per_sec: tx_pps,
            });
            next_prev.insert(cur.name.clone(), cur);
        }

        self.prev = next_prev;
        self.prev_time = Some(now);

        NetworkSnapshot {
            interfaces,
            total_rx_bytes_per_sec,
            total_tx_bytes_per_sec,
            timestamp: now,
        }
    }
}

fn enrich_live_interfaces(interfaces: &mut [NetInterfaceSnapshot]) {
    let addrs = read_interface_ips();
    let gateways = read_default_gateways();
    let wireless_signals = read_wireless_signals();

    for iface in interfaces {
        if let Some((v4, v6)) = addrs.get(&iface.name) {
            iface.ipv4 = v4.clone();
            iface.ipv6 = v6.clone();
        }
        iface.gateway = gateways.get(&iface.name).cloned();
        iface.wireless_signal_dbm = wireless_signals.get(&iface.name).copied();
        if iface.kind == NetInterfaceKind::Wifi {
            let freq_path = format!("/sys/class/net/{}/phy80211/frequency", iface.name);
            iface.wireless_freq_mhz = read_sysfs_u32(Path::new(&freq_path));
        }
    }
}

fn read_default_gateways() -> HashMap<String, String> {
    let mut map = HashMap::new();
    let Ok(route) = std::fs::read_to_string("/proc/net/route") else {
        return map;
    };
    for line in route.lines().skip(1) {
        let mut cols = line.split_whitespace();
        let (Some(iface), Some(dest), Some(gw)) = (cols.next(), cols.next(), cols.next()) else {
            continue;
        };
        if dest == "00000000"
            && let Ok(gw_u32) = u32::from_str_radix(gw, 16)
        {
            let b = gw_u32.to_ne_bytes();
            map.entry(iface.to_owned())
                .or_insert_with(|| format!("{}.{}.{}.{}", b[0], b[1], b[2], b[3]));
        }
    }
    map
}

fn read_wireless_signals() -> HashMap<String, i32> {
    let mut map = HashMap::new();
    let Ok(raw) = std::fs::read_to_string("/proc/net/wireless") else {
        return map;
    };
    for line in raw.lines().skip(2) {
        let Some((iface, rest)) = line.split_once(':') else {
            continue;
        };
        let cols: Vec<&str> = rest.split_whitespace().collect();
        if let Some(level) = cols
            .get(2)
            .and_then(|s| s.trim_end_matches('.').parse::<f32>().ok())
        {
            map.insert(iface.trim().to_owned(), level.round() as i32);
        }
    }
    map
}

fn read_interface_ips() -> HashMap<String, (Option<String>, Option<String>)> {
    use std::ffi::CStr;
    use std::net::{Ipv4Addr, Ipv6Addr};

    let mut map: HashMap<String, (Option<String>, Option<String>)> = HashMap::new();
    let mut ifap: *mut libc::ifaddrs = std::ptr::null_mut();
    // SAFETY: `getifaddrs` initializes `ifap` on success; freed via `freeifaddrs` below.
    if unsafe { libc::getifaddrs(&mut ifap) } != 0 {
        return map;
    }
    let mut cur = ifap;
    while !cur.is_null() {
        // SAFETY: `cur` is a non-null node in the `getifaddrs` linked list.
        let ifa = unsafe { &*cur };
        if !ifa.ifa_name.is_null() && !ifa.ifa_addr.is_null() {
            // SAFETY: `ifa_name` is a NUL-terminated C string; `ifa_addr` is a valid `sockaddr`.
            let name = unsafe { CStr::from_ptr(ifa.ifa_name) }
                .to_string_lossy()
                .into_owned();
            // SAFETY: `ifa_addr` is checked non-null above and points to a valid `sockaddr`
            // owned by the `getifaddrs` list, so reading `sa_family` is sound.
            let family = i32::from(unsafe { (*ifa.ifa_addr).sa_family });
            let entry = map.entry(name).or_default();
            if family == libc::AF_INET && entry.0.is_none() {
                // SAFETY: `sa_family == AF_INET` guarantees `sockaddr_in` layout.
                let sin = unsafe { &*(ifa.ifa_addr as *const libc::sockaddr_in) };
                let ip = Ipv4Addr::from(u32::from_be(sin.sin_addr.s_addr));
                entry.0 = Some(ip.to_string());
            } else if family == libc::AF_INET6 && entry.1.is_none() {
                // SAFETY: `sa_family == AF_INET6` guarantees `sockaddr_in6` layout.
                let sin6 = unsafe { &*(ifa.ifa_addr as *const libc::sockaddr_in6) };
                let ip = Ipv6Addr::from(sin6.sin6_addr.s6_addr);
                if !ip.is_unicast_link_local() {
                    entry.1 = Some(ip.to_string());
                }
            }
        }
        cur = ifa.ifa_next;
    }
    // SAFETY: `ifap` was returned by `getifaddrs` and is freed exactly once.
    unsafe { libc::freeifaddrs(ifap) };
    map
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn classify_interface_prefers_sysfs_facts_over_name_guesses() {
        let base = std::env::temp_dir().join(format!("waywi-net-sysfs-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&base);

        // 1. Custom-named bridge ("lan0") with type=1 and no `device` symlink -> Virtual, not Ethernet.
        let br_dir = base.join("lan0");
        std::fs::create_dir_all(&br_dir).unwrap();
        std::fs::write(br_dir.join("type"), "1\n").unwrap();
        std::fs::write(br_dir.join("uevent"), "DEVTYPE=bridge\n").unwrap();
        assert_eq!(
            classify_interface("lan0", Some(&br_dir)),
            NetInterfaceKind::Virtual
        );

        // 2. Physical NIC named "eth-custom" with type=1 and backing `device` dir -> Ethernet.
        let eth_dir = base.join("eth-custom");
        std::fs::create_dir_all(eth_dir.join("device")).unwrap();
        std::fs::write(eth_dir.join("type"), "1\n").unwrap();
        assert_eq!(
            classify_interface("eth-custom", Some(&eth_dir)),
            NetInterfaceKind::Ethernet
        );

        // 3. WireGuard tunnel named "office0" with ARPHRD_NONE (65534) -> Virtual.
        let wg_dir = base.join("office0");
        std::fs::create_dir_all(&wg_dir).unwrap();
        std::fs::write(wg_dir.join("type"), "65534\n").unwrap();
        assert_eq!(
            classify_interface("office0", Some(&wg_dir)),
            NetInterfaceKind::Virtual
        );

        // 4. Wi-Fi device with DEVTYPE=wlan regardless of name.
        let wifi_dir = base.join("net0");
        std::fs::create_dir_all(&wifi_dir).unwrap();
        std::fs::write(wifi_dir.join("type"), "1\n").unwrap();
        std::fs::write(wifi_dir.join("uevent"), "DEVTYPE=wlan\n").unwrap();
        assert_eq!(
            classify_interface("net0", Some(&wifi_dir)),
            NetInterfaceKind::Wifi
        );

        let _ = std::fs::remove_dir_all(&base);
    }
}
