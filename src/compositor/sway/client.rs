use std::io::{Read, Write};
use std::os::unix::net::UnixStream;
use std::path::{Path, PathBuf};
use std::time::Duration;

use crate::error::{Result, WaywiError};

use super::types::{CommandOutcome, SwayOutput, SwayVersion, SwayWorkspace, WorkspaceRef};

pub(super) const SWAY_MAGIC: &[u8; 6] = b"i3-ipc";
pub(super) const DEFAULT_TIMEOUT: Duration = Duration::from_secs(2);
const MAX_PAYLOAD_BYTES: usize = 8 * 1024 * 1024;

// Sway IPC request types.
const MSG_RUN_COMMAND: u32 = 0;
const MSG_GET_WORKSPACES: u32 = 1;
pub(super) const MSG_SUBSCRIBE: u32 = 2;
const MSG_GET_OUTPUTS: u32 = 3;
const MSG_GET_TREE: u32 = 4;
const MSG_GET_VERSION: u32 = 7;

/// Synchronous Sway IPC client implementing documented `i3-ipc` binary framing over `$SWAYSOCK`.
#[derive(Debug, Clone)]
pub struct SwayClient {
    socket_path: PathBuf,
    timeout: Duration,
}

impl SwayClient {
    pub fn from_env() -> Result<Self> {
        let raw = std::env::var_os("SWAYSOCK")
            .or_else(|| std::env::var_os("I3SOCK"))
            .ok_or_else(|| WaywiError::ProviderUnavailable("SWAYSOCK is not set".to_owned()))?;
        let path = PathBuf::from(raw);
        if !path.exists() {
            return Err(WaywiError::ProviderUnavailable(format!(
                "Sway socket does not exist at {}",
                path.display()
            )));
        }
        Ok(Self {
            socket_path: path,
            timeout: DEFAULT_TIMEOUT,
        })
    }

    #[must_use]
    pub fn with_socket(socket_path: impl AsRef<Path>) -> Self {
        Self {
            socket_path: socket_path.as_ref().to_path_buf(),
            timeout: DEFAULT_TIMEOUT,
        }
    }

    #[must_use]
    pub fn with_timeout(mut self, timeout: Duration) -> Self {
        self.timeout = timeout.max(Duration::from_millis(50));
        self
    }

    #[must_use]
    pub fn socket_path(&self) -> &Path {
        &self.socket_path
    }

    pub fn workspaces(&self) -> Result<Vec<SwayWorkspace>> {
        let (_, json) = self.request(MSG_GET_WORKSPACES, b"")?;
        serde_json::from_slice(&json)
            .map_err(|e| WaywiError::IpcError(format!("failed to parse Sway workspaces: {e}")))
    }

    pub fn outputs(&self) -> Result<Vec<SwayOutput>> {
        let (_, json) = self.request(MSG_GET_OUTPUTS, b"")?;
        serde_json::from_slice(&json)
            .map_err(|e| WaywiError::IpcError(format!("failed to parse Sway outputs: {e}")))
    }

    pub fn tree(&self) -> Result<serde_json::Value> {
        let (_, json) = self.request(MSG_GET_TREE, b"")?;
        serde_json::from_slice(&json)
            .map_err(|e| WaywiError::IpcError(format!("failed to parse Sway tree: {e}")))
    }

    pub fn version(&self) -> Result<SwayVersion> {
        let (_, json) = self.request(MSG_GET_VERSION, b"")?;
        serde_json::from_slice(&json)
            .map_err(|e| WaywiError::IpcError(format!("failed to parse Sway version: {e}")))
    }

    /// Switches workspace safely using structured escaping (never `sh -c`).
    pub fn switch_workspace(&self, target: WorkspaceRef<'_>) -> Result<()> {
        let cmd = match target {
            WorkspaceRef::Number(n) => format!("workspace number {n}"),
            WorkspaceRef::Name(name) => {
                if name.bytes().any(|b| b == 0 || b == b'\n' || b == b'\r') {
                    return Err(WaywiError::IpcError(
                        "workspace name contains control characters".to_owned(),
                    ));
                }
                let escaped = name.replace('\\', "\\\\").replace('"', "\\\"");
                format!("workspace \"{escaped}\"")
            }
        };
        self.raw_command(&cmd)
    }

    /// Switches all keyboards to the next XKB layout (`input type:keyboard xkb_switch_layout next`).
    pub fn next_keyboard_layout(&self) -> Result<()> {
        self.raw_command("input type:keyboard xkb_switch_layout next")
    }

    /// Switches all keyboards to a specific zero-based XKB layout index.
    pub fn switch_keyboard_layout(&self, index: u32) -> Result<()> {
        self.raw_command(&format!("input type:keyboard xkb_switch_layout {index}"))
    }

    /// Low-level raw Sway `RUN_COMMAND` IPC invocation without shell execution.
    pub fn raw_command(&self, command: &str) -> Result<()> {
        let (_, json) = self.request(MSG_RUN_COMMAND, command.as_bytes())?;
        let outcomes: Vec<CommandOutcome> = serde_json::from_slice(&json).map_err(|e| {
            WaywiError::IpcError(format!("failed to parse Sway command reply: {e}"))
        })?;
        for out in outcomes {
            if !out.success {
                return Err(WaywiError::IpcError(
                    out.error
                        .unwrap_or_else(|| "Sway command failed".to_owned()),
                ));
            }
        }
        Ok(())
    }

    fn request(&self, msg_type: u32, payload: &[u8]) -> Result<(u32, Vec<u8>)> {
        let mut stream = UnixStream::connect(&self.socket_path).map_err(|e| {
            WaywiError::IpcError(format!(
                "failed to connect to Sway socket {}: {e}",
                self.socket_path.display()
            ))
        })?;
        stream.set_read_timeout(Some(self.timeout))?;
        stream.set_write_timeout(Some(self.timeout))?;

        write_frame(&mut stream, msg_type, payload)?;
        read_frame(&mut stream)
    }
}

/// Encodes one Sway `i3-ipc` binary frame (`"i3-ipc" || u32 len || u32 type || payload`).
pub fn encode_frame(msg_type: u32, payload: &[u8]) -> Vec<u8> {
    let mut frame = Vec::with_capacity(14 + payload.len());
    frame.extend_from_slice(SWAY_MAGIC);
    frame.extend_from_slice(&(payload.len() as u32).to_le_bytes());
    frame.extend_from_slice(&msg_type.to_le_bytes());
    frame.extend_from_slice(payload);
    frame
}

pub(crate) fn write_frame(stream: &mut impl Write, msg_type: u32, payload: &[u8]) -> Result<()> {
    let frame = encode_frame(msg_type, payload);
    stream.write_all(&frame)?;
    stream.flush()?;
    Ok(())
}

pub(crate) fn read_frame(stream: &mut impl Read) -> Result<(u32, Vec<u8>)> {
    let mut header = [0u8; 14];
    stream.read_exact(&mut header)?;
    if &header[..6] != SWAY_MAGIC {
        return Err(WaywiError::IpcError(
            "invalid Sway IPC frame magic header".to_owned(),
        ));
    }
    let len = u32::from_le_bytes([header[6], header[7], header[8], header[9]]) as usize;
    let msg_type = u32::from_le_bytes([header[10], header[11], header[12], header[13]]);
    if len > MAX_PAYLOAD_BYTES {
        return Err(WaywiError::IpcError(format!(
            "Sway IPC frame length {len} exceeds {MAX_PAYLOAD_BYTES} byte limit"
        )));
    }
    let mut payload = vec![0u8; len];
    if len > 0 {
        stream.read_exact(&mut payload)?;
    }
    Ok((msg_type, payload))
}
