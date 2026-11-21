use std::io::{Read, Write};
use std::os::unix::net::UnixStream;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use crate::error::{Result, WaywiError};

use super::types::{
    ActiveWorkspaceJson, ClientInfo, CursorPos, HyprlandDevices, HyprlandEvent, HyprlandOption,
    HyprlandSockets, HyprlandVersion, MonitorInfo, WorkspaceId, WorkspaceInfo,
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
    lua_probed: Arc<AtomicBool>,
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
            lua_probed: Arc::new(AtomicBool::new(false)),
        })
    }

    #[must_use]
    pub fn with_sockets(sockets: HyprlandSockets) -> Self {
        Self {
            sockets,
            timeout: DEFAULT_TIMEOUT,
            lua_mode: Arc::new(AtomicBool::new(false)),
            lua_probed: Arc::new(AtomicBool::new(false)),
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

    /// Queries all open client windows (`j/clients`).
    pub fn clients(&self) -> Result<Vec<ClientInfo>> {
        let raw = self.request("j/clients")?;
        serde_json::from_str(&raw).map_err(|e| {
            WaywiError::IpcError(format!("failed to parse Hyprland clients JSON: {e}"))
        })
    }

    /// Queries connected monitors (`j/monitors`).
    pub fn monitors(&self) -> Result<Vec<MonitorInfo>> {
        let raw = self.request("j/monitors")?;
        serde_json::from_str(&raw).map_err(|e| {
            WaywiError::IpcError(format!("failed to parse Hyprland monitors JSON: {e}"))
        })
    }

    /// Queries input devices (`j/devices`), including active keyboard XKB layouts and mice.
    pub fn devices(&self) -> Result<HyprlandDevices> {
        let raw = self.request("j/devices")?;
        serde_json::from_str(&raw).map_err(|e| {
            WaywiError::IpcError(format!("failed to parse Hyprland devices JSON: {e}"))
        })
    }

    /// Queries running Hyprland build/version metadata (`j/version`).
    pub fn version(&self) -> Result<HyprlandVersion> {
        let raw = self.request("j/version")?;
        serde_json::from_str(&raw).map_err(|e| {
            WaywiError::IpcError(format!("failed to parse Hyprland version JSON: {e}"))
        })
    }

    /// Queries the current global compositor cursor coordinates (`j/cursorpos`).
    pub fn cursor_pos(&self) -> Result<CursorPos> {
        let raw = self.request("j/cursorpos")?;
        serde_json::from_str(&raw).map_err(|e| {
            WaywiError::IpcError(format!("failed to parse Hyprland cursorpos JSON: {e}"))
        })
    }

    /// Queries a Hyprland runtime option (`j/getoption <name>`), e.g. `"general:gaps_out"` or
    /// `"decoration:rounding"`.
    pub fn get_option(&self, name: &str) -> Result<HyprlandOption> {
        validate_token("option", name)?;
        let raw = self.request(&format!("j/getoption {name}"))?;
        serde_json::from_str(&raw).map_err(|e| {
            WaywiError::IpcError(format!(
                "failed to parse Hyprland option `{name}` JSON: {e}"
            ))
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

    /// Switches to a named workspace (`name:<name>` or `special:<name>`) without spawning a shell,
    /// supporting both Hyprlang and HyprLua v0.55+ dispatch syntax.
    pub fn switch_workspace_name(&self, name: &str) -> Result<()> {
        validate_arg(name)?;
        if name.is_empty() {
            return Err(WaywiError::IpcError(
                "Hyprland workspace name must not be empty".to_owned(),
            ));
        }
        let target = if name.starts_with("special:") || name.starts_with("name:") {
            name.to_owned()
        } else {
            format!("name:{name}")
        };
        let escaped = escape_lua_string(&target);
        self.dispatch_with_lua_fallback(
            "workspace",
            &target,
            &format!("dispatch hl.dsp.focus({{ workspace = \"{escaped}\" }})"),
        )
    }

    /// Focuses a window matching `selector` (e.g. `"0x55a8f1234560"`, `"address:0x55a8f1234560"`,
    /// `"class:firefox"`, `"pid:1234"`). Raw `"0x..."` addresses are prefixed with `"address:"`.
    pub fn focus_window(&self, selector: &str) -> Result<()> {
        let sel = normalize_window_selector(selector)?;
        let escaped = escape_lua_string(&sel);
        self.dispatch_with_lua_fallback(
            "focuswindow",
            &sel,
            &format!("dispatch hl.dsp.focus({{ window = \"{escaped}\" }})"),
        )
    }

    /// Closes a window matching `selector` (raw `"0x..."` addresses are prefixed with
    /// `"address:"`).
    pub fn close_window(&self, selector: &str) -> Result<()> {
        let sel = normalize_window_selector(selector)?;
        let escaped = escape_lua_string(&sel);
        self.dispatch_with_lua_fallback(
            "closewindow",
            &sel,
            &format!("dispatch hl.dsp.window.close({{ window = \"{escaped}\" }})"),
        )
    }

    /// Moves a window matching `selector` to `workspace` (e.g. `"2"`, `"name:code"`,
    /// `"special:magic"`). Set `silent = true` to move without switching workspace focus.
    pub fn move_window_to_workspace(
        &self,
        selector: &str,
        workspace: &str,
        silent: bool,
    ) -> Result<()> {
        let sel = normalize_window_selector(selector)?;
        validate_arg(workspace)?;
        if workspace.is_empty() {
            return Err(WaywiError::IpcError(
                "Hyprland target workspace must not be empty".to_owned(),
            ));
        }
        let dispatcher = if silent {
            "movetoworkspacesilent"
        } else {
            "movetoworkspace"
        };
        let arg = format!("{workspace},{sel}");
        let escaped_sel = escape_lua_string(&sel);
        let follow = if silent { "false" } else { "true" };
        let lua_ws = if let Ok(id) = workspace.parse::<i32>() {
            id.to_string()
        } else {
            format!("\"{}\"", escape_lua_string(workspace))
        };
        let lua_cmd = format!(
            "dispatch hl.dsp.window.move({{ window = \"{escaped_sel}\", workspace = {lua_ws}, follow = {follow} }})"
        );
        self.dispatch_with_lua_fallback(dispatcher, &arg, &lua_cmd)
    }

    /// Toggles a special workspace (scratchpad). Pass `None` for the default special workspace.
    pub fn toggle_special_workspace(&self, name: Option<&str>) -> Result<()> {
        match name {
            Some(n) if !n.is_empty() => {
                validate_arg(n)?;
                let escaped = escape_lua_string(n);
                self.dispatch_with_lua_fallback(
                    "togglespecialworkspace",
                    n,
                    &format!("dispatch hl.dsp.workspace.toggle_special(\"{escaped}\")"),
                )
            }
            _ => self.dispatch_with_lua_fallback(
                "togglespecialworkspace",
                "",
                "dispatch hl.dsp.workspace.toggle_special()",
            ),
        }
    }

    /// Sets a Hyprland runtime configuration option (`keyword <key> <value>` on legacy Hyprlang,
    /// automatically falling back to `eval hl.config({ ... })` on Hyprland v0.55+ Lua / v0.57+).
    pub fn keyword(&self, key: &str, value: &str) -> Result<()> {
        validate_token("keyword", key)?;
        validate_arg(value)?;
        let lua_cmd = format_lua_config_eval(key, value)?;
        self.ensure_config_provider_probed();
        if !self.lua_mode.load(Ordering::Relaxed) {
            let legacy_cmd = format!("keyword {key} {value}");
            match self.send_ok_command(&legacy_cmd, "keyword") {
                Ok(()) => return Ok(()),
                Err(WaywiError::IpcError(msg)) if is_hyprlua_keyword_error(&msg) => {
                    self.lua_mode.store(true, Ordering::Relaxed);
                }
                Err(e) => return Err(e),
            }
        }
        self.send_ok_command(&lua_cmd, "eval")
    }

    /// Evaluates a Lua expression inside Hyprland's embedded Lua runtime (`eval <lua_code>`),
    /// supported on Hyprland v0.55+ Lua and v0.57+.
    ///
    /// # Trust boundary
    /// Runs on Hyprland's main thread; callers must never pass untrusted input to `eval`.
    pub fn eval(&self, lua_code: &str) -> Result<String> {
        let trimmed = lua_code.trim();
        if trimmed.is_empty() {
            return Err(WaywiError::IpcError(
                "Hyprland eval expression must not be empty".to_owned(),
            ));
        }
        self.request(&format!("eval {trimmed}"))
    }

    /// Executes multiple Hyprland IPC commands in a single Unix socket roundtrip using
    /// `[[BATCH]]cmd1;cmd2;...`, returning one trimmed response per command.
    pub fn batch(&self, commands: &[&str]) -> Result<Vec<String>> {
        if commands.is_empty() {
            return Ok(Vec::new());
        }
        let mut payload =
            String::with_capacity(9 + commands.iter().map(|c| c.len() + 1).sum::<usize>());
        payload.push_str("[[BATCH]]");
        for (idx, cmd) in commands.iter().enumerate() {
            validate_arg(cmd)?;
            if cmd.contains(';') {
                return Err(WaywiError::IpcError(
                    "Hyprland batch command entries must not contain `;`".to_owned(),
                ));
            }
            if idx > 0 {
                payload.push(';');
            }
            payload.push_str(cmd);
        }
        let raw = self.request(&payload)?;
        Ok(raw
            .split("\n\n\n")
            .map(|part| part.trim().to_owned())
            .collect())
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
        self.ensure_config_provider_probed();
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

    /// Detects the config provider once per client via `j/systeminfo` and seeds `lua_mode` when
    /// the compositor reports `configProvider: lua` (Hyprland v0.55+ Lua configs, mandatory on
    /// v0.57+), so the first dispatch/keyword skips the guaranteed-failing legacy request. Any
    /// detection failure (missing field, older version, I/O error) keeps `lua_mode` untouched and
    /// leaves the error-based fallback as the authoritative decision path.
    fn ensure_config_provider_probed(&self) {
        if self.lua_probed.swap(true, Ordering::Relaxed) {
            return;
        }
        if let Ok(info) = self.request("j/systeminfo")
            && systeminfo_says_lua(&info)
        {
            self.lua_mode.store(true, Ordering::Relaxed);
        }
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

fn normalize_window_selector(selector: &str) -> Result<String> {
    validate_arg(selector)?;
    let trimmed = selector.trim();
    if trimmed.is_empty() {
        return Err(WaywiError::IpcError(
            "Hyprland window selector must not be empty".to_owned(),
        ));
    }
    if trimmed.starts_with("0x") || trimmed.starts_with("0X") {
        Ok(format!("address:{trimmed}"))
    } else {
        Ok(trimmed.to_owned())
    }
}

fn escape_lua_string(input: &str) -> String {
    let mut out = String::with_capacity(input.len());
    for ch in input.chars() {
        match ch {
            '\\' => out.push_str("\\\\"),
            '"' => out.push_str("\\\""),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            other => out.push(other),
        }
    }
    out
}

fn is_hyprlua_dispatch_error(msg: &str) -> bool {
    msg.contains("hl.dispatch") || msg.contains("syntax might need to be updated")
}

/// Returns `true` when `j/systeminfo` text output reports `configProvider: lua`.
fn systeminfo_says_lua(info: &str) -> bool {
    info.lines()
        .filter_map(|line| line.split_once(':'))
        .any(|(key, value)| {
            key.trim().eq_ignore_ascii_case("configProvider")
                && value.trim().eq_ignore_ascii_case("lua")
        })
}

fn is_hyprlua_keyword_error(msg: &str) -> bool {
    msg.contains("non-legacy")
        || msg.contains("Use eval")
        || msg.contains("hl.config")
        || is_hyprlua_dispatch_error(msg)
}

fn format_lua_config_eval(key: &str, value: &str) -> Result<String> {
    let segments: Vec<&str> = key.split(':').collect();
    if segments.is_empty() || segments.iter().any(|s| s.is_empty()) {
        return Err(WaywiError::IpcError(format!(
            "invalid Hyprland option path `{key}`"
        )));
    }
    let mut out = String::with_capacity(key.len() + value.len() + 32);
    out.push_str("eval hl.config(");
    for seg in &segments {
        out.push_str("{ ");
        if is_lua_identifier(seg) {
            out.push_str(seg);
        } else {
            out.push_str("[\"");
            out.push_str(&escape_lua_string(seg));
            out.push_str("\"]");
        }
        out.push_str(" = ");
    }
    out.push_str(&format_lua_literal(value));
    for _ in &segments {
        out.push_str(" }");
    }
    out.push(')');
    Ok(out)
}

fn is_lua_identifier(seg: &str) -> bool {
    let mut bytes = seg.bytes();
    let Some(first) = bytes.next() else {
        return false;
    };
    if !(first.is_ascii_alphabetic() || first == b'_') {
        return false;
    }
    if !bytes.all(|b| b.is_ascii_alphanumeric() || b == b'_') {
        return false;
    }
    !matches!(
        seg,
        "and"
            | "break"
            | "do"
            | "else"
            | "elseif"
            | "end"
            | "false"
            | "for"
            | "function"
            | "goto"
            | "if"
            | "in"
            | "local"
            | "nil"
            | "not"
            | "or"
            | "repeat"
            | "return"
            | "then"
            | "true"
            | "until"
            | "while"
    )
}

fn format_lua_literal(value: &str) -> String {
    let trimmed = value.trim();
    if trimmed == "true" || trimmed == "false" {
        return trimmed.to_owned();
    }
    if trimmed.parse::<i64>().is_ok() {
        return trimmed.to_owned();
    }
    if trimmed
        .parse::<f64>()
        .is_ok_and(|f| f.is_finite() && !trimmed.eq_ignore_ascii_case("nan"))
    {
        return trimmed.to_owned();
    }
    format!("\"{}\"", escape_lua_string(trimmed))
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
        HyprlandEvent::RenameWorkspace { id, name } => {
            if let Some(ws) = workspaces.iter_mut().find(|w| w.id == *id)
                && ws.name != *name
            {
                ws.name = name.clone();
                return true;
            }
            false
        }
        _ => false,
    }
}

#[cfg(test)]
mod tests {
    use std::os::unix::net::UnixListener;
    use std::sync::atomic::{AtomicU64, Ordering};

    use super::*;

    static SOCK_SEQ: AtomicU64 = AtomicU64::new(0);

    #[test]
    fn clients_and_active_client_parse_ipc_responses_over_unix_socket() {
        let dir = std::env::temp_dir().join(format!(
            "waywi-hypr-test-{}-{}",
            std::process::id(),
            SOCK_SEQ.fetch_add(1, Ordering::Relaxed)
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let cmd_sock = dir.join(".socket.sock");
        let ev_sock = dir.join(".socket2.sock");
        let listener = UnixListener::bind(&cmd_sock).unwrap();

        let server = std::thread::spawn(move || {
            for _ in 0..2 {
                let (mut conn, _) = listener.accept().unwrap();
                let mut req = [0u8; 64];
                let n = conn.read(&mut req).unwrap();
                let cmd = std::str::from_utf8(&req[..n]).unwrap();
                let resp = match cmd {
                    "j/activewindow" => {
                        r#"{"address":"0xabc","class":"kitty","title":"shell","pid":1234,"fullscreen":0,"contentType":"none","tags":["term"]}"#
                    }
                    "j/clients" => {
                        r#"[{"address":"0xabc","class":"kitty","title":"shell","pid":1234,"fullscreen":0,"tags":["term"]},{"address":"0xdef","class":"org.example.Game","title":"Game","pid":5678,"fullscreen":2,"contentType":"game","xwayland":true}]"#
                    }
                    other => panic!("unexpected command: {other}"),
                };
                conn.write_all(resp.as_bytes()).unwrap();
            }
        });

        let client = HyprlandClient::with_sockets(HyprlandSockets {
            command_socket: cmd_sock.clone(),
            event_socket: ev_sock,
        });

        let active = client.active_client().unwrap().expect("active window");
        assert_eq!(active.pid, Some(1234));
        assert_eq!(active.content_type, None);
        assert_eq!(active.tags, vec!["term"]);

        let all = client.clients().unwrap();
        assert_eq!(all.len(), 2);
        assert_eq!(all[1].class, "org.example.Game");
        assert_eq!(all[1].pid, Some(5678));
        assert_eq!(all[1].content_type.as_deref(), Some("game"));
        assert!(all[1].fullscreen);
        assert!(all[1].xwayland);

        server.join().unwrap();
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn batch_queries_options_and_hyprlua_window_dispatchers_work_over_unix_socket() {
        let dir = std::env::temp_dir().join(format!(
            "waywi-hypr-extended-{}-{}",
            std::process::id(),
            SOCK_SEQ.fetch_add(1, Ordering::Relaxed)
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let cmd_sock = dir.join(".socket.sock");
        let ev_sock = dir.join(".socket2.sock");
        let listener = UnixListener::bind(&cmd_sock).unwrap();

        let server = std::thread::spawn(move || {
            let mut seen = Vec::new();
            for _ in 0..12 {
                let (mut conn, _) = listener.accept().unwrap();
                let mut req = [0u8; 512];
                let n = conn.read(&mut req).unwrap();
                let cmd = std::str::from_utf8(&req[..n]).unwrap().to_owned();
                let resp: &str = match cmd.as_str() {
                    "j/systeminfo" => "System Information:\nconfigProvider: hypr\n",
                    "j/cursorpos" => r#"{"x":1206,"y":515}"#,
                    "j/version" => {
                        r#"{"branch":"v0.56.2","version":"0.56.2","tag":"v0.56.2","dirty":false,"flags":[]}"#
                    }
                    "j/devices" => {
                        r#"{"keyboards":[{"name":"main-kb","layout":"us,id","active_layout_index":1,"active_keymap":"Indonesian","main":true}],"mice":[{"name":"gaming-mouse","defaultSpeed":0.25,"scrollFactor":1.0}]}"#
                    }
                    "j/getoption general:gaps_out" => {
                        r#"{"option":"general:gaps_out","css":"4 4 4 4","set":true}"#
                    }
                    "[[BATCH]]j/cursorpos;keyword decoration:blur:enabled false" => {
                        "{\"x\":10,\"y\":20}\n\n\nok"
                    }
                    "keyword animations:enabled false" => "ok",
                    "dispatch focuswindow address:0xabc" => {
                        "error: [string \"return hl.dispatch(...)\"]: your syntax might need to be updated."
                    }
                    "dispatch hl.dsp.focus({ window = \"address:0xabc\" })" => "ok",
                    "dispatch hl.dsp.window.close({ window = \"address:0xdef\" })" => "ok",
                    "dispatch hl.dsp.window.move({ window = \"address:0xabc\", workspace = 2, follow = false })" => {
                        "ok"
                    }
                    "dispatch hl.dsp.workspace.toggle_special(\"magic\")" => "ok",
                    other => panic!("unexpected command: {other}"),
                };
                seen.push(cmd);
                conn.write_all(resp.as_bytes()).unwrap();
            }
            seen
        });

        let client = HyprlandClient::with_sockets(HyprlandSockets {
            command_socket: cmd_sock,
            event_socket: ev_sock,
        });

        assert_eq!(client.cursor_pos().unwrap(), CursorPos { x: 1206, y: 515 });
        let ver = client.version().unwrap();
        assert_eq!(ver.version, "0.56.2");

        let devs = client.devices().unwrap();
        assert_eq!(devs.keyboards.len(), 1);
        assert!(devs.keyboards[0].main);
        assert_eq!(devs.keyboards[0].active_layout_index, 1);
        assert_eq!(devs.mice.len(), 1);

        let opt = client.get_option("general:gaps_out").unwrap();
        assert_eq!(opt.value.as_str(), Some("4 4 4 4"));

        let batch_out = client
            .batch(&["j/cursorpos", "keyword decoration:blur:enabled false"])
            .unwrap();
        assert_eq!(batch_out, vec![r#"{"x":10,"y":20}"#, "ok"]);
        assert!(client.batch(&["cmd1;cmd2"]).is_err());

        client.keyword("animations:enabled", "false").unwrap();
        // Probe reports a non-Lua provider, so focus_window still hits the v0.57 hydration error
        // first, then uses Lua directly for the rest:
        client.focus_window("0xabc").unwrap();
        client.close_window("0xdef").unwrap();
        client.move_window_to_workspace("0xabc", "2", true).unwrap();
        client.toggle_special_workspace(Some("magic")).unwrap();

        let seen = server.join().unwrap();
        assert_eq!(seen.len(), 12);

        // Verify RenameWorkspace updates WorkspaceInfo in place.
        let mut wss = vec![WorkspaceInfo {
            id: WorkspaceId(2),
            name: "2".to_owned(),
            monitor: "DP-1".to_owned(),
            windows: 1,
            focused: false,
        }];
        assert!(apply_workspace_event(
            &mut wss,
            &HyprlandEvent::RenameWorkspace {
                id: WorkspaceId(2),
                name: "dev".to_owned(),
            }
        ));
        assert_eq!(wss[0].name, "dev");

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn keyword_falls_back_to_hyprland_0_57_lua_eval_and_escapes_dotted_keys() {
        assert_eq!(
            format_lua_config_eval("decoration:blur:passes", "3").unwrap(),
            "eval hl.config({ decoration = { blur = { passes = 3 } } })"
        );
        assert_eq!(
            format_lua_config_eval("animations:enabled", "false").unwrap(),
            "eval hl.config({ animations = { enabled = false } })"
        );
        assert_eq!(
            format_lua_config_eval("input:sensitivity", "0.25").unwrap(),
            "eval hl.config({ input = { sensitivity = 0.25 } })"
        );
        assert_eq!(
            format_lua_config_eval("general:col.active_border", "rgba(cba6f7ff)").unwrap(),
            "eval hl.config({ general = { [\"col.active_border\"] = \"rgba(cba6f7ff)\" } })"
        );
        assert!(format_lua_config_eval("general::gaps_out", "4").is_err());

        let dir = std::env::temp_dir().join(format!(
            "waywi-hypr-v057-{}-{}",
            std::process::id(),
            SOCK_SEQ.fetch_add(1, Ordering::Relaxed)
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let cmd_sock = dir.join(".socket.sock");
        let ev_sock = dir.join(".socket2.sock");
        let listener = UnixListener::bind(&cmd_sock).unwrap();

        let server = std::thread::spawn(move || {
            let mut seen = Vec::new();
            for _ in 0..5 {
                let (mut conn, _) = listener.accept().unwrap();
                let mut req = [0u8; 512];
                let n = conn.read(&mut req).unwrap();
                let cmd = std::str::from_utf8(&req[..n]).unwrap().to_owned();
                let resp: &str = match cmd.as_str() {
                    "j/systeminfo" => "System Information:\nconfigProvider: hypr\n",
                    "keyword animations:enabled false" => {
                        "error: keyword can't work with non-legacy parsers. Use eval."
                    }
                    "eval hl.config({ animations = { enabled = false } })" => "ok",
                    "eval hl.config({ decoration = { blur = { passes = 2 } } })" => "ok",
                    "eval return hl.reload_config()" => "ok",
                    other => panic!("unexpected v0.57 command: {other}"),
                };
                seen.push(cmd);
                conn.write_all(resp.as_bytes()).unwrap();
            }
            seen
        });

        let client = HyprlandClient::with_sockets(HyprlandSockets {
            command_socket: cmd_sock,
            event_socket: ev_sock,
        });

        // Probe reports a non-Lua provider, so the first keyword call hits the v0.57 hydration
        // error -> flips lua_mode -> retries via eval.
        client.keyword("animations:enabled", "false").unwrap();
        // Second keyword call goes straight to eval in 1 roundtrip.
        client.keyword("decoration:blur:passes", "2").unwrap();
        assert_eq!(client.eval("return hl.reload_config()").unwrap(), "ok");
        assert!(client.eval("   ").is_err());

        let seen = server.join().unwrap();
        assert_eq!(seen.len(), 5);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn systeminfo_probe_reports_lua_provider_and_skips_legacy_requests() {
        // Pure parser checks first:
        assert!(systeminfo_says_lua(
            "System Information:\nconfigProvider: lua\n"
        ));
        assert!(systeminfo_says_lua("configProvider:  LUA"));
        assert!(!systeminfo_says_lua("configProvider: hypr\n"));
        assert!(!systeminfo_says_lua("some text without the field"));
        assert!(!systeminfo_says_lua("unknown request"));

        // Then the full client flow: the probe flips lua_mode before any dispatch, so every
        // request goes straight to the Lua syntax in one roundtrip.
        let dir = std::env::temp_dir().join(format!(
            "waywi-hypr-fastpath-{}-{}",
            std::process::id(),
            SOCK_SEQ.fetch_add(1, Ordering::Relaxed)
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let cmd_sock = dir.join(".socket.sock");
        let ev_sock = dir.join(".socket2.sock");
        let listener = UnixListener::bind(&cmd_sock).unwrap();

        let server = std::thread::spawn(move || {
            let mut seen = Vec::new();
            for _ in 0..3 {
                let (mut conn, _) = listener.accept().unwrap();
                let mut req = [0u8; 512];
                let n = conn.read(&mut req).unwrap();
                let cmd = std::str::from_utf8(&req[..n]).unwrap().to_owned();
                let resp: &str = match cmd.as_str() {
                    "j/systeminfo" => "System Information:\nconfigProvider: lua\n",
                    "eval hl.config({ animations = { enabled = false } })" => "ok",
                    "dispatch hl.dsp.focus({ window = \"address:0xabc\" })" => "ok",
                    other => panic!("unexpected legacy command: {other}"),
                };
                seen.push(cmd);
                conn.write_all(resp.as_bytes()).unwrap();
            }
            seen
        });

        let client = HyprlandClient::with_sockets(HyprlandSockets {
            command_socket: cmd_sock,
            event_socket: ev_sock,
        });

        client.keyword("animations:enabled", "false").unwrap();
        client.focus_window("0xabc").unwrap();

        let seen = server.join().unwrap();
        assert_eq!(
            seen,
            vec![
                "j/systeminfo".to_owned(),
                "eval hl.config({ animations = { enabled = false } })".to_owned(),
                "dispatch hl.dsp.focus({ window = \"address:0xabc\" })".to_owned(),
            ]
        );
        let _ = std::fs::remove_dir_all(&dir);
    }
}
