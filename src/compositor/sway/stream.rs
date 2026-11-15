use std::os::unix::net::UnixStream;
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use crate::error::WaywiError;

use super::client::{DEFAULT_TIMEOUT, MSG_SUBSCRIBE, read_frame, write_frame};
use super::types::{SwayEvent, SwayWorkspace};

// Sway IPC event types (bit 31 set).
const EVT_WORKSPACE: u32 = 0x8000_0000;
const EVT_OUTPUT: u32 = 0x8000_0001;
const EVT_MODE: u32 = 0x8000_0002;
const EVT_WINDOW: u32 = 0x8000_0003;
const EVT_SHUTDOWN: u32 = 0x8000_0006;

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

pub(crate) struct SwayListenerHandle {
    stop: Arc<AtomicBool>,
    join: Option<std::thread::JoinHandle<()>>,
}

impl SwayListenerHandle {
    pub(crate) fn spawn(socket_path: PathBuf, sender: calloop::channel::Sender<SwayEvent>) -> Self {
        let stop = Arc::new(AtomicBool::new(false));
        let stop_flag = Arc::clone(&stop);
        let join = std::thread::Builder::new()
            .name("waywi-sway-events".to_owned())
            .spawn(move || {
                let mut ever_connected = false;
                while !stop_flag.load(Ordering::Relaxed) {
                    match UnixStream::connect(&socket_path) {
                        Ok(mut stream) => {
                            let _ = stream.set_read_timeout(Some(Duration::from_millis(250)));
                            let _ = stream.set_write_timeout(Some(DEFAULT_TIMEOUT));
                            let sub_payload =
                                br#"["workspace","output","window","mode","shutdown"]"#;
                            if write_frame(&mut stream, MSG_SUBSCRIBE, sub_payload).is_err()
                                || read_frame(&mut stream).is_err()
                            {
                                std::thread::sleep(Duration::from_millis(300));
                                continue;
                            }
                            if ever_connected {
                                let _ = sender.send(SwayEvent::Reconnected);
                            }
                            ever_connected = true;

                            while !stop_flag.load(Ordering::Relaxed) {
                                match read_frame(&mut stream) {
                                    Ok((evt_type, payload)) => {
                                        let _ = sender.send(parse_event_frame(evt_type, &payload));
                                    }
                                    Err(WaywiError::Io(ref e))
                                        if matches!(
                                            e.kind(),
                                            std::io::ErrorKind::WouldBlock
                                                | std::io::ErrorKind::TimedOut
                                        ) =>
                                    {
                                        continue;
                                    }
                                    Err(WaywiError::Timeout(_)) => continue,
                                    Err(_) => {
                                        let _ = sender.send(SwayEvent::Disconnected);
                                        break;
                                    }
                                }
                            }
                        }
                        Err(_) => {
                            std::thread::sleep(Duration::from_millis(300));
                        }
                    }
                }
            })
            .ok();
        Self { stop, join }
    }
}

impl Drop for SwayListenerHandle {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
        if let Some(j) = self.join.take() {
            let _ = j.join();
        }
    }
}
