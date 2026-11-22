use std::io::Read;
use std::os::fd::OwnedFd;
use std::os::unix::net::UnixStream;
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use rustix::event::{EventfdFlags, PollFd, PollFlags, Timespec, eventfd, poll};

use super::types::{HyprlandEvent, WorkspaceId};

const MAX_EVENT_LINE_BYTES: usize = 16 * 1024;
const READ_CHUNK_BYTES: usize = 4096;
const RECONNECT_INITIAL_MS: u64 = 150;
const RECONNECT_MAX_MS: u64 = 2_000;

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
            HyprlandEvent::WorkspaceV2 {
                id: parse_workspace_id_or_address(id_str),
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
            HyprlandEvent::FocusedMonitorV2 {
                monitor: mon.to_owned(),
                workspace_id: parse_workspace_id_or_address(id_str),
            }
        }
        "createworkspace" => HyprlandEvent::CreateWorkspace {
            name: data.to_owned(),
        },
        "createworkspacev2" => {
            let (id_str, ws_name) = data.split_once(',')?;
            HyprlandEvent::CreateWorkspaceV2 {
                id: parse_workspace_id_or_address(id_str),
                name: ws_name.to_owned(),
            }
        }
        "destroyworkspace" => HyprlandEvent::DestroyWorkspace {
            name: data.to_owned(),
        },
        "destroyworkspacev2" => {
            let (id_str, ws_name) = data.split_once(',')?;
            HyprlandEvent::DestroyWorkspaceV2 {
                id: parse_workspace_id_or_address(id_str),
                name: ws_name.to_owned(),
            }
        }
        "moveworkspacev2" => {
            let mut parts = data.splitn(3, ',');
            let id = parse_workspace_id_or_address(parts.next()?);
            let ws_name = parts.next()?.to_owned();
            let monitor = parts.next()?.to_owned();
            HyprlandEvent::MoveWorkspaceV2 {
                id,
                name: ws_name,
                monitor,
            }
        }
        "renameworkspace" => {
            let (id_str, ws_name) = data.split_once(',')?;
            HyprlandEvent::RenameWorkspace {
                id: parse_workspace_id_or_address(id_str),
                name: ws_name.to_owned(),
            }
        }
        "activespecial" => {
            let (ws_name, monitor) = data.rsplit_once(',')?;
            HyprlandEvent::ActiveSpecial {
                workspace_name: ws_name.to_owned(),
                monitor: monitor.to_owned(),
            }
        }
        "activespecialv2" => {
            // Format: `WORKSPACEID_OR_ADDR,WORKSPACENAME,MONNAME` (empty when closed: `,,DP-1`).
            // On Hyprland v0.57+, special workspaces pass string addresses (`special:magic`) in the
            // first field rather than negative numeric IDs.
            let (id_str, rest) = data.split_once(',')?;
            let (ws_name, monitor) = rest.rsplit_once(',')?;
            let workspace_id = id_str.trim().parse::<i32>().ok().map(WorkspaceId);
            HyprlandEvent::ActiveSpecialV2 {
                workspace_id,
                workspace_name: ws_name.to_owned(),
                monitor: monitor.to_owned(),
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
        "windowtitle" => HyprlandEvent::WindowTitle {
            address: data.to_owned(),
        },
        "windowtitlev2" => {
            let (address, title) = data.split_once(',')?;
            HyprlandEvent::WindowTitleV2 {
                address: address.to_owned(),
                title: title.to_owned(),
            }
        }
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
        "movewindowv2" => {
            let mut parts = data.splitn(3, ',');
            let address = parts.next()?.to_owned();
            let workspace_id = parse_workspace_id_or_address(parts.next()?);
            let workspace_name = parts.next()?.to_owned();
            HyprlandEvent::MoveWindowV2 {
                address,
                workspace_id,
                workspace_name,
            }
        }
        "changefloatingmode" => {
            let (address, flag) = data.split_once(',')?;
            HyprlandEvent::ChangeFloatingMode {
                address: address.to_owned(),
                floating: flag.trim() == "1",
            }
        }
        "pin" => {
            let (address, flag) = data.split_once(',')?;
            HyprlandEvent::Pin {
                address: address.to_owned(),
                pinned: flag.trim() == "1",
            }
        }
        "minimized" => {
            let (address, flag) = data.split_once(',')?;
            HyprlandEvent::Minimized {
                address: address.to_owned(),
                minimized: flag.trim() == "1",
            }
        }
        "urgent" => HyprlandEvent::Urgent {
            address: data.to_owned(),
        },
        "bell" => HyprlandEvent::Bell {
            address: data.to_owned(),
        },
        "monitoradded" => HyprlandEvent::MonitorAdded {
            name: data.to_owned(),
        },
        "monitoraddedv2" => {
            let mut parts = data.splitn(3, ',');
            let id = parts.next()?.parse::<i64>().ok()?;
            let name = parts.next()?.to_owned();
            let description = parts.next()?.to_owned();
            HyprlandEvent::MonitorAddedV2 {
                id,
                name,
                description,
            }
        }
        "monitorremoved" => HyprlandEvent::MonitorRemoved {
            name: data.to_owned(),
        },
        "monitorremovedv2" => {
            let mut parts = data.splitn(3, ',');
            let id = parts.next()?.parse::<i64>().ok()?;
            let name = parts.next()?.to_owned();
            let description = parts.next()?.to_owned();
            HyprlandEvent::MonitorRemovedV2 {
                id,
                name,
                description,
            }
        }
        "fullscreen" => HyprlandEvent::Fullscreen(data.trim() == "1"),
        "screencast" => {
            let (state, owner) = data.split_once(',')?;
            HyprlandEvent::Screencast {
                active: state.trim() == "1",
                owner: owner.trim().parse::<u8>().ok()?,
            }
        }
        "submap" => HyprlandEvent::Submap(data.to_owned()),
        "activelayout" => {
            let (kb, layout) = data.split_once(',').unwrap_or((data, ""));
            HyprlandEvent::ActiveLayout {
                keyboard: kb.to_owned(),
                layout: layout.to_owned(),
            }
        }
        "configreloaded" => HyprlandEvent::ConfigReloaded,
        _ => HyprlandEvent::Unknown {
            name: name.to_owned(),
            data: data.to_owned(),
        },
    };
    Some(event)
}

/// Parses a Hyprland `socket2` workspace ID field: numeric on Hyprland `<0.57` and for regular
/// numbered workspaces, or a string address (e.g. `"special:magic"`) on Hyprland `v0.57+` (mapped
/// to `WorkspaceId(0)` so the accompanying workspace name/address stays intact).
#[inline]
fn parse_workspace_id_or_address(raw: &str) -> WorkspaceId {
    raw.trim()
        .parse::<i32>()
        .map(WorkspaceId)
        .unwrap_or(WorkspaceId(0))
}

/// Zero-polling `socket2` listener bridge feeding Hyprland events into `calloop::channel`.
///
/// Uses `poll(2)` + `eventfd(2)` so the worker thread triggers **zero wakeups while idle** and
/// shuts down instantaneously on [`Drop`], even during reconnect backoff.
pub(crate) struct HyprlandListenerHandle {
    stop: Arc<AtomicBool>,
    stop_fd: Option<Arc<OwnedFd>>,
    join: Option<std::thread::JoinHandle<()>>,
}

impl HyprlandListenerHandle {
    pub(crate) fn spawn(
        socket_path: PathBuf,
        sender: calloop::channel::Sender<HyprlandEvent>,
    ) -> Self {
        let stop = Arc::new(AtomicBool::new(false));
        let stop_fd = eventfd(0, EventfdFlags::CLOEXEC | EventfdFlags::NONBLOCK)
            .ok()
            .map(Arc::new);
        let stop_flag = Arc::clone(&stop);
        let worker_stop_fd = stop_fd.clone();
        let join = std::thread::Builder::new()
            .name("waywi-hyprland-events".to_owned())
            .spawn(move || {
                run_event_listener(socket_path, sender, stop_flag, worker_stop_fd);
            })
            .ok();
        Self {
            stop,
            stop_fd,
            join,
        }
    }
}

impl Drop for HyprlandListenerHandle {
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

fn run_event_listener(
    socket_path: PathBuf,
    sender: calloop::channel::Sender<HyprlandEvent>,
    stop: Arc<AtomicBool>,
    stop_fd: Option<Arc<OwnedFd>>,
) {
    let mut ever_connected = false;
    let mut backoff_ms = RECONNECT_INITIAL_MS;
    let mut chunk = [0u8; READ_CHUNK_BYTES];
    let mut line_buf: Vec<u8> = Vec::new();

    while !stop.load(Ordering::Acquire) {
        match UnixStream::connect(&socket_path) {
            Ok(mut stream) => {
                if stream.set_nonblocking(true).is_err() {
                    if wait_reconnect_or_stop(stop_fd.as_deref(), &stop, backoff_ms) {
                        return;
                    }
                    backoff_ms = (backoff_ms * 2).min(RECONNECT_MAX_MS);
                    continue;
                }
                backoff_ms = RECONNECT_INITIAL_MS;
                if ever_connected && sender.send(HyprlandEvent::Reconnected).is_err() {
                    return;
                }
                ever_connected = true;

                line_buf.clear();
                if line_buf.capacity() > READ_CHUNK_BYTES {
                    line_buf.shrink_to(READ_CHUNK_BYTES);
                }
                let mut discarding_oversized = false;

                let receiver_closed = match stop_fd.as_deref() {
                    Some(wake_fd) => pump_stream_eventfd(
                        &mut stream,
                        wake_fd,
                        &stop,
                        &sender,
                        &mut chunk,
                        &mut line_buf,
                        &mut discarding_oversized,
                    ),
                    None => pump_stream_fallback(
                        &mut stream,
                        &stop,
                        &sender,
                        &mut chunk,
                        &mut line_buf,
                        &mut discarding_oversized,
                    ),
                };

                if receiver_closed || stop.load(Ordering::Acquire) {
                    return;
                }
                if sender.send(HyprlandEvent::Disconnected).is_err() {
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
fn pump_stream_eventfd(
    stream: &mut UnixStream,
    stop_fd: &OwnedFd,
    stop: &AtomicBool,
    sender: &calloop::channel::Sender<HyprlandEvent>,
    chunk: &mut [u8; READ_CHUNK_BYTES],
    line_buf: &mut Vec<u8>,
    discarding_oversized: &mut bool,
) -> bool {
    loop {
        if stop.load(Ordering::Acquire) {
            return true;
        }
        let mut fds = [
            PollFd::new(&*stream, PollFlags::IN),
            PollFd::new(stop_fd, PollFlags::IN),
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
        if revents.contains(PollFlags::IN) || revents.contains(PollFlags::HUP) {
            match drain_nonblocking_socket(stream, sender, chunk, line_buf, discarding_oversized) {
                DrainStatus::Continue => {
                    if revents.contains(PollFlags::HUP) || revents.contains(PollFlags::ERR) {
                        return false;
                    }
                }
                DrainStatus::EofOrError => return false,
                DrainStatus::ReceiverClosed => return true,
            }
        } else if revents.contains(PollFlags::ERR) || revents.contains(PollFlags::NVAL) {
            return false;
        }
    }
}

/// Fallback loop used only if `eventfd(2)` could not be allocated.
fn pump_stream_fallback(
    stream: &mut UnixStream,
    stop: &AtomicBool,
    sender: &calloop::channel::Sender<HyprlandEvent>,
    chunk: &mut [u8; READ_CHUNK_BYTES],
    line_buf: &mut Vec<u8>,
    discarding_oversized: &mut bool,
) -> bool {
    let timeout = duration_to_timespec(Duration::from_millis(250));
    loop {
        if stop.load(Ordering::Acquire) {
            return true;
        }
        let mut fds = [PollFd::new(&*stream, PollFlags::IN)];
        match poll(&mut fds, Some(&timeout)) {
            Ok(0) => continue,
            Ok(_) => {}
            Err(rustix::io::Errno::INTR) => continue,
            Err(_) => return false,
        }
        match drain_nonblocking_socket(stream, sender, chunk, line_buf, discarding_oversized) {
            DrainStatus::Continue => {}
            DrainStatus::EofOrError => return false,
            DrainStatus::ReceiverClosed => return true,
        }
    }
}

enum DrainStatus {
    Continue,
    EofOrError,
    ReceiverClosed,
}

fn drain_nonblocking_socket(
    stream: &mut UnixStream,
    sender: &calloop::channel::Sender<HyprlandEvent>,
    chunk: &mut [u8; READ_CHUNK_BYTES],
    line_buf: &mut Vec<u8>,
    discarding_oversized: &mut bool,
) -> DrainStatus {
    loop {
        match stream.read(chunk) {
            Ok(0) => {
                if !*discarding_oversized && !line_buf.is_empty() {
                    let closed = emit_line_bytes(line_buf, sender);
                    line_buf.clear();
                    if closed {
                        return DrainStatus::ReceiverClosed;
                    }
                }
                return DrainStatus::EofOrError;
            }
            Ok(n) => {
                if consume_chunk(&chunk[..n], line_buf, discarding_oversized, sender) {
                    return DrainStatus::ReceiverClosed;
                }
            }
            Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                return DrainStatus::Continue;
            }
            Err(e) if e.kind() == std::io::ErrorKind::Interrupted => continue,
            Err(_) => return DrainStatus::EofOrError,
        }
    }
}

/// Processes a freshly read chunk of bytes, using a zero-copy fast path whenever a complete line
/// arrives in `data` with an empty carry-over `line_buf`. Returns `true` if `sender` closed.
fn consume_chunk(
    mut remaining: &[u8],
    line_buf: &mut Vec<u8>,
    discarding_oversized: &mut bool,
    sender: &calloop::channel::Sender<HyprlandEvent>,
) -> bool {
    while !remaining.is_empty() {
        if *discarding_oversized {
            if let Some(nl) = remaining.iter().position(|&b| b == b'\n') {
                remaining = &remaining[nl + 1..];
                *discarding_oversized = false;
            } else {
                break;
            }
            continue;
        }

        if let Some(nl) = remaining.iter().position(|&b| b == b'\n') {
            let segment = &remaining[..nl];
            remaining = &remaining[nl + 1..];

            if line_buf.is_empty() {
                if segment.len() <= MAX_EVENT_LINE_BYTES && emit_line_bytes(segment, sender) {
                    return true;
                }
            } else if line_buf.len() + segment.len() <= MAX_EVENT_LINE_BYTES {
                line_buf.extend_from_slice(segment);
                let closed = emit_line_bytes(line_buf, sender);
                line_buf.clear();
                if closed {
                    return true;
                }
            } else {
                line_buf.clear();
            }
        } else {
            if line_buf.len() + remaining.len() > MAX_EVENT_LINE_BYTES {
                line_buf.clear();
                if line_buf.capacity() > READ_CHUNK_BYTES {
                    line_buf.shrink_to(READ_CHUNK_BYTES);
                }
                *discarding_oversized = true;
            } else {
                line_buf.extend_from_slice(remaining);
            }
            break;
        }
    }
    false
}

fn emit_line_bytes(bytes: &[u8], sender: &calloop::channel::Sender<HyprlandEvent>) -> bool {
    let Ok(line) = std::str::from_utf8(bytes) else {
        return false;
    };
    if let Some(ev) = parse_event_line(line) {
        return sender.send(ev).is_err();
    }
    false
}

/// Waits up to `delay_ms` for reconnect, returning `true` immediately if stop is signalled.
fn wait_reconnect_or_stop(stop_fd: Option<&OwnedFd>, stop: &AtomicBool, delay_ms: u64) -> bool {
    if stop.load(Ordering::Acquire) {
        return true;
    }
    match stop_fd {
        Some(fd) => {
            let ts = duration_to_timespec(Duration::from_millis(delay_ms));
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

fn duration_to_timespec(d: Duration) -> Timespec {
    Timespec {
        tv_sec: d.as_secs() as i64,
        tv_nsec: i64::from(d.subsec_nanos()),
    }
}

#[cfg(test)]
mod tests {
    use std::io::Write;
    use std::os::unix::net::UnixListener;
    use std::sync::atomic::{AtomicU64, Ordering};
    use std::time::Instant;

    use super::*;

    static STREAM_SEQ: AtomicU64 = AtomicU64::new(0);

    #[test]
    fn parses_extended_hyprland_socket2_events_including_edge_cases() {
        assert_eq!(
            parse_event_line("windowtitlev2>>0x55a,Mozilla Firefox, Work"),
            Some(HyprlandEvent::WindowTitleV2 {
                address: "0x55a".to_owned(),
                title: "Mozilla Firefox, Work".to_owned(),
            })
        );
        assert_eq!(
            parse_event_line("movewindowv2>>0x55a,3,code,main"),
            Some(HyprlandEvent::MoveWindowV2 {
                address: "0x55a".to_owned(),
                workspace_id: WorkspaceId(3),
                workspace_name: "code,main".to_owned(),
            })
        );
        assert_eq!(
            parse_event_line("activespecialv2>>-98,special:magic,DP-1"),
            Some(HyprlandEvent::ActiveSpecialV2 {
                workspace_id: Some(WorkspaceId(-98)),
                workspace_name: "special:magic".to_owned(),
                monitor: "DP-1".to_owned(),
            })
        );
        // Closing a special workspace sends empty ID and name: `,,DP-1`.
        assert_eq!(
            parse_event_line("activespecialv2>>,,DP-1"),
            Some(HyprlandEvent::ActiveSpecialV2 {
                workspace_id: None,
                workspace_name: String::new(),
                monitor: "DP-1".to_owned(),
            })
        );
        assert_eq!(
            parse_event_line("renameworkspace>>2,dev"),
            Some(HyprlandEvent::RenameWorkspace {
                id: WorkspaceId(2),
                name: "dev".to_owned(),
            })
        );
        assert_eq!(
            parse_event_line("changefloatingmode>>0xabc,1"),
            Some(HyprlandEvent::ChangeFloatingMode {
                address: "0xabc".to_owned(),
                floating: true,
            })
        );
        assert_eq!(
            parse_event_line("pin>>0xabc,1"),
            Some(HyprlandEvent::Pin {
                address: "0xabc".to_owned(),
                pinned: true,
            })
        );
        assert_eq!(
            parse_event_line("minimized>>0xabc,0"),
            Some(HyprlandEvent::Minimized {
                address: "0xabc".to_owned(),
                minimized: false,
            })
        );
        assert_eq!(
            parse_event_line("urgent>>0xabc"),
            Some(HyprlandEvent::Urgent {
                address: "0xabc".to_owned(),
            })
        );
        assert_eq!(
            parse_event_line("bell>>0xabc"),
            Some(HyprlandEvent::Bell {
                address: "0xabc".to_owned(),
            })
        );
        assert_eq!(
            parse_event_line("monitoraddedv2>>0,DP-1,Acme Display 27"),
            Some(HyprlandEvent::MonitorAddedV2 {
                id: 0,
                name: "DP-1".to_owned(),
                description: "Acme Display 27".to_owned(),
            })
        );
        assert_eq!(
            parse_event_line("monitorremovedv2>>1,HDMI-A-1,TV"),
            Some(HyprlandEvent::MonitorRemovedV2 {
                id: 1,
                name: "HDMI-A-1".to_owned(),
                description: "TV".to_owned(),
            })
        );
        assert_eq!(
            parse_event_line("screencast>>1,0"),
            Some(HyprlandEvent::Screencast {
                active: true,
                owner: 0,
            })
        );
        assert_eq!(
            parse_event_line("configreloaded>>"),
            Some(HyprlandEvent::ConfigReloaded)
        );
        // Hyprland v0.57+ string-addressed special/named workspace events.
        assert_eq!(
            parse_event_line("createworkspacev2>>special:magic,special:magic"),
            Some(HyprlandEvent::CreateWorkspaceV2 {
                id: WorkspaceId(0),
                name: "special:magic".to_owned(),
            })
        );
        assert_eq!(
            parse_event_line("destroyworkspacev2>>special:magic,special:magic"),
            Some(HyprlandEvent::DestroyWorkspaceV2 {
                id: WorkspaceId(0),
                name: "special:magic".to_owned(),
            })
        );
        assert_eq!(
            parse_event_line("activespecialv2>>special:magic,special:magic,DP-1"),
            Some(HyprlandEvent::ActiveSpecialV2 {
                workspace_id: None,
                workspace_name: "special:magic".to_owned(),
                monitor: "DP-1".to_owned(),
            })
        );
        assert_eq!(
            parse_event_line("movewindowv2>>0xabc,special:magic,special:magic"),
            Some(HyprlandEvent::MoveWindowV2 {
                address: "0xabc".to_owned(),
                workspace_id: WorkspaceId(0),
                workspace_name: "special:magic".to_owned(),
            })
        );
    }

    #[test]
    fn zero_polling_listener_handles_split_oversized_and_instant_shutdown() {
        let dir = std::env::temp_dir().join(format!(
            "waywi-hypr-stream-{}-{}",
            std::process::id(),
            STREAM_SEQ.fetch_add(1, Ordering::Relaxed)
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let sock_path = dir.join(".socket2.sock");
        let listener = UnixListener::bind(&sock_path).unwrap();

        let (tx, rx) = calloop::channel::channel::<HyprlandEvent>();
        let handle = HyprlandListenerHandle::spawn(sock_path.clone(), tx);

        let (mut peer, _) = listener.accept().unwrap();
        // Send a split event, an oversized event (> 16 KiB), and a valid event in sequence.
        peer.write_all(b"urgent>>0x11").unwrap();
        peer.flush().unwrap();
        std::thread::sleep(Duration::from_millis(5));
        peer.write_all(b"22\n").unwrap();

        let huge = format!(
            "activewindow>>foo,{}\n",
            "x".repeat(MAX_EVENT_LINE_BYTES + 256)
        );
        peer.write_all(huge.as_bytes()).unwrap();
        peer.write_all(b"configreloaded>>\n").unwrap();
        peer.flush().unwrap();

        let mut received = Vec::new();
        let deadline = Instant::now() + Duration::from_secs(1);
        while received.len() < 2 && Instant::now() < deadline {
            match rx.try_recv() {
                Ok(ev) => received.push(ev),
                Err(_) => std::thread::sleep(Duration::from_millis(5)),
            }
        }
        assert_eq!(
            received,
            vec![
                HyprlandEvent::Urgent {
                    address: "0x1122".to_owned(),
                },
                HyprlandEvent::ConfigReloaded,
            ]
        );

        // Verify instant Drop shutdown while connected & idle (eventfd wakes poll immediately).
        let drop_start = Instant::now();
        drop(handle);
        assert!(
            drop_start.elapsed() < Duration::from_millis(100),
            "expected instant eventfd shutdown, took {:?}",
            drop_start.elapsed()
        );

        drop(peer);
        drop(listener);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn zero_polling_listener_shuts_down_instantly_during_reconnect_backoff() {
        let missing_sock = std::env::temp_dir().join(format!(
            "waywi-hypr-missing-{}-{}.sock",
            std::process::id(),
            STREAM_SEQ.fetch_add(1, Ordering::Relaxed)
        ));
        let (tx, _rx) = calloop::channel::channel::<HyprlandEvent>();
        let handle = HyprlandListenerHandle::spawn(missing_sock, tx);
        std::thread::sleep(Duration::from_millis(15));

        let drop_start = Instant::now();
        drop(handle);
        assert!(
            drop_start.elapsed() < Duration::from_millis(100),
            "expected instant shutdown during backoff, took {:?}",
            drop_start.elapsed()
        );
    }
}
