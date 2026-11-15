//! Local single-instance Unix control socket transport.

use std::io::{Read, Write};
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::{Path, PathBuf};
use std::time::Duration;

use crate::error::{Result, WaywiError};

const MAX_CONTROL_CMD_BYTES: usize = 4096;

/// Resolves a socket name (e.g. `"my-launcher"`) into `$XDG_RUNTIME_DIR/my-launcher.sock`,
/// preserving explicit absolute/relative paths containing `/`.
#[must_use]
pub fn resolve_socket_path(name_or_path: &Path) -> PathBuf {
    if name_or_path.components().count() > 1 || name_or_path.is_absolute() {
        return name_or_path.to_path_buf();
    }
    let runtime_dir = std::env::var_os("XDG_RUNTIME_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(std::env::temp_dir);
    let mut name = name_or_path.as_os_str().to_os_string();
    if name_or_path.extension().is_none() {
        name.push(".sock");
    }
    runtime_dir.join(name)
}

/// Sends a single bounded command line to an already-running WayWi control socket.
///
/// Secondary CLI invocations (e.g. `my-app --toggle`) can call this and exit immediately without
/// opening a second Wayland connection.
pub fn send_command(socket_name_or_path: impl AsRef<Path>, command: &str) -> Result<()> {
    if command.len() > MAX_CONTROL_CMD_BYTES {
        return Err(WaywiError::IpcError(
            "control command exceeds 4096 byte limit".to_owned(),
        ));
    }
    let path = resolve_socket_path(socket_name_or_path.as_ref());
    let mut stream = UnixStream::connect(&path)
        .map_err(|e| WaywiError::IpcError(format!("failed to connect {}: {e}", path.display())))?;
    stream.set_write_timeout(Some(Duration::from_secs(1)))?;
    stream.write_all(command.trim_end_matches('\n').as_bytes())?;
    stream.write_all(b"\n")?;
    stream.flush()?;
    Ok(())
}

/// Binds a non-blocking local control listener, safely replacing stale unowned socket files.
pub(crate) fn bind_control_listener(path: &Path) -> Result<UnixListener> {
    if path.exists() {
        // Probe whether a live instance is already bound before unlinking stale socket.
        if UnixStream::connect(path).is_ok() {
            return Err(WaywiError::IpcError(format!(
                "control socket {} already active in another instance",
                path.display()
            )));
        }
        let _ = std::fs::remove_file(path);
    }
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let listener = UnixListener::bind(path)?;
    listener.set_nonblocking(true)?;
    Ok(listener)
}

/// Drains bounded command lines from pending incoming client connections on `listener`.
pub(crate) fn accept_commands(listener: &UnixListener) -> Vec<String> {
    let mut out = Vec::new();
    loop {
        match listener.accept() {
            Ok((mut stream, _)) => {
                let _ = stream.set_read_timeout(Some(Duration::from_millis(100)));
                let mut buf = [0u8; MAX_CONTROL_CMD_BYTES];
                if let Ok(n) = stream.read(&mut buf)
                    && n > 0
                {
                    let text = String::from_utf8_lossy(&buf[..n]);
                    for line in text.lines() {
                        let trimmed = line.trim();
                        if !trimmed.is_empty() {
                            out.push(trimmed.to_owned());
                        }
                    }
                }
            }
            Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => break,
            Err(_) => break,
        }
    }
    out
}
