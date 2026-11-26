use std::future::Future;
use std::pin::Pin;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::task::{Context, Poll, Waker};
use std::thread::JoinHandle;

use super::client::{DbusSharedBus, connect_shared_bus, decode_message_body};
use super::types::{DbusBus, DbusSignalEvent};

/// Zero-polling asynchronous stop signal shared across D-Bus background workers.
#[derive(Clone, Default)]
pub(crate) struct AsyncStopSignal {
    stopped: Arc<AtomicBool>,
    waker: Arc<Mutex<Option<Waker>>>,
}

impl AsyncStopSignal {
    pub(crate) fn new() -> Self {
        Self::default()
    }

    pub(crate) fn is_stopped(&self) -> bool {
        self.stopped.load(Ordering::Acquire)
    }

    pub(crate) fn trigger(&self) {
        self.stopped.store(true, Ordering::Release);
        if let Ok(mut guard) = self.waker.lock()
            && let Some(w) = guard.take()
        {
            w.wake();
        }
    }

    pub(crate) fn wait_stopped(&self) -> StopFuture {
        StopFuture {
            stopped: Arc::clone(&self.stopped),
            waker: Arc::clone(&self.waker),
        }
    }
}

pub(crate) struct StopFuture {
    stopped: Arc<AtomicBool>,
    waker: Arc<Mutex<Option<Waker>>>,
}

impl Future for StopFuture {
    type Output = ();

    fn poll(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<()> {
        if self.stopped.load(Ordering::Acquire) {
            return Poll::Ready(());
        }
        if let Ok(mut guard) = self.waker.lock() {
            *guard = Some(cx.waker().clone());
        }
        if self.stopped.load(Ordering::Acquire) {
            Poll::Ready(())
        } else {
            Poll::Pending
        }
    }
}

struct AsyncReplyState<T> {
    reply: Option<T>,
    waker: Option<Waker>,
}

/// Reusable zero-polling waker-backed oneshot slot shared across D-Bus interactive agents
/// (BlueZ `Agent1`, NetworkManager `SecretAgent`, Polkit `AuthenticationAgent`) and background
/// helper threads.
pub(crate) struct AsyncReplySlot<T> {
    state: Mutex<AsyncReplyState<T>>,
}

impl<T> AsyncReplySlot<T> {
    pub(crate) fn new() -> Arc<Self> {
        Arc::new(Self {
            state: Mutex::new(AsyncReplyState {
                reply: None,
                waker: None,
            }),
        })
    }

    pub(crate) fn resolve(&self, reply: T) {
        if let Ok(mut guard) = self.state.lock() {
            if guard.reply.is_none() {
                guard.reply = Some(reply);
            }
            if let Some(waker) = guard.waker.take() {
                waker.wake();
            }
        }
    }

    pub(crate) fn wait(&self) -> AsyncReplyFuture<'_, T> {
        AsyncReplyFuture { slot: self }
    }
}

pub(crate) struct AsyncReplyFuture<'a, T> {
    slot: &'a AsyncReplySlot<T>,
}

impl<T> Future for AsyncReplyFuture<'_, T> {
    type Output = Option<T>;

    fn poll(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
        let Ok(mut guard) = self.slot.state.lock() else {
            return Poll::Ready(None);
        };
        if let Some(reply) = guard.reply.take() {
            return Poll::Ready(Some(reply));
        }
        guard.waker = Some(cx.waker().clone());
        Poll::Pending
    }
}

/// Reusable RAII thread handle for background D-Bus workers.
///
/// Triggers [`AsyncStopSignal`] and joins the worker thread cleanly on drop.
pub(crate) struct DbusWorkerHandle {
    stop: AsyncStopSignal,
    join: Option<JoinHandle<()>>,
}

impl DbusWorkerHandle {
    pub(crate) fn spawn<F>(name: &'static str, f: F) -> Self
    where
        F: FnOnce(AsyncStopSignal) + Send + 'static,
    {
        let stop = AsyncStopSignal::new();
        let stop_sig = stop.clone();
        let join = std::thread::Builder::new()
            .name(name.to_owned())
            .spawn(move || f(stop_sig))
            .ok();
        Self { stop, join }
    }

    pub(crate) fn trigger_stop(&self) {
        self.stop.trigger();
    }
}

impl Drop for DbusWorkerHandle {
    fn drop(&mut self) {
        self.stop.trigger();
        if let Some(j) = self.join.take() {
            let _ = j.join();
        }
    }
}

/// Dedicated lightweight D-Bus signal listener thread using `zbus` + `futures_lite` (zero `tokio`).
pub(crate) struct DbusSignalWorker(#[allow(dead_code)] DbusWorkerHandle);

impl DbusSignalWorker {
    pub(crate) fn spawn(
        bus: DbusBus,
        interface: String,
        member: String,
        path: Option<String>,
        sender: calloop::channel::Sender<DbusSignalEvent>,
        shared: DbusSharedBus,
    ) -> Self {
        Self(DbusWorkerHandle::spawn(
            "waywi-dbus-signal",
            move |stop_sig| {
                let Ok(conn) = connect_shared_bus(bus, &shared) else {
                    return;
                };
                futures_lite::future::block_on(async move {
                    let Ok(rule) = zbus::MatchRule::builder()
                        .msg_type(zbus::message::Type::Signal)
                        .interface(interface.as_str())
                        .and_then(|b| b.member(member.as_str()))
                    else {
                        return;
                    };
                    let rule = if let Some(ref p) = path {
                        let Ok(r2) = rule.path(p.as_str()) else {
                            return;
                        };
                        r2
                    } else {
                        rule
                    };
                    let Ok(mut stream) =
                        zbus::MessageStream::for_match_rule(rule.build(), &conn, Some(64)).await
                    else {
                        return;
                    };

                    use futures_lite::StreamExt;
                    while !stop_sig.is_stopped() {
                        let next_msg =
                            futures_lite::future::or(async { Some(stream.next().await) }, async {
                                stop_sig.wait_stopped().await;
                                None
                            })
                            .await;
                        let Some(item) = next_msg else {
                            break;
                        };
                        match item {
                            Some(Ok(msg)) => {
                                let header = msg.header();
                                let ev = DbusSignalEvent {
                                    sender: header.sender().map(|s| s.to_string()),
                                    path: header.path().map(|p| p.to_string()),
                                    interface: header
                                        .interface()
                                        .map_or_else(|| interface.clone(), |i| i.to_string()),
                                    member: header
                                        .member()
                                        .map_or_else(|| member.clone(), |m| m.to_string()),
                                    args: decode_message_body(&msg),
                                };
                                let _ = sender.send(ev);
                            }
                            Some(Err(_)) | None => break,
                        }
                    }
                });
            },
        ))
    }
}

/// Reusable async D-Bus watcher thread that emits initial snapshots and updates on matching signals.
pub(crate) struct ServiceWatchWorker(#[allow(dead_code)] DbusWorkerHandle);

impl ServiceWatchWorker {
    pub(crate) fn spawn<T, F, Fut>(
        bus: DbusBus,
        thread_name: &'static str,
        sender_name: Option<&'static str>,
        interface: Option<&'static str>,
        sender: calloop::channel::Sender<T>,
        shared: DbusSharedBus,
        mut fetch: F,
    ) -> Self
    where
        T: Clone + PartialEq + Send + 'static,
        F: FnMut(zbus::Connection) -> Fut + Send + 'static,
        Fut: std::future::Future<Output = Option<T>>,
    {
        Self(DbusWorkerHandle::spawn(thread_name, move |stop_sig| {
            let Ok(conn) = connect_shared_bus(bus, &shared) else {
                return;
            };
            futures_lite::future::block_on(async move {
                let mut last: Option<T> = None;
                if let Some(snap) = fetch(conn.clone()).await {
                    last = Some(snap.clone());
                    let _ = sender.send(snap);
                }

                let mut rule_builder =
                    zbus::MatchRule::builder().msg_type(zbus::message::Type::Signal);
                if let Some(s) = sender_name {
                    let Ok(r) = rule_builder.sender(s) else {
                        return;
                    };
                    rule_builder = r;
                }
                if let Some(i) = interface {
                    let Ok(r) = rule_builder.interface(i) else {
                        return;
                    };
                    rule_builder = r;
                }
                let Ok(mut stream) =
                    zbus::MessageStream::for_match_rule(rule_builder.build(), &conn, Some(64))
                        .await
                else {
                    return;
                };

                use futures_lite::StreamExt;
                while !stop_sig.is_stopped() {
                    let next_msg =
                        futures_lite::future::or(async { Some(stream.next().await) }, async {
                            stop_sig.wait_stopped().await;
                            None
                        })
                        .await;
                    let Some(Some(Ok(_))) = next_msg else {
                        break;
                    };
                    // Drain all immediately-ready buffered signals so BLE discovery or UDisks
                    // signal bursts trigger a single refresh instead of hammering the bus.
                    let mut stream_closed = false;
                    while let Some(item) = futures_lite::future::poll_once(stream.next()).await {
                        match item {
                            Some(Ok(_)) => {}
                            Some(Err(_)) | None => {
                                stream_closed = true;
                                break;
                            }
                        }
                    }
                    if stop_sig.is_stopped() {
                        break;
                    }
                    if let Some(snap) = fetch(conn.clone()).await
                        && last.as_ref() != Some(&snap)
                    {
                        last = Some(snap.clone());
                        let _ = sender.send(snap);
                    }
                    if stream_closed {
                        break;
                    }
                }
            });
        }))
    }
}
