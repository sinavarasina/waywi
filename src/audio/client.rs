//! Backend-agnostic public client handle for audio telemetry and volume/device control.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;

use crate::error::{Result, WaywiError};

use super::backend::{
    AudioBackend, AudioCommand, AudioCommandSender, SharedAudioState, candidate_backends,
};
use super::types::{AudioEvent, AudioSnapshot, sanitize_linear_channels};

struct RunningWorker {
    sender: Option<Box<dyn AudioCommandSender>>,
    join: Option<JoinHandle<()>>,
}

struct AudioClientInner {
    shared: Arc<Mutex<SharedAudioState>>,
    stop_flag: Arc<AtomicBool>,
    backends: Vec<Box<dyn AudioBackend>>,
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
                backends: candidate_backends(),
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
        for backend in &self.inner.backends {
            let shared = Arc::clone(&self.inner.shared);
            let stop_flag = Arc::clone(&self.inner.stop_flag);
            if let Some((sender, join)) = backend.start(shared, stop_flag) {
                *guard = Some(RunningWorker {
                    sender: Some(sender),
                    join: Some(join),
                });
                return;
            }
        }
        *guard = Some(RunningWorker {
            sender: None,
            join: None,
        });
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

    /// Sets the linear volume multiplier on the active sink (clamped to `[0.0, 1.5]`),
    /// preserving any existing per-channel stereo balance ratio.
    pub fn set_volume_linear(&self, level: f32) -> Result<()> {
        self.send_command(AudioCommand::SetVolumeLinear(level.clamp(0.0, 1.5)))
    }

    /// Adjusts stereo L/R balance on the active sink (`-1.0` = full left, `0.0` = center,
    /// `+1.0` = full right) while preserving its peak master level.
    pub fn set_balance(&self, balance: f32) -> Result<()> {
        let b = if balance.is_finite() {
            balance.clamp(-1.0, 1.0)
        } else {
            0.0
        };
        self.send_command(AudioCommand::SetBalance(b))
    }

    /// Sets explicit per-channel linear volumes on the active sink (each channel clamped to
    /// `[0.0, 1.5]`).
    pub fn set_channel_volumes(&self, volumes: &[f32]) -> Result<()> {
        let Some(clean) = sanitize_linear_channels(volumes) else {
            return Ok(());
        };
        self.send_command(AudioCommand::SetChannelVolumes(clean))
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

    /// Switches the active output port/route on the default sink by port name
    /// (e.g. `"analog-output-headphones"` or `"analog-output-lineout"`).
    pub fn select_port(&self, port_name: impl Into<String>) -> Result<()> {
        self.send_command(AudioCommand::SelectPort(None, port_name.into()))
    }

    /// Switches the active output port/route on a specific sink `sink_id` by port name.
    pub fn select_device_port(&self, sink_id: u32, port_name: impl Into<String>) -> Result<()> {
        self.send_command(AudioCommand::SelectPort(Some(sink_id), port_name.into()))
    }

    /// Sets the linear volume multiplier on the active input source / mic (clamped to `[0.0, 1.5]`),
    /// preserving any existing per-channel stereo balance ratio.
    pub fn set_source_volume_linear(&self, level: f32) -> Result<()> {
        self.send_command(AudioCommand::SetSourceVolumeLinear(level.clamp(0.0, 1.5)))
    }

    /// Adjusts stereo L/R balance on the active input source / mic (`-1.0..=1.0`).
    pub fn set_source_balance(&self, balance: f32) -> Result<()> {
        let b = if balance.is_finite() {
            balance.clamp(-1.0, 1.0)
        } else {
            0.0
        };
        self.send_command(AudioCommand::SetSourceBalance(b))
    }

    /// Sets explicit per-channel linear volumes on the active input source / mic (each channel
    /// clamped to `[0.0, 1.5]`).
    pub fn set_source_channel_volumes(&self, volumes: &[f32]) -> Result<()> {
        let Some(clean) = sanitize_linear_channels(volumes) else {
            return Ok(());
        };
        self.send_command(AudioCommand::SetSourceChannelVolumes(clean))
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

    /// Switches the active input port/route on the default source / mic by port name
    /// (e.g. `"analog-input-front-mic"` or `"analog-input-rear-mic"`).
    pub fn select_source_port(&self, port_name: impl Into<String>) -> Result<()> {
        self.send_command(AudioCommand::SelectSourcePort(None, port_name.into()))
    }

    /// Switches the active input port/route on a specific source `source_id` by port name.
    pub fn select_source_device_port(
        &self,
        source_id: u32,
        port_name: impl Into<String>,
    ) -> Result<()> {
        self.send_command(AudioCommand::SelectSourcePort(
            Some(source_id),
            port_name.into(),
        ))
    }

    /// Switches the active card profile on `card_id` by profile name
    /// (e.g. `"output:analog-stereo+input:analog-stereo"`, `"pro-audio"`, `"off"`).
    pub fn select_card_profile(&self, card_id: u32, profile_name: impl Into<String>) -> Result<()> {
        self.send_command(AudioCommand::SelectCardProfile(
            card_id,
            profile_name.into(),
        ))
    }

    /// Sets the linear volume multiplier on a playback stream (clamped to `[0.0, 1.5]`),
    /// preserving any existing per-channel stereo balance ratio.
    ///
    /// Unknown stream IDs are silently ignored: the stream may have ended between the snapshot
    /// and this command.
    pub fn set_stream_volume_linear(&self, id: u32, level: f32) -> Result<()> {
        self.send_command(AudioCommand::SetStreamVolumeLinear(
            id,
            level.clamp(0.0, 1.5),
        ))
    }

    /// Adjusts stereo L/R balance on a playback stream (`-1.0..=1.0`); unknown stream IDs are ignored.
    pub fn set_stream_balance(&self, id: u32, balance: f32) -> Result<()> {
        let b = if balance.is_finite() {
            balance.clamp(-1.0, 1.0)
        } else {
            0.0
        };
        self.send_command(AudioCommand::SetStreamBalance(id, b))
    }

    /// Sets explicit per-channel linear volumes on a playback stream; unknown stream IDs are ignored.
    pub fn set_stream_channel_volumes(&self, id: u32, volumes: &[f32]) -> Result<()> {
        let Some(clean) = sanitize_linear_channels(volumes) else {
            return Ok(());
        };
        self.send_command(AudioCommand::SetStreamChannelVolumes(id, clean))
    }

    /// Sets mute state on a playback stream; unknown stream IDs are ignored.
    pub fn set_stream_muted(&self, id: u32, muted: bool) -> Result<()> {
        self.send_command(AudioCommand::SetStreamMuted(id, muted))
    }

    /// Toggles mute state on a playback stream; unknown stream IDs are ignored.
    pub fn toggle_stream_mute(&self, id: u32) -> Result<()> {
        self.send_command(AudioCommand::ToggleStreamMute(id))
    }

    /// Sets the linear volume multiplier on a capture stream (clamped to `[0.0, 1.5]`),
    /// preserving any existing per-channel stereo balance ratio.
    ///
    /// Unknown stream IDs are silently ignored: the stream may have ended between the snapshot
    /// and this command.
    pub fn set_capture_volume_linear(&self, id: u32, level: f32) -> Result<()> {
        self.send_command(AudioCommand::SetCaptureVolumeLinear(
            id,
            level.clamp(0.0, 1.5),
        ))
    }

    /// Adjusts stereo L/R balance on a capture stream (`-1.0..=1.0`); unknown stream IDs are ignored.
    pub fn set_capture_balance(&self, id: u32, balance: f32) -> Result<()> {
        let b = if balance.is_finite() {
            balance.clamp(-1.0, 1.0)
        } else {
            0.0
        };
        self.send_command(AudioCommand::SetCaptureBalance(id, b))
    }

    /// Sets explicit per-channel linear volumes on a capture stream; unknown stream IDs are ignored.
    pub fn set_capture_channel_volumes(&self, id: u32, volumes: &[f32]) -> Result<()> {
        let Some(clean) = sanitize_linear_channels(volumes) else {
            return Ok(());
        };
        self.send_command(AudioCommand::SetCaptureChannelVolumes(id, clean))
    }

    /// Sets mute state on a capture stream; unknown stream IDs are ignored.
    pub fn set_capture_muted(&self, id: u32, muted: bool) -> Result<()> {
        self.send_command(AudioCommand::SetCaptureMuted(id, muted))
    }

    /// Toggles mute state on a capture stream; unknown stream IDs are ignored.
    pub fn toggle_capture_mute(&self, id: u32) -> Result<()> {
        self.send_command(AudioCommand::ToggleCaptureMute(id))
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
