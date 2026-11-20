use std::os::fd::OwnedFd;
use std::os::unix::net::UnixStream;
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use rustix::event::{EventfdFlags, PollFd, PollFlags, Timespec, eventfd, poll};

use super::client::{DEFAULT_TIMEOUT, MSG_SUBSCRIBE, read_frame, write_frame};
use super::types::{SwayEvent, SwayWorkspace};

// Sway IPC event types (bit 31 set).
const EVT_WORKSPACE: u32 = 0x8000_0000;
const EVT_OUTPUT: u32 = 0x8000_0001;
const EVT_MODE: u32 = 0x8000_0002;
const EVT_WINDOW: u32 = 0x8000_0003;
const EVT_SHUTDOWN: u32 = 0x8000_0006;

const RECONNECT_INITIAL_MS: u64 = 150;
const RECONNECT_MAX_MS: u64 = 2_000;

/// Parses an incremental Sway event frame payload without issuing follow-up queries.
#[must_use]
pub fn parse_event_frame(event_type: u32, payload: &[u8]) -> SwayEvent {
    let value: serde_json::Value = match serde_json::from_slice(payload) {
        Ok(v) => v,
        Err(_) => {
            return SwayEvent::Unknown {
                event_type,
                payload: String::from_utf8_lossy(payload).into_owned(),
            };
        }
    };
    let change = value
        .get("change")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_owned();

    match event_type {
        EVT_WORKSPACE => {
            let current = value
                .get("current")
                .and_then(|v| serde_json::from_value::<SwayWorkspace>(v.clone()).ok());
            let old = value
                .get("old")
                .and_then(|v| serde_json::from_value::<SwayWorkspace>(v.clone()).ok());
            SwayEvent::Workspace {
                change,
                current,
                old,
            }
        }
        EVT_OUTPUT => SwayEvent::Output { change },
        EVT_WINDOW => {
            let container = value.get("container");
            SwayEvent::Window {
                change,
                container_id: container.and_then(|c| c.get("id")).and_then(|v| v.as_i64()),
                name: container
                    .and_then(|c| c.get("name"))
                    .and_then(|v| v.as_str())
                    .map(str::to_owned),
                app_id: container
                    .and_then(|c| c.get("app_id"))
                    .and_then(|v| v.as_str())
                    .map(str::to_owned),
            }
        }
        EVT_MODE => SwayEvent::Mode { change },
        EVT_SHUTDOWN => SwayEvent::Shutdown { reason: change },
        _ => SwayEvent::Unknown {
            event_type,
            payload: String::from_utf8_lossy(payload).into_owned(),
        },
    }
}

/// Zero-polling Sway IPC event listener bridge using `poll(2)` + `eventfd(2)` so the worker
/// thread triggers zero wakeups while idle and shuts down instantaneously on [`Drop`].
pub(crate) struct SwayListenerHandle {
    stop: Arc<AtomicBool>,
    stop_fd: Option<Arc<OwnedFd>>,
    join: Option<std::thread::JoinHandle<()>>,
}

impl SwayListenerHandle {
    pub(crate) fn spawn(socket_path: PathBuf, sender: calloop::channel::Sender<SwayEvent>) -> Self {
        let stop = Arc::new(AtomicBool::new(false));
        let stop_fd = eventfd(0, EventfdFlags::CLOEXEC | EventfdFlags::NONBLOCK)
            .ok()
            .map(Arc::new);
        let stop_flag = Arc::clone(&stop);
        let worker_stop_fd = stop_fd.clone();
        let join = std::thread::Builder::new()
            .name("waywi-sway-events".to_owned())
            .spawn(move || {
                run_sway_listener(socket_path, sender, stop_flag, worker_stop_fd);
            })
            .ok();
        Self {
            stop,
            stop_fd,
            join,
        }
    }
}

impl Drop for SwayListenerHandle {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Release);
        if let Some(ref fd) = self.stop_fd {
            let _ = rustix::io::write(&**fd, &1u64.to_ne_bytes());
        }
        if let Some(j) = self.join.take() {
            let _ = j.join();
        }
    }
}

fn run_sway_listener(
    socket_path: PathBuf,
    sender: calloop::channel::Sender<SwayEvent>,
    stop: Arc<AtomicBool>,
    stop_fd: Option<Arc<OwnedFd>>,
) {
    let mut ever_connected = false;
    let mut backoff_ms = RECONNECT_INITIAL_MS;

    while !stop.load(Ordering::Acquire) {
        match UnixStream::connect(&socket_path) {
            Ok(mut stream) => {
                let _ = stream.set_read_timeout(Some(DEFAULT_TIMEOUT));
                let _ = stream.set_write_timeout(Some(DEFAULT_TIMEOUT));
                let sub_payload = br#"["workspace","output","window","mode","shutdown"]"#;
                if write_frame(&mut stream, MSG_SUBSCRIBE, sub_payload).is_err()
                    || read_frame(&mut stream).is_err()
                {
                    if wait_reconnect_or_stop(stop_fd.as_deref(), &stop, backoff_ms) {
                        return;
                    }
                    backoff_ms = (backoff_ms * 2).min(RECONNECT_MAX_MS);
                    continue;
                }
                backoff_ms = RECONNECT_INITIAL_MS;
                if ever_connected && sender.send(SwayEvent::Reconnected).is_err() {
                    return;
                }
                ever_connected = true;

                let receiver_closed =
                    pump_sway_frames(&mut stream, stop_fd.as_deref(), &stop, &sender);
                if receiver_closed || stop.load(Ordering::Acquire) {
                    return;
                }
                if sender.send(SwayEvent::Disconnected).is_err() {
                    return;
                }
                if wait_reconnect_or_stop(stop_fd.as_deref(), &stop, backoff_ms) {
                    return;
                }
            }
            Err(_) => {
                if wait_reconnect_or_stop(stop_fd.as_deref(), &stop, backoff_ms) {
                    return;
                }
                backoff_ms = (backoff_ms * 2).min(RECONNECT_MAX_MS);
            }
        }
    }
}

/// Returns `true` if the channel receiver closed or stop was requested.
fn pump_sway_frames(
    stream: &mut UnixStream,
    stop_fd: Option<&OwnedFd>,
    stop: &AtomicBool,
    sender: &calloop::channel::Sender<SwayEvent>,
) -> bool {
    let fallback_timeout = Timespec {
        tv_sec: 0,
        tv_nsec: 250_000_000,
    };
    while !stop.load(Ordering::Acquire) {
        match stop_fd {
            Some(wake_fd) => {
                let mut fds = [
                    PollFd::new(&*stream, PollFlags::IN),
                    PollFd::new(wake_fd, PollFlags::IN),
                ];
                match poll(&mut fds, None) {
                    Ok(_) => {}
                    Err(rustix::io::Errno::INTR) => continue,
                    Err(_) => return false,
                }
                if fds[1].revents().contains(PollFlags::IN) || stop.load(Ordering::Acquire) {
                    return true;
                }
                let revents = fds[0].revents();
                if !revents.contains(PollFlags::IN)
                    && (revents.contains(PollFlags::HUP)
                        || revents.contains(PollFlags::ERR)
                        || revents.contains(PollFlags::NVAL))
                {
                    return false;
                }
            }
            None => {
                let mut fds = [PollFd::new(&*stream, PollFlags::IN)];
                match poll(&mut fds, Some(&fallback_timeout)) {
                    Ok(0) => continue,
                    Ok(_) => {}
                    Err(rustix::io::Errno::INTR) => continue,
                    Err(_) => return false,
                }
            }
        }

        match read_frame(stream) {
            Ok((evt_type, payload)) => {
                if sender.send(parse_event_frame(evt_type, &payload)).is_err() {
                    return true;
                }
            }
            Err(_) => return false,
        }
    }
    true
}

fn wait_reconnect_or_stop(stop_fd: Option<&OwnedFd>, stop: &AtomicBool, delay_ms: u64) -> bool {
    if stop.load(Ordering::Acquire) {
        return true;
    }
    match stop_fd {
        Some(fd) => {
            let d = Duration::from_millis(delay_ms);
            let ts = Timespec {
                tv_sec: d.as_secs() as i64,
                tv_nsec: i64::from(d.subsec_nanos()),
            };
            let mut fds = [PollFd::new(fd, PollFlags::IN)];
            let _ = poll(&mut fds, Some(&ts));
            stop.load(Ordering::Acquire) || fds[0].revents().contains(PollFlags::IN)
        }
        None => {
            std::thread::sleep(Duration::from_millis(delay_ms));
            stop.load(Ordering::Acquire)
        }
    }
}
