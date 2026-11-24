use std::io::{Read, Write};
use std::os::unix::net::UnixStream;
use std::path::{Path, PathBuf};
use std::time::Duration;

use crate::compositor::WorkspaceTarget;
use crate::error::{Result, WaywiError};

use super::types::{CommandOutcome, SwayOutput, SwayVersion, SwayWorkspace};

pub(super) const SWAY_MAGIC: &[u8; 6] = b"i3-ipc";
pub(super) const DEFAULT_TIMEOUT: Duration = Duration::from_secs(2);
pub(super) const MAX_PAYLOAD_BYTES: usize = 8 * 1024 * 1024;
const INITIAL_PAYLOAD_CAPACITY: usize = 4096;
const MAX_RELATIVE_STEPS: u32 = 64;

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
    pub fn switch_workspace<'a>(&self, target: impl Into<WorkspaceTarget<'a>>) -> Result<()> {
        let cmd = match target.into() {
            WorkspaceTarget::Id(n) => format!("workspace number {n}"),
            WorkspaceTarget::Name(name) => {
                if name.is_empty() {
                    return Err(WaywiError::IpcError(
                        "Sway workspace name must not be empty".to_owned(),
                    ));
                }
                if name.bytes().any(|b| b == 0 || b == b'\n' || b == b'\r') {
                    return Err(WaywiError::IpcError(
                        "workspace name contains control characters".to_owned(),
                    ));
                }
                let escaped = name.replace('\\', "\\\\").replace('"', "\\\"");
                format!("workspace \"{escaped}\"")
            }
            WorkspaceTarget::Relative {
                delta,
                existing_only,
            } => {
                if delta == 0 {
                    return Ok(());
                }
                let step = match (delta > 0, existing_only) {
                    (true, true) => "workspace next_on_output",
                    (false, true) => "workspace prev_on_output",
                    (true, false) => "workspace next",
                    (false, false) => "workspace prev",
                };
                let steps = delta.unsigned_abs().min(MAX_RELATIVE_STEPS);
                if steps == 1 {
                    step.to_owned()
                } else {
                    let mut chained = String::with_capacity((step.len() + 2) * steps as usize);
                    for i in 0..steps {
                        if i > 0 {
                            chained.push_str("; ");
                        }
                        chained.push_str(step);
                    }
                    chained
                }
            }
            WorkspaceTarget::Previous => "workspace back_and_forth".to_owned(),
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
pub(crate) fn encode_frame(msg_type: u32, payload: &[u8]) -> Vec<u8> {
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
    let mut payload = Vec::with_capacity(len.min(INITIAL_PAYLOAD_CAPACITY));
    if len > 0 {
        let read = stream.by_ref().take(len as u64).read_to_end(&mut payload)?;
        if read != len {
            return Err(WaywiError::IpcError(format!(
                "truncated Sway IPC frame payload: expected {len} bytes, got {read}"
            )));
        }
    }
    Ok((msg_type, payload))
}

#[cfg(test)]
mod tests {
    use std::io::Cursor;
    use std::os::unix::net::UnixListener;
    use std::sync::atomic::{AtomicU64, Ordering};

    use super::*;

    static SWAY_SOCK_SEQ: AtomicU64 = AtomicU64::new(0);

    #[test]
    fn read_frame_validates_magic_limit_and_truncated_payloads_without_eager_oom() {
        let frame = encode_frame(MSG_GET_VERSION, br#"{"major":1}"#);
        let mut cursor = Cursor::new(frame);
        let (msg_type, payload) = read_frame(&mut cursor).unwrap();
        assert_eq!(msg_type, MSG_GET_VERSION);
        assert_eq!(payload, br#"{"major":1}"#);

        // Invalid magic header is rejected.
        let mut bad_magic = Cursor::new(b"notipc\x00\x00\x00\x00\x00\x00\x00\x00".to_vec());
        assert!(read_frame(&mut bad_magic).is_err());

        // Oversized frame length (> 8 MiB) is rejected.
        let mut oversized = Vec::from(&SWAY_MAGIC[..]);
        oversized.extend_from_slice(&((MAX_PAYLOAD_BYTES as u32) + 1).to_le_bytes());
        oversized.extend_from_slice(&0u32.to_le_bytes());
        assert!(read_frame(&mut Cursor::new(oversized)).is_err());

        // Header claiming 4 MiB with only 3 trailing bytes fails cleanly as truncated without
        // pre-allocating 4 MiB.
        let mut truncated = Vec::from(&SWAY_MAGIC[..]);
        truncated.extend_from_slice(&(4u32 * 1024 * 1024).to_le_bytes());
        truncated.extend_from_slice(&MSG_GET_WORKSPACES.to_le_bytes());
        truncated.extend_from_slice(b"abc");
        let err = read_frame(&mut Cursor::new(truncated)).unwrap_err();
        assert!(err.to_string().contains("truncated"));
    }

    #[test]
    fn switch_workspace_handles_all_workspace_target_variants() {
        let dir = std::env::temp_dir().join(format!(
            "waywi-sway-client-{}-{}",
            std::process::id(),
            SWAY_SOCK_SEQ.fetch_add(1, Ordering::Relaxed)
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let sock = dir.join("sway.sock");
        let listener = UnixListener::bind(&sock).unwrap();

        let srv = std::thread::spawn(move || {
            let mut cmds = Vec::new();
            for _ in 0..8 {
                let (mut stream, _) = listener.accept().unwrap();
                let (msg_type, payload) = read_frame(&mut stream).unwrap();
                assert_eq!(msg_type, MSG_RUN_COMMAND);
                cmds.push(String::from_utf8(payload).unwrap());
                write_frame(&mut stream, MSG_RUN_COMMAND, br#"[{"success":true}]"#).unwrap();
            }
            cmds
        });

        let client = SwayClient::with_socket(&sock);
        client.switch_workspace(3).unwrap();
        client.switch_workspace(r#"dev "1""#).unwrap();
        client
            .switch_workspace(WorkspaceTarget::Relative {
                delta: 1,
                existing_only: true,
            })
            .unwrap();
        client
            .switch_workspace(WorkspaceTarget::Relative {
                delta: -1,
                existing_only: true,
            })
            .unwrap();
        client
            .switch_workspace(WorkspaceTarget::Relative {
                delta: 2,
                existing_only: false,
            })
            .unwrap();
        client
            .switch_workspace(WorkspaceTarget::Relative {
                delta: 0,
                existing_only: true,
            })
            .unwrap();
        client.switch_workspace(WorkspaceTarget::Previous).unwrap();

        let numbered = SwayWorkspace {
            num: 7,
            name: "7:code".to_owned(),
            ..SwayWorkspace::default()
        };
        client.switch_workspace(&numbered).unwrap();

        let named = SwayWorkspace {
            num: -1,
            name: "music".to_owned(),
            ..SwayWorkspace::default()
        };
        client.switch_workspace(&named).unwrap();

        assert!(client.switch_workspace("").is_err());
        assert!(client.switch_workspace("bad\nname").is_err());

        let seen = srv.join().unwrap();
        assert_eq!(
            seen,
            vec![
                "workspace number 3",
                "workspace \"dev \\\"1\\\"\"",
                "workspace next_on_output",
                "workspace prev_on_output",
                "workspace next; workspace next",
                "workspace back_and_forth",
                "workspace number 7",
                "workspace \"music\"",
            ]
        );

        let _ = std::fs::remove_dir_all(&dir);
    }
}
