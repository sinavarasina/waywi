use std::collections::{HashMap, VecDeque};
use std::future::Future;
use std::pin::Pin;
use std::sync::{Arc, Condvar, Mutex, RwLock};
use std::time::Duration;

use zbus::zvariant::{ObjectPath, OwnedValue, Value};

use crate::arena::FrameSharedFn;
use crate::error::{Result, WaywiError};
use crate::subscription::{IntoSubscriptionKey, Subscription, SubscriptionSpec};

use super::bluetooth::BluetoothSnapshot;
use super::logind::LogindSnapshot;
use super::modem::ModemSnapshot;
use super::mpris::MprisSnapshot;
use super::network_manager::NetworkManagerSnapshot;
use super::power_profiles::PowerProfileSnapshot;
use super::types::{Dbus, DbusBus, DbusValue};
use super::upower::UPowerSnapshot;
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

/// Optional structured return metadata from a completed background D-Bus action.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub(crate) struct DbusActionPayload {
    pub(crate) reply_object_path: Option<String>,
    pub(crate) count: Option<usize>,
}

impl DbusActionPayload {
    pub(crate) fn with_object_path(path: String) -> Self {
        Self {
            reply_object_path: Some(path),
            count: None,
        }
    }

    pub(crate) fn with_count(count: usize) -> Self {
        Self {
            reply_object_path: None,
            count: Some(count),
        }
    }
}

impl From<()> for DbusActionPayload {
    fn from((): ()) -> Self {
        Self::default()
    }
}

/// Asynchronous completion or error outcome emitted when a non-blocking [`DbusClient`] lane
/// action finishes.
#[derive(Debug, Clone, PartialEq, Eq)]
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
    /// Optional D-Bus object path returned by the method reply (such as the `ActiveConnection`
    /// object path from [`DbusClient::nm_connect_wifi`], [`DbusClient::nm_connect_hidden_wifi`],
    /// or [`DbusClient::nm_activate_connection`]).
    pub reply_object_path: Option<String>,
    /// Optional item count returned by the operation (such as the number of deleted saved Wi-Fi
    /// profiles from [`DbusClient::nm_forget_wifi_ssid`]).
    pub count: Option<usize>,
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
                key: "dbus:action-results".into_subscription_key(),
                map: FrameSharedFn::new(map),
            },
        }
    }
}

#[derive(Default)]
pub(crate) struct DbusActionOutbox {
    recent: Mutex<VecDeque<DbusActionResult>>,
    last_error: Mutex<Option<String>>,
    sender: Mutex<Option<calloop::channel::Sender<DbusActionResult>>>,
}

impl DbusActionOutbox {
    fn record(&self, result: DbusActionResult) {
        if let Some(ref err) = result.error
            && let Ok(mut g) = self.last_error.lock()
        {
            *g = Some(err.clone());
        }
        if let Ok(mut q) = self.recent.lock() {
            if q.len() >= LANE_CAPACITY {
                q.pop_front();
            }
            q.push_back(result.clone());
        }
        if let Ok(g) = self.sender.lock()
            && let Some(ref tx) = *g
        {
            let _ = tx.send(result);
        }
    }

    fn drain_results(&self) -> Vec<DbusActionResult> {
        let Ok(mut q) = self.recent.lock() else {
            return Vec::new();
        };
        q.drain(..).collect()
    }

    fn last_result(&self) -> Option<DbusActionResult> {
        let Ok(q) = self.recent.lock() else {
            return None;
        };
        q.back().cloned()
    }

    fn last_error(&self) -> Option<String> {
        let Ok(g) = self.last_error.lock() else {
            return None;
        };
        g.clone()
    }

    fn take_last_error(&self) -> Option<String> {
        let Ok(mut g) = self.last_error.lock() else {
            return None;
        };
        g.take()
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
        if let Ok(q) = outbox.recent.lock() {
            for item in q.iter() {
                let _ = sender.send(item.clone());
            }
        }
        if let Ok(mut g) = outbox.sender.lock() {
            *g = Some(sender);
        }
        Self { outbox }
    }
}

impl Drop for DbusActionResultWorker {
    fn drop(&mut self) {
        if let Ok(mut g) = self.outbox.sender.lock() {
            *g = None;
        }
    }
}

type BoxedDbusFuture = Pin<Box<dyn Future<Output = Result<DbusActionPayload>> + Send + 'static>>;
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
                    reply_object_path: None,
                    count: None,
                    error: Some(err_msg),
                }
            }
            LaneExecOutcome::Finished(Ok(payload)) => DbusActionResult {
                lane,
                bus: item.bus,
                operation: item.operation,
                target: item.target,
                reply_object_path: payload.reply_object_path,
                count: payload.count,
                error: None,
            },
            LaneExecOutcome::Finished(Err(err)) => DbusActionResult {
                lane,
                bus: item.bus,
                operation: item.operation,
                target: item.target,
                reply_object_path: None,
                count: None,
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
    Finished(Result<DbusActionPayload>),
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
        P: Into<DbusActionPayload> + Send + 'static,
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

/// Shared in-memory cache of the latest snapshots observed by D-Bus background workers, so UI
/// queries (`mpris_snapshot`, `bluetooth_snapshot`, `network_manager_snapshot`, etc.) read the
/// latest snapshot directly instead of issuing D-Bus calls.
#[derive(Default)]
pub(crate) struct DbusSnapshotCache {
    pub(crate) mpris: RwLock<Option<Vec<MprisSnapshot>>>,
    pub(crate) bluetooth: RwLock<Option<BluetoothSnapshot>>,
    pub(crate) network_manager: RwLock<Option<NetworkManagerSnapshot>>,
    pub(crate) upower: RwLock<Option<UPowerSnapshot>>,
    pub(crate) power_profiles: RwLock<Option<PowerProfileSnapshot>>,
    pub(crate) logind: RwLock<Option<LogindSnapshot>>,
    pub(crate) modem: RwLock<Option<ModemSnapshot>>,
}

/// D-Bus client backed by `zbus` (bounded per-domain action lanes + shared snapshot cache +
/// dedicated async-io subscription workers).
pub struct DbusClient {
    session: RwLock<Option<zbus::blocking::Connection>>,
    system: RwLock<Option<zbus::blocking::Connection>>,
    shared_session: DbusSharedBus,
    shared_system: DbusSharedBus,
    action_pool: DbusActionPool,
    pub(crate) action_outbox: Arc<DbusActionOutbox>,
    pub(crate) cache: Arc<DbusSnapshotCache>,
    pub(crate) notifications_conn: Arc<Mutex<Option<zbus::Connection>>>,
    pub(crate) bluetooth_agent_pending: super::bluetooth::PendingAgentMap,
}

impl Default for DbusClient {
    fn default() -> Self {
        Self::new()
    }
}

impl DbusClient {
    #[must_use]
    pub fn new() -> Self {
        let shared_session = Arc::new(Mutex::new(None));
        let shared_system = Arc::new(Mutex::new(None));
        let action_pool =
            DbusActionPool::new(Arc::clone(&shared_session), Arc::clone(&shared_system));
        let action_outbox = Arc::clone(&action_pool.outbox);
        Self {
            session: RwLock::new(None),
            system: RwLock::new(None),
            shared_session,
            shared_system,
            action_pool,
            action_outbox,
            cache: Arc::new(DbusSnapshotCache::default()),
            notifications_conn: Arc::new(Mutex::new(None)),
            bluetooth_agent_pending: Arc::new(Mutex::new(HashMap::new())),
        }
    }

    pub(crate) fn shared_bus(&self, bus: DbusBus) -> DbusSharedBus {
        match bus {
            DbusBus::Session => Arc::clone(&self.shared_session),
            DbusBus::System => Arc::clone(&self.shared_system),
        }
    }

    /// Drains buffered asynchronous [`DbusActionResult`] outcomes completed on background lanes.
    #[must_use]
    pub fn drain_action_results(&self) -> Vec<DbusActionResult> {
        self.action_outbox.drain_results()
    }

    /// Returns the most recently completed [`DbusActionResult`], if any.
    #[must_use]
    pub fn last_action_result(&self) -> Option<DbusActionResult> {
        self.action_outbox.last_result()
    }

    /// Returns the most recent runtime error from a background D-Bus action lane, if any.
    #[must_use]
    pub fn last_action_error(&self) -> Option<String> {
        self.action_outbox.last_error()
    }

    /// Takes and clears the most recent runtime error from a background D-Bus action lane.
    #[must_use]
    pub fn take_last_action_error(&self) -> Option<String> {
        self.action_outbox.take_last_error()
    }

    /// Schedules a bounded, timeout-protected asynchronous D-Bus action described by `spec`.
    pub(crate) fn submit_lane_action<P, F, Fut>(&self, spec: LaneActionSpec, f: F) -> Result<()>
    where
        P: Into<DbusActionPayload> + Send + 'static,
        F: FnOnce(zbus::Connection) -> Fut + Send + 'static,
        Fut: Future<Output = Result<P>> + Send + 'static,
    {
        self.action_pool.submit(spec, f)
    }

    pub(super) fn connection(&self, bus: DbusBus) -> Result<zbus::blocking::Connection> {
        let slot = match bus {
            DbusBus::Session => &self.session,
            DbusBus::System => &self.system,
        };
        if let Ok(guard) = slot.read()
            && let Some(ref conn) = *guard
        {
            return Ok(conn.clone());
        }
        let shared = self.shared_bus(bus);
        let async_conn = connect_shared_bus(bus, &shared)?;
        let conn = zbus::blocking::Connection::from(async_conn);
        if let Ok(mut guard) = slot.write() {
            *guard = Some(conn.clone());
        }
        Ok(conn)
    }

    /// Reads a D-Bus property via `org.freedesktop.DBus.Properties.Get`.
    pub fn get_property(
        &self,
        bus: DbusBus,
        destination: &str,
        path: &str,
        interface: &str,
        property: &str,
    ) -> Result<DbusValue> {
        let conn = self.connection(bus)?;
        let proxy = zbus::blocking::fdo::PropertiesProxy::builder(&conn)
            .destination(destination)
            .and_then(|b| b.path(path))
            .map_err(|e| WaywiError::DbusError(e.to_string()))?
            .build()
            .map_err(|e| WaywiError::DbusError(e.to_string()))?;
        let iface_name = zbus::names::InterfaceName::try_from(interface)
            .map_err(|e| WaywiError::DbusError(e.to_string()))?;
        let val = proxy
            .get(iface_name, property)
            .map_err(|e| WaywiError::DbusError(format!("property read failed: {e}")))?;
        Ok(owned_value_to_dbus(&val))
    }

    /// Enqueues a zero-argument D-Bus method call on the appropriate domain lane.
    pub fn call_method_unit(
        &self,
        bus: DbusBus,
        destination: &str,
        path: &str,
        interface: &str,
        method: &str,
    ) -> Result<()> {
        let lane = classify_target_lane(destination, interface);
        self.call_lane_method_unit(lane, bus, destination, path, interface, method)
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
    /// appropriate domain lane.
    pub(super) fn set_property(
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
        Value::ObjectPath(p) => DbusValue::String(p.to_string()),
        Value::Bool(b) => DbusValue::Bool(*b),
        Value::I16(v) => DbusValue::I64(i64::from(*v)),
        Value::I32(v) => DbusValue::I64(i64::from(*v)),
        Value::I64(v) => DbusValue::I64(*v),
        Value::U8(v) => DbusValue::U64(u64::from(*v)),
        Value::U16(v) => DbusValue::U64(u64::from(*v)),
        Value::U32(v) => DbusValue::U64(u64::from(*v)),
        Value::U64(v) => DbusValue::U64(*v),
        Value::F64(v) => DbusValue::F64(*v),
        Value::Array(arr) => DbusValue::Array(arr.iter().map(value_to_dbus).collect()),
        Value::Structure(st) => DbusValue::Array(st.fields().iter().map(value_to_dbus).collect()),
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
    fn records_action_results_errors_and_timeouts() {
        let outbox = DbusActionOutbox::default();
        outbox.record(DbusActionResult {
            lane: DbusActionLane::NetworkManager,
            bus: DbusBus::System,
            operation: "NetworkManager.AddAndActivateConnection".to_owned(),
            target: "ITERA_WiFi".to_owned(),
            reply_object_path: Some(
                "/org/freedesktop/NetworkManager/ActiveConnection/9".to_owned(),
            ),
            count: None,
            error: None,
        });
        assert!(outbox.last_result().is_some_and(|r| r.is_ok()));
        assert!(outbox.last_error().is_none());

        outbox.record(DbusActionResult {
            lane: DbusActionLane::BluetoothDevice,
            bus: DbusBus::System,
            operation: "Device1.Connect".to_owned(),
            target: "/org/bluez/hci0/dev_11_22_33_44_55_66".to_owned(),
            reply_object_path: None,
            count: None,
            error: Some("Device1.Connect failed: Host is down".to_owned()),
        });
        assert!(outbox.last_result().is_some_and(|r| r.is_err()));
        assert_eq!(
            outbox.take_last_error().as_deref(),
            Some("Device1.Connect failed: Host is down")
        );
        assert!(outbox.last_error().is_none());
        assert_eq!(outbox.drain_results().len(), 2);

        let timer = SharedDbusTimer::new();
        let stop = AsyncStopSignal::new();
        let outcome = run_with_timeout_or_stop(
            Box::pin(async { futures_lite::future::pending::<Result<DbusActionPayload>>().await }),
            Duration::from_millis(25),
            &stop,
            &timer,
        );
        timer.shutdown();
        assert!(matches!(outcome, LaneExecOutcome::TimedOut));
    }
}
