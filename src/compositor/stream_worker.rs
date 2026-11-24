use std::io::{ErrorKind, Read};
use std::os::fd::OwnedFd;
use std::os::unix::net::UnixStream;
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use rustix::event::{EventfdFlags, PollFd, PollFlags, Timespec, eventfd, poll};

pub(crate) const READ_CHUNK_BYTES: usize = 4096;
const RECONNECT_INITIAL_MS: u64 = 150;
const RECONNECT_MAX_MS: u64 = 2_000;
const FALLBACK_POLL_SLICE: Duration = Duration::from_millis(250);

/// Result of a protocol-specific post-connect handshake.
#[allow(dead_code)] // Some variants are only constructed by specific backends (e.g. sway-ipc).
pub(crate) enum HandshakeResult {
    Ready,
    Failed,
    Stopped,
}

/// Outcome of feeding freshly read bytes into a [`StreamProtocol`] decoder.
#[allow(dead_code)] // Some variants are only constructed by specific backends (e.g. sway-ipc).
pub(crate) enum FeedOutcome {
    Continue,
    ProtocolError,
    ReceiverClosed,
}

/// Result of waiting for socket readiness or shutdown on a [`StopWaiter`].
pub(crate) enum WaitReady {
    Ready(PollFlags),
    Stopped,
    Timeout,
    Error,
}

/// Helper passed to protocol handshakes and the read loop so all waiting is immediately
/// interruptible via `eventfd(2)` (or short poll slices if `eventfd` is unavailable).
pub(crate) struct StopWaiter<'a> {
    stop: &'a AtomicBool,
    stop_fd: Option<&'a OwnedFd>,
}

impl<'a> StopWaiter<'a> {
    #[inline]
    pub(crate) fn is_stopped(&self) -> bool {
        self.stop.load(Ordering::Acquire)
    }

    /// Waits for `stream` to satisfy `flags`, waking immediately if shutdown is requested.
    pub(crate) fn wait_ready(
        &self,
        stream: &UnixStream,
        flags: PollFlags,
        timeout: Option<Duration>,
    ) -> WaitReady {
        let deadline = timeout.map(|d| Instant::now() + d);
        loop {
            if self.is_stopped() {
                return WaitReady::Stopped;
            }
            let remaining = match deadline {
                Some(dl) => {
                    let now = Instant::now();
                    if now >= dl {
                        return WaitReady::Timeout;
                    }
                    Some(dl - now)
                }
                None => None,
            };
            match self.stop_fd {
                Some(wake_fd) => {
                    let ts = remaining.map(duration_to_timespec);
                    let mut fds = [
                        PollFd::new(stream, flags),
                        PollFd::new(wake_fd, PollFlags::IN),
                    ];
                    match poll(&mut fds, ts.as_ref()) {
                        Ok(0) => {
                            if deadline.is_some() {
                                return WaitReady::Timeout;
                            }
                        }
                        Ok(_) => {
                            if fds[1].revents().contains(PollFlags::IN) || self.is_stopped() {
                                return WaitReady::Stopped;
                            }
                            return WaitReady::Ready(fds[0].revents());
                        }
                        Err(rustix::io::Errno::INTR) => continue,
                        Err(_) => return WaitReady::Error,
                    }
                }
                None => {
                    let slice = remaining
                        .map(|r| r.min(FALLBACK_POLL_SLICE))
                        .unwrap_or(FALLBACK_POLL_SLICE);
                    let ts = duration_to_timespec(slice);
                    let mut fds = [PollFd::new(stream, flags)];
                    match poll(&mut fds, Some(&ts)) {
                        Ok(0) => continue,
                        Ok(_) => {
                            if self.is_stopped() {
                                return WaitReady::Stopped;
                            }
                            return WaitReady::Ready(fds[0].revents());
                        }
                        Err(rustix::io::Errno::INTR) => continue,
                        Err(_) => return WaitReady::Error,
                    }
                }
            }
        }
    }

    /// Waits up to `delay_ms` before reconnecting, returning `true` immediately on stop.
    pub(crate) fn wait_backoff(&self, delay_ms: u64) -> bool {
        if self.is_stopped() {
            return true;
        }
        match self.stop_fd {
            Some(fd) => {
                let ts = duration_to_timespec(Duration::from_millis(delay_ms));
                let mut fds = [PollFd::new(fd, PollFlags::IN)];
                let _ = poll(&mut fds, Some(&ts));
                self.is_stopped() || fds[0].revents().contains(PollFlags::IN)
            }
            None => {
                let deadline = Instant::now() + Duration::from_millis(delay_ms);
                while !self.is_stopped() {
                    let Some(rem) = deadline.checked_duration_since(Instant::now()) else {
                        break;
                    };
                    std::thread::sleep(rem.min(FALLBACK_POLL_SLICE));
                }
                self.is_stopped()
            }
        }
    }
}

/// Compositor-specific framing and lifecycle hooks for [`StreamWorkerHandle`].
pub(crate) trait StreamProtocol: Send + 'static {
    type Event: Send + 'static;

    const THREAD_NAME: &'static str;

    fn disconnected_event() -> Self::Event;
    fn reconnected_event() -> Self::Event;

    /// Resets per-connection parser/accumulator state for a newly opened non-blocking socket.
    fn reset(&mut self);

    /// Optional non-blocking post-connect handshake (e.g. Sway `SUBSCRIBE`).
    fn handshake(&mut self, _stream: &mut UnixStream, _waiter: &StopWaiter<'_>) -> HandshakeResult {
        HandshakeResult::Ready
    }

    /// Feeds newly read bytes (or an empty slice to flush frames buffered during handshake) and
    /// invokes `emit` for each decoded event. `emit` returns `true` when the channel closed.
    fn feed(&mut self, chunk: &[u8], emit: &mut dyn FnMut(Self::Event) -> bool) -> FeedOutcome;

    /// Flushes any trailing partial record on clean EOF. Returns `true` if the receiver closed.
    fn finish(&mut self, _emit: &mut dyn FnMut(Self::Event) -> bool) -> bool {
        false
    }
}

/// Shared zero-polling Unix stream listener worker using `poll(2)` + `eventfd(2)` + exponential
/// reconnect backoff (`150ms..2000ms`), waking and joining instantaneously on [`Drop`].
pub(crate) struct StreamWorkerHandle {
    stop: Arc<AtomicBool>,
    stop_fd: Option<Arc<OwnedFd>>,
    join: Option<std::thread::JoinHandle<()>>,
}

impl StreamWorkerHandle {
    pub(crate) fn spawn<P: StreamProtocol>(
        socket_path: PathBuf,
        sender: calloop::channel::Sender<P::Event>,
        protocol: P,
    ) -> Self {
        let stop = Arc::new(AtomicBool::new(false));
        let stop_fd = eventfd(0, EventfdFlags::CLOEXEC | EventfdFlags::NONBLOCK)
            .ok()
            .map(Arc::new);
        let stop_flag = Arc::clone(&stop);
        let worker_stop_fd = stop_fd.clone();
        let join = std::thread::Builder::new()
            .name(P::THREAD_NAME.to_owned())
            .spawn(move || {
                run_worker(socket_path, sender, protocol, stop_flag, worker_stop_fd);
            })
            .ok();
        Self {
            stop,
            stop_fd,
            join,
        }
    }
}

impl Drop for StreamWorkerHandle {
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

fn run_worker<P: StreamProtocol>(
    socket_path: PathBuf,
    sender: calloop::channel::Sender<P::Event>,
    mut protocol: P,
    stop: Arc<AtomicBool>,
    stop_fd: Option<Arc<OwnedFd>>,
) {
    let waiter = StopWaiter {
        stop: &stop,
        stop_fd: stop_fd.as_deref(),
    };
    let mut ever_connected = false;
    let mut backoff_ms = RECONNECT_INITIAL_MS;
    let mut chunk = [0u8; READ_CHUNK_BYTES];

    while !waiter.is_stopped() {
        match UnixStream::connect(&socket_path) {
            Ok(mut stream) => {
                if stream.set_nonblocking(true).is_err() {
                    if waiter.wait_backoff(backoff_ms) {
                        return;
                    }
                    backoff_ms = (backoff_ms * 2).min(RECONNECT_MAX_MS);
                    continue;
                }

                protocol.reset();
                match protocol.handshake(&mut stream, &waiter) {
                    HandshakeResult::Ready => {}
                    HandshakeResult::Failed => {
                        if waiter.wait_backoff(backoff_ms) {
                            return;
                        }
                        backoff_ms = (backoff_ms * 2).min(RECONNECT_MAX_MS);
                        continue;
                    }
                    HandshakeResult::Stopped => return,
                }

                backoff_ms = RECONNECT_INITIAL_MS;
                if ever_connected && sender.send(P::reconnected_event()).is_err() {
                    return;
                }
                ever_connected = true;

                // Drain any events that arrived pipelined behind a handshake reply.
                let pre_pump = protocol.feed(&[], &mut |ev| sender.send(ev).is_err());
                let stop_worker = match pre_pump {
                    FeedOutcome::ReceiverClosed => true,
                    FeedOutcome::ProtocolError => false,
                    FeedOutcome::Continue => {
                        pump_stream(&mut stream, &waiter, &mut protocol, &sender, &mut chunk)
                    }
                };

                if stop_worker || waiter.is_stopped() {
                    return;
                }
                if sender.send(P::disconnected_event()).is_err() {
                    return;
                }
                if waiter.wait_backoff(backoff_ms) {
                    return;
                }
            }
            Err(_) => {
                if waiter.wait_backoff(backoff_ms) {
                    return;
                }
                backoff_ms = (backoff_ms * 2).min(RECONNECT_MAX_MS);
            }
        }
    }
}

enum DrainStatus {
    Continue,
    EofOrError,
    ReceiverClosed,
}

/// Returns `true` if the channel receiver closed or stop was requested.
fn pump_stream<P: StreamProtocol>(
    stream: &mut UnixStream,
    waiter: &StopWaiter<'_>,
    protocol: &mut P,
    sender: &calloop::channel::Sender<P::Event>,
    chunk: &mut [u8; READ_CHUNK_BYTES],
) -> bool {
    loop {
        match waiter.wait_ready(stream, PollFlags::IN, None) {
            WaitReady::Stopped => return true,
            WaitReady::Timeout | WaitReady::Error => return false,
            WaitReady::Ready(revents) => {
                if revents.contains(PollFlags::IN) || revents.contains(PollFlags::HUP) {
                    match drain_nonblocking_socket(stream, protocol, sender, chunk) {
                        DrainStatus::Continue => {
                            if revents.contains(PollFlags::HUP) || revents.contains(PollFlags::ERR)
                            {
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
    }
}

fn drain_nonblocking_socket<P: StreamProtocol>(
    stream: &mut UnixStream,
    protocol: &mut P,
    sender: &calloop::channel::Sender<P::Event>,
    chunk: &mut [u8; READ_CHUNK_BYTES],
) -> DrainStatus {
    let mut emit = |ev: P::Event| sender.send(ev).is_err();
    loop {
        match stream.read(chunk) {
            Ok(0) => {
                if protocol.finish(&mut emit) {
                    return DrainStatus::ReceiverClosed;
                }
                return DrainStatus::EofOrError;
            }
            Ok(n) => match protocol.feed(&chunk[..n], &mut emit) {
                FeedOutcome::Continue => {}
                FeedOutcome::ProtocolError => return DrainStatus::EofOrError,
                FeedOutcome::ReceiverClosed => return DrainStatus::ReceiverClosed,
            },
            Err(e) if e.kind() == ErrorKind::WouldBlock => return DrainStatus::Continue,
            Err(e) if e.kind() == ErrorKind::Interrupted => continue,
            Err(_) => return DrainStatus::EofOrError,
        }
    }
}

fn duration_to_timespec(d: Duration) -> Timespec {
    Timespec {
        tv_sec: i64::try_from(d.as_secs()).unwrap_or(i64::MAX),
        tv_nsec: i64::from(d.subsec_nanos()),
    }
}
