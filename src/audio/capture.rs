//! On-demand audio capture & output-monitor streaming (VU peak/RMS metering and optional raw
//! interleaved PCM frames) across PipeWire, PulseAudio, and ALSA.
//!
//! Capture workers are spawned lazily only while a [`crate::audio::Audio::capture`] subscription
//! is active, emit zero wakeups during sustained silence, and tear down immediately on
//! unsubscribe. Higher-level DSP (such as FFT spectrum analysis or smoothing) is intentionally
//! left to the application.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

use calloop::channel as calloop_channel;

/// Source target for [`crate::audio::Audio::capture`].
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum AudioCaptureTarget {
    /// Loopback monitor of the active default output sink (ideal for audio visualizers / VU meters).
    DefaultOutputMonitor,
    /// Active default input source (microphone).
    DefaultInput,
    /// Loopback monitor of a specific output sink by backend device ID (`AudioDevice::id`).
    SinkMonitor(u32),
    /// Specific input source by backend device ID (`AudioDevice::id`).
    Source(u32),
    /// Explicit backend device/node name (PipeWire `node.name`, PulseAudio source/monitor name,
    /// or ALSA PCM device string).
    DeviceName(String),
}

/// Configuration for [`crate::audio::Audio::capture`].
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct AudioCaptureConfig {
    /// Requested sample rate in Hz (default `48_000`).
    pub sample_rate: u32,
    /// Requested channel count (`1` for mono, `2` for stereo; default `2`).
    pub channels: u16,
    /// Capture window size in frames (clamped to `64..=4096`; default `1024`).
    pub frame_size: usize,
    /// Minimum interval between emitted [`AudioCaptureFrame`] messages (default `33 ms` / ~30 Hz).
    pub update_interval: Duration,
    /// Include raw interleaved `f32` PCM samples in [`AudioCaptureFrame::pcm`] (default `false`).
    pub include_pcm: bool,
    /// Peak amplitude below which a frame is treated as silent (default `1e-4`).
    ///
    /// WayWi emits one `silent: true` frame upon transitioning from audio to silence, then
    /// suppresses further messages and event-loop wakeups until audio resumes.
    pub silence_threshold: f32,
}

impl Default for AudioCaptureConfig {
    fn default() -> Self {
        Self {
            sample_rate: 48_000,
            channels: 2,
            frame_size: 1024,
            update_interval: Duration::from_millis(33),
            include_pcm: false,
            silence_threshold: 1e-4,
        }
    }
}

impl AudioCaptureConfig {
    /// Sets the requested sample rate in Hz (clamped to `8_000..=192_000`).
    #[must_use]
    pub const fn sample_rate(mut self, hz: u32) -> Self {
        self.sample_rate = if hz < 8_000 {
            8_000
        } else if hz > 192_000 {
            192_000
        } else {
            hz
        };
        self
    }

    /// Sets the requested channel count (`1` mono or `2` stereo).
    #[must_use]
    pub const fn channels(mut self, channels: u16) -> Self {
        self.channels = if channels <= 1 { 1 } else { 2 };
        self
    }

    /// Sets the capture window size in frames (clamped to `64..=4096`).
    #[must_use]
    pub const fn frame_size(mut self, frames: usize) -> Self {
        self.frame_size = if frames < 64 {
            64
        } else if frames > 4096 {
            4096
        } else {
            frames
        };
        self
    }

    /// Sets the minimum emission interval between UI frames.
    #[must_use]
    pub const fn update_interval(mut self, interval: Duration) -> Self {
        self.update_interval = interval;
        self
    }

    /// Enables or disables copying raw interleaved PCM samples into [`AudioCaptureFrame::pcm`].
    #[must_use]
    pub const fn include_pcm(mut self, include: bool) -> Self {
        self.include_pcm = include;
        self
    }

    /// Sets the peak silence gate threshold (`0.0..=1.0`).
    #[must_use]
    pub fn silence_threshold(mut self, threshold: f32) -> Self {
        self.silence_threshold = if threshold.is_finite() {
            threshold.clamp(0.0, 1.0)
        } else {
            1e-4
        };
        self
    }

    pub(crate) fn normalized(&self) -> Self {
        Self {
            sample_rate: self.sample_rate.clamp(8_000, 192_000),
            channels: self.channels.clamp(1, 2),
            frame_size: self.frame_size.clamp(64, 4096),
            update_interval: self.update_interval,
            include_pcm: self.include_pcm,
            silence_threshold: if self.silence_threshold.is_finite() {
                self.silence_threshold.clamp(0.0, 1.0)
            } else {
                1e-4
            },
        }
    }

    /// Returns an exact comparable key for subscription worker reconciliation.
    pub(crate) fn eq_key(&self) -> (u32, u16, usize, Duration, bool, u32) {
        let n = self.normalized();
        (
            n.sample_rate,
            n.channels,
            n.frame_size,
            n.update_interval,
            n.include_pcm,
            n.silence_threshold.to_bits(),
        )
    }
}

/// Real-time audio capture frame emitted by [`crate::audio::Audio::capture`].
#[derive(Debug, Clone, PartialEq)]
pub struct AudioCaptureFrame {
    /// Peak amplitude on the left (or mono) channel (`0.0..=1.0+`).
    pub peak_left: f32,
    /// Peak amplitude on the right channel (equals `peak_left` for mono).
    pub peak_right: f32,
    /// Root-mean-square level on the left (or mono) channel.
    pub rms_left: f32,
    /// Root-mean-square level on the right channel (equals `rms_left` for mono).
    pub rms_right: f32,
    /// Combined peak level in decibels relative to full scale (`-96.0..=0.0+` dBFS).
    pub peak_db: f32,
    /// `true` when peak amplitude is below [`AudioCaptureConfig::silence_threshold`].
    pub silent: bool,
    /// Effective capture sample rate in Hz.
    pub sample_rate: u32,
    /// Effective capture channel count (`1` or `2`).
    pub channels: u16,
    /// Interleaved `f32` PCM samples when [`AudioCaptureConfig::include_pcm`] is `true`; empty otherwise.
    pub pcm: Arc<[f32]>,
}

impl AudioCaptureFrame {
    /// Combined peak amplitude across channels.
    #[must_use]
    pub fn peak(&self) -> f32 {
        self.peak_left.max(self.peak_right)
    }

    /// Combined RMS amplitude across channels.
    #[must_use]
    pub fn rms(&self) -> f32 {
        0.5 * (self.rms_left + self.rms_right)
    }
}

/// Allocation-free sliding-window capture meter and silence gate.
pub(crate) struct CaptureAnalyzer {
    cfg: AudioCaptureConfig,
    ring: Vec<f32>,
    write_pos: usize,
    filled: usize,
    last_emit: Option<Instant>,
    was_silent: bool,
}

impl CaptureAnalyzer {
    pub(crate) fn new(cfg: AudioCaptureConfig) -> Self {
        let cfg = cfg.normalized();
        let ch = usize::from(cfg.channels);
        Self {
            cfg,
            ring: vec![0.0; cfg.frame_size * ch],
            write_pos: 0,
            filled: 0,
            last_emit: None,
            was_silent: false,
        }
    }

    /// Pushes interleaved `f32` samples and returns a ready [`AudioCaptureFrame`] when the cadence
    /// interval has elapsed and the silence gate permits emission.
    pub(crate) fn push_interleaved(
        &mut self,
        samples: &[f32],
        now: Instant,
    ) -> Option<AudioCaptureFrame> {
        let ch = usize::from(self.cfg.channels);
        let frame_count = samples.len() / ch;
        if frame_count == 0 {
            return None;
        }
        let cap = self.ring.len();
        let total = frame_count * ch;
        let slice = &samples[..total];
        if total >= cap {
            self.ring.copy_from_slice(&slice[total - cap..]);
            self.write_pos = 0;
            self.filled = cap;
        } else {
            let first = (cap - self.write_pos).min(total);
            self.ring[self.write_pos..self.write_pos + first].copy_from_slice(&slice[..first]);
            let rem = total - first;
            if rem > 0 {
                self.ring[..rem].copy_from_slice(&slice[first..]);
            }
            self.write_pos = (self.write_pos + total) % cap;
            self.filled = (self.filled + total).min(cap);
        }

        if self.filled < cap {
            return None;
        }
        if let Some(prev) = self.last_emit
            && now.saturating_duration_since(prev) < self.cfg.update_interval
        {
            return None;
        }

        self.emit_window(now)
    }

    fn emit_window(&mut self, now: Instant) -> Option<AudioCaptureFrame> {
        let ch = usize::from(self.cfg.channels);
        let n = self.cfg.frame_size;
        let cap = self.ring.len();

        let mut peak_l = 0.0_f32;
        let mut peak_r = 0.0_f32;
        let mut sum_sq_l = 0.0_f32;
        let mut sum_sq_r = 0.0_f32;

        let mut idx = self.write_pos;
        for _ in 0..n {
            let l = self.ring[idx];
            let l = if l.is_finite() { l } else { 0.0 };
            let r = if ch == 2 {
                let v = self.ring[idx + 1];
                if v.is_finite() { v } else { 0.0 }
            } else {
                l
            };
            idx += ch;
            if idx >= cap {
                idx = 0;
            }
            let al = l.abs();
            let ar = r.abs();
            if al > peak_l {
                peak_l = al;
            }
            if ar > peak_r {
                peak_r = ar;
            }
            sum_sq_l += l * l;
            sum_sq_r += r * r;
        }

        let max_peak = peak_l.max(peak_r);
        let silent = max_peak < self.cfg.silence_threshold;
        if silent {
            if self.was_silent {
                return None;
            }
            self.was_silent = true;
            self.last_emit = Some(now);
            return Some(AudioCaptureFrame {
                peak_left: 0.0,
                peak_right: 0.0,
                rms_left: 0.0,
                rms_right: 0.0,
                peak_db: -96.0,
                silent: true,
                sample_rate: self.cfg.sample_rate,
                channels: self.cfg.channels,
                pcm: Arc::from([]),
            });
        }

        self.was_silent = false;
        self.last_emit = Some(now);

        let inv_n = 1.0 / (n as f32);
        let rms_left = (sum_sq_l * inv_n).sqrt();
        let rms_right = (sum_sq_r * inv_n).sqrt();
        let peak_db = if max_peak > 1e-5 {
            (20.0 * max_peak.log10()).max(-96.0)
        } else {
            -96.0
        };

        let pcm: Arc<[f32]> = if self.cfg.include_pcm {
            let mut out = Vec::with_capacity(cap);
            out.extend_from_slice(&self.ring[self.write_pos..]);
            out.extend_from_slice(&self.ring[..self.write_pos]);
            Arc::from(out)
        } else {
            Arc::from([])
        };

        Some(AudioCaptureFrame {
            peak_left: peak_l,
            peak_right: peak_r,
            rms_left,
            rms_right,
            peak_db,
            silent: false,
            sample_rate: self.cfg.sample_rate,
            channels: self.cfg.channels,
            pcm,
        })
    }
}

/// Active background audio capture worker owned by `SubscriptionManager`.
pub(crate) struct AudioCaptureWorker {
    stop: Arc<AtomicBool>,
    #[cfg(feature = "pipewire")]
    pw_stop_tx: Option<pipewire::channel::Sender<()>>,
    join: Option<JoinHandle<()>>,
}

impl Drop for AudioCaptureWorker {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Release);
        #[cfg(feature = "pipewire")]
        if let Some(ref tx) = self.pw_stop_tx {
            let _ = tx.send(());
        }
        if let Some(handle) = self.join.take() {
            let _ = handle.join();
        }
    }
}

impl AudioCaptureWorker {
    pub(crate) fn spawn(
        target: AudioCaptureTarget,
        config: AudioCaptureConfig,
        tx: calloop_channel::Sender<AudioCaptureFrame>,
    ) -> Self {
        let cfg = config.normalized();
        let stop = Arc::new(AtomicBool::new(false));

        #[cfg(feature = "pipewire")]
        if super::backend::pipewire_runtime_available()
            && let Some(worker) = Self::try_spawn_pipewire(&target, cfg, &tx, &stop)
        {
            return worker;
        }

        #[cfg(feature = "pulseaudio")]
        if super::backend::pulseaudio_runtime_available()
            && let Some(worker) = Self::try_spawn_pulse(&target, cfg, &tx, &stop)
        {
            return worker;
        }

        #[cfg(feature = "alsa")]
        if super::backend::alsa_runtime_available()
            && let Some(worker) = Self::try_spawn_alsa(&target, cfg, &tx, &stop)
        {
            return worker;
        }

        Self {
            stop,
            #[cfg(feature = "pipewire")]
            pw_stop_tx: None,
            join: None,
        }
    }

    #[cfg(feature = "pipewire")]
    fn try_spawn_pipewire(
        target: &AudioCaptureTarget,
        cfg: AudioCaptureConfig,
        tx: &calloop_channel::Sender<AudioCaptureFrame>,
        stop: &Arc<AtomicBool>,
    ) -> Option<Self> {
        let (pw_tx, pw_rx) = pipewire::channel::channel::<()>();
        let (init_tx, init_rx) = std::sync::mpsc::sync_channel::<bool>(1);
        let target_cl = target.clone();
        let tx_cl = tx.clone();
        let stop_cl = Arc::clone(stop);

        let join = thread::Builder::new()
            .name("waywi-capture-pw".to_owned())
            .spawn(move || {
                run_pipewire_capture(target_cl, cfg, tx_cl, stop_cl, pw_rx, init_tx);
            })
            .ok()?;

        if !init_rx.recv().unwrap_or(false) {
            let _ = join.join();
            return None;
        }

        Some(Self {
            stop: Arc::clone(stop),
            pw_stop_tx: Some(pw_tx),
            join: Some(join),
        })
    }

    #[cfg(feature = "pulseaudio")]
    fn try_spawn_pulse(
        target: &AudioCaptureTarget,
        cfg: AudioCaptureConfig,
        tx: &calloop_channel::Sender<AudioCaptureFrame>,
        stop: &Arc<AtomicBool>,
    ) -> Option<Self> {
        let (init_tx, init_rx) = std::sync::mpsc::sync_channel::<bool>(1);
        let target_cl = target.clone();
        let tx_cl = tx.clone();
        let stop_cl = Arc::clone(stop);

        let join = thread::Builder::new()
            .name("waywi-capture-pa".to_owned())
            .spawn(move || {
                run_pulse_capture(target_cl, cfg, tx_cl, stop_cl, init_tx);
            })
            .ok()?;

        if !init_rx.recv().unwrap_or(false) {
            let _ = join.join();
            return None;
        }

        Some(Self {
            stop: Arc::clone(stop),
            #[cfg(feature = "pipewire")]
            pw_stop_tx: None,
            join: Some(join),
        })
    }

    #[cfg(feature = "alsa")]
    fn try_spawn_alsa(
        target: &AudioCaptureTarget,
        cfg: AudioCaptureConfig,
        tx: &calloop_channel::Sender<AudioCaptureFrame>,
        stop: &Arc<AtomicBool>,
    ) -> Option<Self> {
        let dev_name = match target {
            AudioCaptureTarget::DefaultInput => "default".to_owned(),
            AudioCaptureTarget::DeviceName(name) if !name.is_empty() => name.clone(),
            AudioCaptureTarget::DefaultOutputMonitor
            | AudioCaptureTarget::SinkMonitor(_)
            | AudioCaptureTarget::Source(_)
            | AudioCaptureTarget::DeviceName(_) => return None,
        };
        let (init_tx, init_rx) = std::sync::mpsc::sync_channel::<bool>(1);
        let tx_cl = tx.clone();
        let stop_cl = Arc::clone(stop);

        let join = thread::Builder::new()
            .name("waywi-capture-alsa".to_owned())
            .spawn(move || {
                run_alsa_capture(dev_name, cfg, tx_cl, stop_cl, init_tx);
            })
            .ok()?;

        if !init_rx.recv().unwrap_or(false) {
            let _ = join.join();
            return None;
        }

        Some(Self {
            stop: Arc::clone(stop),
            #[cfg(feature = "pipewire")]
            pw_stop_tx: None,
            join: Some(join),
        })
    }
}

#[cfg(feature = "pipewire")]
fn run_pipewire_capture(
    target: AudioCaptureTarget,
    cfg: AudioCaptureConfig,
    tx: calloop_channel::Sender<AudioCaptureFrame>,
    stop: Arc<AtomicBool>,
    pw_rx: pipewire::channel::Receiver<()>,
    init_tx: std::sync::mpsc::SyncSender<bool>,
) {
    use pipewire as pw;
    use pw::spa;
    use spa::param::ParamType;
    use spa::param::audio::{AudioFormat, AudioInfoRaw};
    use spa::pod::Value as PodValue;
    use spa::pod::serialize::PodSerializer;
    use spa::utils::{Direction, SpaTypes};

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

    let mut props = pw::properties::properties! {
        *pw::keys::MEDIA_TYPE => "Audio",
        *pw::keys::MEDIA_CATEGORY => "Capture",
        *pw::keys::APP_NAME => "waywi",
        *pw::keys::NODE_NAME => "waywi-capture",
    };
    match &target {
        AudioCaptureTarget::DefaultOutputMonitor => {
            props.insert(*pw::keys::STREAM_CAPTURE_SINK, "true");
            props.insert(*pw::keys::MEDIA_ROLE, "Monitor");
        }
        AudioCaptureTarget::DefaultInput => {
            props.insert(*pw::keys::MEDIA_ROLE, "Communication");
        }
        AudioCaptureTarget::SinkMonitor(id) => {
            props.insert(*pw::keys::STREAM_CAPTURE_SINK, "true");
            props.insert("target.object", id.to_string());
            props.insert(*pw::keys::MEDIA_ROLE, "Monitor");
        }
        AudioCaptureTarget::Source(id) => {
            props.insert("target.object", id.to_string());
            props.insert(*pw::keys::MEDIA_ROLE, "Communication");
        }
        AudioCaptureTarget::DeviceName(name) => {
            if !name.is_empty() {
                props.insert("target.object", name.as_str());
            }
        }
    }

    let Ok(stream) = pw::stream::StreamRc::new(core, "waywi-capture", props) else {
        let _ = init_tx.send(false);
        return;
    };

    let mainloop_err = mainloop.clone();
    let stop_proc = Arc::clone(&stop);
    let mut scratch: Vec<f32> = Vec::with_capacity(cfg.frame_size * usize::from(cfg.channels));
    let _listener = stream
        .add_local_listener_with_user_data(CaptureAnalyzer::new(cfg))
        .state_changed(move |_stream, _analyzer, _old, new_state| {
            if matches!(new_state, pw::stream::StreamState::Error(_)) {
                mainloop_err.quit();
            }
        })
        .process(move |stream, analyzer| {
            if stop_proc.load(Ordering::Acquire) {
                return;
            }
            let Some(mut buffer) = stream.dequeue_buffer() else {
                return;
            };
            let datas = buffer.datas_mut();
            let Some(data) = datas.first_mut() else {
                return;
            };
            let chunk = data.chunk();
            let offset = chunk.offset() as usize;
            let size = chunk.size() as usize;
            let Some(raw) = data.data() else {
                return;
            };
            let Some(valid) = raw.get(offset..offset.saturating_add(size)) else {
                return;
            };
            if valid.len() < 4 {
                return;
            }
            scratch.clear();
            let (chunks, _) = valid.as_chunks::<4>();
            scratch.reserve(chunks.len());
            for bytes in chunks {
                scratch.push(f32::from_le_bytes(*bytes));
            }
            if let Some(frame) = analyzer.push_interleaved(&scratch, Instant::now()) {
                let _ = tx.send(frame);
            }
        })
        .register();

    let mut audio_info = AudioInfoRaw::new();
    audio_info.set_format(AudioFormat::F32LE);
    audio_info.set_rate(cfg.sample_rate);
    audio_info.set_channels(u32::from(cfg.channels));
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
        | pw::stream::StreamFlags::RT_PROCESS;
    if stream
        .connect(Direction::Input, None, flags, &mut params)
        .is_err()
    {
        let _ = init_tx.send(false);
        return;
    }

    let stream_ctl = stream.clone();
    let mainloop_quit = mainloop.clone();
    let _attached_rx = pw_rx.attach(mainloop.loop_(), move |()| {
        let _ = stream_ctl.disconnect();
        mainloop_quit.quit();
    });

    let _ = init_tx.send(true);
    mainloop.run();
    let _ = stream.disconnect();
}

#[cfg(feature = "pulseaudio")]
fn run_pulse_capture(
    target: AudioCaptureTarget,
    cfg: AudioCaptureConfig,
    tx: calloop_channel::Sender<AudioCaptureFrame>,
    stop: Arc<AtomicBool>,
    init_tx: std::sync::mpsc::SyncSender<bool>,
) {
    use libpulse_binding as pa;
    use pa::callbacks::ListResult;
    use pa::def::BufferAttr;
    use pa::mainloop::threaded::Mainloop;
    use pa::proplist::Proplist;
    use pa::sample::{Format, Spec};
    use pa::stream::{FlagSet as StreamFlags, PeekResult, State as StreamState, Stream};

    let Some(mut ml) = Mainloop::new() else {
        let _ = init_tx.send(false);
        return;
    };
    let Some(mut ctx) = pa::context::Context::new(&ml, "waywi-capture") else {
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

    // Resolve target source/monitor device name cleanly.
    let resolved_dev: Option<String> = match target {
        AudioCaptureTarget::DefaultOutputMonitor => Some("@DEFAULT_MONITOR@".to_owned()),
        AudioCaptureTarget::DefaultInput => Some("@DEFAULT_SOURCE@".to_owned()),
        AudioCaptureTarget::DeviceName(name) if !name.is_empty() => Some(name),
        AudioCaptureTarget::DeviceName(_) => None,
        AudioCaptureTarget::SinkMonitor(id) => {
            let found = Arc::new(std::sync::Mutex::new(None::<String>));
            let found_cb = Arc::clone(&found);
            let done = Arc::new(AtomicBool::new(false));
            let done_cb = Arc::clone(&done);
            let op = ctx
                .introspect()
                .get_sink_info_by_index(id, move |res| match res {
                    ListResult::Item(info) => {
                        if let Some(mon) = info.monitor_source_name.as_deref()
                            && let Ok(mut g) = found_cb.lock()
                        {
                            *g = Some(mon.to_owned());
                        }
                    }
                    ListResult::End | ListResult::Error => {
                        done_cb.store(true, Ordering::Release);
                    }
                });
            while !done.load(Ordering::Acquire)
                && op.get_state() == pa::operation::State::Running
                && !stop.load(Ordering::Acquire)
            {
                ml.unlock();
                thread::sleep(Duration::from_millis(5));
                ml.lock();
            }
            found.lock().ok().and_then(|g| g.clone())
        }
        AudioCaptureTarget::Source(id) => {
            let found = Arc::new(std::sync::Mutex::new(None::<String>));
            let found_cb = Arc::clone(&found);
            let done = Arc::new(AtomicBool::new(false));
            let done_cb = Arc::clone(&done);
            let op = ctx
                .introspect()
                .get_source_info_by_index(id, move |res| match res {
                    ListResult::Item(info) => {
                        if let Some(name) = info.name.as_deref()
                            && let Ok(mut g) = found_cb.lock()
                        {
                            *g = Some(name.to_owned());
                        }
                    }
                    ListResult::End | ListResult::Error => {
                        done_cb.store(true, Ordering::Release);
                    }
                });
            while !done.load(Ordering::Acquire)
                && op.get_state() == pa::operation::State::Running
                && !stop.load(Ordering::Acquire)
            {
                ml.unlock();
                thread::sleep(Duration::from_millis(5));
                ml.lock();
            }
            found.lock().ok().and_then(|g| g.clone())
        }
    };

    let spec = Spec {
        format: Format::F32le,
        channels: cfg.channels as u8,
        rate: cfg.sample_rate,
    };
    let Some(mut proplist) = Proplist::new() else {
        ml.unlock();
        ml.stop();
        let _ = init_tx.send(false);
        return;
    };
    let _ = proplist.set_str(pa::proplist::properties::APPLICATION_NAME, "waywi");
    let Some(mut stream) =
        Stream::new_with_proplist(&mut ctx, "waywi-capture", &spec, None, &mut proplist)
    else {
        ml.unlock();
        ml.stop();
        let _ = init_tx.send(false);
        return;
    };

    let frag_bytes = (cfg.frame_size * usize::from(cfg.channels) * 4) as u32;
    let attr = BufferAttr {
        maxlength: u32::MAX,
        tlength: u32::MAX,
        prebuf: u32::MAX,
        minreq: u32::MAX,
        fragsize: frag_bytes,
    };
    if stream
        .connect_record(
            resolved_dev.as_deref(),
            Some(&attr),
            StreamFlags::ADJUST_LATENCY,
        )
        .is_err()
    {
        ml.unlock();
        ml.stop();
        let _ = init_tx.send(false);
        return;
    }

    ml.unlock();
    let _ = init_tx.send(true);

    let mut analyzer = CaptureAnalyzer::new(cfg);
    let mut scratch: Vec<f32> = Vec::with_capacity(cfg.frame_size * usize::from(cfg.channels));
    while !stop.load(Ordering::Acquire) {
        ml.lock();
        let dead = matches!(
            ctx.get_state(),
            pa::context::State::Failed | pa::context::State::Terminated
        ) || matches!(
            stream.get_state(),
            StreamState::Failed | StreamState::Terminated
        );
        if dead {
            ml.unlock();
            break;
        }
        let mut read_any = false;
        if stream.get_state() == StreamState::Ready {
            while let Ok(peek) = stream.peek() {
                match peek {
                    PeekResult::Empty => break,
                    PeekResult::Hole(_) => {
                        let _ = stream.discard();
                    }
                    PeekResult::Data(bytes) => {
                        read_any = true;
                        scratch.clear();
                        let (chunks, _) = bytes.as_chunks::<4>();
                        scratch.reserve(chunks.len());
                        for b in chunks {
                            scratch.push(f32::from_le_bytes(*b));
                        }
                        let _ = stream.discard();
                        if let Some(frame) = analyzer.push_interleaved(&scratch, Instant::now()) {
                            let _ = tx.send(frame);
                        }
                    }
                }
            }
        }
        ml.unlock();
        if !read_any {
            let sleep_dur = cfg
                .update_interval
                .clamp(Duration::from_millis(5), Duration::from_millis(20));
            thread::sleep(sleep_dur);
        }
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
fn run_alsa_capture(
    dev_name: String,
    cfg: AudioCaptureConfig,
    tx: calloop_channel::Sender<AudioCaptureFrame>,
    stop: Arc<AtomicBool>,
    init_tx: std::sync::mpsc::SyncSender<bool>,
) {
    use alsa::pcm::{Access, Format, HwParams, PCM};
    use alsa::{Direction, ValueOr};

    let Ok(pcm) = PCM::new(&dev_name, Direction::Capture, false) else {
        let _ = init_tx.send(false);
        return;
    };
    let use_f32 = {
        let Ok(hwp) = HwParams::any(&pcm) else {
            let _ = init_tx.send(false);
            return;
        };
        if hwp.set_access(Access::RWInterleaved).is_err() {
            let _ = init_tx.send(false);
            return;
        }
        let f32_ok = hwp.set_format(Format::float()).is_ok();
        if !f32_ok && hwp.set_format(Format::s16()).is_err() {
            let _ = init_tx.send(false);
            return;
        }
        if hwp.set_channels(u32::from(cfg.channels)).is_err()
            || hwp.set_rate(cfg.sample_rate, ValueOr::Nearest).is_err()
            || pcm.hw_params(&hwp).is_err()
        {
            let _ = init_tx.send(false);
            return;
        }
        f32_ok
    };
    let _ = init_tx.send(true);

    let ch = usize::from(cfg.channels);
    let chunk_frames = cfg.frame_size.min(512);
    let mut analyzer = CaptureAnalyzer::new(cfg);

    if use_f32 {
        let Ok(io) = pcm.io_f32() else {
            return;
        };
        let mut buf = vec![0.0_f32; chunk_frames * ch];
        while !stop.load(Ordering::Acquire) {
            match io.readi(&mut buf) {
                Ok(n) if n > 0 => {
                    if let Some(frame) = analyzer.push_interleaved(&buf[..n * ch], Instant::now()) {
                        let _ = tx.send(frame);
                    }
                }
                Ok(_) => {}
                Err(err) => {
                    if pcm.try_recover(err, true).is_err() {
                        break;
                    }
                }
            }
        }
    } else {
        let Ok(io) = pcm.io_i16() else {
            return;
        };
        let mut ibuf = vec![0_i16; chunk_frames * ch];
        let mut fbuf = vec![0.0_f32; chunk_frames * ch];
        while !stop.load(Ordering::Acquire) {
            match io.readi(&mut ibuf) {
                Ok(n) if n > 0 => {
                    let count = n * ch;
                    for (dst, &s) in fbuf[..count].iter_mut().zip(&ibuf[..count]) {
                        *dst = f32::from(s) / 32768.0;
                    }
                    if let Some(frame) = analyzer.push_interleaved(&fbuf[..count], Instant::now()) {
                        let _ = tx.send(frame);
                    }
                }
                Ok(_) => {}
                Err(err) => {
                    if pcm.try_recover(err, true).is_err() {
                        break;
                    }
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use std::f32::consts::PI;

    use super::*;

    #[test]
    fn analyzer_computes_levels_pcm_and_suppresses_sustained_silence() {
        let cfg = AudioCaptureConfig::default()
            .sample_rate(48_000)
            .channels(2)
            .frame_size(1024)
            .update_interval(Duration::from_millis(10))
            .include_pcm(true);
        let mut analyzer = CaptureAnalyzer::new(cfg);

        // Generate 1024 stereo frames of a 1 kHz sine wave (left = 0.8, right = 0.4).
        let mut samples = Vec::with_capacity(1024 * 2);
        for i in 0..1024 {
            let phase = 2.0 * PI * 1000.0 * (i as f32) / 48_000.0;
            let s = phase.sin();
            samples.push(s * 0.8);
            samples.push(s * 0.4);
        }

        let t0 = Instant::now();
        let frame = analyzer
            .push_interleaved(&samples, t0)
            .expect("full window emits frame");
        assert!(!frame.silent);
        assert!((frame.peak_left - 0.8).abs() < 0.02);
        assert!((frame.peak_right - 0.4).abs() < 0.02);
        assert!((frame.rms_left - 0.8 / 2.0_f32.sqrt()).abs() < 0.03);
        assert_eq!(frame.pcm.len(), 2048);

        // Transition to silence emits exactly ONE silent frame, then suppresses wakeups.
        let zeros = vec![0.0_f32; 1024 * 2];
        let silent1 = analyzer
            .push_interleaved(&zeros, t0 + Duration::from_millis(20))
            .expect("first silence transition emits one frame");
        assert!(silent1.silent);
        assert_eq!(silent1.peak(), 0.0);
        assert!(silent1.pcm.is_empty());

        let silent2 = analyzer.push_interleaved(&zeros, t0 + Duration::from_millis(40));
        assert!(
            silent2.is_none(),
            "sustained silence must emit zero wakeups"
        );

        // Resuming audio wakes immediately again.
        let resumed = analyzer
            .push_interleaved(&samples, t0 + Duration::from_millis(60))
            .expect("audio resume emits frame");
        assert!(!resumed.silent);
    }
}
