use std::io::{Read, Write};
use std::os::unix::net::UnixStream;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use crate::error::{Result, WaywiError};

use super::types::{
    ActiveWorkspaceJson, ClientInfo, HyprlandEvent, HyprlandSockets, MonitorInfo, WorkspaceId,
    WorkspaceInfo,
};

const DEFAULT_TIMEOUT: Duration = Duration::from_secs(2);
const MAX_IPC_RESPONSE_BYTES: usize = 4 * 1024 * 1024;

/// Synchronous, bounded request/response client for Hyprland's command socket (`.socket.sock`).
///
/// Opens the Unix socket for each request and closes it immediately afterward so no idle file
/// descriptors are held.
#[derive(Debug, Clone)]
pub struct HyprlandClient {
    sockets: HyprlandSockets,
    timeout: Duration,
    lua_mode: Arc<AtomicBool>,
}

impl HyprlandClient {
    pub fn from_env() -> Result<Self> {
        let sockets = HyprlandSockets::discover()?;
        if !sockets.command_socket.exists() {
            return Err(WaywiError::ProviderUnavailable(format!(
                "Hyprland command socket not found at {}",
                sockets.command_socket.display()
            )));
        }
        Ok(Self {
            sockets,
            timeout: DEFAULT_TIMEOUT,
            lua_mode: Arc::new(AtomicBool::new(false)),
        })
    }

    #[must_use]
    pub fn with_sockets(sockets: HyprlandSockets) -> Self {
        Self {
            sockets,
            timeout: DEFAULT_TIMEOUT,
            lua_mode: Arc::new(AtomicBool::new(false)),
        }
    }

    #[must_use]
    pub fn with_timeout(mut self, timeout: Duration) -> Self {
        self.timeout = timeout.max(Duration::from_millis(50));
        self
    }

    #[must_use]
    pub fn sockets(&self) -> &HyprlandSockets {
        &self.sockets
    }

    /// Queries all workspaces (`j/workspaces`), marking the active workspace as `focused`.
    pub fn workspaces(&self) -> Result<Vec<WorkspaceInfo>> {
        let active_id = self.active_workspace().ok().map(|w| w.id);
        let raw = self.request("j/workspaces")?;
        let mut list: Vec<WorkspaceInfo> = serde_json::from_str(&raw).map_err(|e| {
            WaywiError::IpcError(format!("failed to parse Hyprland workspaces JSON: {e}"))
        })?;
        list.sort_by_key(|w| w.id);
        if let Some(aid) = active_id {
            for w in &mut list {
                w.focused = w.id == aid;
            }
        }
        Ok(list)
    }

    /// Queries the currently active workspace (`j/activeworkspace`).
    pub fn active_workspace(&self) -> Result<WorkspaceInfo> {
        let raw = self.request("j/activeworkspace")?;
        let parsed: ActiveWorkspaceJson = serde_json::from_str(&raw).map_err(|e| {
            WaywiError::IpcError(format!(
                "failed to parse Hyprland activeworkspace JSON: {e}"
            ))
        })?;
        Ok(WorkspaceInfo {
            id: WorkspaceId(parsed.id),
            name: parsed.name,
            monitor: parsed.monitor,
            windows: parsed.windows,
            focused: true,
        })
    }

    /// Queries the active client window (`j/activewindow`).
    pub fn active_client(&self) -> Result<Option<ClientInfo>> {
        let raw = self.request("j/activewindow")?;
        let trimmed = raw.trim();
        if trimmed == "{}" || trimmed.is_empty() {
            return Ok(None);
        }
        let client: ClientInfo = serde_json::from_str(trimmed).map_err(|e| {
            WaywiError::IpcError(format!("failed to parse Hyprland activewindow JSON: {e}"))
        })?;
        if client.address.is_empty() && client.title.is_empty() && client.class.is_empty() {
            Ok(None)
        } else {
            Ok(Some(client))
        }
    }

    /// Queries connected monitors (`j/monitors`).
    pub fn monitors(&self) -> Result<Vec<MonitorInfo>> {
        let raw = self.request("j/monitors")?;
        serde_json::from_str(&raw).map_err(|e| {
            WaywiError::IpcError(format!("failed to parse Hyprland monitors JSON: {e}"))
        })
    }

    /// Switches to typed workspace ID without spawning a shell.
    pub fn switch_workspace(&self, id: WorkspaceId) -> Result<()> {
        self.dispatch_with_lua_fallback(
            "workspace",
            &id.0.to_string(),
            &format!("dispatch hl.dsp.focus({{ workspace = {} }})", id.0),
        )
    }

    /// Turns monitor DPMS off (`monitor = None` targets all monitors).
    pub fn dpms_off(&self, monitor: Option<&str>) -> Result<()> {
        match monitor {
            Some(m) => {
                validate_token("monitor", m)?;
                self.dispatch_with_lua_fallback(
                    "dpms",
                    &format!("off {m}"),
                    &format!("dispatch hl.dsp.dpms({{ action = \"disable\", monitor = \"{m}\" }})"),
                )
            }
            None => self.dispatch_with_lua_fallback(
                "dpms",
                "off",
                "dispatch hl.dsp.dpms({ action = \"disable\" })",
            ),
        }
    }

    /// Turns monitor DPMS on (`monitor = None` targets all monitors).
    pub fn dpms_on(&self, monitor: Option<&str>) -> Result<()> {
        match monitor {
            Some(m) => {
                validate_token("monitor", m)?;
                self.dispatch_with_lua_fallback(
                    "dpms",
                    &format!("on {m}"),
                    &format!("dispatch hl.dsp.dpms({{ action = \"enable\", monitor = \"{m}\" }})"),
                )
            }
            None => self.dispatch_with_lua_fallback(
                "dpms",
                "on",
                "dispatch hl.dsp.dpms({ action = \"enable\" })",
            ),
        }
    }

    fn dispatch_with_lua_fallback(&self, dispatcher: &str, arg: &str, lua_cmd: &str) -> Result<()> {
        if !self.lua_mode.load(Ordering::Relaxed) {
            match self.dispatch(dispatcher, arg) {
                Ok(()) => return Ok(()),
                Err(WaywiError::IpcError(msg)) if is_hyprlua_dispatch_error(&msg) => {
                    self.lua_mode.store(true, Ordering::Relaxed);
                }
                Err(e) => return Err(e),
            }
        }
        self.send_ok_command(lua_cmd, dispatcher)
    }

    /// Switches all keyboards to the next XKB layout (`switchxkblayout all next`).
    pub fn next_keyboard_layout(&self) -> Result<()> {
        self.switch_keyboard_layout_cmd("all", "next")
    }

    /// Switches all keyboards to a specific zero-based XKB layout index (`switchxkblayout all <index>`).
    pub fn switch_keyboard_layout(&self, index: u32) -> Result<()> {
        self.switch_keyboard_layout_cmd("all", &index.to_string())
    }

    /// Sends a `switchxkblayout <device> <action>` command directly over `.socket.sock`.
    pub fn switch_keyboard_layout_cmd(&self, device: &str, action: &str) -> Result<()> {
        validate_token("device", device)?;
        validate_token("layout", action)?;
        let cmd = format!("switchxkblayout {device} {action}");
        self.send_ok_command(&cmd, "switchxkblayout")
    }

    /// Sends a validated Hyprland `dispatch <dispatcher> <arg>` IPC command directly over
    /// `.socket.sock` (no shell execution).
    pub fn dispatch(&self, dispatcher: &str, arg: &str) -> Result<()> {
        validate_token("dispatcher", dispatcher)?;
        validate_arg(arg)?;
        let cmd = if arg.is_empty() {
            format!("dispatch {dispatcher}")
        } else {
            format!("dispatch {dispatcher} {arg}")
        };
        self.send_ok_command(&cmd, dispatcher)
    }

    fn send_ok_command(&self, cmd: &str, label: &str) -> Result<()> {
        let resp = self.request(cmd)?;
        let trimmed = resp.trim();
        if trimmed == "ok" || trimmed.is_empty() {
            Ok(())
        } else {
            Err(WaywiError::IpcError(format!(
                "Hyprland `{label}` returned: {trimmed}"
            )))
        }
    }

    /// Low-level raw IPC request without shell execution.
    ///
    /// # Trust boundary
    /// Callers must validate untrusted strings before passing them to `raw_command`.
    pub fn raw_command(&self, command: &str) -> Result<String> {
        self.request(command)
    }

    fn request(&self, payload: &str) -> Result<String> {
        if payload.contains('\0') {
            return Err(WaywiError::IpcError(
                "IPC request must not contain NUL bytes".to_owned(),
            ));
        }
        let mut stream = UnixStream::connect(&self.sockets.command_socket).map_err(|e| {
            WaywiError::IpcError(format!(
                "failed to connect to Hyprland socket {}: {e}",
                self.sockets.command_socket.display()
            ))
        })?;
        stream.set_read_timeout(Some(self.timeout))?;
        stream.set_write_timeout(Some(self.timeout))?;
        stream.write_all(payload.as_bytes())?;
        stream.flush()?;

        let mut buf = Vec::with_capacity(1024);
        let mut limited = stream.take((MAX_IPC_RESPONSE_BYTES + 1) as u64);
        limited.read_to_end(&mut buf)?;
        if buf.len() > MAX_IPC_RESPONSE_BYTES {
            return Err(WaywiError::IpcError(
                "Hyprland IPC response exceeded 4 MiB safety limit".to_owned(),
            ));
        }
        String::from_utf8(buf)
            .map_err(|e| WaywiError::IpcError(format!("invalid UTF-8 from Hyprland IPC: {e}")))
    }
}

fn validate_token(field: &str, value: &str) -> Result<()> {
    if value.is_empty()
        || !value
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'_' | b'-' | b'.' | b':'))
    {
        return Err(WaywiError::IpcError(format!(
            "invalid Hyprland {field} token `{value}`"
        )));
    }
    Ok(())
}

fn validate_arg(arg: &str) -> Result<()> {
    if arg.bytes().any(|b| b == 0 || b == b'\n' || b == b'\r') {
        return Err(WaywiError::IpcError(
            "Hyprland argument must not contain control characters".to_owned(),
        ));
    }
    Ok(())
}

fn is_hyprlua_dispatch_error(msg: &str) -> bool {
    msg.contains("hl.dispatch") || msg.contains("syntax might need to be updated")
}

/// Updates a `Vec<WorkspaceInfo>` incrementally from a `HyprlandEvent` with zero extra socket
/// roundtrips. Returns `true` if the list changed.
pub fn apply_workspace_event(workspaces: &mut Vec<WorkspaceInfo>, event: &HyprlandEvent) -> bool {
    match event {
        HyprlandEvent::WorkspaceV2 { id, name } => {
            for ws in workspaces.iter_mut() {
                ws.focused = ws.id == *id;
            }
            if !workspaces.iter().any(|w| w.id == *id) {
                workspaces.push(WorkspaceInfo {
                    id: *id,
                    name: name.clone(),
                    monitor: String::new(),
                    windows: 0,
                    focused: true,
                });
                workspaces.sort_by_key(|w| w.id);
            }
            true
        }
        HyprlandEvent::FocusedMonitorV2 { workspace_id, .. } => {
            let mut changed = false;
            for ws in workspaces.iter_mut() {
                let next = ws.id == *workspace_id;
                if ws.focused != next {
                    ws.focused = next;
                    changed = true;
                }
            }
            changed
        }
        HyprlandEvent::CreateWorkspaceV2 { id, name } => {
            if workspaces.iter().any(|w| w.id == *id) {
                return false;
            }
            workspaces.push(WorkspaceInfo {
                id: *id,
                name: name.clone(),
                monitor: String::new(),
                windows: 0,
                focused: false,
            });
            workspaces.sort_by_key(|w| w.id);
            true
        }
        HyprlandEvent::DestroyWorkspaceV2 { id, .. } => {
            let before = workspaces.len();
            workspaces.retain(|w| w.id != *id);
            workspaces.len() != before
        }
        HyprlandEvent::MoveWorkspaceV2 { id, name, monitor } => {
            if let Some(ws) = workspaces.iter_mut().find(|w| w.id == *id) {
                ws.name = name.clone();
                ws.monitor = monitor.clone();
                true
            } else {
                false
            }
        }
        _ => false,
    }
}
