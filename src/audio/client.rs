//! Backend-agnostic public client handle for audio telemetry and volume/device control.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;

use crate::error::{Result, WaywiError};

use super::backend::{
    AudioBackend, AudioCommand, AudioCommandSender, SharedAudioState, default_backend,
};
use super::types::{AudioEvent, AudioSnapshot};

struct RunningWorker {
    sender: Option<Box<dyn AudioCommandSender>>,
    join: Option<JoinHandle<()>>,
}

struct AudioClientInner {
    shared: Arc<Mutex<SharedAudioState>>,
    stop_flag: Arc<AtomicBool>,
    backend: Box<dyn AudioBackend>,
    worker: Mutex<Option<RunningWorker>>,
}

impl Drop for AudioClientInner {
    fn drop(&mut self) {
        self.stop_flag.store(true, Ordering::Release);
        let mut guard = self.worker.lock().unwrap_or_else(|e| e.into_inner());
        if let Some(mut running) = guard.take() {
            if let Some(sender) = running.sender.take() {
                let _ = sender.send(AudioCommand::Shutdown);
            }
            drop(guard);
            if let Some(handle) = running.join.take() {
                let _ = handle.join();
            }
        }
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
        Self {
            inner: Arc::new(AudioClientInner {
                shared: Arc::new(Mutex::new(SharedAudioState::default())),
                stop_flag: Arc::new(AtomicBool::new(false)),
                backend: default_backend(),
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

    pub(crate) fn ensure_started(&self) {
        let mut guard = self.inner.worker.lock().unwrap_or_else(|e| e.into_inner());
        if guard.is_some() {
            return;
        }
        let shared = Arc::clone(&self.inner.shared);
        let stop_flag = Arc::clone(&self.inner.stop_flag);
        *guard = match self.inner.backend.start(shared, stop_flag) {
            Some((sender, join)) => Some(RunningWorker {
                sender: Some(sender),
                join: Some(join),
            }),
            None => Some(RunningWorker {
                sender: None,
                join: None,
            }),
        };
    }

    pub(crate) fn drain_events(&self) -> Vec<AudioEvent> {
        let Ok(mut guard) = self.inner.shared.lock() else {
            return Vec::new();
        };
        guard.pending_events.drain(..).collect()
    }

    /// Returns the latest known audio snapshot, if initialized.
    pub fn snapshot(&self) -> Result<Option<AudioSnapshot>> {
        self.ensure_started();
        let guard = self
            .inner
            .shared
            .lock()
            .map_err(|_| WaywiError::AudioError("audio state lock poisoned".into()))?;
        Ok(guard.snapshot.clone())
    }

    /// Sets the linear volume multiplier on the active sink (clamped to `[0.0, 1.5]`).
    pub fn set_volume_linear(&self, level: f32) -> Result<()> {
        self.send_command(AudioCommand::SetVolumeLinear(level.clamp(0.0, 1.5)))
    }

    /// Sets mute state on the active sink.
    pub fn set_muted(&self, muted: bool) -> Result<()> {
        self.send_command(AudioCommand::SetMuted(muted))
    }

    /// Toggles mute state on the active sink.
    pub fn toggle_mute(&self) -> Result<()> {
        self.send_command(AudioCommand::ToggleMute)
    }

    /// Switches the default output sink by backend node/device ID.
    pub fn select_device(&self, id: u32) -> Result<()> {
        self.send_command(AudioCommand::SelectDevice(id))
    }

    /// Sets the linear volume multiplier on the active input source / mic (clamped to `[0.0, 1.5]`).
    pub fn set_source_volume_linear(&self, level: f32) -> Result<()> {
        self.send_command(AudioCommand::SetSourceVolumeLinear(level.clamp(0.0, 1.5)))
    }

    /// Sets mute state on the active input source / mic.
    pub fn set_source_muted(&self, muted: bool) -> Result<()> {
        self.send_command(AudioCommand::SetSourceMuted(muted))
    }

    /// Toggles mute state on the active input source / mic.
    pub fn toggle_source_mute(&self) -> Result<()> {
        self.send_command(AudioCommand::ToggleSourceMute)
    }

    /// Switches the default input source / mic by backend node/device ID.
    pub fn select_source(&self, id: u32) -> Result<()> {
        self.send_command(AudioCommand::SelectSource(id))
    }

    fn send_command(&self, cmd: AudioCommand) -> Result<()> {
        self.ensure_started();
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
        let Some(sender) = &running.sender else {
            return Err(WaywiError::AudioError(
                "audio worker could not be started".into(),
            ));
        };
        sender.send(cmd)
    }
}
