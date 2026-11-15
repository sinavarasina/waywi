use std::io::{BufRead, BufReader};
use std::os::unix::net::UnixStream;
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use super::types::{HyprlandEvent, WorkspaceId};

const MAX_EVENT_LINE_BYTES: usize = 16 * 1024;

/// Parses a single Hyprland `socket2` event line (`NAME>>DATA`) safely without panicking on
/// unknown or malformed lines.
#[must_use]
pub fn parse_event_line(line: &str) -> Option<HyprlandEvent> {
    let trimmed = line.trim_end_matches(['\r', '\n']);
    if trimmed.is_empty() {
        return None;
    }
    let (name, data) = trimmed.split_once(">>")?;
    let event = match name {
        "workspace" => HyprlandEvent::Workspace {
            name: data.to_owned(),
        },
        "workspacev2" => {
            let (id_str, ws_name) = data.split_once(',')?;
            let id = id_str.parse::<i32>().ok()?;
            HyprlandEvent::WorkspaceV2 {
                id: WorkspaceId(id),
                name: ws_name.to_owned(),
            }
        }
        "focusedmon" => {
            let (mon, ws) = data.split_once(',')?;
            HyprlandEvent::FocusedMonitor {
                monitor: mon.to_owned(),
                workspace_name: ws.to_owned(),
            }
        }
        "focusedmonv2" => {
            let (mon, id_str) = data.split_once(',')?;
            let id = id_str.parse::<i32>().ok()?;
            HyprlandEvent::FocusedMonitorV2 {
                monitor: mon.to_owned(),
                workspace_id: WorkspaceId(id),
            }
        }
        "createworkspace" => HyprlandEvent::CreateWorkspace {
            name: data.to_owned(),
        },
        "createworkspacev2" => {
            let (id_str, ws_name) = data.split_once(',')?;
            let id = id_str.parse::<i32>().ok()?;
            HyprlandEvent::CreateWorkspaceV2 {
                id: WorkspaceId(id),
                name: ws_name.to_owned(),
            }
        }
        "destroyworkspace" => HyprlandEvent::DestroyWorkspace {
            name: data.to_owned(),
        },
        "destroyworkspacev2" => {
            let (id_str, ws_name) = data.split_once(',')?;
            let id = id_str.parse::<i32>().ok()?;
            HyprlandEvent::DestroyWorkspaceV2 {
                id: WorkspaceId(id),
                name: ws_name.to_owned(),
            }
        }
        "moveworkspacev2" => {
            let mut parts = data.splitn(3, ',');
            let id = parts.next()?.parse::<i32>().ok()?;
            let ws_name = parts.next()?.to_owned();
            let monitor = parts.next()?.to_owned();
            HyprlandEvent::MoveWorkspaceV2 {
                id: WorkspaceId(id),
                name: ws_name,
                monitor,
            }
        }
        "activewindow" => {
            let (class, title) = data.split_once(',').unwrap_or((data, ""));
            HyprlandEvent::ActiveWindow {
                class: class.to_owned(),
                title: title.to_owned(),
            }
        }
        "activewindowv2" => HyprlandEvent::ActiveWindowV2 {
            address: data.to_owned(),
        },
        "openwindow" => {
            let mut parts = data.splitn(4, ',');
            let address = parts.next()?.to_owned();
            let workspace = parts.next()?.to_owned();
            let class = parts.next()?.to_owned();
            let title = parts.next().unwrap_or("").to_owned();
            HyprlandEvent::OpenWindow {
                address,
                workspace,
                class,
                title,
            }
        }
        "closewindow" => HyprlandEvent::CloseWindow {
            address: data.to_owned(),
        },
        "monitoradded" => HyprlandEvent::MonitorAdded {
            name: data.to_owned(),
        },
        "monitorremoved" => HyprlandEvent::MonitorRemoved {
            name: data.to_owned(),
        },
        "fullscreen" => HyprlandEvent::Fullscreen(data.trim() == "1"),
        "submap" => HyprlandEvent::Submap(data.to_owned()),
        "activelayout" => {
            let (kb, layout) = data.split_once(',').unwrap_or((data, ""));
            HyprlandEvent::ActiveLayout {
                keyboard: kb.to_owned(),
                layout: layout.to_owned(),
            }
        }
        _ => HyprlandEvent::Unknown {
            name: name.to_owned(),
            data: data.to_owned(),
        },
    };
    Some(event)
}

/// Manages the background `socket2` reader bridge feeding events into `calloop::channel`.
pub(crate) struct HyprlandListenerHandle {
    stop: Arc<AtomicBool>,
    join: Option<std::thread::JoinHandle<()>>,
}

impl HyprlandListenerHandle {
    pub(crate) fn spawn(
        socket_path: PathBuf,
        sender: calloop::channel::Sender<HyprlandEvent>,
    ) -> Self {
        let stop = Arc::new(AtomicBool::new(false));
        let stop_flag = Arc::clone(&stop);
        let join = std::thread::Builder::new()
            .name("waywi-hyprland-events".to_owned())
            .spawn(move || {
                let mut ever_connected = false;
                while !stop_flag.load(Ordering::Relaxed) {
                    match UnixStream::connect(&socket_path) {
                        Ok(stream) => {
                            let _ = stream.set_read_timeout(Some(Duration::from_millis(250)));
                            if ever_connected {
                                let _ = sender.send(HyprlandEvent::Reconnected);
                            }
                            ever_connected = true;

                            let mut reader = BufReader::new(stream);
                            let mut line = String::new();
                            while !stop_flag.load(Ordering::Relaxed) {
                                line.clear();
                                match reader.read_line(&mut line) {
                                    Ok(0) => {
                                        let _ = sender.send(HyprlandEvent::Disconnected);
                                        break;
                                    }
                                    Ok(n) if n <= MAX_EVENT_LINE_BYTES => {
                                        if let Some(ev) = parse_event_line(&line) {
                                            let _ = sender.send(ev);
                                        }
                                    }
                                    Ok(_) => {
                                        // Oversized line skipped cleanly.
                                    }
                                    Err(e)
                                        if matches!(
                                            e.kind(),
                                            std::io::ErrorKind::WouldBlock
                                                | std::io::ErrorKind::TimedOut
                                        ) =>
                                    {
                                        continue;
                                    }
                                    Err(_) => {
                                        let _ = sender.send(HyprlandEvent::Disconnected);
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

impl Drop for HyprlandListenerHandle {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
        if let Some(j) = self.join.take() {
            let _ = j.join();
        }
    }
}
