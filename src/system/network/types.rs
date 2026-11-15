//! Network interface link status and bandwidth throughput snapshot types.

use std::time::Instant;

use crate::system::units::{ByteSize, DataRate};

/// Classified Linux network interface kind.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum NetInterfaceKind {
    Ethernet,
    Wifi,
    Modem,
    Loopback,
    Virtual,
    Other,
}

/// Kernel interface operational state (`/sys/class/net/<iface>/operstate`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum NetLinkState {
    Up,
    Down,
    Unknown,
}

/// Per-interface status, IP/gateway/wireless metadata, cumulative byte counts, and Rx/Tx rates.
#[derive(Debug, Clone, PartialEq)]
pub struct NetInterfaceSnapshot {
    pub name: String,
    pub kind: NetInterfaceKind,
    pub link_state: NetLinkState,
    pub carrier: Option<bool>,
    pub link_speed_mbps: Option<u32>,
    pub ipv4: Option<String>,
    pub ipv6: Option<String>,
    pub gateway: Option<String>,
    pub wireless_signal_dbm: Option<i32>,
    pub wireless_freq_mhz: Option<u32>,
    pub rx_total: ByteSize,
    pub tx_total: ByteSize,
    pub rx_bytes_per_sec: f64,
    pub tx_bytes_per_sec: f64,
    pub rx_packets_per_sec: f64,
    pub tx_packets_per_sec: f64,
}

impl NetInterfaceSnapshot {
    /// Typed receive throughput rate.
    #[must_use]
    pub const fn rx_rate(&self) -> DataRate {
        DataRate::from_bytes_per_sec(self.rx_bytes_per_sec)
    }

    /// Typed transmit throughput rate.
    #[must_use]
    pub const fn tx_rate(&self) -> DataRate {
        DataRate::from_bytes_per_sec(self.tx_bytes_per_sec)
    }

    /// Wireless frequency in GHz (e.g. `5.18`), if available.
    #[must_use]
    pub fn wireless_freq_ghz(&self) -> Option<f64> {
        self.wireless_freq_mhz.map(|mhz| f64::from(mhz) / 1_000.0)
    }
}

/// Multi-interface network telemetry snapshot emitted by `System::network()`.
#[derive(Debug, Clone, PartialEq)]
pub struct NetworkSnapshot {
    pub interfaces: Vec<NetInterfaceSnapshot>,
    /// Summed receive rate across non-loopback interfaces.
    pub total_rx_bytes_per_sec: f64,
    /// Summed transmit rate across non-loopback interfaces.
    pub total_tx_bytes_per_sec: f64,
    pub timestamp: Instant,
}

impl NetworkSnapshot {
    /// Typed aggregate receive throughput across non-loopback interfaces.
    #[must_use]
    pub const fn total_rx_rate(&self) -> DataRate {
        DataRate::from_bytes_per_sec(self.total_rx_bytes_per_sec)
    }

    /// Typed aggregate transmit throughput across non-loopback interfaces.
    #[must_use]
    pub const fn total_tx_rate(&self) -> DataRate {
        DataRate::from_bytes_per_sec(self.total_tx_bytes_per_sec)
    }

    /// Selects the primary active uplink interface across Ethernet, Wi-Fi, Modem/WWAN, etc.
    #[must_use]
    pub fn active_interface(&self) -> Option<&NetInterfaceSnapshot> {
        let is_physical = |iface: &&NetInterfaceSnapshot| {
            !matches!(
                iface.kind,
                NetInterfaceKind::Loopback | NetInterfaceKind::Virtual
            )
        };
        self.interfaces
            .iter()
            .filter(|i| i.link_state == NetLinkState::Up && is_physical(i) && i.gateway.is_some())
            .min_by_key(|i| kind_priority(i.kind))
            .or_else(|| {
                self.interfaces
                    .iter()
                    .filter(|i| {
                        i.link_state == NetLinkState::Up
                            && is_physical(i)
                            && (i.ipv4.is_some() || i.ipv6.is_some() || i.carrier == Some(true))
                    })
                    .min_by_key(|i| kind_priority(i.kind))
            })
            .or_else(|| {
                self.interfaces.iter().find(|i| {
                    i.link_state == NetLinkState::Up && i.kind != NetInterfaceKind::Loopback
                })
            })
    }
}

const fn kind_priority(kind: NetInterfaceKind) -> u8 {
    match kind {
        NetInterfaceKind::Ethernet => 0,
        NetInterfaceKind::Wifi => 1,
        NetInterfaceKind::Modem => 2,
        NetInterfaceKind::Other => 3,
        NetInterfaceKind::Virtual => 4,
        NetInterfaceKind::Loopback => 5,
    }
}
