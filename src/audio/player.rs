//! Lazy background audio playback engine supporting both `rodio` (`audio-playback`) and
//! dependency-light native PipeWire / PulseAudio / ALSA PCM output.

use std::collections::HashMap;
use std::path::Path;
use std::sync::{Arc, Mutex};
use std::thread::{self, JoinHandle};

use crate::asset::decode::hash_bytes;
use crate::error::{Result, WaywiError};

use super::sound::{
    MIXER_SAMPLE_RATE, SoundAsset, SoundAssetStore, SoundData, SoundPreset,
    resolve_sound_theme_path,
};

#[cfg(not(feature = "audio-playback"))]
use super::sound::PolyphonicMixer;

#[cfg(feature = "audio-playback")]
enum PlayerCommand {
    Play { stereo_48k: Arc<[f32]>, volume: f32 },
    StopAll,
    Shutdown,
}

trait PlaybackDriver: Send {
    fn send_play(&mut self, stereo_48k: Arc<[f32]>, volume: f32);
    fn send_stop(&mut self);
    fn shutdown(&mut self);
}

/// Audio asset cache and low-latency polyphonic playback engine.
#[derive(Clone)]
pub struct AudioPlayer {
    inner: Arc<AudioPlayerInner>,
}

struct AudioPlayerInner {
    store: Mutex<SoundAssetStore>,
    path_hashes: Mutex<HashMap<String, u64>>,
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
    /// Creates an idle audio player that opens a playback stream lazily on first play.
    #[must_use]
    pub fn new() -> Self {
        Self {
            inner: Arc::new(AudioPlayerInner {
                store: Mutex::new(SoundAssetStore::default()),
                path_hashes: Mutex::new(HashMap::new()),
                driver: Mutex::new(None),
            }),
        }
    }

    /// Loads and caches a sound file from disk (bounded to 16 MiB).
    pub fn load_file(&self, path: impl AsRef<Path>) -> Result<SoundAsset> {
        let p = path.as_ref();
        let key = p.to_string_lossy().into_owned();
        if let Ok(ph) = self.inner.path_hashes.lock()
            && let Some(&hash) = ph.get(&key)
        {
            let store = self.inner.store.lock().unwrap_or_else(|e| e.into_inner());
            if let Some(existing) = store.get_by_hash(hash) {
                return Ok(existing);
            }
        }
        let data = SoundData::from_file(p)?;
        let raw_len = data.samples.len() as u64;
        let hash = hash_bytes(key.as_bytes()) ^ raw_len;
        if let Ok(mut ph) = self.inner.path_hashes.lock() {
            ph.insert(key, hash);
        }
        let mut store = self.inner.store.lock().unwrap_or_else(|e| e.into_inner());
        Ok(store.insert_sound_data(hash, &data))
    }

    /// Decodes and caches an in-memory audio buffer (`WAV` always; `OGG`/`MP3`/`FLAC` with
    /// `audio-playback`).
    pub fn load_bytes(&self, bytes: &[u8]) -> Result<SoundAsset> {
        let hash = hash_bytes(bytes);
        {
            let store = self.inner.store.lock().unwrap_or_else(|e| e.into_inner());
            if let Some(existing) = store.get_by_hash(hash) {
                return Ok(existing);
            }
        }
        let data = SoundData::from_bytes(bytes)?;
        let mut store = self.inner.store.lock().unwrap_or_else(|e| e.into_inner());
        Ok(store.insert_sound_data(hash, &data))
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
        let tag = match preset {
            SoundPreset::Click => 1u64,
            SoundPreset::DragTick => 2,
            SoundPreset::Hover => 3,
            SoundPreset::Notification => 4,
            SoundPreset::Warning => 5,
        };
        let hash = 0x5741_5957_0000_0000 | tag;
        {
            let store = self.inner.store.lock().unwrap_or_else(|e| e.into_inner());
            if let Some(existing) = store.get_by_hash(hash) {
                return existing;
            }
        }
        let data = SoundData::from_preset(preset);
        let mut store = self.inner.store.lock().unwrap_or_else(|e| e.into_inner());
        store.insert_sound_data(hash, &data)
    }

    /// Plays a cached [`SoundAsset`] at full volume (`1.0`).
    pub fn play(&self, sound: SoundAsset) {
        self.play_with_volume(sound, 1.0);
    }

    /// Plays a cached [`SoundAsset`] scaled by `volume` (`0.0..=2.0`).
    pub fn play_with_volume(&self, sound: SoundAsset, volume: f32) {
        let stereo = {
            let store = self.inner.store.lock().unwrap_or_else(|e| e.into_inner());
            store.get_stereo(sound.id)
        };
        if let Some(buf) = stereo {
            self.enqueue_play(buf, volume);
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
        self.play_with_volume(asset, volume);
    }

    /// Resolves and plays a freedesktop XDG Sound Theme sound if found on the system.
    pub fn play_system_sound(&self, name: &str, volume: f32) -> Result<()> {
        let asset = self.load_system_sound(name)?;
        self.play_with_volume(asset, volume);
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

    /// Queries the decoded buffer for `id` (used for diagnostics/testing).
    #[cfg(test)]
    pub(crate) fn cached_sample_count(&self, id: crate::asset::AssetId) -> Option<usize> {
        let store = self.inner.store.lock().unwrap_or_else(|e| e.into_inner());
        store.get_stereo(id).map(|b| b.len())
    }

    fn enqueue_play(&self, stereo_48k: Arc<[f32]>, volume: f32) {
        if stereo_48k.is_empty() || volume <= 0.0 {
            return;
        }
        let mut guard = self.inner.driver.lock().unwrap_or_else(|e| e.into_inner());
        if guard.is_none() {
            *guard = start_playback_driver();
        }
        if let Some(ref mut drv) = *guard {
            drv.send_play(stereo_48k, volume.clamp(0.0, 2.0));
        }
    }
}

fn start_playback_driver() -> Option<Box<dyn PlaybackDriver>> {
    #[cfg(feature = "audio-playback")]
    {
        if let Some(drv) = RodioDriver::spawn() {
            return Some(Box::new(drv));
        }
    }
    #[cfg(not(feature = "audio-playback"))]
    {
        if let Some(drv) = NativeDriver::spawn() {
            return Some(Box::new(drv));
        }
    }
    None
}

#[cfg(feature = "audio-playback")]
struct RodioDriver {
    tx: std::sync::mpsc::Sender<PlayerCommand>,
    join: Option<JoinHandle<()>>,
}

#[cfg(feature = "audio-playback")]
impl RodioDriver {
    fn spawn() -> Option<Self> {
        let (tx, rx) = std::sync::mpsc::channel::<PlayerCommand>();
        let join = thread::Builder::new()
            .name("waywi-audio-rodio".to_owned())
            .spawn(move || {
                let Ok(sink) = rodio::DeviceSinkBuilder::open_default_sink() else {
                    return;
                };
                let channels = std::num::NonZero::new(2).unwrap();
                let rate = std::num::NonZero::new(MIXER_SAMPLE_RATE).unwrap();
                let mut active_players: Vec<rodio::Player> = Vec::new();
                while let Ok(cmd) = rx.recv() {
                    match cmd {
                        PlayerCommand::Play { stereo_48k, volume } => {
                            active_players.retain(|p| !p.empty());
                            let src = rodio::buffer::SamplesBuffer::new(
                                channels,
                                rate,
                                stereo_48k.to_vec(),
                            );
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
        Some(Self {
            tx,
            join: Some(join),
        })
    }
}

#[cfg(feature = "audio-playback")]
impl PlaybackDriver for RodioDriver {
    fn send_play(&mut self, stereo_48k: Arc<[f32]>, volume: f32) {
        let _ = self.tx.send(PlayerCommand::Play { stereo_48k, volume });
    }

    fn send_stop(&mut self) {
        let _ = self.tx.send(PlayerCommand::StopAll);
    }

    fn shutdown(&mut self) {
        let _ = self.tx.send(PlayerCommand::Shutdown);
        if let Some(j) = self.join.take() {
            let _ = j.join();
        }
    }
}

#[cfg(not(feature = "audio-playback"))]
use std::sync::Condvar;
#[cfg(not(feature = "audio-playback"))]
use std::sync::atomic::{AtomicBool, Ordering};

#[cfg(not(feature = "audio-playback"))]
struct NativeDriver {
    mixer: Arc<(Mutex<PolyphonicMixer>, Condvar)>,
    stop: Arc<AtomicBool>,
    join: Option<JoinHandle<()>>,
}

#[cfg(not(feature = "audio-playback"))]
impl NativeDriver {
    fn spawn() -> Option<Self> {
        let mixer = Arc::new((Mutex::new(PolyphonicMixer::default()), Condvar::new()));
        let stop = Arc::new(AtomicBool::new(false));

        let try_spawn = |name: &'static str,
                         f: fn(Arc<(Mutex<PolyphonicMixer>, Condvar)>, Arc<AtomicBool>)|
         -> Option<Self> {
            let m = Arc::clone(&mixer);
            let s = Arc::clone(&stop);
            thread::Builder::new()
                .name(name.to_owned())
                .spawn(move || f(m, s))
                .ok()
                .map(|j| Self {
                    mixer: Arc::clone(&mixer),
                    stop: Arc::clone(&stop),
                    join: Some(j),
                })
        };

        #[cfg(feature = "pipewire")]
        if super::backend::pipewire_runtime_available()
            && let Some(drv) = try_spawn("waywi-audio-pw", run_pipewire_playback)
        {
            return Some(drv);
        }

        #[cfg(feature = "pulseaudio")]
        if super::backend::pulseaudio_runtime_available()
            && let Some(drv) = try_spawn("waywi-audio-pulse", run_pulse_playback)
        {
            return Some(drv);
        }

        #[cfg(feature = "alsa")]
        if super::backend::alsa_runtime_available()
            && let Some(drv) = try_spawn("waywi-audio-alsa", run_alsa_playback)
        {
            return Some(drv);
        }

        #[cfg(feature = "pipewire")]
        if let Some(drv) = try_spawn("waywi-audio-pw", run_pipewire_playback) {
            return Some(drv);
        }
        #[cfg(all(not(feature = "pipewire"), feature = "pulseaudio"))]
        if let Some(drv) = try_spawn("waywi-audio-pulse", run_pulse_playback) {
            return Some(drv);
        }
        #[cfg(all(
            not(feature = "pipewire"),
            not(feature = "pulseaudio"),
            feature = "alsa"
        ))]
        if let Some(drv) = try_spawn("waywi-audio-alsa", run_alsa_playback) {
            return Some(drv);
        }

        None
    }
}

#[cfg(not(feature = "audio-playback"))]
impl PlaybackDriver for NativeDriver {
    fn send_play(&mut self, stereo_48k: Arc<[f32]>, volume: f32) {
        if let Ok(mut g) = self.mixer.0.lock() {
            g.push(stereo_48k, volume);
            self.mixer.1.notify_one();
        }
    }

    fn send_stop(&mut self) {
        if let Ok(mut g) = self.mixer.0.lock() {
            g.clear();
        }
    }

    fn shutdown(&mut self) {
        let guard = self.mixer.0.lock();
        self.stop.store(true, Ordering::Release);
        self.mixer.1.notify_all();
        drop(guard);
        if let Some(j) = self.join.take() {
            let _ = j.join();
        }
    }
}

#[cfg(all(
    not(feature = "audio-playback"),
    any(feature = "pulseaudio", feature = "alsa")
))]
fn mix_next_chunk(
    mixer: &Arc<(Mutex<PolyphonicMixer>, Condvar)>,
    stop: &AtomicBool,
    out: &mut [f32],
) -> bool {
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

#[cfg(all(not(feature = "audio-playback"), feature = "pipewire"))]
fn run_pipewire_playback(mixer: Arc<(Mutex<PolyphonicMixer>, Condvar)>, stop: Arc<AtomicBool>) {
    use pipewire as pw;
    use pw::spa;
    use spa::param::ParamType;
    use spa::param::audio::{AudioFormat, AudioInfoRaw};
    use spa::pod::Value as PodValue;
    use spa::pod::serialize::PodSerializer;
    use spa::utils::{Direction, SpaTypes};

    super::pipewire::ensure_pw_init();
    let Ok(mainloop) = pw::main_loop::MainLoopBox::new(None) else {
        return;
    };
    let Ok(ctx) = pw::context::ContextBox::new(mainloop.loop_(), None) else {
        return;
    };
    let Ok(core) = ctx.connect(None) else {
        return;
    };

    let props = pw::properties::properties! {
        *pw::keys::MEDIA_TYPE => "Audio",
        *pw::keys::MEDIA_CATEGORY => "Playback",
        *pw::keys::NODE_NAME => "waywi-audio",
    };
    let Ok(stream) = pw::stream::StreamBox::new(&core, "waywi-audio", props) else {
        return;
    };

    let m_proc = Arc::clone(&mixer);
    let _listener = stream
        .add_local_listener_with_user_data([0.0f32; 2048])
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
            if let Ok(mut g) = m_proc.0.lock() {
                g.mix_f32(out_slice);
            }
            for (dst, &sample) in slice.as_chunks_mut::<4>().0.iter_mut().zip(&*out_slice) {
                *dst = sample.to_le_bytes();
            }
            let chunk = data.chunk_mut();
            *chunk.offset_mut() = 0;
            *chunk.stride_mut() = stride as i32;
            *chunk.size_mut() = (float_count * std::mem::size_of::<f32>()) as u32;
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
        return;
    };
    let Some(pod) = spa::pod::Pod::from_bytes(pod_bytes.get_ref()) else {
        return;
    };
    let mut params = [pod];
    let flags = pw::stream::StreamFlags::AUTOCONNECT
        | pw::stream::StreamFlags::MAP_BUFFERS
        | pw::stream::StreamFlags::RT_PROCESS;
    if stream
        .connect(Direction::Output, None, flags, &mut params)
        .is_err()
    {
        return;
    }

    let loop_ref = mainloop.loop_();
    while !stop.load(Ordering::Acquire) {
        let _ = loop_ref.iterate(pw::loop_::Timeout::Finite(
            std::time::Duration::from_millis(50),
        ));
    }
}

#[cfg(all(not(feature = "audio-playback"), feature = "pulseaudio"))]
fn run_pulse_playback(mixer: Arc<(Mutex<PolyphonicMixer>, Condvar)>, stop: Arc<AtomicBool>) {
    use libpulse_binding as pa;
    use pa::mainloop::threaded::Mainloop;
    use pa::sample::{Format, Spec};
    use pa::stream::{FlagSet as StreamFlags, SeekMode, Stream};

    let Some(mut ml) = Mainloop::new() else {
        return;
    };
    let Some(mut ctx) = pa::context::Context::new(&ml, "waywi-audio") else {
        return;
    };
    if ctx
        .connect(None, pa::context::FlagSet::NOFLAGS, None)
        .is_err()
        || ml.start().is_err()
    {
        return;
    }

    ml.lock();
    while !stop.load(Ordering::Acquire) {
        match ctx.get_state() {
            pa::context::State::Ready => break,
            pa::context::State::Failed | pa::context::State::Terminated => {
                ml.unlock();
                ml.stop();
                return;
            }
            _ => ml.wait(),
        }
    }
    if stop.load(Ordering::Acquire) {
        ml.unlock();
        ml.stop();
        return;
    }

    let spec = Spec {
        format: Format::F32le,
        channels: 2,
        rate: MIXER_SAMPLE_RATE,
    };
    let Some(mut stream) = Stream::new(&mut ctx, "waywi-audio", &spec, None) else {
        ml.unlock();
        ml.stop();
        return;
    };
    if stream
        .connect_playback(None, None, StreamFlags::START_CORKED, None, None)
        .is_err()
    {
        ml.unlock();
        ml.stop();
        return;
    }
    let _ = stream.uncork(None);
    ml.unlock();

    let mut fbuf = [0.0f32; 512];
    let mut bbuf = [0u8; 512 * 4];
    while !stop.load(Ordering::Acquire) {
        if !mix_next_chunk(&mixer, &stop, &mut fbuf) {
            continue;
        }
        for (dst, &s) in bbuf.as_chunks_mut::<4>().0.iter_mut().zip(&fbuf) {
            *dst = s.to_le_bytes();
        }
        ml.lock();
        let _ = stream.write_copy(&bbuf, 0, SeekMode::Relative);
        ml.unlock();
    }
    ml.stop();
}

#[cfg(all(not(feature = "audio-playback"), feature = "alsa"))]
fn run_alsa_playback(mixer: Arc<(Mutex<PolyphonicMixer>, Condvar)>, stop: Arc<AtomicBool>) {
    use alsa::pcm::{Access, Format, HwParams, PCM};
    use alsa::{Direction, ValueOr};

    let Ok(pcm) = PCM::new("default", Direction::Playback, false) else {
        return;
    };
    {
        let Ok(hwp) = HwParams::any(&pcm) else {
            return;
        };
        if hwp.set_access(Access::RWInterleaved).is_err()
            || hwp.set_format(Format::s16()).is_err()
            || hwp.set_channels(2).is_err()
            || hwp.set_rate(MIXER_SAMPLE_RATE, ValueOr::Nearest).is_err()
            || pcm.hw_params(&hwp).is_err()
        {
            return;
        }
    }
    let Ok(io) = pcm.io_i16() else {
        return;
    };
    let mut fbuf = [0.0f32; 512];
    let mut ibuf = [0i16; 512];
    while !stop.load(Ordering::Acquire) {
        if !mix_next_chunk(&mixer, &stop, &mut fbuf) {
            continue;
        }
        for (dst, &src) in ibuf.iter_mut().zip(&fbuf) {
            *dst = (src * 32767.0).clamp(-32768.0, 32767.0) as i16;
        }
        if let Err(err) = io.writei(&ibuf) {
            let _ = pcm.try_recover(err, true);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn caches_presets_without_recomputing() {
        let engine = AudioPlayer::new();
        let a = engine.load_preset(SoundPreset::Click);
        let b = engine.load_preset(SoundPreset::Click);
        assert_eq!(a.id, b.id);
        assert!(engine.cached_sample_count(a.id).unwrap_or(0) > 0);
        let _ = a.id.get();
    }
}
