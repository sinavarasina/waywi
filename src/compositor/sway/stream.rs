use std::io::{ErrorKind, Read, Write};
use std::os::unix::net::UnixStream;
use std::path::PathBuf;
use std::time::Instant;

use rustix::event::PollFlags;

use crate::compositor::stream_worker::{
    FeedOutcome, HandshakeResult, StopWaiter, StreamProtocol, StreamWorkerHandle, WaitReady,
};

use super::client::{DEFAULT_TIMEOUT, MAX_PAYLOAD_BYTES, MSG_SUBSCRIBE, SWAY_MAGIC, encode_frame};
use super::types::{CommandOutcome, SwayEvent, SwayWorkspace};

// Sway IPC event types (bit 31 set).
const EVENT_MASK: u32 = 0x8000_0000;
const EVT_WORKSPACE: u32 = 0x8000_0000;
const EVT_OUTPUT: u32 = 0x8000_0001;
const EVT_MODE: u32 = 0x8000_0002;
const EVT_WINDOW: u32 = 0x8000_0003;
const EVT_SHUTDOWN: u32 = 0x8000_0006;

const SUBSCRIBE_PAYLOAD: &[u8] = br#"["workspace","output","window","mode","shutdown"]"#;
const MAX_ACCUM_BYTES: usize = MAX_PAYLOAD_BYTES + 64 * 1024;
const COMPACT_THRESHOLD_BYTES: usize = 4096;
const MAX_RETAINED_BUF_CAPACITY: usize = 64 * 1024;

/// Parses an incremental Sway event frame payload without issuing follow-up queries.
///
/// Returns `None` for non-event frames (missing bit 31) or malformed JSON payloads.
#[must_use]
pub(crate) fn parse_event_frame(event_type: u32, payload: &[u8]) -> Option<SwayEvent> {
    if event_type & EVENT_MASK == 0 {
        return None;
    }
    let value: serde_json::Value = serde_json::from_slice(payload).ok()?;
    let change = value
        .get("change")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_owned();

    let ev = match event_type {
        EVT_WORKSPACE => {
            let current = value
                .get("current")
                .and_then(|v| serde_json::from_value::<SwayWorkspace>(v.clone()).ok());
            let old = value
                .get("old")
                .and_then(|v| serde_json::from_value::<SwayWorkspace>(v.clone()).ok());
            SwayEvent::Workspace {
                change,
                current,
                old,
            }
        }
        EVT_OUTPUT => SwayEvent::Output { change },
        EVT_WINDOW => {
            let container = value.get("container");
            SwayEvent::Window {
                change,
                container_id: container.and_then(|c| c.get("id")).and_then(|v| v.as_i64()),
                name: container
                    .and_then(|c| c.get("name"))
                    .and_then(|v| v.as_str())
                    .map(str::to_owned),
                app_id: container
                    .and_then(|c| c.get("app_id"))
                    .and_then(|v| v.as_str())
                    .map(str::to_owned),
            }
        }
        EVT_MODE => SwayEvent::Mode { change },
        EVT_SHUTDOWN => SwayEvent::Shutdown { reason: change },
        _ => SwayEvent::Unknown {
            event_type,
            payload: String::from_utf8_lossy(payload).into_owned(),
        },
    };
    Some(ev)
}

/// Zero-polling, fully non-blocking Sway IPC event listener bridge backed by
/// [`StreamWorkerHandle`] (`poll(2)` + `eventfd(2)`).
///
/// Both the `SUBSCRIBE` handshake and all subsequent `i3-ipc` binary frame reads run non-blocking
/// with an incremental byte accumulator buffer, so [`Drop`] wakes and joins immediately even if a
/// frame or handshake is stalled mid-transfer.
pub(crate) struct SwayListenerHandle {
    _worker: StreamWorkerHandle,
}

impl SwayListenerHandle {
    pub(crate) fn spawn(socket_path: PathBuf, sender: calloop::channel::Sender<SwayEvent>) -> Self {
        Self {
            _worker: StreamWorkerHandle::spawn(socket_path, sender, SwayStreamProtocol::default()),
        }
    }
}

#[derive(Default)]
struct SwayStreamProtocol {
    buf: Vec<u8>,
    read_pos: usize,
}

enum FramePeek {
    Incomplete,
    Invalid,
    Ready {
        msg_type: u32,
        payload_start: usize,
        payload_end: usize,
        next_pos: usize,
    },
}

impl SwayStreamProtocol {
    fn peek_frame(&self) -> FramePeek {
        let unread = &self.buf[self.read_pos..];
        if unread.len() < 14 {
            return FramePeek::Incomplete;
        }
        if &unread[..6] != SWAY_MAGIC {
            return FramePeek::Invalid;
        }
        let len = u32::from_le_bytes([unread[6], unread[7], unread[8], unread[9]]) as usize;
        if len > MAX_PAYLOAD_BYTES {
            return FramePeek::Invalid;
        }
        let msg_type = u32::from_le_bytes([unread[10], unread[11], unread[12], unread[13]]);
        let total = 14 + len;
        if unread.len() < total {
            return FramePeek::Incomplete;
        }
        FramePeek::Ready {
            msg_type,
            payload_start: self.read_pos + 14,
            payload_end: self.read_pos + total,
            next_pos: self.read_pos + total,
        }
    }

    fn compact_buf(&mut self) {
        if self.read_pos == self.buf.len() {
            self.buf.clear();
            self.read_pos = 0;
            if self.buf.capacity() > MAX_RETAINED_BUF_CAPACITY {
                self.buf.shrink_to(MAX_RETAINED_BUF_CAPACITY);
            }
        } else if self.read_pos >= COMPACT_THRESHOLD_BYTES {
            self.buf.drain(..self.read_pos);
            self.read_pos = 0;
        }
    }
}

impl StreamProtocol for SwayStreamProtocol {
    type Event = SwayEvent;

    const THREAD_NAME: &'static str = "waywi-sway-events";

    fn disconnected_event() -> Self::Event {
        SwayEvent::Disconnected
    }

    fn reconnected_event() -> Self::Event {
        SwayEvent::Reconnected
    }

    fn reset(&mut self) {
        self.buf.clear();
        self.read_pos = 0;
        if self.buf.capacity() > MAX_RETAINED_BUF_CAPACITY {
            self.buf.shrink_to(MAX_RETAINED_BUF_CAPACITY);
        }
    }

    fn handshake(&mut self, stream: &mut UnixStream, waiter: &StopWaiter<'_>) -> HandshakeResult {
        let deadline = Instant::now() + DEFAULT_TIMEOUT;
        let sub_frame = encode_frame(MSG_SUBSCRIBE, SUBSCRIBE_PAYLOAD);
        let mut written = 0usize;

        while written < sub_frame.len() {
            match stream.write(&sub_frame[written..]) {
                Ok(0) => return HandshakeResult::Failed,
                Ok(n) => written += n,
                Err(e) if e.kind() == ErrorKind::Interrupted => continue,
                Err(e) if e.kind() == ErrorKind::WouldBlock => {
                    let Some(rem) = deadline.checked_duration_since(Instant::now()) else {
                        return HandshakeResult::Failed;
                    };
                    match waiter.wait_ready(stream, PollFlags::OUT, Some(rem)) {
                        WaitReady::Ready(rev) => {
                            if rev.contains(PollFlags::ERR)
                                || rev.contains(PollFlags::HUP)
                                || rev.contains(PollFlags::NVAL)
                            {
                                return HandshakeResult::Failed;
                            }
                        }
                        WaitReady::Stopped => return HandshakeResult::Stopped,
                        WaitReady::Timeout | WaitReady::Error => return HandshakeResult::Failed,
                    }
                }
                Err(_) => return HandshakeResult::Failed,
            }
        }

        let mut chunk = [0u8; 4096];
        loop {
            match self.peek_frame() {
                FramePeek::Ready {
                    msg_type,
                    payload_start,
                    payload_end,
                    next_pos,
                } => {
                    let ok = msg_type == MSG_SUBSCRIBE
                        && !matches!(
                            serde_json::from_slice::<CommandOutcome>(
                                &self.buf[payload_start..payload_end],
                            ),
                            Ok(o) if !o.success
                        );
                    self.read_pos = next_pos;
                    self.compact_buf();
                    return if ok {
                        HandshakeResult::Ready
                    } else {
                        HandshakeResult::Failed
                    };
                }
                FramePeek::Invalid => return HandshakeResult::Failed,
                FramePeek::Incomplete => {}
            }

            match stream.read(&mut chunk) {
                Ok(0) => return HandshakeResult::Failed,
                Ok(n) => {
                    self.buf.extend_from_slice(&chunk[..n]);
                    if self.buf.len() - self.read_pos > MAX_ACCUM_BYTES {
                        return HandshakeResult::Failed;
                    }
                }
                Err(e) if e.kind() == ErrorKind::Interrupted => continue,
                Err(e) if e.kind() == ErrorKind::WouldBlock => {
                    let Some(rem) = deadline.checked_duration_since(Instant::now()) else {
                        return HandshakeResult::Failed;
                    };
                    match waiter.wait_ready(stream, PollFlags::IN, Some(rem)) {
                        WaitReady::Ready(rev) => {
                            if !rev.contains(PollFlags::IN)
                                && (rev.contains(PollFlags::HUP)
                                    || rev.contains(PollFlags::ERR)
                                    || rev.contains(PollFlags::NVAL))
                            {
                                return HandshakeResult::Failed;
                            }
                        }
                        WaitReady::Stopped => return HandshakeResult::Stopped,
                        WaitReady::Timeout | WaitReady::Error => return HandshakeResult::Failed,
                    }
                }
                Err(_) => return HandshakeResult::Failed,
            }
        }
    }

    fn feed(&mut self, chunk: &[u8], emit: &mut dyn FnMut(Self::Event) -> bool) -> FeedOutcome {
        if !chunk.is_empty() {
            self.buf.extend_from_slice(chunk);
            if self.buf.len() - self.read_pos > MAX_ACCUM_BYTES {
                return FeedOutcome::ProtocolError;
            }
        }

        loop {
            match self.peek_frame() {
                FramePeek::Incomplete => break,
                FramePeek::Invalid => return FeedOutcome::ProtocolError,
                FramePeek::Ready {
                    msg_type,
                    payload_start,
                    payload_end,
                    next_pos,
                } => {
                    let parsed = parse_event_frame(msg_type, &self.buf[payload_start..payload_end]);
                    self.read_pos = next_pos;
                    if let Some(ev) = parsed
                        && emit(ev)
                    {
                        self.compact_buf();
                        return FeedOutcome::ReceiverClosed;
                    }
                }
            }
        }

        self.compact_buf();
        FeedOutcome::Continue
    }
}

#[cfg(test)]
mod tests {
    use std::os::unix::net::UnixListener;
    use std::sync::atomic::{AtomicU64, Ordering};
    use std::time::Duration;

    use super::super::client::read_frame;
    use super::*;

    static SWAY_STREAM_SEQ: AtomicU64 = AtomicU64::new(0);

    #[test]
    fn parse_frame_handles_events_unknowns_non_events_and_malformed_json() {
        let ws = SwayEvent::parse_frame(
            EVT_WORKSPACE,
            br#"{"change":"focus","current":{"id":1,"num":2,"name":"2","focused":true,"output":"DP-1"},"old":null}"#,
        );
        assert!(matches!(
            ws,
            Some(SwayEvent::Workspace { ref change, current: Some(ref cur), old: None })
                if change == "focus" && cur.num == 2 && cur.output == "DP-1"
        ));

        let unknown_evt = SwayEvent::parse_frame(0x8000_0015, br#"{"change":"xkb_layout"}"#);
        assert!(matches!(
            unknown_evt,
            Some(SwayEvent::Unknown {
                event_type: 0x8000_0015,
                ..
            })
        ));

        // Non-event message types (missing bit 31) return None.
        assert_eq!(
            SwayEvent::parse_frame(MSG_SUBSCRIBE, br#"{"success":true}"#),
            None
        );
        // Malformed JSON payloads return None.
        assert_eq!(SwayEvent::parse_frame(EVT_WINDOW, b"{not valid json"), None);
    }

    #[test]
    fn nonblocking_sway_listener_handles_pipelined_split_frames_and_instant_mid_frame_drop() {
        let dir = std::env::temp_dir().join(format!(
            "waywi-sway-stream-{}-{}",
            std::process::id(),
            SWAY_STREAM_SEQ.fetch_add(1, Ordering::Relaxed)
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let sock_path = dir.join("sway.sock");
        let listener = UnixListener::bind(&sock_path).unwrap();

        let (tx, rx) = calloop::channel::channel::<SwayEvent>();
        let handle = SwayListenerHandle::spawn(sock_path.clone(), tx);

        let (mut peer, _) = listener.accept().unwrap();
        let (sub_type, _) = read_frame(&mut peer).unwrap();
        assert_eq!(sub_type, MSG_SUBSCRIBE);

        // Pipeline subscribe reply + first event in a single write.
        let mut combined = encode_frame(MSG_SUBSCRIBE, br#"{"success":true}"#);
        combined.extend_from_slice(&encode_frame(EVT_OUTPUT, br#"{"change":"unspecified"}"#));
        peer.write_all(&combined).unwrap();
        peer.flush().unwrap();

        // Send a second event split mid-header and mid-payload across separate writes.
        let second = encode_frame(EVT_MODE, br#"{"change":"resize"}"#);
        peer.write_all(&second[..7]).unwrap();
        peer.flush().unwrap();
        std::thread::sleep(Duration::from_millis(5));
        peer.write_all(&second[7..16]).unwrap();
        peer.flush().unwrap();
        std::thread::sleep(Duration::from_millis(5));
        peer.write_all(&second[16..]).unwrap();
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
                SwayEvent::Output {
                    change: "unspecified".to_owned(),
                },
                SwayEvent::Mode {
                    change: "resize".to_owned(),
                },
            ]
        );

        // Send only half of a third frame header and leave the peer stalled: Drop MUST return
        // immediately (< 100ms), never blocking on a 2-second read timeout!
        let partial = encode_frame(EVT_SHUTDOWN, br#"{"change":"exit"}"#);
        peer.write_all(&partial[..10]).unwrap();
        peer.flush().unwrap();
        std::thread::sleep(Duration::from_millis(10));

        let drop_start = Instant::now();
        drop(handle);
        assert!(
            drop_start.elapsed() < Duration::from_millis(100),
            "expected instant non-blocking Drop mid-frame, took {:?}",
            drop_start.elapsed()
        );

        drop(peer);
        drop(listener);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn nonblocking_sway_listener_drops_instantly_during_stalled_handshake_and_backoff() {
        let dir = std::env::temp_dir().join(format!(
            "waywi-sway-stalled-{}-{}",
            std::process::id(),
            SWAY_STREAM_SEQ.fetch_add(1, Ordering::Relaxed)
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let sock_path = dir.join("sway-stalled.sock");
        let listener = UnixListener::bind(&sock_path).unwrap();

        let (tx, _rx) = calloop::channel::channel::<SwayEvent>();
        let handle = SwayListenerHandle::spawn(sock_path.clone(), tx);

        // Accept connection and read subscribe request, but never send subscribe reply.
        let (mut peer, _) = listener.accept().unwrap();
        let (sub_type, _) = read_frame(&mut peer).unwrap();
        assert_eq!(sub_type, MSG_SUBSCRIBE);
        std::thread::sleep(Duration::from_millis(10));

        let drop_start = Instant::now();
        drop(handle);
        assert!(
            drop_start.elapsed() < Duration::from_millis(100),
            "expected instant Drop during stalled handshake, took {:?}",
            drop_start.elapsed()
        );

        drop(peer);
        drop(listener);
        let _ = std::fs::remove_dir_all(&dir);
    }
}
