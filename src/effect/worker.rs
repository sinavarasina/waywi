use std::any::Any;
use std::collections::VecDeque;
use std::path::PathBuf;
use std::sync::mpsc;
use std::sync::{Arc, Condvar, Mutex};
use std::thread::{self, JoinHandle};

use crate::asset::types::ImageFormat;
use crate::asset::{
    DecodedImage, decode_image_bytes, detect_format, hash_bytes, read_bounded_file,
};
use crate::error::{Result, WaywiError};
use crate::image::ImageAsset;

pub(crate) type AssetCallback<M> = Box<dyn FnOnce(Result<ImageAsset>) -> M>;
pub(crate) type ActivationCallback<M> = Box<dyn FnOnce(Result<String>) -> M>;
pub(crate) type ScreencopyCallback<M> =
    Box<dyn FnOnce(Result<crate::wayland_ext::ScreencopyFrame>) -> M>;
pub(crate) type BlockingTaskJob = Box<dyn FnOnce() -> Box<dyn Any + Send> + Send>;
pub(crate) type BlockingTaskCallback<M> = Box<dyn FnOnce(Box<dyn Any + Send>) -> M>;

/// Imperative foreign-toplevel control action queued from [`EffectCtx`](super::EffectCtx).
#[derive(Debug, Clone)]
pub(crate) enum ForeignToplevelAction {
    Activate(crate::wayland_ext::ForeignToplevelId),
    Close(crate::wayland_ext::ForeignToplevelId),
    SetMinimized(crate::wayland_ext::ForeignToplevelId, bool),
    SetMaximized(crate::wayland_ext::ForeignToplevelId, bool),
    SetFullscreen(
        crate::wayland_ext::ForeignToplevelId,
        bool,
        Option<crate::display::OutputId>,
    ),
}

/// Imperative `ext_workspace_v1` mutation queued from [`EffectCtx`](super::EffectCtx).
#[derive(Debug, Clone)]
pub(crate) enum ExtWorkspaceAction {
    Activate(crate::wayland_ext::ExtWorkspaceId),
    Deactivate(crate::wayland_ext::ExtWorkspaceId),
    Create {
        group: crate::wayland_ext::ExtWorkspaceGroupId,
        name: String,
    },
    Remove(crate::wayland_ext::ExtWorkspaceId),
}

/// Imperative `zwp_virtual_keyboard_v1` action queued from [`EffectCtx`](super::EffectCtx).
#[derive(Debug, Clone)]
pub(crate) enum VirtualKeyboardAction {
    Key {
        keycode: u32,
        state: crate::input::VirtualKeyState,
    },
    Modifiers {
        depressed: u32,
        latched: u32,
        locked: u32,
        group: u32,
    },
    Keymap(String),
}

/// Imperative outgoing `wl_data_device.start_drag` request queued from [`EffectCtx`](super::EffectCtx).
#[derive(Debug, Clone)]
pub(crate) struct DragStartRequest {
    pub(crate) surface_key: crate::surface::SurfaceKey,
    pub(crate) entries: Vec<(String, Vec<u8>)>,
    pub(crate) actions: crate::input::DndAction,
}

/// Imperative `zwlr_gamma_control_manager_v1` action queued from [`EffectCtx`](super::EffectCtx).
#[derive(Debug, Clone)]
pub(crate) enum GammaAction {
    SetRamp {
        output: crate::display::Output,
        ramp: crate::wayland_ext::GammaRamp,
    },
    SetTemperature {
        output: crate::display::Output,
        kelvin: u32,
        brightness: f32,
    },
    Reset {
        output: crate::display::Output,
    },
}

pub(crate) struct AsyncAssetCompletion {
    pub(crate) job_id: u64,
    pub(crate) path: PathBuf,
    pub(crate) format: Option<ImageFormat>,
    pub(crate) content_hash: u64,
    pub(crate) decoded: Result<DecodedImage>,
}

struct AsyncAssetJob {
    job_id: u64,
    path: PathBuf,
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
                    let mut format = None;
                    let decoded = read_bounded_file(&canonical).and_then(|bytes| {
                        hash = hash_bytes(&bytes);
                        let fmt = detect_format(&bytes, Some(&canonical))?;
                        format = Some(fmt);
                        decode_image_bytes(&bytes, fmt, 0)
                    });

                    let _ = tx.send(AsyncAssetCompletion {
                        job_id: job.job_id,
                        path: canonical,
                        format,
                        content_hash: hash,
                        decoded,
                    });
                    notify_wakeup(&wakeup);
                }
            });

        if let Ok(h) = spawned {
            *handle_guard = Some(h);
        }
    }

    pub(crate) fn submit(&self, job_id: u64, path: PathBuf) {
        self.ensure_started();
        let (lock, cvar) = &*self.shared;
        if let Ok(mut guard) = lock.lock() {
            if guard.queue.len() >= MAX_PENDING_ASYNC_JOBS {
                let dropped = guard.queue.pop_front();
                if let Some(d) = dropped {
                    let _ = self.tx_done.send(AsyncAssetCompletion {
                        job_id: d.job_id,
                        path: d.path,
                        format: None,
                        content_hash: 0,
                        decoded: Err(WaywiError::AssetDecode(
                            "async asset decode queue capacity reached".to_owned(),
                        )),
                    });
                    notify_wakeup(&self.wakeup);
                }
            }
            guard.queue.push_back(AsyncAssetJob { job_id, path });
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

pub(crate) struct AsyncTaskCompletion {
    pub(crate) job_id: u64,
    pub(crate) payload: Box<dyn Any + Send>,
}

struct AsyncTaskEntry {
    job_id: u64,
    job: BlockingTaskJob,
}

struct TaskWorkerShared {
    queue: VecDeque<AsyncTaskEntry>,
    stopped: bool,
}

/// Lazy single background worker thread used by
/// [`EffectCtx::spawn_blocking`](super::EffectCtx::spawn_blocking) so user blocking tasks never
/// stall the Wayland event loop and wake `calloop` via its registered ping source on completion.
pub(crate) struct AsyncTaskWorker {
    shared: Arc<(Mutex<TaskWorkerShared>, Condvar)>,
    tx_done: mpsc::Sender<AsyncTaskCompletion>,
    rx_done: mpsc::Receiver<AsyncTaskCompletion>,
    wakeup: WakeupHook,
    handle: Mutex<Option<JoinHandle<()>>>,
}

impl Default for AsyncTaskWorker {
    fn default() -> Self {
        Self::new()
    }
}

impl Drop for AsyncTaskWorker {
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

impl AsyncTaskWorker {
    pub(crate) fn new() -> Self {
        let (tx_done, rx_done) = mpsc::channel();
        Self {
            shared: Arc::new((
                Mutex::new(TaskWorkerShared {
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
            .name("waywi-task-worker".to_string())
            .spawn(move || {
                loop {
                    let entry = {
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

                    let Some(entry) = entry else {
                        continue;
                    };
                    let payload = (entry.job)();
                    let _ = tx.send(AsyncTaskCompletion {
                        job_id: entry.job_id,
                        payload,
                    });
                    notify_wakeup(&wakeup);
                }
            });

        if let Ok(h) = spawned {
            *handle_guard = Some(h);
        }
    }

    pub(crate) fn submit(&self, job_id: u64, job: BlockingTaskJob) {
        self.ensure_started();
        let (lock, cvar) = &*self.shared;
        if let Ok(mut guard) = lock.lock() {
            guard.queue.push_back(AsyncTaskEntry { job_id, job });
            cvar.notify_one();
        }
    }

    pub(crate) fn drain_completed(&self) -> Vec<AsyncTaskCompletion> {
        let mut out = Vec::new();
        while let Ok(item) = self.rx_done.try_recv() {
            out.push(item);
        }
        out
    }
}
