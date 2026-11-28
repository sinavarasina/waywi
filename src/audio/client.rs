//! Backend-agnostic public client handle for audio telemetry and volume/device control.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;

use crate::error::{Result, WaywiError};

use super::backend::{
    AudioBackend, AudioCommand, AudioCommandSender, SharedAudioState, candidate_backends,
};
use super::types::{
    AudioDirection, AudioEvent, AudioPortTarget, AudioSnapshot, AudioTarget,
    sanitize_linear_channels,
};

struct RunningWorker {
    sender: Box<dyn AudioCommandSender>,
    join: Option<JoinHandle<()>>,
}

struct AudioClientInner {
    shared: Arc<Mutex<SharedAudioState>>,
    has_pending: Arc<AtomicBool>,
    stop_flag: Arc<AtomicBool>,
    subscriber_attached: AtomicBool,
    backends: Vec<Box<dyn AudioBackend>>,
    worker: Mutex<Option<RunningWorker>>,
}

impl AudioClientInner {
    fn stop_worker(&self) {
        self.stop_flag.store(true, Ordering::Release);
        let running = {
            let mut guard = self.worker.lock().unwrap_or_else(|e| e.into_inner());
            guard.take()
        };
        if let Some(mut running) = running {
            let _ = running.sender.send(AudioCommand::Shutdown);
            if let Some(handle) = running.join.take() {
                let _ = handle.join();
            }
        }
        if let Ok(mut shared) = self.shared.lock() {
            shared.snapshot = None;
            shared.pending_snapshot = None;
            shared.pending_events.clear();
            shared.connected = false;
            shared.has_pending.store(false, Ordering::Release);
        }
    }
}

impl Drop for AudioClientInner {
    fn drop(&mut self) {
        self.stop_worker();
    }
}

/// Handle for interacting with the active audio backend from app effects and subscriptions.
#[derive(Clone)]
pub struct AudioClient {
    inner: Arc<AudioClientInner>,
}

impl std::fmt::Debug for AudioClient {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("AudioClient").finish_non_exhaustive()
    }
}

impl Default for AudioClient {
    fn default() -> Self {
        Self::new()
    }
}

impl AudioClient {
    /// Creates a lazy audio client that starts its backend worker on first subscription or request.
    #[must_use]
    pub fn new() -> Self {
        Self::with_backends_internal(candidate_backends())
    }

    #[cfg(test)]
    pub(crate) fn with_backends(backends: Vec<Box<dyn AudioBackend>>) -> Self {
        Self::with_backends_internal(backends)
    }

    fn with_backends_internal(backends: Vec<Box<dyn AudioBackend>>) -> Self {
        let has_pending = Arc::new(AtomicBool::new(false));
        Self {
            inner: Arc::new(AudioClientInner {
                shared: Arc::new(Mutex::new(SharedAudioState::new(Arc::clone(&has_pending)))),
                has_pending,
                stop_flag: Arc::new(AtomicBool::new(false)),
                subscriber_attached: AtomicBool::new(false),
                backends,
                worker: Mutex::new(None),
            }),
        }
    }

    pub(crate) fn set_wakeup<F>(&self, wake: F)
    where
        F: Fn() + Send + Sync + 'static,
    {
        if let Ok(mut guard) = self.inner.shared.lock() {
            guard.wakeup = Some(Box::new(wake));
        }
    }

    fn ensure_worker_started(&self) -> bool {
        let mut guard = self.inner.worker.lock().unwrap_or_else(|e| e.into_inner());
        if let Some(running) = guard.as_mut() {
            let finished = running.join.as_ref().is_some_and(JoinHandle::is_finished);
            if !finished {
                return true;
            }
            if let Some(handle) = running.join.take() {
                let _ = handle.join();
            }
            *guard = None;
        }

        self.inner.stop_flag.store(false, Ordering::Release);
        for backend in &self.inner.backends {
            let shared = Arc::clone(&self.inner.shared);
            let stop_flag = Arc::clone(&self.inner.stop_flag);
            if let Some((sender, join)) = backend.start(shared, stop_flag) {
                *guard = Some(RunningWorker {
                    sender,
                    join: Some(join),
                });
                return true;
            }
        }
        false
    }

    /// Ensures the backend worker is running for active subscriptions.
    ///
    /// Returns `true` when a worker is running, or `false` if no backend could start (without
    /// permanently poisoning future retry attempts). When a subscriber attaches to an
    /// already-running worker that holds a snapshot, the current snapshot is queued immediately
    /// for delivery.
    pub(crate) fn ensure_started(&self) -> bool {
        if !self.ensure_worker_started() {
            return false;
        }
        let was_attached = self.inner.subscriber_attached.swap(true, Ordering::AcqRel);
        if !was_attached && let Ok(mut shared) = self.inner.shared.lock() {
            if shared.pending_snapshot.is_none()
                && let Some(snap) = shared.snapshot.clone()
            {
                shared.pending_snapshot = Some(snap);
                shared.has_pending.store(true, Ordering::Release);
                shared.wake();
            } else if shared.pending_snapshot.is_some() {
                shared.has_pending.store(true, Ordering::Release);
                shared.wake();
            }
        }
        true
    }

    /// Gracefully stops the backend worker when active audio subscriptions drop to zero.
    pub(crate) fn stop_if_idle(&self) {
        if !self.inner.subscriber_attached.swap(false, Ordering::AcqRel) {
            return;
        }
        self.inner.stop_worker();
    }

    /// Lock-free check indicating whether a snapshot or discrete event is queued for draining.
    #[must_use]
    pub(crate) fn has_pending(&self) -> bool {
        self.inner.has_pending.load(Ordering::Acquire)
    }

    /// Drains the latest pending `Arc<AudioSnapshot>` (if changed) and all queued [`AudioEvent`]s.
    pub(crate) fn drain_updates(&self) -> (Option<Arc<AudioSnapshot>>, Vec<AudioEvent>) {
        if !self.inner.has_pending.load(Ordering::Acquire) {
            return (None, Vec::new());
        }
        let Ok(mut guard) = self.inner.shared.lock() else {
            return (None, Vec::new());
        };
        let snapshot = guard.pending_snapshot.take();
        let events = guard.pending_events.drain(..).collect();
        guard.has_pending.store(false, Ordering::Release);
        (snapshot, events)
    }

    /// Returns the latest known audio snapshot, starting the backend lazily if needed.
    #[must_use]
    pub fn snapshot(&self) -> Option<Arc<AudioSnapshot>> {
        self.ensure_worker_started();
        let guard = self.inner.shared.lock().ok()?;
        guard.snapshot.clone()
    }

    /// Sets the peak linear volume (`0.0..=1.5`) on `target`, preserving relative per-channel
    /// balance ratios.
    pub fn set_volume(&self, target: AudioTarget, linear: f32) -> Result<()> {
        let clamped = if linear.is_finite() {
            linear.clamp(0.0, 1.5)
        } else {
            0.0
        };
        self.send_command(AudioCommand::SetVolume(target, clamped))
    }

    /// Sets volume expressed as a percentage (`0.0..=150.0`, where `100.0` is unity gain) on
    /// `target`, preserving relative per-channel balance ratios.
    pub fn set_volume_percent(&self, target: AudioTarget, percent: f32) -> Result<()> {
        let linear = if percent.is_finite() {
            (percent / 100.0).clamp(0.0, 1.5)
        } else {
            0.0
        };
        self.set_volume(target, linear)
    }

    /// Adjusts linear volume on `target` by `delta_linear` (clamped to `0.0..=1.5`), preserving
    /// relative per-channel balance ratios.
    pub fn adjust_volume(&self, target: AudioTarget, delta_linear: f32) -> Result<()> {
        if !delta_linear.is_finite() {
            return Ok(());
        }
        self.send_command(AudioCommand::AdjustVolume(
            target,
            delta_linear.clamp(-1.5, 1.5),
        ))
    }

    /// Sets explicit per-channel linear volumes (`0.0..=1.5`) on `target` (supports mono, stereo,
    /// and surround 4/6/8+ channel layouts).
    pub fn set_channel_volumes(&self, target: AudioTarget, channels: &[f32]) -> Result<()> {
        let Some(clean) = sanitize_linear_channels(channels) else {
            return Ok(());
        };
        self.send_command(AudioCommand::SetChannelVolumes(target, clean))
    }

    /// Adjusts stereo/surround L/R balance (`-1.0..=1.0`) on `target` while preserving peak master
    /// level and center/LFE channels.
    pub fn set_balance(&self, target: AudioTarget, balance: f32) -> Result<()> {
        let b = if balance.is_finite() {
            balance.clamp(-1.0, 1.0)
        } else {
            0.0
        };
        self.send_command(AudioCommand::SetBalance(target, b))
    }

    /// Sets mute state on `target`.
    pub fn set_mute(&self, target: AudioTarget, muted: bool) -> Result<()> {
        self.send_command(AudioCommand::SetMute(target, muted))
    }

    /// Toggles mute state on `target`.
    pub fn toggle_mute(&self, target: AudioTarget) -> Result<()> {
        self.send_command(AudioCommand::ToggleMute(target))
    }

    /// Switches the active default output sink or input source by backend `device_id`.
    pub fn set_default_device(&self, direction: AudioDirection, device_id: u32) -> Result<()> {
        self.send_command(AudioCommand::SetDefaultDevice(direction, device_id))
    }

    /// Moves a per-application playback (`AudioDirection::Output`) or capture
    /// (`AudioDirection::Input`) stream identified by `stream_id` to the sink/source identified by
    /// `device_id`.
    pub fn move_stream(
        &self,
        direction: AudioDirection,
        stream_id: u32,
        device_id: u32,
    ) -> Result<()> {
        self.send_command(AudioCommand::MoveStream(direction, stream_id, device_id))
    }

    /// Switches the active physical or logical port/route on `target` by `port_name`.
    pub fn set_port(&self, target: AudioPortTarget, port_name: &str) -> Result<()> {
        self.send_command(AudioCommand::SetPort(target, port_name.to_owned()))
    }

    /// Switches the active profile on sound card `card_id` by `profile_name`.
    pub fn set_card_profile(&self, card_id: u32, profile_name: &str) -> Result<()> {
        self.send_command(AudioCommand::SetCardProfile(
            card_id,
            profile_name.to_owned(),
        ))
    }

    fn send_command(&self, cmd: AudioCommand) -> Result<()> {
        if !self.ensure_worker_started() {
            return Err(WaywiError::AudioError(
                "audio worker could not be started".into(),
            ));
        }
        let guard = self
            .inner
            .worker
            .lock()
            .map_err(|_| WaywiError::AudioError("audio worker lock poisoned".into()))?;
        let Some(running) = guard.as_ref() else {
            return Err(WaywiError::AudioError(
                "audio worker could not be started".into(),
            ));
        };
        running.sender.send(cmd)
    }
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
    use std::sync::{Arc, Mutex, mpsc};
    use std::thread::{self, JoinHandle};

    use super::AudioClient;
    use crate::audio::backend::{
        AudioBackend, AudioCommand, AudioCommandSender, SharedAudioState, update_snapshot,
    };
    use crate::audio::types::{
        AudioDevice, AudioDirection, AudioPortTarget, AudioSnapshot, AudioTarget, AudioVolume,
    };
    use crate::error::{Result, WaywiError};

    struct MockSender(mpsc::Sender<AudioCommand>);
    impl AudioCommandSender for MockSender {
        fn send(&self, cmd: AudioCommand) -> Result<()> {
            self.0
                .send(cmd)
                .map_err(|_| WaywiError::AudioError("mock disconnected".into()))
        }
    }

    struct MockBackend {
        allow_start: Arc<AtomicBool>,
        starts: Arc<AtomicUsize>,
        stops: Arc<AtomicUsize>,
        recorded: Arc<Mutex<Vec<AudioCommand>>>,
    }

    impl AudioBackend for MockBackend {
        fn start(
            &self,
            shared: Arc<Mutex<SharedAudioState>>,
            _stop_flag: Arc<AtomicBool>,
        ) -> Option<(Box<dyn AudioCommandSender>, JoinHandle<()>)> {
            if !self.allow_start.load(Ordering::Acquire) {
                return None;
            }
            self.starts.fetch_add(1, Ordering::AcqRel);
            let (tx, rx) = mpsc::channel::<AudioCommand>();
            let recorded = Arc::clone(&self.recorded);
            let stops = Arc::clone(&self.stops);
            let join = thread::spawn(move || {
                let mut last = None;
                update_snapshot(
                    &shared,
                    &mut last,
                    AudioSnapshot {
                        sinks: vec![AudioDevice {
                            id: 1,
                            name: "mock-sink".into(),
                            description: "Mock Sink".into(),
                            is_default: true,
                            volume: AudioVolume {
                                linear: 0.5,
                                channels: vec![0.5, 0.5],
                                ..AudioVolume::default()
                            },
                            ..AudioDevice::default()
                        }],
                        default_sink_id: Some(1),
                        ..AudioSnapshot::default()
                    },
                );
                while let Ok(cmd) = rx.recv() {
                    let is_shutdown = matches!(cmd, AudioCommand::Shutdown);
                    recorded.lock().unwrap().push(cmd);
                    if is_shutdown {
                        break;
                    }
                }
                stops.fetch_add(1, Ordering::AcqRel);
            });
            Some((Box::new(MockSender(tx)), join))
        }
    }

    #[test]
    fn failed_start_does_not_poison_client_and_lifecycle_restarts_cleanly() {
        let allow = Arc::new(AtomicBool::new(false));
        let starts = Arc::new(AtomicUsize::new(0));
        let stops = Arc::new(AtomicUsize::new(0));
        let recorded = Arc::new(Mutex::new(Vec::new()));
        let wakes = Arc::new(AtomicUsize::new(0));

        let client = AudioClient::with_backends(vec![Box::new(MockBackend {
            allow_start: Arc::clone(&allow),
            starts: Arc::clone(&starts),
            stops: Arc::clone(&stops),
            recorded: Arc::clone(&recorded),
        })]);
        let wakes_cl = Arc::clone(&wakes);
        client.set_wakeup(move || {
            wakes_cl.fetch_add(1, Ordering::AcqRel);
        });

        // Initially backend cannot start: returns false without poisoning.
        assert!(!client.ensure_started());
        assert!(!client.has_pending());
        assert_eq!(starts.load(Ordering::Acquire), 0);

        // Backend becomes available; imperative snapshot() starts the worker.
        allow.store(true, Ordering::Release);
        for _ in 0..50 {
            if client.snapshot().is_some() {
                break;
            }
            thread::sleep(std::time::Duration::from_millis(5));
        }
        assert!(client.snapshot().is_some());
        assert_eq!(starts.load(Ordering::Acquire), 1);

        // Drain initial updates, then attach subscriber to the already-running worker:
        // current snapshot must be re-emitted immediately!
        let (snap1, _) = client.drain_updates();
        assert!(snap1.is_some());
        assert!(!client.has_pending());

        let wakes_before = wakes.load(Ordering::Acquire);
        assert!(client.ensure_started());
        assert!(client.has_pending());
        assert!(wakes.load(Ordering::Acquire) > wakes_before);
        let (snap2, _) = client.drain_updates();
        assert_eq!(snap2.unwrap().default_sink().unwrap().id, 1);
        assert!(!client.has_pending());

        // Repeated ensure_started while already attached does not re-queue indefinitely.
        assert!(client.ensure_started());
        assert!(!client.has_pending());

        // Orthogonal commands deliver cleanly.
        client.set_volume(AudioTarget::DefaultSink, 0.75).unwrap();
        client
            .set_volume_percent(AudioTarget::Sink(1), 120.0)
            .unwrap();
        client
            .adjust_volume(AudioTarget::DefaultSource, 0.05)
            .unwrap();
        client
            .set_channel_volumes(AudioTarget::PlaybackStream(9), &[0.8, 0.4, 0.7, 0.5])
            .unwrap();
        client.set_balance(AudioTarget::DefaultSink, -0.25).unwrap();
        client
            .set_mute(AudioTarget::CaptureStream(4), true)
            .unwrap();
        client.toggle_mute(AudioTarget::Source(2)).unwrap();
        client
            .set_default_device(AudioDirection::Output, 1)
            .unwrap();
        client.move_stream(AudioDirection::Output, 9, 1).unwrap();
        client
            .set_port(AudioPortTarget::DefaultSink, "headphones")
            .unwrap();
        client.set_card_profile(45, "pro-audio").unwrap();

        // stop_if_idle shuts down the worker gracefully.
        client.stop_if_idle();
        assert_eq!(stops.load(Ordering::Acquire), 1);
        assert!(!client.has_pending());

        let cmds = recorded.lock().unwrap();
        assert!(cmds.len() >= 12);
        assert!(
            cmds.iter()
                .any(|c| matches!(c, AudioCommand::MoveStream(AudioDirection::Output, 9, 1)))
        );
        assert!(matches!(cmds.last(), Some(AudioCommand::Shutdown)));
        drop(cmds);

        // Re-subscribing after stop_if_idle restarts the worker cleanly.
        assert!(client.ensure_started());
        assert_eq!(starts.load(Ordering::Acquire), 2);
        client.stop_if_idle();
        assert_eq!(stops.load(Ordering::Acquire), 2);
    }
}
