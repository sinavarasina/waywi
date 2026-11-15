//! Linux network interface link status and Rx/Tx throughput domain.

pub mod proc_net;
pub mod types;

pub use proc_net::{NetDevCounters, NetworkSampler, classify_interface, parse_proc_net_dev};
pub use types::{NetInterfaceKind, NetInterfaceSnapshot, NetLinkState, NetworkSnapshot};
