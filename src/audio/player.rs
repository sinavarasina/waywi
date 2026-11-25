//! Lazy background audio playback engine supporting both `rodio` (`audio-playback`) and
//! dependency-light native PipeWire / PulseAudio / ALSA PCM output.

use std::fs;
use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
#[cfg(any(
    feature = "audio-playback",
    feature = "pipewire",
    feature = "pulseaudio",
    feature = "alsa"
))]
use std::thread::{self, JoinHandle};

use crate::asset::decode::hash_bytes;
use crate::error::{Result, WaywiError};

use super::sound::{
    DEFAULT_SOUND_CACHE_BYTES, MAX_SOUND_FILE_BYTES, SoundAsset, SoundAssetStore, SoundCacheKey,
    SoundData, SoundPlay, SoundPreset, SoundSource, resolve_sound_theme_path,
};

#[cfg(any(
    feature = "audio-playback",
    feature = "pipewire",
    feature = "pulseaudio",
    feature = "alsa"
))]
use super::sound::MIXER_SAMPLE_RATE;

#[cfg(any(feature = "pipewire", feature = "pulseaudio", feature = "alsa"))]
use super::sound::PolyphonicMixer;

#[cfg(feature = "audio-playback")]
enum PlayerCommand {
    Play { stereo_48k: Arc<[f32]>, volume: f32 },
    StopAll,
    Shutdown,
}

trait PlaybackDriver: Send {
    fn is_alive(&self) -> bool;
    fn send_play(&mut self, stereo_48k: Arc<[f32]>, volume: f32);
    fn send_stop(&mut self);
    fn shutdown(&mut self);
}

/// RAII guard marking a background audio playback thread as dead on any exit path.
#[cfg(any(
    feature = "audio-playback",
    feature = "pipewire",
    feature = "pulseaudio",
    feature = "alsa"
))]
struct AliveGuard(Arc<AtomicBool>);

#[cfg(any(
    feature = "audio-playback",
    feature = "pipewire",
    feature = "pulseaudio",
    feature = "alsa"
))]
impl Drop for AliveGuard {
    fn drop(&mut self) {
        self.0.store(false, Ordering::Release);
    }
}

/// Audio asset cache and low-latency polyphonic playback engine.
#[derive(Clone)]
pub struct AudioPlayer {
    inner: Arc<AudioPlayerInner>,
}

struct AudioPlayerInner {
    store: Mutex<SoundAssetStore>,
    driver: Mutex<Option<Box<dyn PlaybackDriver>>>,
}

impl Drop for AudioPlayerInner {
    fn drop(&mut self) {
        if let Ok(mut guard) = self.driver.lock()
            && let Some(mut drv) = guard.take()
        {
            drv.shutdown();
        }
    }
}

impl Default for AudioPlayer {
    fn default() -> Self {
        Self::new()
    }
}

impl std::fmt::Debug for AudioPlayer {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("AudioPlayer").finish_non_exhaustive()
    }
}

impl AudioPlayer {
    /// Creates an idle audio player with the default 8 MiB decoded PCM cache budget that opens a
    /// playback stream lazily on first play.
    #[must_use]
    pub fn new() -> Self {
        Self::with_cache_limit(DEFAULT_SOUND_CACHE_BYTES)
    }

    /// Creates an idle audio player with an explicit decoded PCM cache byte limit.
    #[must_use]
    pub(crate) fn with_cache_limit(max_bytes: usize) -> Self {
        Self {
            inner: Arc::new(AudioPlayerInner {
                store: Mutex::new(SoundAssetStore::with_cache_limit(max_bytes)),
                driver: Mutex::new(None),
            }),
        }
    }

    /// Canonicalizes, reads, hashes file bytes for content deduplication, decodes, and caches a
    /// sound file from disk (bounded to 16 MiB).
    pub fn load_file(&self, path: impl AsRef<Path>) -> Result<SoundAsset> {
        let canonical = fs::canonicalize(path.as_ref())?;
        let meta = fs::metadata(&canonical)?;
        if meta.len() > MAX_SOUND_FILE_BYTES {
            return Err(WaywiError::AudioError(format!(
                "sound {} exceeds max size ({} > {MAX_SOUND_FILE_BYTES} bytes)",
                canonical.display(),
                meta.len()
            )));
        }
        let bytes = fs::read(&canonical)?;
        let key = SoundCacheKey::ContentHash(hash_bytes(&bytes));
        {
            let mut store = self.inner.store.lock().unwrap_or_else(|e| e.into_inner());
            if let Some(existing) = store.get_by_key(key) {
                return Ok(existing);
            }
        }
        let hint = canonical
            .extension()
            .and_then(|e| e.to_str())
            .map(str::to_owned);
        let data = SoundData::from_vec_with_hint(bytes, hint.as_deref())?;
        let source = SoundSource::File {
            path: canonical,
            ext_hint: hint,
        };
        let mut store = self.inner.store.lock().unwrap_or_else(|e| e.into_inner());
        Ok(store.insert_sound_data(key, source, &data))
    }

    /// Decodes and caches an in-memory audio buffer (`WAV` always; `OGG`/`MP3`/`FLAC` with
    /// `audio-playback`), deduplicating by content hash.
    pub fn load_bytes(&self, bytes: &[u8]) -> Result<SoundAsset> {
        let key = SoundCacheKey::ContentHash(hash_bytes(bytes));
        {
            let mut store = self.inner.store.lock().unwrap_or_else(|e| e.into_inner());
            if let Some(existing) = store.get_by_key(key) {
                return Ok(existing);
            }
        }
        let data = SoundData::from_bytes(bytes)?;
        let source = SoundSource::EncodedBytes(Arc::from(bytes));
        let mut store = self.inner.store.lock().unwrap_or_else(|e| e.into_inner());
        Ok(store.insert_sound_data(key, source, &data))
    }

    /// Resolves and caches a freedesktop XDG Sound Theme event sound (e.g. `"message-new-instant"`).
    pub fn load_system_sound(&self, name: &str) -> Result<SoundAsset> {
        let path = resolve_sound_theme_path(name, None).ok_or_else(|| {
            WaywiError::AudioError(format!(
                "system sound `{name}` not found in XDG sound themes"
            ))
        })?;
        self.load_file(path)
    }

    /// Caches and returns a procedural sound preset without external files.
    #[must_use]
    pub fn load_preset(&self, preset: SoundPreset) -> SoundAsset {
        let key = SoundCacheKey::Preset(preset);
        {
            let mut store = self.inner.store.lock().unwrap_or_else(|e| e.into_inner());
            if let Some(existing) = store.get_by_key(key) {
                return existing;
            }
        }
        let data = SoundData::from_preset(preset);
        let mut store = self.inner.store.lock().unwrap_or_else(|e| e.into_inner());
        store.insert_sound_data(key, SoundSource::Preset(preset), &data)
    }

    /// Removes a cached [`SoundAsset`] from memory, returning `true` if it was present.
    pub fn unload(&self, sound: SoundAsset) -> bool {
        let mut store = self.inner.store.lock().unwrap_or_else(|e| e.into_inner());
        store.remove(sound.id)
    }

    /// Frees all cached decoded PCM buffers and invalidates all [`SoundAsset`] handles.
    pub fn clear_cache(&self) {
        let mut store = self.inner.store.lock().unwrap_or_else(|e| e.into_inner());
        store.clear();
    }

    /// Releases resident decoded PCM buffers while preserving all [`SoundAsset`] handles and
    /// their backing sources for transparent on-demand re-decoding (called by `trim_memory()`).
    pub(crate) fn trim_memory(&self) {
        let mut store = self.inner.store.lock().unwrap_or_else(|e| e.into_inner());
        store.trim_reclaimable();
    }

    /// Resolves a live [`SoundId`]'s decoded stereo PCM buffer, transparently re-decoding from
    /// its [`SoundSource`] without holding the store mutex if it was trimmed or LRU-evicted.
    fn resolve_stereo(&self, id: super::sound::SoundId) -> Option<Arc<[f32]>> {
        let source = {
            let mut store = self.inner.store.lock().unwrap_or_else(|e| e.into_inner());
            if let Some(buf) = store.get_stereo(id) {
                return Some(buf);
            }
            store.source_for(id)?
        };
        let decoded = match source {
            SoundSource::File { path, ext_hint } => {
                let meta = fs::metadata(&path).ok()?;
                if meta.len() > MAX_SOUND_FILE_BYTES {
                    return None;
                }
                let bytes = fs::read(&path).ok()?;
                SoundData::from_vec_with_hint(bytes, ext_hint.as_deref()).ok()?
            }
            SoundSource::EncodedBytes(bytes) => SoundData::from_bytes(&bytes).ok()?,
            SoundSource::Preset(preset) => SoundData::from_preset(preset),
        };
        let stereo_48k = decoded.to_stereo_48k_arc();
        let mut store = self.inner.store.lock().unwrap_or_else(|e| e.into_inner());
        store.restore_stereo(id, stereo_48k)
    }

    /// Plays a cached [`SoundAsset`] at full volume (`1.0`).
    pub fn play(&self, sound: SoundAsset) {
        self.play_at(sound, 1.0);
    }

    /// Plays a cached [`SoundAsset`] scaled by `volume` (`0.0..=2.0`).
    pub fn play_at(&self, sound: SoundAsset, volume: f32) {
        if let Some(buf) = self.resolve_stereo(sound.id) {
            self.enqueue_play(buf, volume);
        }
    }

    /// Plays a [`SoundPlay`] trigger (either a cached [`SoundAsset`] or a [`SoundPreset`]).
    pub fn play_trigger(&self, play: SoundPlay) {
        match play {
            SoundPlay::Asset { asset, volume } => self.play_at(asset, volume),
            SoundPlay::Preset { preset, volume } => self.play_preset(preset, volume),
        }
    }

    /// Plays an ad-hoc decoded or synthesized [`SoundData`] buffer immediately.
    pub fn play_data(&self, data: &SoundData, volume: f32) {
        let buf = data.to_stereo_48k_arc();
        self.enqueue_play(buf, volume);
    }

    /// Synthesizes (and caches) a [`SoundPreset`] and plays it immediately.
    pub fn play_preset(&self, preset: SoundPreset, volume: f32) {
        let asset = self.load_preset(preset);
        self.play_at(asset, volume);
    }

    /// Resolves and plays a freedesktop XDG Sound Theme sound if found on the system.
    pub fn play_system_sound(&self, name: &str, volume: f32) -> Result<()> {
        let asset = self.load_system_sound(name)?;
        self.play_at(asset, volume);
        Ok(())
    }

    /// Stops all currently playing voices immediately.
    pub fn stop_all(&self) {
        if let Ok(mut guard) = self.inner.driver.lock()
            && let Some(ref mut drv) = *guard
        {
            drv.send_stop();
        }
    }

    /// Queries the decoded buffer sample count for `id` (used for diagnostics/testing).
    #[cfg(test)]
    pub(crate) fn cached_sample_count(&self, id: super::sound::SoundId) -> Option<usize> {
        let mut store = self.inner.store.lock().unwrap_or_else(|e| e.into_inner());
        store.get_stereo(id).map(|b| b.len())
    }

    fn enqueue_play(&self, stereo_48k: Arc<[f32]>, volume: f32) {
        if stereo_48k.is_empty() || !volume.is_finite() || volume <= 0.0 {
            return;
        }
        let mut guard = self.inner.driver.lock().unwrap_or_else(|e| e.into_inner());
        if guard.as_ref().is_some_and(|drv| !drv.is_alive())
            && let Some(mut dead) = guard.take()
        {
            dead.shutdown();
        }
        if guard.is_none() {
            *guard = start_playback_driver();
        }
        if let Some(ref mut drv) = *guard {
            drv.send_play(stereo_48k, volume.clamp(0.0, 2.0));
        }
    }
}

fn start_playback_driver() -> Option<Box<dyn PlaybackDriver>> {
    #[cfg(any(feature = "pipewire", feature = "pulseaudio", feature = "alsa"))]
    if let Some(drv) = NativeDriver::spawn() {
        return Some(Box::new(drv));
    }
    #[cfg(feature = "audio-playback")]
    if let Some(drv) = RodioDriver::spawn() {
        return Some(Box::new(drv));
    }
    None
}

/// Zero-clone `rodio::Source` streaming directly out of a shared `Arc<[f32]>` PCM buffer.
#[cfg(feature = "audio-playback")]
struct ArcSliceSource {
    data: Arc<[f32]>,
    pos: usize,
    channels: rodio::ChannelCount,
    sample_rate: rodio::SampleRate,
    duration: std::time::Duration,
}

#[cfg(feature = "audio-playback")]
impl ArcSliceSource {
    fn new(
        channels: rodio::ChannelCount,
        sample_rate: rodio::SampleRate,
        data: Arc<[f32]>,
    ) -> Self {
        let frames = data.len() / usize::from(channels.get());
        let duration =
            std::time::Duration::from_secs_f64(frames as f64 / f64::from(sample_rate.get()));
        Self {
            data,
            pos: 0,
            channels,
            sample_rate,
            duration,
        }
    }
}

#[cfg(feature = "audio-playback")]
impl Iterator for ArcSliceSource {
    type Item = rodio::Sample;

    #[inline]
    fn next(&mut self) -> Option<Self::Item> {
        let sample = *self.data.get(self.pos)?;
        self.pos += 1;
        Some(sample)
    }

    #[inline]
    fn size_hint(&self) -> (usize, Option<usize>) {
        let remaining = self.data.len().saturating_sub(self.pos);
        (remaining, Some(remaining))
    }
}

#[cfg(feature = "audio-playback")]
impl ExactSizeIterator for ArcSliceSource {}

#[cfg(feature = "audio-playback")]
impl rodio::Source for ArcSliceSource {
    #[inline]
    fn current_span_len(&self) -> Option<usize> {
        if self.pos >= self.data.len() {
            Some(0)
        } else {
            Some(self.data.len())
        }
    }

    #[inline]
    fn channels(&self) -> rodio::ChannelCount {
        self.channels
    }

    #[inline]
    fn sample_rate(&self) -> rodio::SampleRate {
        self.sample_rate
    }

    #[inline]
    fn total_duration(&self) -> Option<std::time::Duration> {
        Some(self.duration)
    }
}

#[cfg(feature = "audio-playback")]
struct RodioDriver {
    tx: std::sync::mpsc::Sender<PlayerCommand>,
    alive: Arc<AtomicBool>,
    join: Option<JoinHandle<()>>,
}

#[cfg(feature = "audio-playback")]
impl RodioDriver {
    fn spawn() -> Option<Self> {
        let (tx, rx) = std::sync::mpsc::channel::<PlayerCommand>();
        let (init_tx, init_rx) = std::sync::mpsc::sync_channel::<bool>(1);
        let alive = Arc::new(AtomicBool::new(true));
        let alive_worker = Arc::clone(&alive);
        let join = thread::Builder::new()
            .name("waywi-audio-rodio".to_owned())
            .spawn(move || {
                let _alive_guard = AliveGuard(alive_worker);
                let Ok(sink) = rodio::DeviceSinkBuilder::open_default_sink() else {
                    let _ = init_tx.send(false);
                    return;
                };
                let _ = init_tx.send(true);
                let channels = std::num::NonZero::new(2).unwrap();
                let rate = std::num::NonZero::new(MIXER_SAMPLE_RATE).unwrap();
                let mut active_players: Vec<rodio::Player> = Vec::new();
                while let Ok(cmd) = rx.recv() {
                    match cmd {
                        PlayerCommand::Play { stereo_48k, volume } => {
                            active_players.retain(|p| !p.empty());
                            let src = ArcSliceSource::new(channels, rate, stereo_48k);
                            let player = rodio::Player::connect_new(sink.mixer());
                            player.set_volume(volume);
                            player.append(src);
                            active_players.push(player);
                        }
                        PlayerCommand::StopAll => {
                            for p in active_players.drain(..) {
                                p.stop();
                            }
                        }
                        PlayerCommand::Shutdown => break,
                    }
                }
            })
            .ok()?;
        if !init_rx.recv().unwrap_or(false) {
            let _ = join.join();
            return None;
        }
        Some(Self {
            tx,
            alive,
            join: Some(join),
        })
    }
}

#[cfg(feature = "audio-playback")]
impl PlaybackDriver for RodioDriver {
    fn is_alive(&self) -> bool {
        self.alive.load(Ordering::Acquire)
    }

    fn send_play(&mut self, stereo_48k: Arc<[f32]>, volume: f32) {
        if self
            .tx
            .send(PlayerCommand::Play { stereo_48k, volume })
            .is_err()
        {
            self.alive.store(false, Ordering::Release);
        }
    }

    fn send_stop(&mut self) {
        if self.tx.send(PlayerCommand::StopAll).is_err() {
            self.alive.store(false, Ordering::Release);
        }
    }

    fn shutdown(&mut self) {
        let _ = self.tx.send(PlayerCommand::Shutdown);
        if let Some(j) = self.join.take() {
            let _ = j.join();
        }
    }
}

#[cfg(any(feature = "pipewire", feature = "pulseaudio", feature = "alsa"))]
use std::sync::Condvar;
#[cfg(feature = "pipewire")]
use std::sync::atomic::AtomicUsize;

/// Silent frames (~250 ms at 48 kHz) drained after the last voice finishes before corking the
/// PipeWire stream so short tactile clicks never clip at the tail while idle sinks can suspend.
#[cfg(feature = "pipewire")]
const IDLE_CORK_FRAMES: usize = (MIXER_SAMPLE_RATE / 4) as usize;

/// Maximum time to wait for PulseAudio server latency drain before corking an idle stream.
#[cfg(feature = "pulseaudio")]
const PULSE_DRAIN_TIMEOUT: std::time::Duration = std::time::Duration::from_millis(350);

/// Idle timeout after which an open ALSA hardware PCM device is drained and closed until needed.
#[cfg(feature = "alsa")]
const ALSA_IDLE_CLOSE_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(2);

#[cfg(feature = "pipewire")]
enum PwPlaySignal {
    EnsureActive,
    CheckIdleCork,
    StopAll,
    Shutdown,
}

/// Mixer state shared between the player driver and the native output threads.
#[cfg(any(feature = "pipewire", feature = "pulseaudio", feature = "alsa"))]
type SharedMixer = Arc<(Mutex<PolyphonicMixer>, Condvar)>;

#[cfg(any(feature = "pipewire", feature = "pulseaudio", feature = "alsa"))]
struct NativeDriver {
    mixer: SharedMixer,
    stop: Arc<AtomicBool>,
    alive: Arc<AtomicBool>,
    #[cfg(feature = "pipewire")]
    pw_wake: Option<pipewire::channel::Sender<PwPlaySignal>>,
    #[cfg(feature = "pipewire")]
    idle_frames: Arc<AtomicUsize>,
    join: Option<JoinHandle<()>>,
}

#[cfg(any(feature = "pipewire", feature = "pulseaudio", feature = "alsa"))]
impl NativeDriver {
    fn spawn() -> Option<Self> {
        #[cfg(feature = "pipewire")]
        let pw_avail = super::backend::pipewire_runtime_available();
        #[cfg(feature = "pulseaudio")]
        let pulse_avail = super::backend::pulseaudio_runtime_available();
        #[cfg(feature = "alsa")]
        let alsa_avail = super::backend::alsa_runtime_available();

        #[cfg(feature = "pipewire")]
        if pw_avail && let Some(drv) = Self::try_spawn_pipewire() {
            return Some(drv);
        }

        #[cfg(feature = "pulseaudio")]
        if pulse_avail
            && let Some(drv) = Self::try_spawn_worker("waywi-audio-pulse", run_pulse_playback)
        {
            return Some(drv);
        }

        #[cfg(feature = "alsa")]
        if alsa_avail
            && let Some(drv) = Self::try_spawn_worker("waywi-audio-alsa", run_alsa_playback)
        {
            return Some(drv);
        }

        // Runtime fallback pass for non-standard socket environments where the fast path skipped
        // a compiled-in backend.
        #[cfg(feature = "pipewire")]
        if !pw_avail && let Some(drv) = Self::try_spawn_pipewire() {
            return Some(drv);
        }

        #[cfg(feature = "pulseaudio")]
        if !pulse_avail
            && let Some(drv) = Self::try_spawn_worker("waywi-audio-pulse", run_pulse_playback)
        {
            return Some(drv);
        }

        #[cfg(feature = "alsa")]
        if !alsa_avail
            && let Some(drv) = Self::try_spawn_worker("waywi-audio-alsa", run_alsa_playback)
        {
            return Some(drv);
        }

        None
    }

    #[cfg(feature = "pipewire")]
    fn try_spawn_pipewire() -> Option<Self> {
        let mixer = Arc::new((Mutex::new(PolyphonicMixer::default()), Condvar::new()));
        let stop = Arc::new(AtomicBool::new(false));
        let alive = Arc::new(AtomicBool::new(true));
        let idle_frames = Arc::new(AtomicUsize::new(0));
        let (pw_tx, pw_rx) = pipewire::channel::channel::<PwPlaySignal>();
        let (init_tx, init_rx) = std::sync::mpsc::sync_channel::<bool>(1);

        let m = Arc::clone(&mixer);
        let s = Arc::clone(&stop);
        let a = Arc::clone(&alive);
        let idle = Arc::clone(&idle_frames);
        let tx_proc = pw_tx.clone();
        let join = thread::Builder::new()
            .name("waywi-audio-pw".to_owned())
            .spawn(move || run_pipewire_playback(m, s, a, idle, tx_proc, pw_rx, init_tx))
            .ok()?;

        if !init_rx.recv().unwrap_or(false) {
            let _ = join.join();
            return None;
        }

        Some(Self {
            mixer,
            stop,
            alive,
            pw_wake: Some(pw_tx),
            idle_frames,
            join: Some(join),
        })
    }

    #[cfg(any(feature = "pulseaudio", feature = "alsa"))]
    fn try_spawn_worker(
        name: &'static str,
        f: fn(SharedMixer, Arc<AtomicBool>, Arc<AtomicBool>, std::sync::mpsc::SyncSender<bool>),
    ) -> Option<Self> {
        let mixer = Arc::new((Mutex::new(PolyphonicMixer::default()), Condvar::new()));
        let stop = Arc::new(AtomicBool::new(false));
        let alive = Arc::new(AtomicBool::new(true));
        let (init_tx, init_rx) = std::sync::mpsc::sync_channel::<bool>(1);
        let m = Arc::clone(&mixer);
        let s = Arc::clone(&stop);
        let a = Arc::clone(&alive);
        let join = thread::Builder::new()
            .name(name.to_owned())
            .spawn(move || f(m, s, a, init_tx))
            .ok()?;

        if !init_rx.recv().unwrap_or(false) {
            let _ = join.join();
            return None;
        }

        Some(Self {
            mixer,
            stop,
            alive,
            #[cfg(feature = "pipewire")]
            pw_wake: None,
            #[cfg(feature = "pipewire")]
            idle_frames: Arc::new(AtomicUsize::new(0)),
            join: Some(join),
        })
    }
}

#[cfg(any(feature = "pipewire", feature = "pulseaudio", feature = "alsa"))]
impl PlaybackDriver for NativeDriver {
    fn is_alive(&self) -> bool {
        self.alive.load(Ordering::Acquire)
    }

    fn send_play(&mut self, stereo_48k: Arc<[f32]>, volume: f32) {
        #[cfg(feature = "pipewire")]
        self.idle_frames.store(0, Ordering::Release);
        if let Ok(mut g) = self.mixer.0.lock() {
            g.push(stereo_48k, volume);
            self.mixer.1.notify_one();
        }
        #[cfg(feature = "pipewire")]
        if let Some(ref tx) = self.pw_wake
            && tx.send(PwPlaySignal::EnsureActive).is_err()
        {
            self.alive.store(false, Ordering::Release);
        }
    }

    fn send_stop(&mut self) {
        if let Ok(mut g) = self.mixer.0.lock() {
            g.clear();
        }
        #[cfg(feature = "pipewire")]
        if let Some(ref tx) = self.pw_wake {
            let _ = tx.send(PwPlaySignal::StopAll);
        }
    }

    fn shutdown(&mut self) {
        let guard = self.mixer.0.lock();
        self.stop.store(true, Ordering::Release);
        self.mixer.1.notify_all();
        drop(guard);
        #[cfg(feature = "pipewire")]
        if let Some(ref tx) = self.pw_wake {
            let _ = tx.send(PwPlaySignal::Shutdown);
        }
        if let Some(j) = self.join.take() {
            let _ = j.join();
        }
    }
}

#[cfg(feature = "pulseaudio")]
fn mix_next_chunk(mixer: &SharedMixer, stop: &AtomicBool, out: &mut [f32]) -> bool {
    let mut g = mixer.0.lock().unwrap_or_else(|e| e.into_inner());
    g = mixer
        .1
        .wait_while(g, |m| m.is_empty() && !stop.load(Ordering::Acquire))
        .unwrap_or_else(|e| e.into_inner());
    if stop.load(Ordering::Acquire) {
        return false;
    }
    g.mix_f32(out)
}

#[cfg(feature = "pipewire")]
fn run_pipewire_playback(
    mixer: SharedMixer,
    stop: Arc<AtomicBool>,
    alive: Arc<AtomicBool>,
    idle_frames: Arc<AtomicUsize>,
    pw_tx: pipewire::channel::Sender<PwPlaySignal>,
    pw_rx: pipewire::channel::Receiver<PwPlaySignal>,
    init_tx: std::sync::mpsc::SyncSender<bool>,
) {
    use std::cell::Cell;
    use std::rc::Rc;

    use pipewire as pw;
    use pw::spa;
    use spa::param::ParamType;
    use spa::param::audio::{AudioFormat, AudioInfoRaw};
    use spa::pod::Value as PodValue;
    use spa::pod::serialize::PodSerializer;
    use spa::utils::{Direction, SpaTypes};

    let _alive_guard = AliveGuard(Arc::clone(&alive));
    super::pipewire::ensure_pw_init();
    let Ok(mainloop) = pw::main_loop::MainLoopRc::new(None) else {
        let _ = init_tx.send(false);
        return;
    };
    let Ok(ctx) = pw::context::ContextRc::new(&mainloop, None) else {
        let _ = init_tx.send(false);
        return;
    };
    let Ok(core) = ctx.connect_rc(None) else {
        let _ = init_tx.send(false);
        return;
    };

    let props = pw::properties::properties! {
        *pw::keys::MEDIA_TYPE => "Audio",
        *pw::keys::MEDIA_CATEGORY => "Playback",
        *pw::keys::MEDIA_ROLE => "Event",
        *pw::keys::APP_NAME => "waywi",
        *pw::keys::NODE_NAME => "waywi-audio",
    };
    let Ok(stream) = pw::stream::StreamRc::new(core, "waywi-audio", props) else {
        let _ = init_tx.send(false);
        return;
    };

    let is_active = Rc::new(Cell::new(false));
    let active_state = Rc::clone(&is_active);
    let alive_state = Arc::clone(&alive);
    let mainloop_err = mainloop.clone();
    let m_proc = Arc::clone(&mixer);
    let idle_proc = Arc::clone(&idle_frames);
    let _listener = stream
        .add_local_listener_with_user_data([0.0f32; 2048])
        .state_changed(move |_stream, _fbuf, _old, new_state| match new_state {
            pw::stream::StreamState::Streaming => active_state.set(true),
            pw::stream::StreamState::Paused | pw::stream::StreamState::Unconnected => {
                active_state.set(false);
            }
            pw::stream::StreamState::Error(_) => {
                active_state.set(false);
                alive_state.store(false, Ordering::Release);
                mainloop_err.quit();
            }
            pw::stream::StreamState::Connecting => {}
        })
        .process(move |stream, fbuf| {
            let Some(mut buffer) = stream.dequeue_buffer() else {
                return;
            };
            let datas = buffer.datas_mut();
            let Some(data) = datas.first_mut() else {
                return;
            };
            let stride = std::mem::size_of::<f32>() * 2;
            let Some(slice) = data.data() else {
                return;
            };
            let frame_count = (slice.len() / stride).min(1024);
            let float_count = frame_count * 2;
            let out_slice = &mut fbuf[..float_count];
            let has_audio = if let Ok(mut g) = m_proc.0.lock() {
                g.mix_f32(out_slice)
            } else {
                out_slice.fill(0.0);
                false
            };
            for (dst, &sample) in slice.as_chunks_mut::<4>().0.iter_mut().zip(&*out_slice) {
                *dst = sample.to_le_bytes();
            }
            let chunk = data.chunk_mut();
            *chunk.offset_mut() = 0;
            *chunk.stride_mut() = stride as i32;
            *chunk.size_mut() = (float_count * std::mem::size_of::<f32>()) as u32;

            if has_audio {
                idle_proc.store(0, Ordering::Release);
            } else {
                let prev = idle_proc.fetch_add(frame_count, Ordering::AcqRel);
                if prev < IDLE_CORK_FRAMES && prev + frame_count >= IDLE_CORK_FRAMES {
                    let _ = pw_tx.send(PwPlaySignal::CheckIdleCork);
                }
            }
        })
        .register();

    let mut audio_info = AudioInfoRaw::new();
    audio_info.set_format(AudioFormat::F32LE);
    audio_info.set_rate(MIXER_SAMPLE_RATE);
    audio_info.set_channels(2);
    let pod_obj = PodValue::Object(spa::pod::Object {
        type_: SpaTypes::ObjectParamFormat.as_raw(),
        id: ParamType::EnumFormat.as_raw(),
        properties: audio_info.into(),
    });
    let Ok((pod_bytes, _)) = PodSerializer::serialize(std::io::Cursor::new(Vec::new()), &pod_obj)
    else {
        let _ = init_tx.send(false);
        return;
    };
    let Some(pod) = spa::pod::Pod::from_bytes(pod_bytes.get_ref()) else {
        let _ = init_tx.send(false);
        return;
    };
    let mut params = [pod];
    let flags = pw::stream::StreamFlags::AUTOCONNECT
        | pw::stream::StreamFlags::MAP_BUFFERS
        | pw::stream::StreamFlags::RT_PROCESS
        | pw::stream::StreamFlags::INACTIVE;
    if stream
        .connect(Direction::Output, None, flags, &mut params)
        .is_err()
    {
        let _ = init_tx.send(false);
        return;
    }

    let stream_ctl = stream.clone();
    let active_ctl = Rc::clone(&is_active);
    let mixer_ctl = Arc::clone(&mixer);
    let idle_ctl = Arc::clone(&idle_frames);
    let stop_ctl = Arc::clone(&stop);
    let mainloop_quit = mainloop.clone();
    let _attached_rx = pw_rx.attach(mainloop.loop_(), move |sig| {
        if stop_ctl.load(Ordering::Acquire) {
            let _ = stream_ctl.disconnect();
            mainloop_quit.quit();
            return;
        }
        match sig {
            PwPlaySignal::EnsureActive => {
                if !active_ctl.get() && stream_ctl.set_active(true).is_ok() {
                    active_ctl.set(true);
                }
            }
            PwPlaySignal::CheckIdleCork => {
                let empty = mixer_ctl.0.lock().map(|g| g.is_empty()).unwrap_or(true);
                if empty
                    && idle_ctl.load(Ordering::Acquire) >= IDLE_CORK_FRAMES
                    && active_ctl.get()
                    && stream_ctl.set_active(false).is_ok()
                {
                    active_ctl.set(false);
                }
            }
            PwPlaySignal::StopAll => {
                if active_ctl.get() {
                    let _ = stream_ctl.flush(false);
                    if stream_ctl.set_active(false).is_ok() {
                        active_ctl.set(false);
                    }
                }
            }
            PwPlaySignal::Shutdown => {
                let _ = stream_ctl.disconnect();
                mainloop_quit.quit();
            }
        }
    });

    let _ = init_tx.send(true);
    mainloop.run();
}

#[cfg(feature = "pulseaudio")]
fn run_pulse_playback(
    mixer: SharedMixer,
    stop: Arc<AtomicBool>,
    alive: Arc<AtomicBool>,
    init_tx: std::sync::mpsc::SyncSender<bool>,
) {
    use libpulse_binding as pa;
    use pa::mainloop::threaded::Mainloop;
    use pa::proplist::Proplist;
    use pa::sample::{Format, Spec};
    use pa::stream::{FlagSet as StreamFlags, SeekMode, State as StreamState, Stream};

    let _alive_guard = AliveGuard(alive);

    let Some(mut ml) = Mainloop::new() else {
        let _ = init_tx.send(false);
        return;
    };
    let Some(mut ctx) = pa::context::Context::new(&ml, "waywi-audio") else {
        let _ = init_tx.send(false);
        return;
    };
    if ctx
        .connect(None, pa::context::FlagSet::NOFLAGS, None)
        .is_err()
        || ml.start().is_err()
    {
        let _ = init_tx.send(false);
        return;
    }

    ml.lock();
    while !stop.load(Ordering::Acquire) {
        match ctx.get_state() {
            pa::context::State::Ready => break,
            pa::context::State::Failed | pa::context::State::Terminated => {
                ml.unlock();
                ml.stop();
                let _ = init_tx.send(false);
                return;
            }
            _ => ml.wait(),
        }
    }
    if stop.load(Ordering::Acquire) {
        ml.unlock();
        ml.stop();
        let _ = init_tx.send(false);
        return;
    }

    let spec = Spec {
        format: Format::F32le,
        channels: 2,
        rate: MIXER_SAMPLE_RATE,
    };
    let Some(mut proplist) = Proplist::new() else {
        ml.unlock();
        ml.stop();
        let _ = init_tx.send(false);
        return;
    };
    let _ = proplist.set_str(pa::proplist::properties::MEDIA_ROLE, "event");
    let _ = proplist.set_str(pa::proplist::properties::APPLICATION_NAME, "waywi");
    let Some(mut stream) =
        Stream::new_with_proplist(&mut ctx, "waywi-audio", &spec, None, &mut proplist)
    else {
        ml.unlock();
        ml.stop();
        let _ = init_tx.send(false);
        return;
    };
    if stream
        .connect_playback(None, None, StreamFlags::START_CORKED, None, None)
        .is_err()
    {
        ml.unlock();
        ml.stop();
        let _ = init_tx.send(false);
        return;
    }
    ml.unlock();
    let _ = init_tx.send(true);

    let mut is_corked = true;
    let mut fbuf = [0.0f32; 512];
    let mut bbuf = [0u8; 512 * 4];
    while !stop.load(Ordering::Acquire) {
        let empty = mixer.0.lock().map(|g| g.is_empty()).unwrap_or(true);
        if empty && !is_corked {
            // Drain server playback latency briefly before corking so tail audio is never clipped.
            let drain_done = Arc::new(AtomicBool::new(false));
            let drain_done_cb = Arc::clone(&drain_done);
            let mixer_cb = Arc::clone(&mixer);
            ml.lock();
            let drain_op = if stream.get_state() == StreamState::Ready {
                Some(stream.drain(Some(Box::new(move |_| {
                    drain_done_cb.store(true, Ordering::Release);
                    if let Ok(_g) = mixer_cb.0.lock() {
                        mixer_cb.1.notify_one();
                    }
                }))))
            } else {
                None
            };
            ml.unlock();

            let still_empty = if drain_op.is_some() {
                let deadline = std::time::Instant::now() + PULSE_DRAIN_TIMEOUT;
                let mut g = mixer.0.lock().unwrap_or_else(|e| e.into_inner());
                loop {
                    if stop.load(Ordering::Acquire)
                        || !g.is_empty()
                        || drain_done.load(Ordering::Acquire)
                    {
                        break g.is_empty();
                    }
                    let Some(rem) = deadline.checked_duration_since(std::time::Instant::now())
                    else {
                        break g.is_empty();
                    };
                    let (next_g, _) = mixer
                        .1
                        .wait_timeout(g, rem)
                        .unwrap_or_else(|e| e.into_inner());
                    g = next_g;
                }
            } else {
                true
            };

            ml.lock();
            if let Some(mut op) = drain_op
                && op.get_state() == pa::operation::State::Running
            {
                op.cancel();
            }
            if still_empty
                && !stop.load(Ordering::Acquire)
                && stream.get_state() == StreamState::Ready
            {
                let _ = stream.cork(None);
                is_corked = true;
            }
            ml.unlock();
        }
        if !mix_next_chunk(&mixer, &stop, &mut fbuf) {
            continue;
        }
        for (dst, &s) in bbuf.as_chunks_mut::<4>().0.iter_mut().zip(&fbuf) {
            *dst = s.to_le_bytes();
        }
        ml.lock();
        let ctx_dead = matches!(
            ctx.get_state(),
            pa::context::State::Failed | pa::context::State::Terminated
        );
        let stream_dead = matches!(
            stream.get_state(),
            StreamState::Failed | StreamState::Terminated
        );
        if ctx_dead || stream_dead {
            ml.unlock();
            break;
        }
        if stream.get_state() == StreamState::Ready {
            if is_corked {
                let _ = stream.uncork(None);
                is_corked = false;
            }
            let _ = stream.write_copy(&bbuf, 0, SeekMode::Relative);
        }
        ml.unlock();
    }
    ml.lock();
    if stream.get_state() != StreamState::Unconnected {
        let _ = stream.disconnect();
    }
    ctx.disconnect();
    ml.unlock();
    ml.stop();
}

#[cfg(feature = "alsa")]
fn open_alsa_pcm() -> Option<alsa::pcm::PCM> {
    use alsa::pcm::{Access, Format, HwParams, PCM};
    use alsa::{Direction, ValueOr};

    let pcm = PCM::new("default", Direction::Playback, false).ok()?;
    {
        let hwp = HwParams::any(&pcm).ok()?;
        hwp.set_access(Access::RWInterleaved).ok()?;
        hwp.set_format(Format::s16()).ok()?;
        hwp.set_channels(2).ok()?;
        hwp.set_rate(MIXER_SAMPLE_RATE, ValueOr::Nearest).ok()?;
        pcm.hw_params(&hwp).ok()?;
    }
    // Verify i16 I/O is supported on the configured device before returning.
    pcm.io_i16().ok()?;
    Some(pcm)
}

#[cfg(feature = "alsa")]
fn run_alsa_playback(
    mixer: SharedMixer,
    stop: Arc<AtomicBool>,
    alive: Arc<AtomicBool>,
    init_tx: std::sync::mpsc::SyncSender<bool>,
) {
    let _alive_guard = AliveGuard(alive);
    let Some(initial_pcm) = open_alsa_pcm() else {
        let _ = init_tx.send(false);
        return;
    };
    let _ = init_tx.send(true);

    let mut pcm = Some(initial_pcm);
    let mut fbuf = [0.0f32; 512];
    let mut ibuf = [0i16; 512];
    while !stop.load(Ordering::Acquire) {
        let has_chunk = {
            let mut g = mixer.0.lock().unwrap_or_else(|e| e.into_inner());
            if pcm.is_some() {
                let (next_g, _) = mixer
                    .1
                    .wait_timeout_while(g, ALSA_IDLE_CLOSE_TIMEOUT, |m| {
                        m.is_empty() && !stop.load(Ordering::Acquire)
                    })
                    .unwrap_or_else(|e| e.into_inner());
                g = next_g;
            } else {
                g = mixer
                    .1
                    .wait_while(g, |m| m.is_empty() && !stop.load(Ordering::Acquire))
                    .unwrap_or_else(|e| e.into_inner());
            }
            if stop.load(Ordering::Acquire) {
                break;
            }
            if g.is_empty() {
                false
            } else {
                g.mix_f32(&mut fbuf)
            }
        };

        if !has_chunk {
            // Release hardware PCM handle after idle timeout so other apps/power management can
            // use or suspend the ALSA device.
            if let Some(handle) = pcm.take() {
                let _ = handle.drain();
                drop(handle);
            }
            continue;
        }

        if pcm.is_none() {
            pcm = open_alsa_pcm();
        }
        let Some(ref handle) = pcm else {
            continue;
        };
        for (dst, &src) in ibuf.iter_mut().zip(&fbuf) {
            *dst = (src * 32767.0).clamp(-32768.0, 32767.0) as i16;
        }
        let write_failed = if let Ok(io) = handle.io_i16() {
            if let Err(err) = io.writei(&ibuf) {
                handle.try_recover(err, true).is_err()
            } else {
                false
            }
        } else {
            true
        };
        if write_failed {
            // Drop dead hardware handle (e.g. HDMI/USB DAC disconnected on DPMS) so the next
            // chunk re-opens the default ALSA device cleanly.
            pcm = None;
        }
    }
    if let Some(handle) = pcm.take() {
        let _ = handle.drain();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn build_wav_16bit(sample_rate: u32, samples: &[i16]) -> Vec<u8> {
        let data_bytes = (samples.len() * 2) as u32;
        let byte_rate = sample_rate * 2;
        let riff_len = 36 + data_bytes;
        let mut out = Vec::with_capacity((riff_len + 8) as usize);
        out.extend_from_slice(b"RIFF");
        out.extend_from_slice(&riff_len.to_le_bytes());
        out.extend_from_slice(b"WAVE");
        out.extend_from_slice(b"fmt ");
        out.extend_from_slice(&16u32.to_le_bytes());
        out.extend_from_slice(&1u16.to_le_bytes());
        out.extend_from_slice(&1u16.to_le_bytes());
        out.extend_from_slice(&sample_rate.to_le_bytes());
        out.extend_from_slice(&byte_rate.to_le_bytes());
        out.extend_from_slice(&2u16.to_le_bytes());
        out.extend_from_slice(&16u16.to_le_bytes());
        out.extend_from_slice(b"data");
        out.extend_from_slice(&data_bytes.to_le_bytes());
        for s in samples {
            out.extend_from_slice(&s.to_le_bytes());
        }
        out
    }

    #[test]
    fn caches_presets_and_supports_unload_and_clear() {
        let engine = AudioPlayer::new();
        let a = engine.load_preset(SoundPreset::Click);
        let b = engine.load_preset(SoundPreset::Click);
        assert_eq!(a.id, b.id);
        assert!(engine.cached_sample_count(a.id).unwrap_or(0) > 0);
        let _ = a.id.get();

        assert!(engine.unload(a));
        assert!(!engine.unload(a));
        assert!(engine.cached_sample_count(a.id).is_none());

        let c = engine.load_preset(SoundPreset::Hover);
        assert_ne!(c.id, a.id);
        engine.clear_cache();
        assert!(engine.cached_sample_count(c.id).is_none());
    }

    #[test]
    fn load_file_deduplicates_by_content_bytes_and_propagates_io_errors() {
        let engine = AudioPlayer::new();
        let dir = std::env::temp_dir().join(format!("waywi-player-dedup-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();

        let wav = build_wav_16bit(48_000, &[1000, -1000, 500, -500]);
        let path_a = dir.join("a.wav");
        let path_b = dir.join("b.wav");
        fs::write(&path_a, &wav).unwrap();
        fs::write(&path_b, &wav).unwrap();

        let asset_a = engine.load_file(&path_a).expect("load a.wav");
        let asset_b = engine.load_file(&path_b).expect("load b.wav");
        let asset_mem = engine.load_bytes(&wav).expect("load wav bytes");
        assert_eq!(asset_a.id, asset_b.id);
        assert_eq!(asset_a.id, asset_mem.id);

        let missing_err = engine
            .load_file(dir.join("missing.wav"))
            .expect_err("missing file must error");
        assert!(matches!(
            missing_err,
            WaywiError::Io(ref e) if e.kind() == std::io::ErrorKind::NotFound
        ));

        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn with_cache_limit_evicts_oldest_sound_and_redecodes_lazily() {
        let wav_a = build_wav_16bit(48_000, &[100; 32]);
        let wav_b = build_wav_16bit(48_000, &[200; 32]);
        let wav_c = build_wav_16bit(48_000, &[300; 32]);
        // Each 32-frame mono 48kHz WAV resamples to 64 stereo f32 samples = 256 bytes.
        let engine = AudioPlayer::with_cache_limit(512);
        let a = engine.load_bytes(&wav_a).unwrap();
        let b = engine.load_bytes(&wav_b).unwrap();
        let c = engine.load_bytes(&wav_c).unwrap();
        // `a`'s decoded PCM was evicted to honor the 512-byte budget, while `b` & `c` stay cached.
        assert!(engine.cached_sample_count(a.id).is_none());
        assert!(engine.cached_sample_count(b.id).is_some());
        assert!(engine.cached_sample_count(c.id).is_some());
        // Playing/resolving `a` transparently re-decodes without invalidating its handle.
        let redecoded = engine
            .resolve_stereo(a.id)
            .expect("lazy re-decode of evicted sound");
        assert_eq!(redecoded.len(), 64);
        assert_eq!(engine.cached_sample_count(a.id), Some(64));
    }

    #[test]
    fn trim_memory_releases_pcm_but_keeps_file_bytes_and_preset_handles_live() {
        let engine = AudioPlayer::new();
        let dir = std::env::temp_dir().join(format!("waywi-player-trim-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();

        let wav_file = build_wav_16bit(48_000, &[500, -500, 250, -250]);
        let wav_mem = build_wav_16bit(48_000, &[900, -900, 100, -100]);
        let path = dir.join("cue.wav");
        fs::write(&path, &wav_file).unwrap();

        let file_snd = engine.load_file(&path).expect("load file");
        let mem_snd = engine.load_bytes(&wav_mem).expect("load bytes");
        let preset_snd = engine.load_preset(SoundPreset::Click);

        assert!(engine.cached_sample_count(file_snd.id).is_some());
        assert!(engine.cached_sample_count(mem_snd.id).is_some());
        assert!(engine.cached_sample_count(preset_snd.id).is_some());

        engine.trim_memory();
        assert!(engine.cached_sample_count(file_snd.id).is_none());
        assert!(engine.cached_sample_count(mem_snd.id).is_none());
        assert!(engine.cached_sample_count(preset_snd.id).is_none());

        // All handles survive trim_memory() and transparently re-decode on demand.
        assert!(engine.resolve_stereo(file_snd.id).is_some());
        assert!(engine.resolve_stereo(mem_snd.id).is_some());
        assert!(engine.resolve_stereo(preset_snd.id).is_some());

        // Explicit unload / clear_cache permanently invalidate handles.
        assert!(engine.unload(file_snd));
        assert!(engine.resolve_stereo(file_snd.id).is_none());
        engine.clear_cache();
        assert!(engine.resolve_stereo(mem_snd.id).is_none());
        assert!(engine.resolve_stereo(preset_snd.id).is_none());

        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn dead_playback_driver_is_torn_down_before_respawn() {
        struct DeadDriver {
            shutdown_called: Arc<AtomicBool>,
        }
        impl PlaybackDriver for DeadDriver {
            fn is_alive(&self) -> bool {
                false
            }
            fn send_play(&mut self, _stereo_48k: Arc<[f32]>, _volume: f32) {
                panic!("dead driver must not receive play commands");
            }
            fn send_stop(&mut self) {}
            fn shutdown(&mut self) {
                self.shutdown_called.store(true, Ordering::Release);
            }
        }

        let shutdown_called = Arc::new(AtomicBool::new(false));
        let engine = AudioPlayer::new();
        *engine.inner.driver.lock().unwrap() = Some(Box::new(DeadDriver {
            shutdown_called: Arc::clone(&shutdown_called),
        }));
        engine.enqueue_play(Arc::from([0.1_f32, 0.1_f32]), 0.5);
        assert!(
            shutdown_called.load(Ordering::Acquire),
            "dead driver must be shut down and evicted on next play"
        );
    }
}
