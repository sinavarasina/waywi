//! BlueZ Bluetooth adapter, device, battery, connection, and pairing agent management (`org.bluez`).

use std::collections::HashMap;
use std::future::Future;
use std::pin::Pin;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::task::{Context, Poll, Waker};

use zbus::interface;
use zbus::zvariant::{ObjectPath, OwnedObjectPath, Value};

use crate::arena::FrameSharedFn;
use crate::error::{Result, WaywiError};
use crate::subscription::{Subscription, SubscriptionSpec};

use super::client::{
    DBUS_SLOW_TIMEOUT, DbusActionLane, DbusClient, DbusSharedBus, LaneActionSpec,
    validate_object_path,
};
use super::types::{Dbus, DbusBus};
use super::worker::{AsyncStopSignal, DbusWorkerHandle, ServiceWatchWorker};
use super::zvariant_helpers::{
    ManagedObjects, dict_bool, dict_i16, dict_str, dict_str_vec, dict_u8, dict_u32,
    get_managed_objects, unwrap_value,
};

const BLUEZ_DEST: &str = "org.bluez";
const BLUEZ_ROOT_PATH: &str = "/";
const BLUEZ_BUS_PATH: &str = "/org/bluez";
const BLUEZ_ADAPTER_IFACE: &str = "org.bluez.Adapter1";
const BLUEZ_DEVICE_IFACE: &str = "org.bluez.Device1";
const BLUEZ_BATTERY_IFACE: &str = "org.bluez.Battery1";
const BLUEZ_AGENT_MGR_IFACE: &str = "org.bluez.AgentManager1";
const BLUEZ_AGENT_OBJECT_PATH: &str = "/org/waywi/BluetoothAgent";

/// Bluetooth controller state (`org.bluez.Adapter1`).
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct BluetoothAdapter {
    pub object_path: String,
    pub address: String,
    pub name: String,
    pub alias: String,
    pub powered: bool,
    pub discoverable: bool,
    pub discoverable_timeout_secs: u32,
    pub pairable: bool,
    pub pairable_timeout_secs: u32,
    pub discovering: bool,
}

/// Discovered or paired Bluetooth device (`org.bluez.Device1` + optional `org.bluez.Battery1`).
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct BluetoothDevice {
    pub object_path: String,
    pub adapter_path: String,
    pub address: String,
    pub address_type: String,
    pub name: String,
    pub alias: String,
    pub icon: Option<String>,
    pub class: Option<u32>,
    pub appearance: Option<u16>,
    pub paired: bool,
    pub bonded: bool,
    pub trusted: bool,
    pub connected: bool,
    pub blocked: bool,
    pub legacy_pairing: bool,
    pub services_resolved: bool,
    pub rssi: Option<i16>,
    pub tx_power: Option<i16>,
    pub battery_percentage: Option<u8>,
    pub uuids: Vec<String>,
}

/// Aggregate snapshot of BlueZ adapters and devices.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct BluetoothSnapshot {
    pub adapters: Vec<BluetoothAdapter>,
    pub devices: Vec<BluetoothDevice>,
}

/// Transport filter for Bluetooth discovery (`org.bluez.Adapter1.SetDiscoveryFilter`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub enum BluetoothTransportFilter {
    #[default]
    Auto,
    BrEdr,
    Le,
}

impl BluetoothTransportFilter {
    /// Returns the BlueZ `Transport` filter string.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Auto => "auto",
            Self::BrEdr => "bredr",
            Self::Le => "le",
        }
    }
}

/// Pairing and authorization prompts received by [`Dbus::bluetooth_agent`] (`org.bluez.Agent1`).
#[non_exhaustive]
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BluetoothAgentRequest {
    /// BlueZ requests an alphanumeric PIN code (`1..=16` chars) for legacy pairing.
    RequestPinCode {
        request_id: u64,
        device_path: String,
    },
    /// BlueZ asks the UI to display a PIN code (`entered` = number of keys typed on remote).
    DisplayPinCode {
        device_path: String,
        pincode: String,
    },
    /// BlueZ requests a 6-digit numeric passkey (`0..=999_999`).
    RequestPasskey {
        request_id: u64,
        device_path: String,
    },
    /// BlueZ asks the UI to display a 6-digit passkey and number of typed digits.
    DisplayPasskey {
        device_path: String,
        passkey: u32,
        entered: u16,
    },
    /// Numeric comparison prompt: user must confirm whether `passkey` matches the remote display.
    RequestConfirmation {
        request_id: u64,
        device_path: String,
        passkey: u32,
    },
    /// Incoming connection/pairing authorization request without a passkey.
    RequestAuthorization {
        request_id: u64,
        device_path: String,
    },
    /// Incoming Bluetooth service profile (`uuid`) authorization request from `device_path`.
    AuthorizeService {
        request_id: u64,
        device_path: String,
        uuid: String,
    },
}

/// Events emitted by [`Dbus::bluetooth_agent`] when acting as the BlueZ pairing agent.
#[non_exhaustive]
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BluetoothAgentEvent {
    /// Agent successfully registered with `org.bluez.AgentManager1` (`is_default_agent` is `true`
    /// when `RequestDefaultAgent` also succeeded).
    Registered { is_default_agent: bool },
    /// Interactive pairing or connection authorization request from BlueZ.
    Request(BluetoothAgentRequest),
    /// The in-flight agent request was cancelled by BlueZ (`org.bluez.Agent1.Cancel`).
    Cancelled,
    /// BlueZ unregistered and released this agent (`org.bluez.Agent1.Release`).
    Released,
    /// Agent registration or D-Bus transport error.
    Error(String),
}

/// Response payload sent back to BlueZ via [`BluetoothClient::reply_agent`] for a pending
/// [`BluetoothAgentRequest`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BluetoothAgentReply {
    Accept,
    PinCode(String),
    Passkey(u32),
    Reject,
    Cancel,
}

/// Zero-polling waker-backed oneshot slot shared between `BluetoothClient`, `BluezAgentIface`, and
/// the raw D-Bus `Cancel`/`Release` signal interceptor.
pub(crate) struct AgentReplySlot {
    inner: Mutex<AgentReplyState>,
}

#[derive(Default)]
struct AgentReplyState {
    reply: Option<BluetoothAgentReply>,
    waker: Option<Waker>,
}

impl AgentReplySlot {
    pub(crate) fn new() -> Arc<Self> {
        Arc::new(Self {
            inner: Mutex::new(AgentReplyState::default()),
        })
    }

    pub(crate) fn resolve(&self, reply: BluetoothAgentReply) {
        if let Ok(mut guard) = self.inner.lock() {
            if guard.reply.is_none() {
                guard.reply = Some(reply);
            }
            if let Some(waker) = guard.waker.take() {
                waker.wake();
            }
        }
    }
}

struct AgentReplyFuture {
    slot: Arc<AgentReplySlot>,
}

impl Future for AgentReplyFuture {
    type Output = Option<BluetoothAgentReply>;

    fn poll(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
        let Ok(mut guard) = self.slot.inner.lock() else {
            return Poll::Ready(None);
        };
        if let Some(reply) = guard.reply.take() {
            return Poll::Ready(Some(reply));
        }
        guard.waker = Some(cx.waker().clone());
        Poll::Pending
    }
}

pub(crate) type PendingAgentMap = Arc<Mutex<HashMap<u64, Arc<AgentReplySlot>>>>;

pub(crate) fn cancel_all_pending_slots(pending: &PendingAgentMap) {
    if let Ok(mut guard) = pending.lock() {
        for (_, slot) in guard.drain() {
            slot.resolve(BluetoothAgentReply::Cancel);
        }
    }
}

impl Dbus {
    /// Subscribes to BlueZ Bluetooth adapter, device, and battery updates.
    pub fn bluetooth<M: 'static>(
        map: impl Fn(BluetoothSnapshot) -> M + 'static,
    ) -> Subscription<M> {
        Subscription {
            spec: SubscriptionSpec::DbusBluetooth {
                map: FrameSharedFn::new(map),
            },
        }
    }

    /// Registers a BlueZ pairing and authorization agent (`org.bluez.Agent1`) on the system D-Bus
    /// and streams incoming pairing/connection requests (`RequestConfirmation`, `AuthorizeService`,
    /// `RequestPasskey`, `RequestPinCode`, etc.).
    pub fn bluetooth_agent<M: 'static>(
        map: impl Fn(BluetoothAgentEvent) -> M + 'static,
    ) -> Subscription<M> {
        Subscription {
            spec: SubscriptionSpec::DbusBluetoothAgent {
                map: FrameSharedFn::new(map),
            },
        }
    }
}

/// Zero-cost domain handle for BlueZ Bluetooth adapter, device, and pairing agent operations.
#[derive(Clone, Copy)]
pub struct BluetoothClient<'a>(pub(super) &'a DbusClient);

impl BluetoothClient<'_> {
    /// Returns the latest cached [`BluetoothSnapshot`] from an active [`Dbus::bluetooth`]
    /// subscription without issuing any D-Bus calls (0 ms).
    #[must_use]
    pub fn cached_snapshot(&self) -> Option<BluetoothSnapshot> {
        self.0.cache.bluetooth.read().ok().and_then(|g| g.clone())
    }

    /// Queries `org.bluez` synchronously on the system bus for the current [`BluetoothSnapshot`].
    pub fn snapshot_blocking(&self) -> Result<BluetoothSnapshot> {
        let conn = self.0.async_connection(DbusBus::System)?;
        futures_lite::future::block_on(async move {
            let objs = get_managed_objects(&conn, BLUEZ_DEST, BLUEZ_ROOT_PATH).await?;
            Ok(parse_bluez_objects(&objs))
        })
    }

    /// Powers a Bluetooth adapter (`org.bluez.Adapter1.Powered`) on or off.
    pub fn set_powered(&self, adapter_path: &str, powered: bool) -> Result<()> {
        self.0.set_property_zvariant(
            DbusBus::System,
            BLUEZ_DEST,
            adapter_path,
            BLUEZ_ADAPTER_IFACE,
            "Powered",
            Value::from(powered),
        )
    }

    /// Sets whether a Bluetooth adapter is visible to nearby devices (`org.bluez.Adapter1.Discoverable`).
    pub fn set_discoverable(&self, adapter_path: &str, discoverable: bool) -> Result<()> {
        self.0.set_property_zvariant(
            DbusBus::System,
            BLUEZ_DEST,
            adapter_path,
            BLUEZ_ADAPTER_IFACE,
            "Discoverable",
            Value::from(discoverable),
        )
    }

    /// Sets whether a Bluetooth adapter accepts pairing requests (`org.bluez.Adapter1.Pairable`).
    pub fn set_pairable(&self, adapter_path: &str, pairable: bool) -> Result<()> {
        self.0.set_property_zvariant(
            DbusBus::System,
            BLUEZ_DEST,
            adapter_path,
            BLUEZ_ADAPTER_IFACE,
            "Pairable",
            Value::from(pairable),
        )
    }

    /// Starts Bluetooth device discovery on `adapter_path` on the serialized Bluetooth adapter lane,
    /// optionally applying a transport filter (`SetDiscoveryFilter`) beforehand.
    pub fn start_discovery(
        &self,
        adapter_path: &str,
        filter: Option<BluetoothTransportFilter>,
    ) -> Result<()> {
        let adapter = validate_object_path(adapter_path, "adapter path")?;
        let target = adapter.clone();
        self.0.submit_lane_action(
            LaneActionSpec::fast(
                DbusActionLane::BluetoothAdapter,
                DbusBus::System,
                "Adapter1.StartDiscovery",
                target,
            ),
            move |conn| async move {
                if let Some(transport) = filter {
                    let mut dict: HashMap<&str, Value<'_>> = HashMap::new();
                    dict.insert("Transport", Value::from(transport.as_str()));
                    conn.call_method(
                        Some(BLUEZ_DEST),
                        adapter.as_str(),
                        Some(BLUEZ_ADAPTER_IFACE),
                        "SetDiscoveryFilter",
                        &(dict,),
                    )
                    .await
                    .map_err(|e| {
                        WaywiError::DbusError(format!("Adapter1.SetDiscoveryFilter failed: {e}"))
                    })?;
                }
                conn.call_method(
                    Some(BLUEZ_DEST),
                    adapter.as_str(),
                    Some(BLUEZ_ADAPTER_IFACE),
                    "StartDiscovery",
                    &(),
                )
                .await
                .map_err(|e| {
                    WaywiError::DbusError(format!("Adapter1.StartDiscovery failed: {e}"))
                })?;
                Ok(())
            },
        )
    }

    /// Stops Bluetooth device discovery on `adapter_path` on the serialized Bluetooth adapter lane.
    pub fn stop_discovery(&self, adapter_path: &str) -> Result<()> {
        self.0.call_lane_method_unit(
            DbusActionLane::BluetoothAdapter,
            DbusBus::System,
            BLUEZ_DEST,
            adapter_path,
            BLUEZ_ADAPTER_IFACE,
            "StopDiscovery",
        )
    }

    /// Connects a known Bluetooth device (`org.bluez.Device1.Connect`) on the Bluetooth device lane
    /// with deduplication and a 25-second paging timeout so the UI thread never freezes.
    pub fn connect(&self, device_path: &str) -> Result<()> {
        let dev = validate_object_path(device_path, "Bluetooth device path")?;
        let target = dev.clone();
        let dedup = format!("bt:connect:{dev}");
        self.0.submit_lane_action(
            LaneActionSpec::fast(
                DbusActionLane::BluetoothDevice,
                DbusBus::System,
                "Device1.Connect",
                target,
            )
            .with_dedup(dedup)
            .with_timeout(DBUS_SLOW_TIMEOUT),
            move |conn| async move {
                conn.call_method(
                    Some(BLUEZ_DEST),
                    dev.as_str(),
                    Some(BLUEZ_DEVICE_IFACE),
                    "Connect",
                    &(),
                )
                .await
                .map_err(|e| WaywiError::DbusError(format!("Device1.Connect failed: {e}")))?;
                Ok(())
            },
        )
    }

    /// Disconnects a connected Bluetooth device (`org.bluez.Device1.Disconnect`) on the Bluetooth
    /// device lane.
    pub fn disconnect(&self, device_path: &str) -> Result<()> {
        self.0.call_lane_method_unit(
            DbusActionLane::BluetoothDevice,
            DbusBus::System,
            BLUEZ_DEST,
            device_path,
            BLUEZ_DEVICE_IFACE,
            "Disconnect",
        )
    }

    /// Pairs a discovered Bluetooth device (`org.bluez.Device1.Pair`) on the Bluetooth device lane
    /// with deduplication and a 25-second timeout so neither the UI thread nor `org.bluez.Agent1`
    /// prompts are blocked.
    pub fn pair(&self, device_path: &str) -> Result<()> {
        let dev = validate_object_path(device_path, "Bluetooth device path")?;
        let target = dev.clone();
        let dedup = format!("bt:pair:{dev}");
        self.0.submit_lane_action(
            LaneActionSpec::fast(
                DbusActionLane::BluetoothDevice,
                DbusBus::System,
                "Device1.Pair",
                target,
            )
            .with_dedup(dedup)
            .with_timeout(DBUS_SLOW_TIMEOUT),
            move |conn| async move {
                conn.call_method(
                    Some(BLUEZ_DEST),
                    dev.as_str(),
                    Some(BLUEZ_DEVICE_IFACE),
                    "Pair",
                    &(),
                )
                .await
                .map_err(|e| WaywiError::DbusError(format!("Device1.Pair failed: {e}")))?;
                Ok(())
            },
        )
    }

    /// Cancels an in-progress pairing exchange with `device_path` (`org.bluez.Device1.CancelPairing`).
    pub fn cancel_pairing(&self, device_path: &str) -> Result<()> {
        self.0.call_lane_method_unit(
            DbusActionLane::BluetoothAdapter,
            DbusBus::System,
            BLUEZ_DEST,
            device_path,
            BLUEZ_DEVICE_IFACE,
            "CancelPairing",
        )
    }

    /// Sets the `Trusted` flag on a Bluetooth device (`org.bluez.Device1.Trusted`).
    pub fn trust(&self, device_path: &str, trusted: bool) -> Result<()> {
        self.0.set_property_zvariant(
            DbusBus::System,
            BLUEZ_DEST,
            device_path,
            BLUEZ_DEVICE_IFACE,
            "Trusted",
            Value::from(trusted),
        )
    }

    /// Unpairs/removes a Bluetooth device from its adapter (`org.bluez.Adapter1.RemoveDevice`)
    /// on the Bluetooth adapter lane.
    pub fn remove(&self, adapter_path: &str, device_path: &str) -> Result<()> {
        let adapter = validate_object_path(adapter_path, "adapter path")?;
        let obj = OwnedObjectPath::try_from(device_path.to_owned())
            .map_err(|e| WaywiError::DbusError(e.to_string()))?;
        let target = device_path.to_owned();
        self.0.submit_lane_action(
            LaneActionSpec::fast(
                DbusActionLane::BluetoothAdapter,
                DbusBus::System,
                "Adapter1.RemoveDevice",
                target,
            ),
            move |conn| async move {
                conn.call_method(
                    Some(BLUEZ_DEST),
                    adapter.as_str(),
                    Some(BLUEZ_ADAPTER_IFACE),
                    "RemoveDevice",
                    &(obj,),
                )
                .await
                .map_err(|e| WaywiError::DbusError(format!("Adapter1.RemoveDevice failed: {e}")))?;
                Ok(())
            },
        )
    }

    /// Sends a typed [`BluetoothAgentReply`] for a pending [`BluetoothAgentRequest`] identified by
    /// `request_id`.
    pub fn reply_agent(&self, request_id: u64, reply: BluetoothAgentReply) -> Result<()> {
        let slot = self
            .0
            .bluetooth_agent_pending
            .lock()
            .map_err(|_| WaywiError::DbusError("bluetooth agent pending lock poisoned".into()))?
            .remove(&request_id)
            .ok_or_else(|| {
                WaywiError::DbusError(format!(
                    "no pending Bluetooth agent request with id {request_id}"
                ))
            })?;
        slot.resolve(reply);
        Ok(())
    }
}

pub(crate) fn spawn_bluetooth_worker(
    sender: calloop::channel::Sender<BluetoothSnapshot>,
    shared: DbusSharedBus,
) -> ServiceWatchWorker {
    ServiceWatchWorker::spawn(
        DbusBus::System,
        "waywi-dbus-bluez",
        Some(BLUEZ_DEST),
        None,
        sender,
        shared,
        |conn| async move {
            let objs = get_managed_objects(&conn, BLUEZ_DEST, BLUEZ_ROOT_PATH)
                .await
                .ok()?;
            Some(parse_bluez_objects(&objs))
        },
    )
}

pub(crate) struct BluetoothAgentWorker {
    pending: PendingAgentMap,
    handle: DbusWorkerHandle,
}

impl BluetoothAgentWorker {
    pub(crate) fn spawn(
        sender: calloop::channel::Sender<BluetoothAgentEvent>,
        pending: PendingAgentMap,
    ) -> Self {
        let pending_for_thread = Arc::clone(&pending);
        let handle = DbusWorkerHandle::spawn("waywi-dbus-bt-agent", move |stop_sig| {
            futures_lite::future::block_on(run_bluetooth_agent_worker(
                sender,
                pending_for_thread,
                stop_sig,
            ));
        });
        Self { pending, handle }
    }
}

impl Drop for BluetoothAgentWorker {
    fn drop(&mut self) {
        self.handle.trigger_stop();
        cancel_all_pending_slots(&self.pending);
    }
}

async fn run_bluetooth_agent_worker(
    sender: calloop::channel::Sender<BluetoothAgentEvent>,
    pending: PendingAgentMap,
    stop_sig: AsyncStopSignal,
) {
    let iface = BluezAgentIface {
        next_id: AtomicU64::new(1),
        sender: sender.clone(),
        pending: Arc::clone(&pending),
        stop: stop_sig.clone(),
    };

    let conn_res = match zbus::connection::Builder::system() {
        Ok(b) => match b.serve_at(BLUEZ_AGENT_OBJECT_PATH, iface) {
            Ok(b2) => b2.build().await,
            Err(e) => Err(e),
        },
        Err(e) => Err(e),
    };

    let conn = match conn_res {
        Ok(c) => c,
        Err(e) => {
            let _ = sender.send(BluetoothAgentEvent::Error(format!(
                "failed to initialize BlueZ Agent1 server: {e}"
            )));
            return;
        }
    };

    let Ok(agent_path) = ObjectPath::try_from(BLUEZ_AGENT_OBJECT_PATH) else {
        return;
    };

    if let Err(e) = conn
        .call_method(
            Some(BLUEZ_DEST),
            BLUEZ_BUS_PATH,
            Some(BLUEZ_AGENT_MGR_IFACE),
            "RegisterAgent",
            &(&agent_path, "KeyboardDisplay"),
        )
        .await
    {
        let _ = sender.send(BluetoothAgentEvent::Error(format!(
            "AgentManager1.RegisterAgent failed: {e}"
        )));
        return;
    }

    let is_default_agent = match conn
        .call_method(
            Some(BLUEZ_DEST),
            BLUEZ_BUS_PATH,
            Some(BLUEZ_AGENT_MGR_IFACE),
            "RequestDefaultAgent",
            &(&agent_path,),
        )
        .await
    {
        Ok(_) => true,
        Err(e) => {
            let _ = sender.send(BluetoothAgentEvent::Error(format!(
                "AgentManager1.RequestDefaultAgent failed: {e}"
            )));
            false
        }
    };
    let _ = sender.send(BluetoothAgentEvent::Registered { is_default_agent });

    // Intercept incoming org.bluez.Agent1.Cancel / Release method calls directly on the
    // connection stream so an in-flight `.await` inside `BluezAgentIface` is woken immediately
    // even while zbus's ObjectServer serializes interface method dispatch.
    let mut raw_stream = zbus::MessageStream::from(&conn);
    use futures_lite::StreamExt;
    while !stop_sig.is_stopped() {
        let next_msg = futures_lite::future::or(async { Some(raw_stream.next().await) }, async {
            stop_sig.wait_stopped().await;
            None
        })
        .await;
        let Some(Some(Ok(msg))) = next_msg else {
            break;
        };
        let header = msg.header();
        if header.message_type() == zbus::message::Type::MethodCall
            && header.path().map(|p| p.as_str()) == Some(BLUEZ_AGENT_OBJECT_PATH)
            && header.interface().map(|i| i.as_str()) == Some("org.bluez.Agent1")
            && let Some("Cancel" | "Release") = header.member().map(|m| m.as_str())
        {
            cancel_all_pending_slots(&pending);
        }
    }

    let _ = conn
        .call_method(
            Some(BLUEZ_DEST),
            BLUEZ_BUS_PATH,
            Some(BLUEZ_AGENT_MGR_IFACE),
            "UnregisterAgent",
            &(&agent_path,),
        )
        .await;
    cancel_all_pending_slots(&pending);
}

struct BluezAgentIface {
    next_id: AtomicU64,
    sender: calloop::channel::Sender<BluetoothAgentEvent>,
    pending: PendingAgentMap,
    stop: AsyncStopSignal,
}

impl BluezAgentIface {
    async fn await_user_reply(
        &self,
        make_req: impl FnOnce(u64) -> BluetoothAgentRequest,
    ) -> std::result::Result<BluetoothAgentReply, zbus::fdo::Error> {
        let request_id = self.next_id.fetch_add(1, Ordering::Relaxed);
        let slot = AgentReplySlot::new();
        if let Ok(mut guard) = self.pending.lock() {
            guard.insert(request_id, Arc::clone(&slot));
        }
        let _ = self
            .sender
            .send(BluetoothAgentEvent::Request(make_req(request_id)));

        let reply = futures_lite::future::or(AgentReplyFuture { slot }, async {
            self.stop.wait_stopped().await;
            None
        })
        .await;

        if let Ok(mut guard) = self.pending.lock() {
            guard.remove(&request_id);
        }
        match reply {
            Some(BluetoothAgentReply::Reject) => {
                Err(zbus::fdo::Error::Failed("org.bluez.Error.Rejected".into()))
            }
            Some(BluetoothAgentReply::Cancel) | None => {
                Err(zbus::fdo::Error::Failed("org.bluez.Error.Canceled".into()))
            }
            Some(other) => Ok(other),
        }
    }
}

#[interface(name = "org.bluez.Agent1")]
impl BluezAgentIface {
    async fn release(&self) {
        cancel_all_pending_slots(&self.pending);
        let _ = self.sender.send(BluetoothAgentEvent::Released);
    }

    async fn request_pin_code(
        &self,
        device: OwnedObjectPath,
    ) -> std::result::Result<String, zbus::fdo::Error> {
        let dev = device.to_string();
        match self
            .await_user_reply(|request_id| BluetoothAgentRequest::RequestPinCode {
                request_id,
                device_path: dev,
            })
            .await?
        {
            BluetoothAgentReply::PinCode(pin) => Ok(pin),
            _ => Err(zbus::fdo::Error::Failed("org.bluez.Error.Rejected".into())),
        }
    }

    async fn display_pin_code(&self, device: OwnedObjectPath, pincode: String) {
        let _ = self.sender.send(BluetoothAgentEvent::Request(
            BluetoothAgentRequest::DisplayPinCode {
                device_path: device.to_string(),
                pincode,
            },
        ));
    }

    async fn request_passkey(
        &self,
        device: OwnedObjectPath,
    ) -> std::result::Result<u32, zbus::fdo::Error> {
        let dev = device.to_string();
        match self
            .await_user_reply(|request_id| BluetoothAgentRequest::RequestPasskey {
                request_id,
                device_path: dev,
            })
            .await?
        {
            BluetoothAgentReply::Passkey(pk) => Ok(pk),
            _ => Err(zbus::fdo::Error::Failed("org.bluez.Error.Rejected".into())),
        }
    }

    async fn display_passkey(&self, device: OwnedObjectPath, passkey: u32, entered: u16) {
        let _ = self.sender.send(BluetoothAgentEvent::Request(
            BluetoothAgentRequest::DisplayPasskey {
                device_path: device.to_string(),
                passkey,
                entered,
            },
        ));
    }

    async fn request_confirmation(
        &self,
        device: OwnedObjectPath,
        passkey: u32,
    ) -> std::result::Result<(), zbus::fdo::Error> {
        let dev = device.to_string();
        match self
            .await_user_reply(|request_id| BluetoothAgentRequest::RequestConfirmation {
                request_id,
                device_path: dev,
                passkey,
            })
            .await?
        {
            BluetoothAgentReply::Accept | BluetoothAgentReply::Passkey(_) => Ok(()),
            _ => Err(zbus::fdo::Error::Failed("org.bluez.Error.Rejected".into())),
        }
    }

    async fn request_authorization(
        &self,
        device: OwnedObjectPath,
    ) -> std::result::Result<(), zbus::fdo::Error> {
        let dev = device.to_string();
        match self
            .await_user_reply(|request_id| BluetoothAgentRequest::RequestAuthorization {
                request_id,
                device_path: dev,
            })
            .await?
        {
            BluetoothAgentReply::Accept => Ok(()),
            _ => Err(zbus::fdo::Error::Failed("org.bluez.Error.Rejected".into())),
        }
    }

    async fn authorize_service(
        &self,
        device: OwnedObjectPath,
        uuid: String,
    ) -> std::result::Result<(), zbus::fdo::Error> {
        let dev = device.to_string();
        match self
            .await_user_reply(|request_id| BluetoothAgentRequest::AuthorizeService {
                request_id,
                device_path: dev,
                uuid,
            })
            .await?
        {
            BluetoothAgentReply::Accept => Ok(()),
            _ => Err(zbus::fdo::Error::Failed("org.bluez.Error.Rejected".into())),
        }
    }

    async fn cancel(&self) {
        cancel_all_pending_slots(&self.pending);
        let _ = self.sender.send(BluetoothAgentEvent::Cancelled);
    }
}

pub(crate) fn parse_bluez_objects(objects: &ManagedObjects) -> BluetoothSnapshot {
    let mut adapters = Vec::new();
    let mut devices = Vec::new();

    for (path, ifaces) in objects {
        let path_str = path.to_string();
        if let Some(props) = ifaces.get(BLUEZ_ADAPTER_IFACE) {
            adapters.push(BluetoothAdapter {
                object_path: path_str.clone(),
                address: dict_str(props, "Address").unwrap_or_default(),
                name: dict_str(props, "Name").unwrap_or_default(),
                alias: dict_str(props, "Alias").unwrap_or_default(),
                powered: dict_bool(props, "Powered").unwrap_or(false),
                discoverable: dict_bool(props, "Discoverable").unwrap_or(false),
                discoverable_timeout_secs: dict_u32(props, "DiscoverableTimeout").unwrap_or(180),
                pairable: dict_bool(props, "Pairable").unwrap_or(false),
                pairable_timeout_secs: dict_u32(props, "PairableTimeout").unwrap_or(0),
                discovering: dict_bool(props, "Discovering").unwrap_or(false),
            });
        }
        if let Some(props) = ifaces.get(BLUEZ_DEVICE_IFACE) {
            let battery_percentage = ifaces
                .get(BLUEZ_BATTERY_IFACE)
                .and_then(|bat| dict_u8(bat, "Percentage"));
            let appearance = match props.get("Appearance").map(|v| unwrap_value(v)) {
                Some(Value::U16(v)) => Some(*v),
                _ => None,
            };
            devices.push(BluetoothDevice {
                object_path: path_str,
                adapter_path: dict_str(props, "Adapter").unwrap_or_default(),
                address: dict_str(props, "Address").unwrap_or_default(),
                address_type: dict_str(props, "AddressType").unwrap_or_default(),
                name: dict_str(props, "Name").unwrap_or_default(),
                alias: dict_str(props, "Alias").unwrap_or_default(),
                icon: dict_str(props, "Icon").filter(|s| !s.is_empty()),
                class: dict_u32(props, "Class"),
                appearance,
                paired: dict_bool(props, "Paired").unwrap_or(false),
                bonded: dict_bool(props, "Bonded").unwrap_or(false),
                trusted: dict_bool(props, "Trusted").unwrap_or(false),
                connected: dict_bool(props, "Connected").unwrap_or(false),
                blocked: dict_bool(props, "Blocked").unwrap_or(false),
                legacy_pairing: dict_bool(props, "LegacyPairing").unwrap_or(false),
                services_resolved: dict_bool(props, "ServicesResolved").unwrap_or(false),
                rssi: dict_i16(props, "RSSI"),
                tx_power: dict_i16(props, "TxPower"),
                battery_percentage,
                uuids: dict_str_vec(props, "UUIDs"),
            });
        }
    }

    adapters.sort_by(|a, b| a.object_path.cmp(&b.object_path));
    devices.sort_by(|a, b| {
        b.connected
            .cmp(&a.connected)
            .then_with(|| b.paired.cmp(&a.paired))
            .then_with(|| b.rssi.unwrap_or(i16::MIN).cmp(&a.rssi.unwrap_or(i16::MIN)))
            .then_with(|| a.alias.cmp(&b.alias))
    });
    BluetoothSnapshot { adapters, devices }
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;
    use zbus::zvariant::{OwnedValue, Value};

    use super::*;

    #[test]
    fn parses_bluez_managed_objects_with_battery_and_extended_fields() {
        let mut objs: ManagedObjects = HashMap::new();
        let mut hci_ifaces = HashMap::new();
        let mut hci_props = HashMap::new();
        hci_props.insert(
            "Powered".into(),
            OwnedValue::try_from(Value::from(true)).unwrap(),
        );
        hci_props.insert(
            "Alias".into(),
            OwnedValue::try_from(Value::from("hci0")).unwrap(),
        );
        hci_props.insert(
            "DiscoverableTimeout".into(),
            OwnedValue::try_from(Value::from(120u32)).unwrap(),
        );
        hci_ifaces.insert(BLUEZ_ADAPTER_IFACE.to_owned(), hci_props);
        objs.insert(
            OwnedObjectPath::try_from("/org/bluez/hci0").unwrap(),
            hci_ifaces,
        );

        let mut dev_ifaces = HashMap::new();
        let mut dev_props = HashMap::new();
        dev_props.insert(
            "Address".into(),
            OwnedValue::try_from(Value::from("AA:BB:CC:DD:EE:FF")).unwrap(),
        );
        dev_props.insert(
            "AddressType".into(),
            OwnedValue::try_from(Value::from("public")).unwrap(),
        );
        dev_props.insert(
            "Alias".into(),
            OwnedValue::try_from(Value::from("WH-1000XM5")).unwrap(),
        );
        dev_props.insert(
            "Connected".into(),
            OwnedValue::try_from(Value::from(true)).unwrap(),
        );
        dev_props.insert(
            "Paired".into(),
            OwnedValue::try_from(Value::from(true)).unwrap(),
        );
        dev_props.insert(
            "Bonded".into(),
            OwnedValue::try_from(Value::from(true)).unwrap(),
        );
        dev_props.insert(
            "ServicesResolved".into(),
            OwnedValue::try_from(Value::from(true)).unwrap(),
        );
        dev_props.insert(
            "TxPower".into(),
            OwnedValue::try_from(Value::from(-4i16)).unwrap(),
        );
        let mut bat_props = HashMap::new();
        bat_props.insert(
            "Percentage".into(),
            OwnedValue::try_from(Value::from(75u8)).unwrap(),
        );
        dev_ifaces.insert(BLUEZ_DEVICE_IFACE.to_owned(), dev_props);
        dev_ifaces.insert(BLUEZ_BATTERY_IFACE.to_owned(), bat_props);
        objs.insert(
            OwnedObjectPath::try_from("/org/bluez/hci0/dev_AA_BB_CC_DD_EE_FF").unwrap(),
            dev_ifaces,
        );

        let snap = parse_bluez_objects(&objs);
        assert_eq!(snap.adapters.len(), 1);
        assert!(snap.adapters[0].powered);
        assert_eq!(snap.adapters[0].discoverable_timeout_secs, 120);
        assert_eq!(snap.devices.len(), 1);
        assert_eq!(snap.devices[0].alias, "WH-1000XM5");
        assert_eq!(snap.devices[0].address_type, "public");
        assert!(snap.devices[0].bonded);
        assert!(snap.devices[0].services_resolved);
        assert_eq!(snap.devices[0].tx_power, Some(-4));
        assert_eq!(snap.devices[0].battery_percentage, Some(75));
    }

    #[test]
    fn agent_reply_slot_wakes_immediately_on_resolve_and_cancel() {
        let pending: PendingAgentMap = Arc::new(Mutex::new(HashMap::new()));
        let slot = AgentReplySlot::new();
        pending.lock().unwrap().insert(1, Arc::clone(&slot));

        cancel_all_pending_slots(&pending);
        let res = futures_lite::future::block_on(AgentReplyFuture { slot });
        assert_eq!(res, Some(BluetoothAgentReply::Cancel));

        let client = DbusClient::new();
        let slot2 = AgentReplySlot::new();
        client
            .bluetooth_agent_pending
            .lock()
            .unwrap()
            .insert(7, Arc::clone(&slot2));
        client
            .bluetooth()
            .reply_agent(7, BluetoothAgentReply::Passkey(123_456))
            .unwrap();
        let res2 = futures_lite::future::block_on(AgentReplyFuture { slot: slot2 });
        assert_eq!(res2, Some(BluetoothAgentReply::Passkey(123_456)));
    }
}
