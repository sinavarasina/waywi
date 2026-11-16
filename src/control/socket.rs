//! Local single-instance Unix control socket transport.

use std::io::{Read, Write};
use std::os::unix::fs::MetadataExt;
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::{Path, PathBuf};
use std::time::Duration;

use rustix::net::sockopt::socket_peercred;
use rustix::process::geteuid;

use crate::error::{Result, WaywiError};

const MAX_CONTROL_CMD_BYTES: usize = 4096;

/// Resolves a socket name (e.g. `"my-launcher"`) into `$XDG_RUNTIME_DIR/my-launcher.sock`,
/// falling back to the user-private `/run/user/<euid>/` (never to a world-writable directory),
/// and preserving explicit absolute/relative paths containing `/`.
#[must_use]
pub fn resolve_socket_path(name_or_path: &Path) -> PathBuf {
    if name_or_path.components().count() > 1 || name_or_path.is_absolute() {
        return name_or_path.to_path_buf();
    }
    let runtime_dir = match std::env::var_os("XDG_RUNTIME_DIR") {
        Some(dir) => PathBuf::from(dir),
        None => PathBuf::from(format!("/run/user/{}", geteuid().as_raw())),
    };
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
///
/// Replacement only happens after a failed connect probe returned `ECONNREFUSED` and the socket
/// file proven to be owned by this user, avoiding both unlinking a live foreign socket and the
/// unprotected probe-then-unlink race.
pub(crate) fn bind_control_listener(path: &Path) -> Result<UnixListener> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    match UnixListener::bind(path) {
        Ok(listener) => return finish_bind(listener),
        Err(e) if e.kind() == std::io::ErrorKind::AddrInUse => {}
        Err(e) => return Err(e.into()),
    }
    match UnixStream::connect(path) {
        Ok(_) => {
            return Err(WaywiError::IpcError(format!(
                "control socket {} already active in another instance",
                path.display()
            )));
        }
        Err(e) if e.kind() == std::io::ErrorKind::ConnectionRefused => {}
        Err(e) => {
            return Err(WaywiError::IpcError(format!(
                "control socket {} is not connectable ({e})",
                path.display()
            )));
        }
    }
    match std::fs::metadata(path) {
        Ok(meta) if meta.uid() != geteuid().as_raw() => {
            return Err(WaywiError::IpcError(format!(
                "refusing to replace control socket {} owned by another user",
                path.display()
            )));
        }
        Ok(_) => {}
        Err(_) => {}
    }
    let _ = std::fs::remove_file(path);
    let listener = UnixListener::bind(path)?;
    finish_bind(listener)
}

fn finish_bind(listener: UnixListener) -> Result<UnixListener> {
    listener.set_nonblocking(true)?;
    Ok(listener)
}

/// Returns `true` when the connected peer runs as the same (effective) user as this process.
///
/// The control socket lives in a user-private runtime directory, so this only defends against
/// misconfigured directory permissions.
fn peer_is_same_user(stream: &UnixStream) -> bool {
    match socket_peercred(stream) {
        Ok(cred) => cred.uid == geteuid(),
        Err(_) => false,
    }
}

/// Drains bounded newline-terminated command lines from pending incoming client connections on
/// `listener`. Connections from other users are dropped without reading.
pub(crate) fn accept_commands(listener: &UnixListener) -> Vec<String> {
    let mut out = Vec::new();
    loop {
        match listener.accept() {
            Ok((mut stream, _)) => {
                let _ = stream.set_read_timeout(Some(Duration::from_millis(100)));
                if !peer_is_same_user(&stream) {
                    continue;
                }
                let mut data: Vec<u8> = Vec::with_capacity(MAX_CONTROL_CMD_BYTES);
                loop {
                    if data.len() >= MAX_CONTROL_CMD_BYTES {
                        break;
                    }
                    let mut chunk = [0u8; 512];
                    match stream.read(&mut chunk) {
                        Ok(0) => break,
                        Ok(n) => {
                            let terminated = chunk[..n].contains(&b'\n');
                            let take = n.min(MAX_CONTROL_CMD_BYTES - data.len());
                            data.extend_from_slice(&chunk[..take]);
                            // The documented client terminator is a trailing newline; keep
                            // reading across short writes until it arrives.
                            if terminated {
                                break;
                            }
                        }
                        Err(e)
                            if e.kind() == std::io::ErrorKind::WouldBlock
                                || e.kind() == std::io::ErrorKind::TimedOut =>
                        {
                            break;
                        }
                        Err(_) => break,
                    }
                }
                for line in String::from_utf8_lossy(&data).lines() {
                    let trimmed = line.trim();
                    if !trimmed.is_empty() {
                        out.push(trimmed.to_owned());
                    }
                }
            }
            Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => break,
            Err(_) => break,
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    fn temp_socket_dir(test: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("waywi-sockctl-{}-{test}", std::process::id()));
        let _ = std::fs::create_dir_all(&dir);
        dir
    }

    #[test]
    fn accumulates_commands_split_across_writes() {
        let dir = temp_socket_dir("split");
        let path = dir.join("split.sock");
        let listener = bind_control_listener(&path).unwrap();

        let t = std::thread::spawn({
            let path = path.clone();
            move || {
                let mut s = UnixStream::connect(&path).unwrap();
                s.write_all(b"tog").unwrap();
                s.flush().unwrap();
                std::thread::sleep(Duration::from_millis(50));
                s.write_all(b"gle\nbeep\n").unwrap();
            }
        });

        std::thread::sleep(Duration::from_millis(80));
        let cmds = accept_commands(&listener);
        t.join().unwrap();
        assert_eq!(cmds, vec!["toggle".to_owned(), "beep".to_owned()]);

        let _ = std::fs::remove_file(&path);
        let _ = std::fs::remove_dir(&dir);
    }

    #[test]
    fn bind_replaces_stale_socket_but_reports_live_instance() {
        let dir = temp_socket_dir("stale");
        let path = dir.join("stale.sock");

        {
            let live = UnixListener::bind(&path).unwrap();
            let err = bind_control_listener(&path).unwrap_err();
            assert!(err.to_string().contains("already active"));
            // Dropping the raw listener leaves a stale (unconnectable) socket file.
            drop(live);
        }
        // The kernel may briefly complete connects that the closed listener already queued.
        std::thread::sleep(Duration::from_millis(20));
        assert!(UnixStream::connect(&path).is_err());

        let rebound = bind_control_listener(&path).unwrap();
        assert!(UnixStream::connect(&path).is_ok());
        drop(rebound);

        let _ = std::fs::remove_file(&path);
        let _ = std::fs::remove_dir(&dir);
    }
}
