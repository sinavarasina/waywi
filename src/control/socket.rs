//! Local single-instance Unix control socket transport.

use std::cell::{Cell, RefCell};
use std::collections::HashMap;
use std::fmt;
use std::io::{ErrorKind, Read, Write};
use std::net::Shutdown;
use std::os::unix::fs::MetadataExt;
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::time::{Duration, Instant};

use calloop::generic::Generic;
use calloop::timer::{TimeoutAction, Timer};
use calloop::{Interest, LoopHandle, Mode, PostAction, RegistrationToken};
use rustix::net::sockopt::socket_peercred;
use rustix::process::geteuid;

use crate::error::{Result, WaywiError};

const MAX_CONTROL_CMD_BYTES: usize = 4096;
const MAX_CONTROL_REPLY_BYTES: usize = 64 * 1024;
pub(crate) const MAX_PENDING_CONTROL_CLIENTS: usize = 16;
pub(crate) const CONTROL_CLIENT_DEADLINE: Duration = Duration::from_secs(2);

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

/// Sends a single bounded command line to an active WayWi control socket and waits up to `timeout`
/// for the server response (one request and one bounded response per connection).
pub fn send_command_with_reply(
    socket_name_or_path: impl AsRef<Path>,
    command: &str,
    timeout: Duration,
) -> Result<String> {
    if command.len() > MAX_CONTROL_CMD_BYTES {
        return Err(WaywiError::IpcError(
            "control command exceeds 4096 byte limit".to_owned(),
        ));
    }
    let path = resolve_socket_path(socket_name_or_path.as_ref());
    let mut stream = UnixStream::connect(&path)
        .map_err(|e| WaywiError::IpcError(format!("failed to connect {}: {e}", path.display())))?;
    stream.set_write_timeout(Some(timeout))?;
    stream.set_read_timeout(Some(timeout))?;
    stream.write_all(command.trim_end_matches('\n').as_bytes())?;
    stream.write_all(b"\n")?;
    stream.flush()?;
    let _ = stream.shutdown(Shutdown::Write);

    let mut reply_buf = Vec::new();
    let mut chunk = [0u8; 512];
    loop {
        if reply_buf.len() >= MAX_CONTROL_REPLY_BYTES {
            break;
        }
        match stream.read(&mut chunk) {
            Ok(0) => break,
            Ok(n) => {
                let take = n.min(MAX_CONTROL_REPLY_BYTES - reply_buf.len());
                reply_buf.extend_from_slice(&chunk[..take]);
            }
            Err(e) if e.kind() == ErrorKind::Interrupted => continue,
            Err(e) if e.kind() == ErrorKind::WouldBlock || e.kind() == ErrorKind::TimedOut => {
                return Err(WaywiError::Timeout(format!(
                    "timed out waiting for control reply from {}",
                    path.display()
                )));
            }
            Err(e) => {
                return Err(WaywiError::IpcError(format!(
                    "failed reading control reply from {}: {e}",
                    path.display()
                )));
            }
        }
    }
    Ok(String::from_utf8_lossy(&reply_buf).into_owned())
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

/// Accumulates partial raw bytes from a non-blocking control client up to
/// [`MAX_CONTROL_CMD_BYTES`] and emits trimmed, non-empty lines cleanly.
#[derive(Debug, Default)]
pub(crate) struct LineAccumulator {
    buf: Vec<u8>,
    total_bytes: usize,
}

impl LineAccumulator {
    /// Returns `true` when this connection has reached the per-connection byte cap.
    #[inline]
    #[must_use]
    pub(crate) fn is_full(&self) -> bool {
        self.total_bytes >= MAX_CONTROL_CMD_BYTES
    }

    /// Returns `true` when incomplete bytes for a line without a terminating `\n` remain buffered.
    #[cfg(test)]
    #[inline]
    #[must_use]
    pub(crate) fn has_partial_bytes(&self) -> bool {
        !self.buf.is_empty()
    }

    /// Feeds raw bytes into the accumulator, pushing any complete newline-terminated commands to `out`.
    pub(crate) fn feed(&mut self, bytes: &[u8], out: &mut Vec<String>) {
        let remaining = MAX_CONTROL_CMD_BYTES.saturating_sub(self.total_bytes);
        let capped = &bytes[..bytes.len().min(remaining)];
        self.total_bytes += capped.len();

        let mut start = 0;
        for (idx, &b) in capped.iter().enumerate() {
            if b == b'\n' {
                let seg = &capped[start..idx];
                if self.buf.is_empty() {
                    push_trimmed_line(seg, out);
                } else {
                    self.buf.extend_from_slice(seg);
                    push_trimmed_line(&self.buf, out);
                    self.buf.clear();
                }
                start = idx + 1;
            }
        }
        if start < capped.len() {
            self.buf.extend_from_slice(&capped[start..]);
        }
    }

    /// Flushes any trailing buffered line on EOF or buffer limit without requiring a trailing `\n`.
    pub(crate) fn finish(&mut self, out: &mut Vec<String>) {
        if !self.buf.is_empty() {
            push_trimmed_line(&self.buf, out);
            self.buf.clear();
        }
    }
}

fn push_trimmed_line(raw: &[u8], out: &mut Vec<String>) {
    let text = String::from_utf8_lossy(raw);
    let trimmed = text.trim();
    if !trimmed.is_empty() {
        out.push(trimmed.to_owned());
    }
}

type ReplyWriterInstaller = Rc<dyn Fn(UnixStream, Vec<u8>)>;

struct PendingReply {
    stream: Option<UnixStream>,
    insert_writer: Option<ReplyWriterInstaller>,
}

impl PendingReply {
    fn new(stream: UnixStream, insert_writer: ReplyWriterInstaller) -> Self {
        Self {
            stream: Some(stream),
            insert_writer: Some(insert_writer),
        }
    }

    fn send(&mut self, bytes: &[u8]) -> Result<()> {
        let Some(stream) = self.stream.take() else {
            return Ok(());
        };
        let insert_writer = self.insert_writer.take();
        let capped = &bytes[..bytes.len().min(MAX_CONTROL_REPLY_BYTES)];
        let mut written = 0usize;
        let mut s_ref = &stream;

        while written < capped.len() {
            match s_ref.write(&capped[written..]) {
                Ok(0) => {
                    return Err(WaywiError::IpcError(
                        "control client disconnected during reply".into(),
                    ));
                }
                Ok(n) => written += n,
                Err(e) if e.kind() == ErrorKind::Interrupted => continue,
                Err(e) if e.kind() == ErrorKind::WouldBlock => {
                    if let Some(installer) = insert_writer {
                        installer(stream, capped[written..].to_vec());
                        return Ok(());
                    }
                    return Err(WaywiError::IpcError(
                        "control reply socket buffer full".into(),
                    ));
                }
                Err(e) => {
                    return Err(WaywiError::IpcError(format!(
                        "failed writing control reply: {e}"
                    )));
                }
            }
        }
        let _ = stream.shutdown(Shutdown::Write);
        Ok(())
    }
}

impl Drop for PendingReply {
    fn drop(&mut self) {
        if let Some(stream) = self.stream.take() {
            let _ = stream.shutdown(Shutdown::Write);
        }
    }
}

/// Incoming command line from a local control socket, paired with a one-shot reply handle when
/// received through [`Control::socket_requests`](super::Control::socket_requests).
#[derive(Clone)]
pub struct ControlRequest {
    id: u64,
    command: String,
    reply: Option<Rc<RefCell<Option<PendingReply>>>>,
}

impl PartialEq for ControlRequest {
    fn eq(&self, other: &Self) -> bool {
        self.id == other.id && self.command == other.command
    }
}

impl Eq for ControlRequest {}

impl fmt::Debug for ControlRequest {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("ControlRequest")
            .field("id", &self.id)
            .field("command", &self.command)
            .field("can_reply", &self.can_reply())
            .finish()
    }
}

impl ControlRequest {
    /// Returns the trimmed command line sent by the peer.
    #[inline]
    #[must_use]
    pub fn command(&self) -> &str {
        &self.command
    }

    /// Consumes this request and returns the trimmed command string.
    #[must_use]
    pub fn into_command(self) -> String {
        self.command
    }

    /// Returns `true` when this request still holds an unsent one-shot reply slot.
    #[must_use]
    pub fn can_reply(&self) -> bool {
        self.reply
            .as_ref()
            .is_some_and(|slot| slot.borrow().is_some())
    }

    /// Writes a bounded reply back to the requesting peer and closes the reply half cleanly.
    ///
    /// Returns `Ok(true)` if the reply was dispatched, or `Ok(false)` if this request has no active
    /// reply channel or was already answered.
    pub fn reply(&self, response: impl AsRef<str>) -> Result<bool> {
        let Some(ref slot) = self.reply else {
            return Ok(false);
        };
        let Some(mut pending) = slot.borrow_mut().take() else {
            return Ok(false);
        };
        pending.send(response.as_ref().as_bytes())?;
        Ok(true)
    }
}

/// Outcome of draining a non-blocking control client stream.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ReadStatus {
    Done,
    Pending,
    Error,
}

/// Reads from a non-blocking `UnixStream` until `WouldBlock`, `EOF`, or [`MAX_CONTROL_CMD_BYTES`].
///
/// Any complete `\n`-terminated lines are pushed into `out` immediately even while the connection
/// remains open waiting for further lines or `EOF`.
pub(crate) fn read_nonblocking(
    mut stream: &UnixStream,
    acc: &mut LineAccumulator,
    out: &mut Vec<String>,
) -> ReadStatus {
    let mut chunk = [0u8; 512];
    loop {
        if acc.is_full() {
            acc.finish(out);
            return ReadStatus::Done;
        }
        match stream.read(&mut chunk) {
            Ok(0) => {
                acc.finish(out);
                return ReadStatus::Done;
            }
            Ok(n) => {
                acc.feed(&chunk[..n], out);
                if acc.is_full() {
                    acc.finish(out);
                    return ReadStatus::Done;
                }
            }
            Err(e) if e.kind() == ErrorKind::Interrupted => continue,
            Err(e) if e.kind() == ErrorKind::WouldBlock => return ReadStatus::Pending,
            Err(_) => return ReadStatus::Error,
        }
    }
}

struct PendingControlClient {
    token: RegistrationToken,
    acc: LineAccumulator,
    deadline: Instant,
}

#[derive(Default)]
struct PendingControlPool {
    next_id: u64,
    next_req_id: u64,
    clients: HashMap<u64, PendingControlClient>,
    timer_token: Option<RegistrationToken>,
}

/// Active `calloop` registration managing the control `UnixListener`, bounded non-blocking
/// client streams, and an on-demand expiration timer.
pub(crate) struct ControlSocketRegistration {
    path: PathBuf,
    listener_token: RegistrationToken,
    pool: Rc<RefCell<PendingControlPool>>,
    reply_enabled: Rc<Cell<bool>>,
}

impl ControlSocketRegistration {
    #[inline]
    #[must_use]
    pub(crate) fn path(&self) -> &Path {
        &self.path
    }

    #[inline]
    pub(crate) fn set_reply_enabled(&self, enabled: bool) {
        self.reply_enabled.set(enabled);
    }

    #[cfg(test)]
    #[must_use]
    pub(crate) fn pending_count(&self) -> usize {
        self.pool.borrow().clients.len()
    }

    #[cfg(test)]
    #[must_use]
    pub(crate) fn has_active_timer(&self) -> bool {
        self.pool.borrow().timer_token.is_some()
    }

    /// Binds `path` and registers the control socket listener on `loop_handle` using default
    /// pending-client limits and deadlines.
    pub(crate) fn bind_and_insert<D: 'static, F>(
        path: PathBuf,
        loop_handle: &LoopHandle<'static, D>,
        reply_enabled: bool,
        on_requests: F,
    ) -> Result<Self>
    where
        F: Fn(Vec<ControlRequest>) + 'static,
    {
        Self::bind_and_insert_with_limits(
            path,
            loop_handle,
            MAX_PENDING_CONTROL_CLIENTS,
            CONTROL_CLIENT_DEADLINE,
            reply_enabled,
            on_requests,
        )
    }

    pub(crate) fn bind_and_insert_with_limits<D: 'static, F>(
        path: PathBuf,
        loop_handle: &LoopHandle<'static, D>,
        max_pending: usize,
        client_deadline: Duration,
        reply_enabled: bool,
        on_requests: F,
    ) -> Result<Self>
    where
        F: Fn(Vec<ControlRequest>) + 'static,
    {
        let listener = bind_control_listener(&path)?;
        let pool = Rc::new(RefCell::new(PendingControlPool::default()));
        let reply_flag = Rc::new(Cell::new(reply_enabled));
        let on_reqs: Rc<dyn Fn(Vec<ControlRequest>)> = Rc::new(on_requests);

        let lh_writer = loop_handle.clone();
        let insert_writer: ReplyWriterInstaller = Rc::new(move |stream, remaining| {
            insert_reply_writer(&lh_writer, stream, remaining, client_deadline);
        });

        let pool_cb = Rc::clone(&pool);
        let lh_cb = loop_handle.clone();
        let on_reqs_cb = Rc::clone(&on_reqs);
        let reply_flag_cb = Rc::clone(&reply_flag);

        let src = Generic::new(listener, Interest::READ, Mode::Level);
        let listener_token = loop_handle
            .insert_source(src, move |_, fd, _| {
                accept_pending_clients(
                    fd,
                    &lh_cb,
                    &pool_cb,
                    &on_reqs_cb,
                    &insert_writer,
                    &reply_flag_cb,
                    max_pending,
                    client_deadline,
                );
                Ok(PostAction::Continue)
            })
            .map_err(|e| {
                let _ = std::fs::remove_file(&path);
                WaywiError::IpcError(format!(
                    "failed to register control socket {}: {e}",
                    path.display()
                ))
            })?;

        Ok(Self {
            path,
            listener_token,
            pool,
            reply_enabled: reply_flag,
        })
    }

    /// Unregisters the listener, all pending client streams, and any active timer, and unlinks the
    /// socket file.
    pub(crate) fn remove<D>(self, loop_handle: &LoopHandle<'static, D>) {
        loop_handle.remove(self.listener_token);
        let _ = std::fs::remove_file(&self.path);
        let mut pool = self.pool.borrow_mut();
        for (_, client) in pool.clients.drain() {
            loop_handle.remove(client.token);
        }
        if let Some(timer_tok) = pool.timer_token.take() {
            loop_handle.remove(timer_tok);
        }
    }
}

fn insert_reply_writer<D: 'static>(
    loop_handle: &LoopHandle<'static, D>,
    stream: UnixStream,
    remaining: Vec<u8>,
    deadline_dur: Duration,
) {
    let mut pos = 0usize;
    let deadline = Instant::now() + deadline_dur;
    let writer_src = Generic::new(stream, Interest::WRITE, Mode::Level);
    let _ = loop_handle.insert_source(writer_src, move |_, fd, _| {
        if Instant::now() >= deadline {
            return Ok(PostAction::Remove);
        }
        let mut s_ref: &UnixStream = fd;
        while pos < remaining.len() {
            match s_ref.write(&remaining[pos..]) {
                Ok(0) => return Ok(PostAction::Remove),
                Ok(n) => pos += n,
                Err(e) if e.kind() == ErrorKind::Interrupted => continue,
                Err(e) if e.kind() == ErrorKind::WouldBlock => return Ok(PostAction::Continue),
                Err(_) => return Ok(PostAction::Remove),
            }
        }
        let _ = fd.shutdown(Shutdown::Write);
        Ok(PostAction::Remove)
    });
}

fn build_control_requests(
    cmds: Vec<String>,
    stream: &UnixStream,
    pool: &Rc<RefCell<PendingControlPool>>,
    insert_writer: &ReplyWriterInstaller,
    with_reply: bool,
) -> Vec<ControlRequest> {
    let mut out = Vec::with_capacity(cmds.len());
    let shared_reply = if with_reply {
        stream.try_clone().ok().map(|dup| {
            Rc::new(RefCell::new(Some(PendingReply::new(
                dup,
                Rc::clone(insert_writer),
            ))))
        })
    } else {
        None
    };

    let mut p = pool.borrow_mut();
    for command in cmds {
        let id = p.next_req_id;
        p.next_req_id = p.next_req_id.wrapping_add(1);
        out.push(ControlRequest {
            id,
            command,
            reply: shared_reply.as_ref().map(Rc::clone),
        });
    }
    out
}

#[allow(clippy::too_many_arguments)]
fn accept_pending_clients<D: 'static>(
    listener: &UnixListener,
    loop_handle: &LoopHandle<'static, D>,
    pool: &Rc<RefCell<PendingControlPool>>,
    on_reqs: &Rc<dyn Fn(Vec<ControlRequest>)>,
    insert_writer: &ReplyWriterInstaller,
    reply_enabled: &Rc<Cell<bool>>,
    max_pending: usize,
    client_deadline: Duration,
) {
    loop {
        match listener.accept() {
            Ok((stream, _)) => {
                if !peer_is_same_user(&stream) || stream.set_nonblocking(true).is_err() {
                    continue;
                }
                let mut acc = LineAccumulator::default();
                let mut cmds = Vec::new();
                let status = read_nonblocking(&stream, &mut acc, &mut cmds);
                if !cmds.is_empty() {
                    let reqs = build_control_requests(
                        cmds,
                        &stream,
                        pool,
                        insert_writer,
                        reply_enabled.get(),
                    );
                    on_reqs(reqs);
                }
                if status != ReadStatus::Pending {
                    continue;
                }

                if max_pending == 0 {
                    continue;
                }

                let now = Instant::now();
                let mut at_cap = pool.borrow().clients.len() >= max_pending;
                if at_cap {
                    evict_expired_clients(loop_handle, pool, now, false);
                    at_cap = pool.borrow().clients.len() >= max_pending;
                }
                if at_cap {
                    evict_oldest_client(loop_handle, pool);
                }

                let (client_id, deadline) = {
                    let mut p = pool.borrow_mut();
                    let id = p.next_id;
                    p.next_id = p.next_id.wrapping_add(1);
                    (id, now + client_deadline)
                };

                let pool_client = Rc::clone(pool);
                let lh_client = loop_handle.clone();
                let on_reqs_client = Rc::clone(on_reqs);
                let iw_client = Rc::clone(insert_writer);
                let reply_flag_client = Rc::clone(reply_enabled);
                let client_src = Generic::new(stream, Interest::READ, Mode::Level);
                let Ok(client_token) = loop_handle.insert_source(client_src, move |_, fd, _| {
                    handle_client_readable(
                        fd,
                        client_id,
                        &lh_client,
                        &pool_client,
                        &on_reqs_client,
                        &iw_client,
                        reply_flag_client.get(),
                    )
                }) else {
                    continue;
                };

                let need_timer = {
                    let mut p = pool.borrow_mut();
                    p.clients.insert(
                        client_id,
                        PendingControlClient {
                            token: client_token,
                            acc,
                            deadline,
                        },
                    );
                    p.timer_token.is_none()
                };

                if need_timer {
                    arm_deadline_timer(loop_handle, pool, deadline);
                }
            }
            Err(e) if e.kind() == ErrorKind::Interrupted => continue,
            Err(e) if e.kind() == ErrorKind::WouldBlock => break,
            Err(_) => break,
        }
    }
}

fn handle_client_readable<D: 'static>(
    stream: &UnixStream,
    client_id: u64,
    loop_handle: &LoopHandle<'static, D>,
    pool: &Rc<RefCell<PendingControlPool>>,
    on_reqs: &Rc<dyn Fn(Vec<ControlRequest>)>,
    insert_writer: &ReplyWriterInstaller,
    reply_enabled: bool,
) -> std::io::Result<PostAction> {
    let mut cmds = Vec::new();
    let status = {
        let mut p = pool.borrow_mut();
        let Some(client) = p.clients.get_mut(&client_id) else {
            return Ok(PostAction::Remove);
        };
        read_nonblocking(stream, &mut client.acc, &mut cmds)
    };
    if !cmds.is_empty() {
        let reqs = build_control_requests(cmds, stream, pool, insert_writer, reply_enabled);
        on_reqs(reqs);
    }
    match status {
        ReadStatus::Pending => Ok(PostAction::Continue),
        ReadStatus::Done | ReadStatus::Error => {
            let timer_to_remove = {
                let mut p = pool.borrow_mut();
                p.clients.remove(&client_id);
                if p.clients.is_empty() {
                    p.timer_token.take()
                } else {
                    None
                }
            };
            if let Some(t) = timer_to_remove {
                loop_handle.remove(t);
            }
            Ok(PostAction::Remove)
        }
    }
}

fn evict_oldest_client<D>(
    loop_handle: &LoopHandle<'static, D>,
    pool: &Rc<RefCell<PendingControlPool>>,
) {
    let oldest_token = {
        let mut p = pool.borrow_mut();
        p.clients
            .keys()
            .copied()
            .min()
            .and_then(|id| p.clients.remove(&id))
            .map(|c| c.token)
    };
    if let Some(tok) = oldest_token {
        loop_handle.remove(tok);
    }
}

fn evict_expired_clients<D>(
    loop_handle: &LoopHandle<'static, D>,
    pool: &Rc<RefCell<PendingControlPool>>,
    now: Instant,
    from_timer: bool,
) -> Option<Instant> {
    let (expired_tokens, next_deadline, timer_to_remove) = {
        let mut p = pool.borrow_mut();
        let expired_ids: Vec<u64> = p
            .clients
            .iter()
            .filter_map(|(&id, c)| (c.deadline <= now).then_some(id))
            .collect();
        let mut tokens = Vec::with_capacity(expired_ids.len());
        for id in expired_ids {
            if let Some(c) = p.clients.remove(&id) {
                tokens.push(c.token);
            }
        }
        let next = p.clients.values().map(|c| c.deadline).min();
        let timer_to_remove = if next.is_none() {
            let tok = p.timer_token.take();
            if from_timer { None } else { tok }
        } else {
            None
        };
        (tokens, next, timer_to_remove)
    };
    for tok in expired_tokens {
        loop_handle.remove(tok);
    }
    if let Some(t) = timer_to_remove {
        loop_handle.remove(t);
    }
    next_deadline
}

fn arm_deadline_timer<D: 'static>(
    loop_handle: &LoopHandle<'static, D>,
    pool: &Rc<RefCell<PendingControlPool>>,
    deadline: Instant,
) {
    let pool_timer = Rc::clone(pool);
    let lh_timer = loop_handle.clone();
    if let Ok(timer_token) =
        loop_handle.insert_source(Timer::from_deadline(deadline), move |_, _, _| {
            match evict_expired_clients(&lh_timer, &pool_timer, Instant::now(), true) {
                Some(next) => TimeoutAction::ToInstant(next),
                None => TimeoutAction::Drop,
            }
        })
    {
        pool.borrow_mut().timer_token = Some(timer_token);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::net::Shutdown;

    fn temp_socket_dir(test: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("waywi-sockctl-{}-{test}", std::process::id()));
        let _ = std::fs::create_dir_all(&dir);
        dir
    }

    #[test]
    fn line_accumulator_handles_split_multiple_blank_utf8_and_max_bytes() {
        let mut acc = LineAccumulator::default();
        let mut out = Vec::new();

        // Multi-byte UTF-8 ("日" = 0xE6 0x97 0xA5) split across two feeds before newline.
        acc.feed(b"tog", &mut out);
        assert!(out.is_empty());
        assert!(acc.has_partial_bytes());

        acc.feed(b"gle\n   \n\r\n  be\r\ne\xE6\x97\xA5 \n", &mut out);
        // Split emoji / Kanji explicitly across chunks.
        acc.feed(&[0xE6, 0x97], &mut out);
        acc.feed(&[0xA5, b'\n'], &mut out);
        // Invalid raw UTF-8 sequence falls back safely via lossy conversion.
        acc.feed(&[0xFF, b'x', b'\n'], &mut out);

        assert_eq!(
            out,
            vec![
                "toggle".to_owned(),
                "be".to_owned(),
                "e日".to_owned(),
                "日".to_owned(),
                "\u{FFFD}x".to_owned(),
            ]
        );
        assert!(!acc.has_partial_bytes());

        // Cap enforcement at MAX_CONTROL_CMD_BYTES (4096) including prior bytes.
        let mut big = LineAccumulator::default();
        let mut big_out = Vec::new();
        let payload = vec![b'a'; MAX_CONTROL_CMD_BYTES + 256];
        big.feed(&payload, &mut big_out);
        assert!(big.is_full());
        big.finish(&mut big_out);
        assert_eq!(big_out.len(), 1);
        assert_eq!(big_out[0].len(), MAX_CONTROL_CMD_BYTES);
    }

    #[test]
    fn event_loop_handles_split_write_eof_without_newline_and_silent_or_trickle_clients() {
        let dir = temp_socket_dir("async-loop");
        let path = dir.join("ctrl.sock");
        let mut el: calloop::EventLoop<'static, ()> = calloop::EventLoop::try_new().unwrap();
        let lh = el.handle();

        let received: Rc<RefCell<Vec<String>>> = Rc::new(RefCell::new(Vec::new()));
        let rec_cb = Rc::clone(&received);
        let reg = ControlSocketRegistration::bind_and_insert_with_limits(
            path.clone(),
            &lh,
            MAX_PENDING_CONTROL_CLIENTS,
            Duration::from_secs(5),
            false,
            move |reqs| {
                rec_cb
                    .borrow_mut()
                    .extend(reqs.into_iter().map(ControlRequest::into_command));
            },
        )
        .unwrap();

        // 1. Silent client stays connected without sending anything -> dispatch must not block 100ms!
        let _silent = UnixStream::connect(&path).unwrap();
        let start = Instant::now();
        el.dispatch(Some(Duration::from_millis(5)), &mut ())
            .unwrap();
        assert!(
            start.elapsed() < Duration::from_millis(40),
            "silent client stalled loop for {:?}",
            start.elapsed()
        );
        assert_eq!(reg.pending_count(), 1);
        assert!(reg.has_active_timer());

        // 2. While silent client hangs, split-write, multi-line paused connection, trickle client,
        //    and EOF-without-newline client all succeed without blocking or BrokenPipe.
        let mut split = UnixStream::connect(&path).unwrap();
        split.write_all(b"tog").unwrap();
        el.dispatch(Some(Duration::from_millis(5)), &mut ())
            .unwrap();
        assert_eq!(reg.pending_count(), 2);

        // Trickle client writes 1 byte at a time across loop turns without stalling.
        let mut trickle = UnixStream::connect(&path).unwrap();
        for &b in b"tr" {
            trickle.write_all(&[b]).unwrap();
            let t0 = Instant::now();
            el.dispatch(Some(Duration::from_millis(5)), &mut ())
                .unwrap();
            assert!(t0.elapsed() < Duration::from_millis(40));
        }

        // Complete split-write ("toggle", "beep") and keep socket open to send a delayed "second".
        split.write_all(b"gle\nbeep\n").unwrap();
        let d1 = Instant::now() + Duration::from_millis(200);
        while received.borrow().len() < 2 && Instant::now() < d1 {
            el.dispatch(Some(Duration::from_millis(5)), &mut ())
                .unwrap();
        }
        assert_eq!(
            *received.borrow(),
            vec!["toggle".to_owned(), "beep".to_owned()]
        );

        // Connection stayed open after \n; sending another command later on the same socket works!
        std::thread::sleep(Duration::from_millis(20));
        split.write_all(b"second\n").unwrap();
        let eof_client = UnixStream::connect(&path).unwrap();
        (&eof_client).write_all(b"eof-cmd").unwrap();
        eof_client.shutdown(Shutdown::Write).unwrap();

        let deadline = Instant::now() + Duration::from_millis(300);
        while received.borrow().len() < 4 && Instant::now() < deadline {
            el.dispatch(Some(Duration::from_millis(5)), &mut ())
                .unwrap();
        }
        assert_eq!(
            *received.borrow(),
            vec![
                "toggle".to_owned(),
                "beep".to_owned(),
                "second".to_owned(),
                "eof-cmd".to_owned(),
            ]
        );

        drop(_silent);
        drop(trickle);
        reg.remove(&lh);
        let _ = std::fs::remove_dir(&dir);
    }

    #[test]
    fn event_loop_evicts_oldest_at_cap_and_expires_idle_clients_with_eof() {
        let dir = temp_socket_dir("cap-deadline");
        let path = dir.join("cap.sock");
        let mut el: calloop::EventLoop<'static, ()> = calloop::EventLoop::try_new().unwrap();
        let lh = el.handle();

        let received: Rc<RefCell<Vec<String>>> = Rc::new(RefCell::new(Vec::new()));
        let rec_cb = Rc::clone(&received);
        let reg = ControlSocketRegistration::bind_and_insert_with_limits(
            path.clone(),
            &lh,
            MAX_PENDING_CONTROL_CLIENTS,
            Duration::from_millis(250),
            false,
            move |reqs| {
                rec_cb
                    .borrow_mut()
                    .extend(reqs.into_iter().map(ControlRequest::into_command));
            },
        )
        .unwrap();

        // Connect > 16 silent clients sequentially so FIFO order is deterministic.
        let mut silent_clients: Vec<UnixStream> = Vec::with_capacity(20);
        for _ in 0..20 {
            silent_clients.push(UnixStream::connect(&path).unwrap());
            el.dispatch(Some(Duration::from_millis(10)), &mut ())
                .unwrap();
        }
        el.dispatch(Some(Duration::ZERO), &mut ()).unwrap();
        assert_eq!(reg.pending_count(), MAX_PENDING_CONTROL_CLIENTS);
        assert!(reg.has_active_timer());

        // Oldest 4 silent clients (0..4) were evicted by FIFO and already received EOF.
        // Use a short blocking read timeout instead of a single non-blocking poll so concurrent
        // `fork -> exec` in parallel tests cannot transiently pin the server FD before `CLOEXEC`.
        for (idx, old) in silent_clients[..4].iter_mut().enumerate() {
            old.set_read_timeout(Some(Duration::from_millis(250)))
                .unwrap();
            let mut b = [0u8; 1];
            let res = old.read(&mut b);
            assert!(
                matches!(res, Ok(0)),
                "evicted silent client {idx} expected Ok(0), got {res:?}"
            );
        }

        // Even while 16 pending slots are full (< deadline), a split-write 17th client still gets
        // accepted via FIFO eviction and delivers its command without blocking.
        let mut split17 = UnixStream::connect(&path).unwrap();
        el.dispatch(Some(Duration::from_millis(10)), &mut ())
            .unwrap();
        split17.write_all(b"while-full\n").unwrap();
        drop(split17);
        let full_end = Instant::now() + Duration::from_millis(100);
        while received.borrow().is_empty() && Instant::now() < full_end {
            el.dispatch(Some(Duration::from_millis(5)), &mut ())
                .unwrap();
        }
        assert_eq!(*received.borrow(), vec!["while-full".to_owned()]);
        received.borrow_mut().clear();

        // Wait past the 250ms deadline without dispatching, then connect a fresh silent client:
        // listener evicts expired clients cleanly (cancelling stale timer) so pending_count <= 1.
        std::thread::sleep(Duration::from_millis(270));
        let _fresh_silent = UnixStream::connect(&path).unwrap();
        el.dispatch(Some(Duration::from_millis(10)), &mut ())
            .unwrap();
        assert!(reg.pending_count() <= 1);

        // Wait past the deadline for remaining entries -> all silent clients are evicted with EOF.
        let wait_end = Instant::now() + Duration::from_millis(450);
        while (reg.pending_count() > 0 || reg.has_active_timer()) && Instant::now() < wait_end {
            el.dispatch(Some(Duration::from_millis(15)), &mut ())
                .unwrap();
        }
        assert_eq!(reg.pending_count(), 0);
        assert!(!reg.has_active_timer());

        for s in &mut silent_clients {
            s.set_nonblocking(true).unwrap();
            let mut b = [0u8; 1];
            assert!(matches!(s.read(&mut b), Ok(0) | Err(_)));
        }

        // Listener still accepts valid commands cleanly after eviction.
        send_command(&path, "after-cap").unwrap();
        let cmd_end = Instant::now() + Duration::from_millis(200);
        while received.borrow().is_empty() && Instant::now() < cmd_end {
            el.dispatch(Some(Duration::from_millis(5)), &mut ())
                .unwrap();
        }
        assert_eq!(*received.borrow(), vec!["after-cap".to_owned()]);
        assert_eq!(reg.pending_count(), 0);
        assert!(!reg.has_active_timer());

        reg.remove(&lh);
        let _ = std::fs::remove_dir(&dir);
    }

    #[test]
    fn event_loop_supports_request_reply_and_auto_eof_on_ignored_request() {
        let dir = temp_socket_dir("req-reply");
        let path = dir.join("reply.sock");
        let mut el: calloop::EventLoop<'static, ()> = calloop::EventLoop::try_new().unwrap();
        let lh = el.handle();

        let pending_reqs: Rc<RefCell<Vec<ControlRequest>>> = Rc::new(RefCell::new(Vec::new()));
        let req_cb = Rc::clone(&pending_reqs);
        let reg = ControlSocketRegistration::bind_and_insert_with_limits(
            path.clone(),
            &lh,
            MAX_PENDING_CONTROL_CLIENTS,
            Duration::from_secs(2),
            true,
            move |reqs| {
                req_cb.borrow_mut().extend(reqs);
            },
        )
        .unwrap();

        let path_status = path.clone();
        let t_status = std::thread::spawn(move || {
            send_command_with_reply(&path_status, "status", Duration::from_secs(1))
        });
        let end = Instant::now() + Duration::from_millis(200);
        while pending_reqs.borrow().is_empty() && Instant::now() < end {
            el.dispatch(Some(Duration::from_millis(5)), &mut ())
                .unwrap();
        }
        let req = pending_reqs.borrow_mut().remove(0);
        assert_eq!(req.command(), "status");
        assert!(req.can_reply());
        assert!(req.reply("{\"visible\":true}\n").unwrap());
        // Second call on a one-shot request is a safe no-op returning false.
        assert!(!req.reply("second\n").unwrap());
        assert_eq!(
            t_status.join().unwrap().unwrap(),
            "{\"visible\":true}\n".to_owned()
        );

        // Dropping a request without calling reply() closes write-half so CLI sees EOF immediately.
        let path_ignored = path.clone();
        let t_ignored = std::thread::spawn(move || {
            send_command_with_reply(&path_ignored, "ignored", Duration::from_secs(1))
        });
        let end2 = Instant::now() + Duration::from_millis(200);
        while pending_reqs.borrow().is_empty() && Instant::now() < end2 {
            el.dispatch(Some(Duration::from_millis(5)), &mut ())
                .unwrap();
        }
        let req_ignored = pending_reqs.borrow_mut().remove(0);
        assert_eq!(req_ignored.command(), "ignored");
        drop(req_ignored);
        let t0 = Instant::now();
        assert_eq!(t_ignored.join().unwrap().unwrap(), "");
        assert!(t0.elapsed() < Duration::from_millis(200));

        reg.remove(&lh);
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
