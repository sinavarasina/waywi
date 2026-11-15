use std::any::Any;
use std::collections::VecDeque;
use std::path::PathBuf;
use std::sync::mpsc;
use std::sync::{Arc, Condvar, Mutex};
use std::thread::{self, JoinHandle};

use crate::asset::{
    DecodedImage, decode_image_bytes, detect_format, hash_bytes, read_bounded_file,
};
use crate::error::{Result, WaywiError};
use crate::image::ImageAsset;

pub(crate) type BoxedMsgFactory = Box<dyn FnOnce() -> Box<dyn Any + Send> + Send>;
pub(crate) type BoxedAssetCallback =
    Box<dyn FnOnce(Result<ImageAsset>) -> Box<dyn Any + Send> + Send>;

pub(crate) struct AsyncAssetCompletion {
    pub(crate) path: PathBuf,
    pub(crate) content_hash: u64,
    pub(crate) decoded: Result<DecodedImage>,
    pub(crate) map: BoxedAssetCallback,
}

struct AsyncAssetJob {
    path: PathBuf,
    map: BoxedAssetCallback,
}

struct WorkerShared {
    queue: VecDeque<AsyncAssetJob>,
    stopped: bool,
}

type WakeupHook = Arc<Mutex<Option<Box<dyn Fn() + Send + Sync>>>>;

/// Bounded background worker thread used by [`EffectCtx::load_image_async`](super::EffectCtx::load_image_async)
/// so disk reads and PNG/SVG/GIF decoding never stall the single Wayland UI event loop.
pub(crate) struct AsyncAssetWorker {
    shared: Arc<(Mutex<WorkerShared>, Condvar)>,
    tx_done: mpsc::Sender<AsyncAssetCompletion>,
    rx_done: mpsc::Receiver<AsyncAssetCompletion>,
    wakeup: WakeupHook,
    handle: Mutex<Option<JoinHandle<()>>>,
}

const MAX_PENDING_ASYNC_JOBS: usize = 64;

impl Default for AsyncAssetWorker {
    fn default() -> Self {
        Self::new()
    }
}

fn notify_wakeup(wakeup: &WakeupHook) {
    if let Ok(guard) = wakeup.lock()
        && let Some(wake) = &*guard
    {
        wake();
    }
}

impl Drop for AsyncAssetWorker {
    fn drop(&mut self) {
        if let Ok(mut guard) = self.shared.0.lock() {
            guard.stopped = true;
            self.shared.1.notify_all();
        }
        if let Ok(mut guard) = self.handle.lock()
            && let Some(h) = guard.take()
        {
            let _ = h.join();
        }
    }
}

impl AsyncAssetWorker {
    pub(crate) fn new() -> Self {
        let (tx_done, rx_done) = mpsc::channel();
        Self {
            shared: Arc::new((
                Mutex::new(WorkerShared {
                    queue: VecDeque::new(),
                    stopped: false,
                }),
                Condvar::new(),
            )),
            tx_done,
            rx_done,
            wakeup: Arc::new(Mutex::new(None)),
            handle: Mutex::new(None),
        }
    }

    pub(crate) fn set_wakeup<F>(&self, wake: F)
    where
        F: Fn() + Send + Sync + 'static,
    {
        if let Ok(mut guard) = self.wakeup.lock() {
            *guard = Some(Box::new(wake));
        }
    }

    fn ensure_started(&self) {
        let mut handle_guard = self.handle.lock().unwrap_or_else(|e| e.into_inner());
        if handle_guard.is_some() {
            return;
        }
        let shared = Arc::clone(&self.shared);
        let tx = self.tx_done.clone();
        let wakeup = Arc::clone(&self.wakeup);

        let spawned = thread::Builder::new()
            .name("waywi-asset-loader".to_string())
            .spawn(move || {
                loop {
                    let job = {
                        let (lock, cvar) = &*shared;
                        let mut guard = match lock.lock() {
                            Ok(g) => g,
                            Err(_) => break,
                        };
                        while guard.queue.is_empty() && !guard.stopped {
                            guard = match cvar.wait(guard) {
                                Ok(g) => g,
                                Err(_) => return,
                            };
                        }
                        if guard.stopped {
                            break;
                        }
                        guard.queue.pop_front()
                    };

                    let Some(job) = job else {
                        continue;
                    };

                    let canonical = std::fs::canonicalize(&job.path).unwrap_or(job.path);
                    let mut hash = 0u64;
                    let decoded = read_bounded_file(&canonical).and_then(|bytes| {
                        hash = hash_bytes(&bytes);
                        let fmt = detect_format(&bytes, Some(&canonical))?;
                        decode_image_bytes(&bytes, fmt, 0)
                    });

                    let _ = tx.send(AsyncAssetCompletion {
                        path: canonical,
                        content_hash: hash,
                        decoded,
                        map: job.map,
                    });
                    notify_wakeup(&wakeup);
                }
            });

        if let Ok(h) = spawned {
            *handle_guard = Some(h);
        }
    }

    pub(crate) fn submit(&self, path: PathBuf, map: BoxedAssetCallback) {
        self.ensure_started();
        let (lock, cvar) = &*self.shared;
        if let Ok(mut guard) = lock.lock() {
            if guard.queue.len() >= MAX_PENDING_ASYNC_JOBS {
                let dropped = guard.queue.pop_front();
                if let Some(d) = dropped {
                    let _ = self.tx_done.send(AsyncAssetCompletion {
                        path: d.path,
                        content_hash: 0,
                        decoded: Err(WaywiError::AssetDecode(
                            "async asset decode queue capacity reached".to_owned(),
                        )),
                        map: d.map,
                    });
                    notify_wakeup(&self.wakeup);
                }
            }
            guard.queue.push_back(AsyncAssetJob { path, map });
            cvar.notify_one();
        }
    }

    pub(crate) fn drain_completed(&self) -> Vec<AsyncAssetCompletion> {
        let mut out = Vec::new();
        while let Ok(item) = self.rx_done.try_recv() {
            out.push(item);
        }
        out
    }
}
