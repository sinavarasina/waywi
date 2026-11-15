//! PipeWire audio backend implementation.

pub(crate) mod pod;
pub(crate) mod worker;

use std::sync::atomic::AtomicBool;
use std::sync::{Arc, Mutex, Once};
use std::thread::{self, JoinHandle};

use pipewire as pw;
use pw::channel as pw_channel;

use crate::error::{Result, WaywiError};

use super::backend::{AudioBackend, AudioCommand, AudioCommandSender, SharedAudioState};

static PW_INIT: Once = Once::new();

pub(crate) fn ensure_pw_init() {
    PW_INIT.call_once(|| {
        pw::init();
    });
}

struct PwCommandSender {
    sender: Arc<Mutex<Option<pw_channel::Sender<AudioCommand>>>>,
}

impl AudioCommandSender for PwCommandSender {
    fn send(&self, cmd: AudioCommand) -> Result<()> {
        let guard = self
            .sender
            .lock()
            .map_err(|_| WaywiError::AudioError("pipewire sender lock poisoned".into()))?;
        let Some(tx) = guard.as_ref() else {
            return Err(WaywiError::AudioError(
                "pipewire worker channel is unavailable".into(),
            ));
        };
        tx.send(cmd)
            .map_err(|_| WaywiError::AudioError("pipewire worker channel disconnected".into()))
    }
}

/// PipeWire [`AudioBackend`] driving a `pipewire::main_loop` worker thread.
pub(crate) struct PipewireBackend;

impl AudioBackend for PipewireBackend {
    fn start(
        &self,
        shared: Arc<Mutex<SharedAudioState>>,
        stop_flag: Arc<AtomicBool>,
    ) -> Option<(Box<dyn AudioCommandSender>, JoinHandle<()>)> {
        let (tx, rx) = pw_channel::channel::<AudioCommand>();
        let sender_slot = Arc::new(Mutex::new(Some(tx)));
        let worker_sender_slot = Arc::clone(&sender_slot);

        let join = thread::Builder::new()
            .name("waywi-pipewire".into())
            .spawn(move || {
                ensure_pw_init();
                let mut rx_slot = Some(rx);
                worker::run_worker_loop(shared, stop_flag, &mut rx_slot, &worker_sender_slot);
            })
            .ok()?;

        Some((
            Box::new(PwCommandSender {
                sender: sender_slot,
            }),
            join,
        ))
    }
}
