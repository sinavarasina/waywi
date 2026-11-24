use std::collections::{HashMap, VecDeque};
use std::future::Future;
use std::path::PathBuf;
use std::pin::Pin;
use std::sync::{Arc, Condvar, Mutex, RwLock};
use std::time::Duration;

use zbus::zvariant::{
    Array, Dict, ObjectPath, OwnedValue, Signature, Structure, StructureBuilder, Value,
};

use crate::arena::FrameSharedFn;
use crate::error::{Result, WaywiError};
use crate::subscription::{Subscription, SubscriptionSpec};

use super::bluetooth::{BluetoothClient, BluetoothSnapshot};
use super::idle_privacy::{GeoClueClient, ScreenSaverClient};
use super::logind::{LogindClient, LogindSnapshot};
use super::modem::{ModemClient, ModemSnapshot};
use super::mpris::{MprisClient, MprisSnapshot};
use super::network_manager::{NetworkManagerClient, NetworkManagerSnapshot};
use super::notifications::NotificationsClient;
use super::portal_settings::PortalClient;
use super::power_profiles::{PowerProfileSnapshot, PowerProfilesClient};
use super::tray::{TrayClient, TrayEvent, TrayItemSnapshot};
use super::types::{Dbus, DbusBus, DbusValue};
use super::udisks::{UDisksClient, UDisksSnapshot};
use super::upower::{UPowerClient, UPowerSnapshot};
use super::worker::AsyncStopSignal;

/// Shared async `zbus::Connection` slot reused across [`DbusClient`] and D-Bus observer workers.
pub(crate) type DbusSharedBus = Arc<Mutex<Option<zbus::Connection>>>;

pub(crate) fn connect_shared_bus(bus: DbusBus, shared: &DbusSharedBus) -> Result<zbus::Connection> {
    let mut guard = shared.lock().unwrap_or_else(|e| e.into_inner());
    if let Some(ref conn) = *guard {
        return Ok(conn.clone());
    }
    let conn = futures_lite::future::block_on(async move {
        match bus {
            DbusBus::Session => zbus::Connection::session().await,
            DbusBus::System => zbus::Connection::system().await,
        }
    })
    .map_err(|e| WaywiError::DbusError(format!("D-Bus connect failed: {e}")))?;
    *guard = Some(conn.clone());
    Ok(conn)
}

/// Validates a non-empty D-Bus bus name or destination string synchronously before scheduling.
pub(crate) fn validate_bus_name(name: &str, label: &str) -> Result<String> {
    let trimmed = name.trim();
    if trimmed.is_empty() {
        return Err(WaywiError::DbusError(format!("empty D-Bus {label}")));
    }
    Ok(trimmed.to_owned())
}

/// Validates a D-Bus object path synchronously before scheduling.
pub(crate) fn validate_object_path(path: &str, label: &str) -> Result<String> {
    let _ = ObjectPath::try_from(path)
        .map_err(|e| WaywiError::DbusError(format!("invalid {label} `{path}`: {e}")))?;
    Ok(path.to_owned())
}

const LANE_CAPACITY: usize = 64;
pub(crate) const DBUS_FAST_TIMEOUT: Duration = Duration::from_secs(4);
pub(crate) const DBUS_SLOW_TIMEOUT: Duration = Duration::from_secs(25);

/// Isolated execution lanes preventing slow radio paging or a hung service from blocking
/// unrelated desktop subsystems while preserving strict FIFO ordering within each subsystem.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum DbusActionLane {
    BluetoothAdapter,
    BluetoothDevice,
    NetworkManager,
    Mpris,
    Desktop,
}

impl DbusActionLane {
    const fn index(self) -> usize {
        match self {
            Self::BluetoothAdapter => 0,
            Self::BluetoothDevice => 1,
            Self::NetworkManager => 2,
            Self::Mpris => 3,
            Self::Desktop => 4,
        }
    }

    const fn thread_name(self) -> &'static str {
        match self {
            Self::BluetoothAdapter => "waywi-dbus-bt-adapter",
            Self::BluetoothDevice => "waywi-dbus-bt-dev",
            Self::NetworkManager => "waywi-dbus-nm-act",
            Self::Mpris => "waywi-dbus-mpris-act",
            Self::Desktop => "waywi-dbus-desktop-act",
        }
    }
}

/// Typed reply payload returned by a completed asynchronous [`DbusClient`] action.
#[derive(Debug, Clone, PartialEq, Default)]
pub enum DbusActionReply {
    #[default]
    Unit,
    ObjectPath(String),
    MountPath(PathBuf),
    Count(usize),
    Values(Vec<DbusValue>),
}

impl From<()> for DbusActionReply {
    fn from((): ()) -> Self {
        Self::Unit
    }
}

/// Asynchronous completion or error outcome emitted when a non-blocking [`DbusClient`] lane
/// action finishes.
#[derive(Debug, Clone, PartialEq)]
pub struct DbusActionResult {
    /// Subsystem lane that executed the action.
    pub lane: DbusActionLane,
    /// Target D-Bus bus (`Session` or `System`).
    pub bus: DbusBus,
    /// Canonical `"Interface.Method"` or operation name (e.g. `"Device1.Connect"`,
    /// `"NetworkManager.AddAndActivateConnection"`, `"Settings.ForgetWifiSsid"`).
    pub operation: String,
    /// Target D-Bus object path, bus name, or SSID associated with the action.
    pub target: String,
    /// Typed reply payload returned by the completed D-Bus action.
    pub reply: DbusActionReply,
    /// `None` when the D-Bus call succeeded within its timeout, or `Some(message)` when the
    /// D-Bus call failed or timed out at runtime.
    pub error: Option<String>,
}

impl DbusActionResult {
    /// Returns `true` if the background D-Bus action succeeded.
    #[must_use]
    pub fn is_ok(&self) -> bool {
        self.error.is_none()
    }

    /// Returns `true` if the background D-Bus action failed or timed out at runtime.
    #[must_use]
    pub fn is_err(&self) -> bool {
        self.error.is_some()
    }

    /// Converts this outcome into a [`Result<()>`].
    pub fn to_result(&self) -> Result<()> {
        match self.error {
            None => Ok(()),
            Some(ref msg) => Err(WaywiError::DbusError(msg.clone())),
        }
    }
}

impl Dbus {
    /// Subscribes to asynchronous completion and error outcomes ([`DbusActionResult`]) from all
    /// non-blocking [`DbusClient`] action lanes.
    pub fn action_results<M: 'static>(
        map: impl Fn(DbusActionResult) -> M + 'static,
    ) -> Subscription<M> {
        Subscription {
            spec: SubscriptionSpec::DbusActionResults {
                map: FrameSharedFn::new(map),
            },
        }
    }
}

#[derive(Default)]
struct ActionOutboxState {
    recent: VecDeque<DbusActionResult>,
    sender: Option<calloop::channel::Sender<DbusActionResult>>,
}

#[derive(Default)]
pub(crate) struct DbusActionOutbox {
    state: Mutex<ActionOutboxState>,
}

impl DbusActionOutbox {
    fn record(&self, result: DbusActionResult) {
        if let Ok(mut st) = self.state.lock() {
            if let Some(ref tx) = st.sender {
                let _ = tx.send(result);
            } else {
                if st.recent.len() >= LANE_CAPACITY {
                    st.recent.pop_front();
                }
                st.recent.push_back(result);
            }
        }
    }

    fn attach(&self, sender: calloop::channel::Sender<DbusActionResult>) {
        if let Ok(mut st) = self.state.lock() {
            for item in st.recent.drain(..) {
                let _ = sender.send(item);
            }
            st.sender = Some(sender);
        }
    }

    fn detach(&self) {
        if let Ok(mut st) = self.state.lock() {
            st.sender = None;
        }
    }

    fn clear(&self) {
        if let Ok(mut st) = self.state.lock() {
            st.recent.clear();
        }
    }
}

pub(crate) struct DbusActionResultWorker {
    outbox: Arc<DbusActionOutbox>,
}

impl DbusActionResultWorker {
    pub(crate) fn spawn(
        sender: calloop::channel::Sender<DbusActionResult>,
        outbox: Arc<DbusActionOutbox>,
    ) -> Self {
        outbox.attach(sender);
        Self { outbox }
    }
}

impl Drop for DbusActionResultWorker {
    fn drop(&mut self) {
        self.outbox.detach();
    }
}

type BoxedDbusFuture = Pin<Box<dyn Future<Output = Result<DbusActionReply>> + Send + 'static>>;
type BoxedDbusJob = Box<dyn FnOnce(zbus::Connection) -> BoxedDbusFuture + Send + 'static>;

/// Metadata describing a single queued D-Bus lane action.
pub(crate) struct LaneActionSpec {
    pub(crate) lane: DbusActionLane,
    pub(crate) bus: DbusBus,
    pub(crate) operation: String,
    pub(crate) target: String,
    pub(crate) dedup_key: Option<String>,
    pub(crate) timeout: Duration,
}

impl LaneActionSpec {
    pub(crate) fn fast(
        lane: DbusActionLane,
        bus: DbusBus,
        operation: impl Into<String>,
        target: impl Into<String>,
    ) -> Self {
        Self {
            lane,
            bus,
            operation: operation.into(),
            target: target.into(),
            dedup_key: None,
            timeout: DBUS_FAST_TIMEOUT,
        }
    }

    pub(crate) fn with_dedup(mut self, dedup_key: impl Into<String>) -> Self {
        self.dedup_key = Some(dedup_key.into());
        self
    }

    pub(crate) fn with_timeout(mut self, timeout: Duration) -> Self {
        self.timeout = timeout;
        self
    }
}

struct LaneJob {
    bus: DbusBus,
    operation: String,
    target: String,
    dedup_key: Option<String>,
    timeout: Duration,
    job: BoxedDbusJob,
}

struct LaneState {
    queue: VecDeque<LaneJob>,
    in_flight_key: Option<String>,
    started: bool,
    stopped: bool,
}

struct LaneWorker {
    lane: DbusActionLane,
    state: Arc<(Mutex<LaneState>, Condvar)>,
    stop: AsyncStopSignal,
    timer: Arc<SharedDbusTimer>,
    outbox: Arc<DbusActionOutbox>,
}

impl LaneWorker {
    fn new(
        lane: DbusActionLane,
        timer: Arc<SharedDbusTimer>,
        outbox: Arc<DbusActionOutbox>,
    ) -> Self {
        Self {
            lane,
            state: Arc::new((
                Mutex::new(LaneState {
                    queue: VecDeque::new(),
                    in_flight_key: None,
                    started: false,
                    stopped: false,
                }),
                Condvar::new(),
            )),
            stop: AsyncStopSignal::new(),
            timer,
            outbox,
        }
    }

    fn submit(
        &self,
        spec: LaneActionSpec,
        shared_session: &DbusSharedBus,
        shared_system: &DbusSharedBus,
        job: BoxedDbusJob,
    ) -> Result<()> {
        let (lock, cvar) = &*self.state;
        let mut st = lock
            .lock()
            .map_err(|_| WaywiError::DbusError("D-Bus action lane lock poisoned".into()))?;
        if st.stopped {
            return Err(WaywiError::Shutdown);
        }
        if let Some(ref key) = spec.dedup_key
            && (st.in_flight_key.as_deref() == Some(key.as_str())
                || st
                    .queue
                    .iter()
                    .any(|item| item.dedup_key.as_deref() == Some(key.as_str())))
        {
            return Ok(());
        }
        if st.queue.len() >= LANE_CAPACITY {
            return Err(WaywiError::DbusError(format!(
                "D-Bus action queue full on lane `{}` (capacity {LANE_CAPACITY})",
                self.lane.thread_name()
            )));
        }
        let need_spawn = !st.started;
        st.started = true;
        st.queue.push_back(LaneJob {
            bus: spec.bus,
            operation: spec.operation,
            target: spec.target,
            dedup_key: spec.dedup_key,
            timeout: spec.timeout,
            job,
        });
        cvar.notify_one();
        drop(st);

        if need_spawn {
            let lane = self.lane;
            let state_cl = Arc::clone(&self.state);
            let stop_cl = self.stop.clone();
            let timer_cl = Arc::clone(&self.timer);
            let outbox_cl = Arc::clone(&self.outbox);
            let sess_cl = Arc::clone(shared_session);
            let sys_cl = Arc::clone(shared_system);
            let tname = self.lane.thread_name();
            std::thread::Builder::new()
                .name(tname.to_owned())
                .spawn(move || {
                    run_lane_thread(
                        lane, state_cl, stop_cl, timer_cl, outbox_cl, sess_cl, sys_cl,
                    )
                })
                .map_err(|e| WaywiError::DbusError(format!("failed to spawn `{tname}`: {e}")))?;
        }
        Ok(())
    }

    fn shutdown(&self) {
        self.stop.trigger();
        let (lock, cvar) = &*self.state;
        if let Ok(mut st) = lock.lock() {
            st.stopped = true;
            st.queue.clear();
            cvar.notify_all();
        }
    }
}

fn run_lane_thread(
    lane: DbusActionLane,
    state: Arc<(Mutex<LaneState>, Condvar)>,
    stop: AsyncStopSignal,
    timer: Arc<SharedDbusTimer>,
    outbox: Arc<DbusActionOutbox>,
    sess: DbusSharedBus,
    sys: DbusSharedBus,
) {
    let (lock, cvar) = &*state;
    loop {
        let item = {
            let mut st = lock.lock().unwrap_or_else(|e| e.into_inner());
            st.in_flight_key = None;
            while st.queue.is_empty() && !st.stopped {
                st = cvar.wait(st).unwrap_or_else(|e| e.into_inner());
            }
            if st.stopped {
                return;
            }
            let Some(next) = st.queue.pop_front() else {
                continue;
            };
            st.in_flight_key = next.dedup_key.clone();
            next
        };

        let slot = match item.bus {
            DbusBus::Session => &sess,
            DbusBus::System => &sys,
        };
        let outcome = match connect_shared_bus(item.bus, slot) {
            Ok(conn) => {
                let fut = (item.job)(conn);
                run_with_timeout_or_stop(fut, item.timeout, &stop, &timer)
            }
            Err(err) => LaneExecOutcome::Finished(Err(err)),
        };
        let result = match outcome {
            LaneExecOutcome::Stopped => return,
            LaneExecOutcome::TimedOut => {
                let err_msg = format!(
                    "{} on `{}` timed out after {}s",
                    item.operation,
                    item.target,
                    item.timeout.as_secs()
                );
                DbusActionResult {
                    lane,
                    bus: item.bus,
                    operation: item.operation,
                    target: item.target,
                    reply: DbusActionReply::Unit,
                    error: Some(err_msg),
                }
            }
            LaneExecOutcome::Finished(Ok(reply)) => DbusActionResult {
                lane,
                bus: item.bus,
                operation: item.operation,
                target: item.target,
                reply,
                error: None,
            },
            LaneExecOutcome::Finished(Err(err)) => DbusActionResult {
                lane,
                bus: item.bus,
                operation: item.operation,
                target: item.target,
                reply: DbusActionReply::Unit,
                error: Some(err.to_string()),
            },
        };
        outbox.record(result);
    }
}

struct TimerEntry {
    deadline: std::time::Instant,
    id: u64,
    signal: AsyncStopSignal,
}

struct SharedTimerState {
    entries: Vec<TimerEntry>,
    next_id: u64,
    started: bool,
    stopped: bool,
}

/// Single shared timeout coordinator thread across all D-Bus action lanes so individual actions
/// never spawn temporary timer threads.
struct SharedDbusTimer {
    shared: Arc<(Mutex<SharedTimerState>, Condvar)>,
}

impl SharedDbusTimer {
    fn new() -> Self {
        Self {
            shared: Arc::new((
                Mutex::new(SharedTimerState {
                    entries: Vec::new(),
                    next_id: 1,
                    started: false,
                    stopped: false,
                }),
                Condvar::new(),
            )),
        }
    }

    fn register(&self, timeout: Duration) -> (u64, AsyncStopSignal) {
        let sig = AsyncStopSignal::new();
        let deadline = std::time::Instant::now() + timeout;
        let (lock, cvar) = &*self.shared;
        let mut st = lock.lock().unwrap_or_else(|e| e.into_inner());
        let id = st.next_id;
        st.next_id = st.next_id.wrapping_add(1);
        let need_spawn = !st.started && !st.stopped;
        if need_spawn {
            st.started = true;
        }
        st.entries.push(TimerEntry {
            deadline,
            id,
            signal: sig.clone(),
        });
        cvar.notify_one();
        drop(st);

        if need_spawn {
            let shared_cl = Arc::clone(&self.shared);
            let _ = std::thread::Builder::new()
                .name("waywi-dbus-timer".to_owned())
                .spawn(move || run_shared_timer_thread(shared_cl));
        }
        (id, sig)
    }

    fn cancel(&self, id: u64) {
        let (lock, cvar) = &*self.shared;
        if let Ok(mut st) = lock.lock() {
            let prev_len = st.entries.len();
            st.entries.retain(|e| e.id != id);
            if st.entries.len() != prev_len {
                cvar.notify_one();
            }
        }
    }

    fn shutdown(&self) {
        let (lock, cvar) = &*self.shared;
        if let Ok(mut st) = lock.lock() {
            st.stopped = true;
            st.entries.clear();
            cvar.notify_all();
        }
    }
}

fn run_shared_timer_thread(shared: Arc<(Mutex<SharedTimerState>, Condvar)>) {
    let (lock, cvar) = &*shared;
    loop {
        let mut st = lock.lock().unwrap_or_else(|e| e.into_inner());
        if st.stopped {
            return;
        }
        let now = std::time::Instant::now();
        let mut idx = 0;
        while idx < st.entries.len() {
            if st.entries[idx].deadline <= now {
                let expired = st.entries.swap_remove(idx);
                expired.signal.trigger();
            } else {
                idx += 1;
            }
        }
        if let Some(earliest) = st.entries.iter().map(|e| e.deadline).min() {
            let wait_dur = earliest.saturating_duration_since(now);
            let (st_after, _) = cvar
                .wait_timeout(st, wait_dur)
                .unwrap_or_else(|e| e.into_inner());
            drop(st_after);
        } else {
            st = cvar.wait(st).unwrap_or_else(|e| e.into_inner());
            drop(st);
        }
    }
}

enum LaneExecOutcome {
    Finished(Result<DbusActionReply>),
    TimedOut,
    Stopped,
}

fn run_with_timeout_or_stop(
    fut: BoxedDbusFuture,
    timeout: Duration,
    stop: &AsyncStopSignal,
    timer: &SharedDbusTimer,
) -> LaneExecOutcome {
    let (timer_id, timeout_sig) = timer.register(timeout);
    let outcome = futures_lite::future::block_on(async {
        let abort = futures_lite::future::or(
            async {
                stop.wait_stopped().await;
                LaneExecOutcome::Stopped
            },
            async {
                timeout_sig.wait_stopped().await;
                LaneExecOutcome::TimedOut
            },
        );
        futures_lite::future::or(async { LaneExecOutcome::Finished(fut.await) }, abort).await
    });
    timer.cancel(timer_id);
    outcome
}

/// Pool of 5 bounded, per-domain D-Bus action lanes sharing a single timeout coordinator.
pub(crate) struct DbusActionPool {
    lanes: [LaneWorker; 5],
    timer: Arc<SharedDbusTimer>,
    outbox: Arc<DbusActionOutbox>,
    shared_session: DbusSharedBus,
    shared_system: DbusSharedBus,
}

impl DbusActionPool {
    fn new(shared_session: DbusSharedBus, shared_system: DbusSharedBus) -> Self {
        let timer = Arc::new(SharedDbusTimer::new());
        let outbox = Arc::new(DbusActionOutbox::default());
        Self {
            lanes: [
                LaneWorker::new(
                    DbusActionLane::BluetoothAdapter,
                    Arc::clone(&timer),
                    Arc::clone(&outbox),
                ),
                LaneWorker::new(
                    DbusActionLane::BluetoothDevice,
                    Arc::clone(&timer),
                    Arc::clone(&outbox),
                ),
                LaneWorker::new(
                    DbusActionLane::NetworkManager,
                    Arc::clone(&timer),
                    Arc::clone(&outbox),
                ),
                LaneWorker::new(
                    DbusActionLane::Mpris,
                    Arc::clone(&timer),
                    Arc::clone(&outbox),
                ),
                LaneWorker::new(
                    DbusActionLane::Desktop,
                    Arc::clone(&timer),
                    Arc::clone(&outbox),
                ),
            ],
            timer,
            outbox,
            shared_session,
            shared_system,
        }
    }

    pub(crate) fn submit<P, F, Fut>(&self, spec: LaneActionSpec, f: F) -> Result<()>
    where
        P: Into<DbusActionReply> + Send + 'static,
        F: FnOnce(zbus::Connection) -> Fut + Send + 'static,
        Fut: Future<Output = Result<P>> + Send + 'static,
    {
        self.lanes[spec.lane.index()].submit(
            spec,
            &self.shared_session,
            &self.shared_system,
            Box::new(move |conn| Box::pin(async move { f(conn).await.map(Into::into) })),
        )
    }
}

impl Drop for DbusActionPool {
    fn drop(&mut self) {
        self.timer.shutdown();
        for lane in &self.lanes {
            lane.shutdown();
        }
    }
}

/// Shared in-memory cache of the latest snapshots observed by D-Bus background workers.
#[derive(Default)]
pub(crate) struct DbusSnapshotCache {
    pub(crate) mpris: RwLock<Option<Vec<MprisSnapshot>>>,
    pub(crate) bluetooth: RwLock<Option<BluetoothSnapshot>>,
    pub(crate) network_manager: RwLock<Option<NetworkManagerSnapshot>>,
    pub(crate) upower: RwLock<Option<UPowerSnapshot>>,
    pub(crate) udisks: RwLock<Option<UDisksSnapshot>>,
    pub(crate) power_profiles: RwLock<Option<PowerProfileSnapshot>>,
    pub(crate) logind: RwLock<Option<LogindSnapshot>>,
    pub(crate) modem: RwLock<Option<ModemSnapshot>>,
    pub(crate) tray: RwLock<Option<Vec<TrayItemSnapshot>>>,
}

impl DbusSnapshotCache {
    pub(crate) fn clear(&self) {
        if let Ok(mut g) = self.mpris.write() {
            *g = None;
        }
        if let Ok(mut g) = self.bluetooth.write() {
            *g = None;
        }
        if let Ok(mut g) = self.network_manager.write() {
            *g = None;
        }
        if let Ok(mut g) = self.upower.write() {
            *g = None;
        }
        if let Ok(mut g) = self.udisks.write() {
            *g = None;
        }
        if let Ok(mut g) = self.power_profiles.write() {
            *g = None;
        }
        if let Ok(mut g) = self.logind.write() {
            *g = None;
        }
        if let Ok(mut g) = self.modem.write() {
            *g = None;
        }
        if let Ok(mut g) = self.tray.write() {
            *g = None;
        }
    }

    pub(crate) fn apply_tray_event(&self, event: &TrayEvent) {
        let Ok(mut guard) = self.tray.write() else {
            return;
        };
        match event {
            TrayEvent::WatcherReady { .. } => {
                if guard.is_none() {
                    *guard = Some(Vec::new());
                }
            }
            TrayEvent::ItemRegistered(item) | TrayEvent::ItemUpdated(item) => {
                let items = guard.get_or_insert_with(Vec::new);
                if let Some(existing) = items.iter_mut().find(|x| x.service == item.service) {
                    *existing = item.clone();
                } else {
                    items.push(item.clone());
                    items.sort_by(|a, b| a.service.cmp(&b.service));
                }
            }
            TrayEvent::ItemUnregistered { service } => {
                if let Some(items) = guard.as_mut() {
                    items.retain(|x| x.service != *service);
                }
            }
            TrayEvent::Error(_) => {}
        }
    }
}

/// D-Bus client backed by async `zbus::Connection` (bounded per-domain action lanes + shared
/// snapshot cache + dedicated async-io subscription workers).
pub struct DbusClient {
    shared_session: DbusSharedBus,
    shared_system: DbusSharedBus,
    action_pool: DbusActionPool,
    pub(crate) action_outbox: Arc<DbusActionOutbox>,
    pub(crate) cache: Arc<DbusSnapshotCache>,
    pub(crate) notifications_state: Arc<super::notifications::NotificationServerState>,
    pub(crate) bluetooth_agent_pending: super::bluetooth::PendingAgentMap,
    cache_snapshots: bool,
    idle_disconnect: bool,
}

impl Default for DbusClient {
    fn default() -> Self {
        Self::new()
    }
}

impl DbusClient {
    #[must_use]
    pub fn new() -> Self {
        Self::with_config(true, true)
    }

    #[must_use]
    pub(crate) fn with_config(cache_snapshots: bool, idle_disconnect: bool) -> Self {
        let shared_session = Arc::new(Mutex::new(None));
        let shared_system = Arc::new(Mutex::new(None));
        let action_pool =
            DbusActionPool::new(Arc::clone(&shared_session), Arc::clone(&shared_system));
        let action_outbox = Arc::clone(&action_pool.outbox);
        Self {
            shared_session,
            shared_system,
            action_pool,
            action_outbox,
            cache: Arc::new(DbusSnapshotCache::default()),
            notifications_state: Arc::new(super::notifications::NotificationServerState::default()),
            bluetooth_agent_pending: Arc::new(Mutex::new(HashMap::new())),
            cache_snapshots,
            idle_disconnect,
        }
    }

    #[must_use]
    pub(crate) fn cache_enabled(&self) -> bool {
        self.cache_snapshots
    }

    /// Clears cached D-Bus snapshots and buffered action results, and drops cached shared bus
    /// connections when idle disconnect is enabled so unused connections close once no worker
    /// holds a clone.
    pub fn trim_cache(&self) {
        self.cache.clear();
        self.action_outbox.clear();
        if self.idle_disconnect {
            if let Ok(mut g) = self.shared_session.lock() {
                *g = None;
            }
            if let Ok(mut g) = self.shared_system.lock() {
                *g = None;
            }
        }
    }

    pub(crate) fn shared_bus(&self, bus: DbusBus) -> DbusSharedBus {
        match bus {
            DbusBus::Session => Arc::clone(&self.shared_session),
            DbusBus::System => Arc::clone(&self.shared_system),
        }
    }

    pub(super) fn async_connection(&self, bus: DbusBus) -> Result<zbus::Connection> {
        let slot = match bus {
            DbusBus::Session => &self.shared_session,
            DbusBus::System => &self.shared_system,
        };
        connect_shared_bus(bus, slot)
    }

    /// Schedules a bounded, timeout-protected asynchronous D-Bus action described by `spec`.
    pub(crate) fn submit_lane_action<P, F, Fut>(&self, spec: LaneActionSpec, f: F) -> Result<()>
    where
        P: Into<DbusActionReply> + Send + 'static,
        F: FnOnce(zbus::Connection) -> Fut + Send + 'static,
        Fut: Future<Output = Result<P>> + Send + 'static,
    {
        self.action_pool.submit(spec, f)
    }

    // ---- Zero-cost Copy domain sub-handles ----

    /// Returns the MPRIS media player domain handle (`org.mpris.MediaPlayer2`).
    #[inline]
    #[must_use]
    pub fn mpris(&self) -> MprisClient<'_> {
        MprisClient(self)
    }

    /// Returns the NetworkManager domain handle (`org.freedesktop.NetworkManager`).
    #[inline]
    #[must_use]
    pub fn nm(&self) -> NetworkManagerClient<'_> {
        NetworkManagerClient(self)
    }

    /// Returns the BlueZ Bluetooth domain handle (`org.bluez`).
    #[inline]
    #[must_use]
    pub fn bluetooth(&self) -> BluetoothClient<'_> {
        BluetoothClient(self)
    }

    /// Returns the UDisks2 storage domain handle (`org.freedesktop.UDisks2`).
    #[inline]
    #[must_use]
    pub fn udisks(&self) -> UDisksClient<'_> {
        UDisksClient(self)
    }

    /// Returns the systemd-logind session/power domain handle (`org.freedesktop.login1`).
    #[inline]
    #[must_use]
    pub fn logind(&self) -> LogindClient<'_> {
        LogindClient(self)
    }

    /// Returns the StatusNotifier system tray domain handle.
    #[inline]
    #[must_use]
    pub fn tray(&self) -> TrayClient<'_> {
        TrayClient(self)
    }

    /// Returns the XDG Desktop Notifications server domain handle (`org.freedesktop.Notifications`).
    #[inline]
    #[must_use]
    pub fn notifications(&self) -> NotificationsClient<'_> {
        NotificationsClient(self)
    }

    /// Returns the power-profiles-daemon domain handle.
    #[inline]
    #[must_use]
    pub fn power_profiles(&self) -> PowerProfilesClient<'_> {
        PowerProfilesClient(self)
    }

    /// Returns the UPower battery/peripheral domain handle (`org.freedesktop.UPower`).
    #[inline]
    #[must_use]
    pub fn upower(&self) -> UPowerClient<'_> {
        UPowerClient(self)
    }

    /// Returns the ModemManager cellular modem domain handle (`org.freedesktop.ModemManager1`).
    #[inline]
    #[must_use]
    pub fn modem(&self) -> ModemClient<'_> {
        ModemClient(self)
    }

    /// Returns the XDG Desktop Portal settings/capabilities domain handle.
    #[inline]
    #[must_use]
    pub fn portal(&self) -> PortalClient<'_> {
        PortalClient(self)
    }

    /// Returns the `org.freedesktop.ScreenSaver` domain handle.
    #[inline]
    #[must_use]
    pub fn screensaver(&self) -> ScreenSaverClient<'_> {
        ScreenSaverClient(self)
    }

    /// Returns the `org.freedesktop.GeoClue2` domain handle.
    #[inline]
    #[must_use]
    pub fn geoclue(&self) -> GeoClueClient<'_> {
        GeoClueClient(self)
    }

    // ---- Core raw D-Bus operations ----

    /// Reads a D-Bus property synchronously via `org.freedesktop.DBus.Properties.Get`.
    pub fn get_property(
        &self,
        bus: DbusBus,
        destination: &str,
        path: &str,
        interface: &str,
        property: &str,
    ) -> Result<DbusValue> {
        let dest = validate_bus_name(destination, "destination")?;
        let path_owned = validate_object_path(path, "object path")?;
        let iface = validate_bus_name(interface, "interface")?;
        let prop = validate_bus_name(property, "property")?;
        let conn = self.async_connection(bus)?;
        futures_lite::future::block_on(async move {
            let reply = conn
                .call_method(
                    Some(dest.as_str()),
                    path_owned.as_str(),
                    Some("org.freedesktop.DBus.Properties"),
                    "Get",
                    &(iface.as_str(), prop.as_str()),
                )
                .await
                .map_err(|e| WaywiError::DbusError(format!("property read failed: {e}")))?;
            let val: OwnedValue = reply
                .body()
                .deserialize()
                .map_err(|e| WaywiError::DbusError(e.to_string()))?;
            Ok(owned_value_to_dbus(&val))
        })
    }

    /// Enqueues a D-Bus property write via `org.freedesktop.DBus.Properties.Set` using a typed
    /// [`DbusValue`].
    pub fn set_property(
        &self,
        bus: DbusBus,
        destination: &str,
        path: &str,
        interface: &str,
        property: &str,
        value: &DbusValue,
    ) -> Result<()> {
        let owned = dbus_value_to_owned(value.unwrap_variant())?;
        self.set_property_zvariant(
            bus,
            destination,
            path,
            interface,
            property,
            Value::from(owned),
        )
    }

    /// Enqueues a generic D-Bus method call with typed [`DbusValue`] arguments (including empty
    /// `&[]` for zero-argument methods) on the appropriate domain lane.
    ///
    /// Arguments are validated and encoded synchronously before enqueuing so invalid values fail
    /// immediately. Decoded reply arguments are delivered asynchronously via
    /// [`DbusActionResult::reply`] on [`Dbus::action_results`].
    pub fn call_method(
        &self,
        bus: DbusBus,
        destination: &str,
        path: &str,
        interface: &str,
        method: &str,
        args: &[DbusValue],
    ) -> Result<()> {
        let lane = classify_target_lane(destination, interface);
        let dest = validate_bus_name(destination, "destination")?;
        let path_owned = validate_object_path(path, "object path")?;
        let iface = validate_bus_name(interface, "interface")?;
        let method_owned = validate_bus_name(method, "method")?;
        let encoded_args = encode_dbus_args(args)?;
        let op = format!("{iface}.{method_owned}");
        let target = path_owned.clone();
        self.submit_lane_action(
            LaneActionSpec::fast(lane, bus, op, target),
            move |conn| async move {
                let msg = if encoded_args.is_empty() {
                    conn.call_method(
                        Some(dest.as_str()),
                        path_owned.as_str(),
                        Some(iface.as_str()),
                        method_owned.as_str(),
                        &(),
                    )
                    .await
                } else {
                    let body = args_to_structure(encoded_args)?;
                    conn.call_method(
                        Some(dest.as_str()),
                        path_owned.as_str(),
                        Some(iface.as_str()),
                        method_owned.as_str(),
                        &body,
                    )
                    .await
                }
                .map_err(|e| {
                    WaywiError::DbusError(format!("{iface}.{method_owned} failed: {e}"))
                })?;
                let values = decode_message_body(&msg);
                if values.is_empty() {
                    Ok(DbusActionReply::Unit)
                } else {
                    Ok(DbusActionReply::Values(values))
                }
            },
        )
    }

    /// Invokes a generic D-Bus method synchronously with typed [`DbusValue`] arguments and returns
    /// all decoded reply body values (`vec![]` when the method returns no values).
    pub fn call_method_blocking(
        &self,
        bus: DbusBus,
        destination: &str,
        path: &str,
        interface: &str,
        method: &str,
        args: &[DbusValue],
    ) -> Result<Vec<DbusValue>> {
        let dest = validate_bus_name(destination, "destination")?;
        let path_owned = validate_object_path(path, "object path")?;
        let iface = validate_bus_name(interface, "interface")?;
        let method_owned = validate_bus_name(method, "method")?;
        let encoded_args = encode_dbus_args(args)?;
        let conn = self.async_connection(bus)?;
        futures_lite::future::block_on(async move {
            let msg = if encoded_args.is_empty() {
                conn.call_method(
                    Some(dest.as_str()),
                    path_owned.as_str(),
                    Some(iface.as_str()),
                    method_owned.as_str(),
                    &(),
                )
                .await
            } else {
                let body = args_to_structure(encoded_args)?;
                conn.call_method(
                    Some(dest.as_str()),
                    path_owned.as_str(),
                    Some(iface.as_str()),
                    method_owned.as_str(),
                    &body,
                )
                .await
            }
            .map_err(|e| WaywiError::DbusError(format!("{iface}.{method_owned} failed: {e}")))?;
            Ok(decode_message_body(&msg))
        })
    }

    pub(crate) fn call_lane_method_unit(
        &self,
        lane: DbusActionLane,
        bus: DbusBus,
        destination: &str,
        path: &str,
        interface: &str,
        method: &str,
    ) -> Result<()> {
        let dest = validate_bus_name(destination, "destination")?;
        let path_owned = validate_object_path(path, "object path")?;
        let iface = validate_bus_name(interface, "interface")?;
        let method_owned = validate_bus_name(method, "method")?;
        let op = format!("{iface}.{method_owned}");
        let target = path_owned.clone();
        self.submit_lane_action(
            LaneActionSpec::fast(lane, bus, op, target),
            move |conn| async move {
                conn.call_method(
                    Some(dest.as_str()),
                    path_owned.as_str(),
                    Some(iface.as_str()),
                    method_owned.as_str(),
                    &(),
                )
                .await
                .map_err(|e| {
                    WaywiError::DbusError(format!("{iface}.{method_owned} failed: {e}"))
                })?;
                Ok(())
            },
        )
    }

    /// Enqueues a D-Bus property write via `org.freedesktop.DBus.Properties.Set` on the
    /// appropriate domain lane using a raw `zvariant::Value`.
    pub(super) fn set_property_zvariant(
        &self,
        bus: DbusBus,
        destination: &str,
        path: &str,
        interface: &str,
        property: &str,
        value: Value<'_>,
    ) -> Result<()> {
        let lane = classify_target_lane(destination, interface);
        let dest = validate_bus_name(destination, "destination")?;
        let path_owned = validate_object_path(path, "object path")?;
        let iface = validate_bus_name(interface, "interface")?;
        let prop = validate_bus_name(property, "property")?;
        let owned_val = OwnedValue::try_from(value).map_err(|e| {
            WaywiError::DbusError(format!("property `{property}` encode failed: {e}"))
        })?;
        let op = format!("{iface}.Set({prop})");
        let target = path_owned.clone();
        self.submit_lane_action(
            LaneActionSpec::fast(lane, bus, op, target),
            move |conn| async move {
                conn.call_method(
                    Some(dest.as_str()),
                    path_owned.as_str(),
                    Some("org.freedesktop.DBus.Properties"),
                    "Set",
                    &(iface.as_str(), prop.as_str(), Value::from(owned_val)),
                )
                .await
                .map_err(|e| {
                    WaywiError::DbusError(format!("property `{prop}` write failed: {e}"))
                })?;
                Ok(())
            },
        )
    }
}

fn classify_target_lane(destination: &str, interface: &str) -> DbusActionLane {
    if destination == "org.bluez" || interface.starts_with("org.bluez.") {
        if interface == "org.bluez.Device1" {
            DbusActionLane::BluetoothDevice
        } else {
            DbusActionLane::BluetoothAdapter
        }
    } else if destination == "org.freedesktop.NetworkManager"
        || interface.starts_with("org.freedesktop.NetworkManager")
    {
        DbusActionLane::NetworkManager
    } else if destination.starts_with("org.mpris.MediaPlayer2")
        || interface.starts_with("org.mpris.MediaPlayer2")
    {
        DbusActionLane::Mpris
    } else {
        DbusActionLane::Desktop
    }
}

pub(super) fn owned_value_to_dbus(val: &OwnedValue) -> DbusValue {
    value_to_dbus(val)
}

pub(super) fn value_to_dbus(val: &Value<'_>) -> DbusValue {
    match val {
        Value::Str(s) => DbusValue::String(s.to_string()),
        Value::ObjectPath(p) => DbusValue::ObjectPath(p.to_string()),
        Value::Bool(b) => DbusValue::Bool(*b),
        Value::I16(v) => DbusValue::I64(i64::from(*v)),
        Value::I32(v) => DbusValue::I32(*v),
        Value::I64(v) => DbusValue::I64(*v),
        Value::U8(v) => DbusValue::U64(u64::from(*v)),
        Value::U16(v) => DbusValue::U64(u64::from(*v)),
        Value::U32(v) => DbusValue::U32(*v),
        Value::U64(v) => DbusValue::U64(*v),
        Value::F64(v) => DbusValue::F64(*v),
        Value::Array(arr) => DbusValue::Array(arr.iter().map(value_to_dbus).collect()),
        Value::Structure(st) => DbusValue::Struct(st.fields().iter().map(value_to_dbus).collect()),
        Value::Dict(dict) => {
            let mut map = HashMap::new();
            for (k, v) in dict.iter() {
                if let Value::Str(ks) = k {
                    map.insert(ks.to_string(), value_to_dbus(v));
                }
            }
            DbusValue::Dict(map)
        }
        Value::Value(inner) => value_to_dbus(inner),
        other => DbusValue::String(format!("{other:?}")),
    }
}

pub(super) fn decode_message_body(msg: &zbus::Message) -> Vec<DbusValue> {
    if matches!(msg.header().signature(), Signature::Unit) {
        return Vec::new();
    }
    msg.body()
        .deserialize::<Structure<'_>>()
        .ok()
        .map(|st| st.fields().iter().map(value_to_dbus).collect())
        .unwrap_or_default()
}

fn encode_dbus_args(args: &[DbusValue]) -> Result<Vec<OwnedValue>> {
    args.iter().map(dbus_value_to_owned).collect()
}

fn args_to_structure(args: Vec<OwnedValue>) -> Result<Structure<'static>> {
    let mut builder = StructureBuilder::new();
    for arg in args {
        builder.push_value(Value::from(arg));
    }
    builder
        .build()
        .map_err(|e| WaywiError::DbusError(format!("failed to build D-Bus argument body: {e}")))
}

fn dbus_value_to_owned(val: &DbusValue) -> Result<OwnedValue> {
    let zv = dbus_value_to_zvariant(val)?;
    OwnedValue::try_from(zv)
        .map_err(|e| WaywiError::DbusError(format!("failed to encode D-Bus value: {e}")))
}

fn dbus_value_to_zvariant(val: &DbusValue) -> Result<Value<'static>> {
    match val {
        DbusValue::String(s) => Ok(Value::from(s.clone())),
        DbusValue::ObjectPath(p) => {
            let path = ObjectPath::try_from(p.clone()).map_err(|e| {
                WaywiError::DbusError(format!("invalid D-Bus object path `{p}`: {e}"))
            })?;
            Ok(Value::from(path))
        }
        DbusValue::Bool(b) => Ok(Value::from(*b)),
        DbusValue::I32(v) => Ok(Value::from(*v)),
        DbusValue::U32(v) => Ok(Value::from(*v)),
        DbusValue::I64(v) => Ok(Value::from(*v)),
        DbusValue::U64(v) => Ok(Value::from(*v)),
        DbusValue::F64(v) => Ok(Value::from(*v)),
        DbusValue::Variant(inner) => {
            let encoded = dbus_value_to_zvariant(inner)?;
            Ok(Value::Value(Box::new(encoded)))
        }
        DbusValue::Struct(fields) => {
            if fields.is_empty() {
                return Err(WaywiError::DbusError(
                    "D-Bus struct must contain at least one field".to_owned(),
                ));
            }
            let mut builder = StructureBuilder::new();
            for field in fields {
                builder.push_value(dbus_value_to_zvariant(field)?);
            }
            let structure = builder.build().map_err(|e| {
                WaywiError::DbusError(format!("failed to encode D-Bus struct: {e}"))
            })?;
            Ok(Value::Structure(structure))
        }
        DbusValue::Array(items) => {
            if items.is_empty() {
                Ok(Value::Array(Array::new(&Signature::Variant)))
            } else {
                let mut converted = Vec::with_capacity(items.len());
                for item in items {
                    converted.push(dbus_value_to_zvariant(item)?);
                }
                let first_sig = converted[0].value_signature().clone();
                let homogeneous = converted
                    .iter()
                    .skip(1)
                    .all(|v| v.value_signature() == &first_sig);
                if homogeneous {
                    let mut arr = Array::new(&first_sig);
                    for item in converted {
                        arr.append(item).map_err(|e| {
                            WaywiError::DbusError(format!("failed to encode D-Bus array: {e}"))
                        })?;
                    }
                    Ok(Value::Array(arr))
                } else {
                    let mut arr = Array::new(&Signature::Variant);
                    for item in converted {
                        let wrapped = match item {
                            Value::Value(_) => item,
                            other => Value::Value(Box::new(other)),
                        };
                        arr.append(wrapped).map_err(|e| {
                            WaywiError::DbusError(format!(
                                "failed to encode D-Bus variant array: {e}"
                            ))
                        })?;
                    }
                    Ok(Value::Array(arr))
                }
            }
        }
        DbusValue::Dict(map) => {
            let mut dict = Dict::new(&Signature::Str, &Signature::Variant);
            for (k, v) in map {
                let inner = dbus_value_to_zvariant(v)?;
                let wrapped = match inner {
                    Value::Value(_) => inner,
                    other => Value::Value(Box::new(other)),
                };
                dict.append(Value::from(k.clone()), wrapped).map_err(|e| {
                    WaywiError::DbusError(format!("failed to encode D-Bus dict entry `{k}`: {e}"))
                })?;
            }
            Ok(Value::Dict(dict))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn validates_paths_and_deduplicates_or_bounds_lane_queue() {
        assert!(validate_object_path("/org/bluez/hci0", "adapter").is_ok());
        assert!(validate_object_path("not_a_path", "adapter").is_err());
        assert!(validate_bus_name("", "service").is_err());
        assert_eq!(
            classify_target_lane("org.bluez", "org.bluez.Adapter1"),
            DbusActionLane::BluetoothAdapter
        );
        assert_eq!(
            classify_target_lane("org.bluez", "org.bluez.Device1"),
            DbusActionLane::BluetoothDevice
        );
        assert_eq!(
            classify_target_lane(
                "org.mpris.MediaPlayer2.spotify",
                "org.mpris.MediaPlayer2.Player"
            ),
            DbusActionLane::Mpris
        );
    }

    #[test]
    fn outbox_drains_once_on_attach_and_streams_without_buffering() {
        let outbox = Arc::new(DbusActionOutbox::default());
        outbox.record(DbusActionResult {
            lane: DbusActionLane::NetworkManager,
            bus: DbusBus::System,
            operation: "NetworkManager.AddAndActivateConnection".to_owned(),
            target: "Example_WiFi".to_owned(),
            reply: DbusActionReply::ObjectPath(
                "/org/freedesktop/NetworkManager/ActiveConnection/9".to_owned(),
            ),
            error: None,
        });

        let (tx1, rx1) = calloop::channel::channel();
        let worker1 = DbusActionResultWorker::spawn(tx1, Arc::clone(&outbox));
        assert!(rx1.try_recv().is_ok());
        assert!(rx1.try_recv().is_err());

        // While attached, recorded results stream directly without growing `recent`.
        outbox.record(DbusActionResult {
            lane: DbusActionLane::BluetoothDevice,
            bus: DbusBus::System,
            operation: "Device1.Connect".to_owned(),
            target: "/org/bluez/hci0/dev_11_22_33_44_55_66".to_owned(),
            reply: DbusActionReply::Unit,
            error: Some("Device1.Connect failed: Host is down".to_owned()),
        });
        let streamed = rx1.try_recv().expect("streamed live while attached");
        assert!(streamed.is_err());
        assert!(outbox.state.lock().unwrap().recent.is_empty());

        drop(worker1);

        // Re-attaching must never replay already-delivered results.
        let (tx2, rx2) = calloop::channel::channel();
        let _worker2 = DbusActionResultWorker::spawn(tx2, Arc::clone(&outbox));
        assert!(rx2.try_recv().is_err());

        let timer = SharedDbusTimer::new();
        let stop = AsyncStopSignal::new();
        let outcome = run_with_timeout_or_stop(
            Box::pin(async { futures_lite::future::pending::<Result<DbusActionReply>>().await }),
            Duration::from_millis(25),
            &stop,
            &timer,
        );
        timer.shutdown();
        assert!(matches!(outcome, LaneExecOutcome::TimedOut));
    }

    #[test]
    fn trim_cache_clears_snapshots_and_respects_config() {
        let client = DbusClient::with_config(false, true);
        assert!(!client.cache_enabled());

        *client.cache.upower.write().unwrap() = Some(UPowerSnapshot {
            on_battery: true,
            ..Default::default()
        });
        assert_eq!(
            client.upower().cached_snapshot().map(|s| s.on_battery),
            Some(true)
        );
        client.trim_cache();
        assert!(client.upower().cached_snapshot().is_none());
    }

    #[test]
    fn encodes_and_decodes_dbus_message_bodies_with_exact_wire_signatures() {
        // 0 args -> Unit signature
        let empty_msg = zbus::Message::method_call("/org/example/Obj", "Ping")
            .unwrap()
            .interface("org.example.Iface")
            .unwrap()
            .build(&())
            .unwrap();
        assert_eq!(empty_msg.header().signature().to_string_no_parens(), "");
        assert!(decode_message_body(&empty_msg).is_empty());

        // Single i32 argument (e.g. GameMode RegisterGame(i))
        let single_args = encode_dbus_args(&[DbusValue::I32(42195)]).unwrap();
        let single_body = args_to_structure(single_args).unwrap();
        let single_msg =
            zbus::Message::method_call("/com/feralinteractive/GameMode", "RegisterGame")
                .unwrap()
                .interface("com.feralinteractive.GameMode")
                .unwrap()
                .build(&single_body)
                .unwrap();
        assert_eq!(single_msg.header().signature().to_string_no_parens(), "i");
        assert_eq!(
            decode_message_body(&single_msg),
            vec![DbusValue::I32(42195)]
        );

        // Multi-argument call with object path, string array, and a{sv} dict
        let mut props = HashMap::new();
        props.insert("weight".to_owned(), DbusValue::U32(90));
        props.insert("active".to_owned(), DbusValue::Bool(true));
        let multi_input = vec![
            DbusValue::ObjectPath("/org/example/Unit".to_owned()),
            DbusValue::String("app-example.scope".to_owned()),
            DbusValue::Array(vec![
                DbusValue::String("alpha".to_owned()),
                DbusValue::String("beta".to_owned()),
            ]),
            DbusValue::Dict(props),
        ];
        let encoded = encode_dbus_args(&multi_input).unwrap();
        let body = args_to_structure(encoded).unwrap();
        let multi_msg = zbus::Message::method_call("/org/example/Service", "ApplyConfig")
            .unwrap()
            .interface("org.example.Service")
            .unwrap()
            .build(&body)
            .unwrap();
        assert_eq!(
            multi_msg.header().signature().to_string_no_parens(),
            "osasa{sv}"
        );
        assert_eq!(decode_message_body(&multi_msg), multi_input);

        // Heterogeneous array falls back to `av`
        let mixed = vec![DbusValue::Array(vec![
            DbusValue::I32(7),
            DbusValue::String("ok".to_owned()),
        ])];
        let mixed_body = args_to_structure(encode_dbus_args(&mixed).unwrap()).unwrap();
        let mixed_msg = zbus::Message::method_call("/org/example/Obj", "Mixed")
            .unwrap()
            .interface("org.example.Iface")
            .unwrap()
            .build(&mixed_body)
            .unwrap();
        assert_eq!(mixed_msg.header().signature().to_string_no_parens(), "av");
        assert_eq!(decode_message_body(&mixed_msg), mixed);

        // systemd1.Manager.SetUnitProperties signature: `sba(sv)`
        let systemd_props = vec![
            DbusValue::String("app-example.scope".to_owned()),
            DbusValue::Bool(true),
            DbusValue::Array(vec![
                DbusValue::Struct(vec![
                    DbusValue::String("MemoryLow".to_owned()),
                    DbusValue::variant(DbusValue::U64(4 * 1024 * 1024 * 1024)),
                ]),
                DbusValue::Struct(vec![
                    DbusValue::String("CPUWeight".to_owned()),
                    DbusValue::variant(DbusValue::U64(200)),
                ]),
            ]),
        ];
        let systemd_body = args_to_structure(encode_dbus_args(&systemd_props).unwrap()).unwrap();
        let systemd_msg =
            zbus::Message::method_call("/org/freedesktop/systemd1", "SetUnitProperties")
                .unwrap()
                .interface("org.freedesktop.systemd1.Manager")
                .unwrap()
                .build(&systemd_body)
                .unwrap();
        assert_eq!(
            systemd_msg.header().signature().to_string_no_parens(),
            "sba(sv)"
        );
        assert_eq!(
            decode_message_body(&systemd_msg),
            vec![
                DbusValue::String("app-example.scope".to_owned()),
                DbusValue::Bool(true),
                DbusValue::Array(vec![
                    DbusValue::Struct(vec![
                        DbusValue::String("MemoryLow".to_owned()),
                        DbusValue::U64(4 * 1024 * 1024 * 1024),
                    ]),
                    DbusValue::Struct(vec![
                        DbusValue::String("CPUWeight".to_owned()),
                        DbusValue::U64(200),
                    ]),
                ]),
            ]
        );

        // Nested struct/variant + explicit Variant inside Dict does not double-wrap `v`
        let mut dict_with_variant = HashMap::new();
        dict_with_variant.insert(
            "mode".to_owned(),
            DbusValue::variant(DbusValue::String("replace".to_owned())),
        );
        let dict_body =
            args_to_structure(encode_dbus_args(&[DbusValue::Dict(dict_with_variant)]).unwrap())
                .unwrap();
        let dict_msg = zbus::Message::method_call("/org/example/Obj", "Opts")
            .unwrap()
            .interface("org.example.Iface")
            .unwrap()
            .build(&dict_body)
            .unwrap();
        assert_eq!(dict_msg.header().signature().to_string_no_parens(), "a{sv}");

        // Invalid object path and empty struct rejected synchronously before enqueuing
        assert!(encode_dbus_args(&[DbusValue::ObjectPath("not/a/valid/path".to_owned())]).is_err());
        assert!(encode_dbus_args(&[DbusValue::Struct(Vec::new())]).is_err());
    }
}
