//! Audio buffer decoding, synthesis, XDG Sound Theme lookup, resampling, and polyphonic mixing.

use std::collections::HashMap;
use std::env;
use std::f32::consts::TAU;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use crate::asset::AssetId;
use crate::error::{Result, WaywiError};

/// Maximum encoded sound file size (16 MiB) for bounded loading.
const MAX_SOUND_FILE_BYTES: u64 = 16 * 1024 * 1024;
/// Standard mixer output rate shared by native PCM and rodio playback.
pub(crate) const MIXER_SAMPLE_RATE: u32 = 48_000;

/// `Copy` handle referencing a decoded audio buffer cached inside [`super::player::AudioPlayer`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct SoundAsset {
    pub id: AssetId,
    pub duration: Duration,
}

/// Built-in procedural audio presets requiring no external asset files.
#[non_exhaustive]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum SoundPreset {
    /// Crisp short tactile click pop (~18 ms).
    Click,
    /// Subtle low-frequency detent tick (~10 ms, suited for drag snaps/sliders).
    DragTick,
    /// Soft airy hover tick (~10 ms).
    Hover,
    /// Two-note rising chime (~240 ms, suited for incoming notifications).
    Notification,
    /// Dual-pulse warning tone (~220 ms).
    Warning,
}

/// Normalized interleaved floating-point PCM audio buffer (`[-1.0, 1.0]`).
#[derive(Debug, Clone, PartialEq)]
pub struct SoundData {
    pub sample_rate: u32,
    pub channels: u16,
    pub samples: Arc<[f32]>,
}

impl SoundData {
    /// Constructs a [`SoundData`] buffer from interleaved `f32` PCM samples.
    pub fn from_pcm_f32(
        sample_rate: u32,
        channels: u16,
        samples: impl Into<Arc<[f32]>>,
    ) -> Result<Self> {
        if sample_rate == 0 || channels == 0 {
            return Err(WaywiError::AudioError(
                "sound sample_rate and channels must be non-zero".to_owned(),
            ));
        }
        Ok(Self {
            sample_rate,
            channels,
            samples: samples.into(),
        })
    }

    /// Computes total playback duration from sample count, channels, and sample rate.
    #[must_use]
    pub fn duration(&self) -> Duration {
        if self.sample_rate == 0 || self.channels == 0 {
            return Duration::ZERO;
        }
        let frames = self.samples.len() / usize::from(self.channels);
        Duration::from_secs_f64(frames as f64 / f64::from(self.sample_rate))
    }

    /// Reads and decodes a sound file from disk (WAV natively; OGG/MP3/FLAC/WAV with `audio-playback`).
    pub fn from_file(path: impl AsRef<Path>) -> Result<Self> {
        let path = path.as_ref();
        let meta = fs::metadata(path).map_err(|e| {
            WaywiError::AudioError(format!("failed to stat sound {}: {e}", path.display()))
        })?;
        if meta.len() > MAX_SOUND_FILE_BYTES {
            return Err(WaywiError::AudioError(format!(
                "sound {} exceeds max size ({} > {MAX_SOUND_FILE_BYTES} bytes)",
                path.display(),
                meta.len()
            )));
        }
        let bytes = fs::read(path).map_err(|e| {
            WaywiError::AudioError(format!("failed to read sound {}: {e}", path.display()))
        })?;
        let hint = path.extension().and_then(|e| e.to_str());
        Self::from_vec_with_hint(bytes, hint)
    }

    /// Decodes encoded audio bytes (`WAV` always supported; `OGG`/`MP3`/`FLAC` with `audio-playback`).
    pub fn from_bytes(bytes: &[u8]) -> Result<Self> {
        Self::from_bytes_with_hint(bytes, None)
    }

    /// Decodes encoded audio bytes using an optional extension hint (e.g. `"ogg"`, `"wav"`).
    pub fn from_bytes_with_hint(bytes: &[u8], hint: Option<&str>) -> Result<Self> {
        if is_riff_wave(bytes) {
            return Self::from_wav_bytes(bytes);
        }
        #[cfg(feature = "audio-playback")]
        {
            decode_via_rodio(bytes.to_vec(), hint)
        }
        #[cfg(not(feature = "audio-playback"))]
        {
            let _ = hint;
            Err(WaywiError::AudioError(
                "non-WAV compressed audio requires enabling the `audio-playback` Cargo feature"
                    .to_owned(),
            ))
        }
    }

    fn from_vec_with_hint(bytes: Vec<u8>, hint: Option<&str>) -> Result<Self> {
        if is_riff_wave(&bytes) {
            return Self::from_wav_bytes(&bytes);
        }
        #[cfg(feature = "audio-playback")]
        {
            decode_via_rodio(bytes, hint)
        }
        #[cfg(not(feature = "audio-playback"))]
        {
            let _ = (bytes, hint);
            Err(WaywiError::AudioError(
                "non-WAV compressed audio requires enabling the `audio-playback` Cargo feature"
                    .to_owned(),
            ))
        }
    }

    /// Pure-Rust RIFF/WAVE decoder supporting PCM (8/16/24/32-bit integer) and IEEE float (32-bit).
    pub fn from_wav_bytes(bytes: &[u8]) -> Result<Self> {
        if !is_riff_wave(bytes) {
            return Err(WaywiError::AudioError(
                "invalid RIFF/WAVE header".to_owned(),
            ));
        }
        let mut pos = 12usize;
        let mut audio_format: Option<u16> = None;
        let mut channels: u16 = 0;
        let mut sample_rate: u32 = 0;
        let mut bits_per_sample: u16 = 0;
        let mut data_slice: Option<&[u8]> = None;

        while pos + 8 <= bytes.len() {
            let chunk_id = &bytes[pos..pos + 4];
            let chunk_len =
                u32::from_le_bytes(bytes[pos + 4..pos + 8].try_into().unwrap()) as usize;
            let body_start = pos + 8;
            let body_end = body_start.saturating_add(chunk_len);
            if body_end > bytes.len() {
                break;
            }
            let body = &bytes[body_start..body_end];
            if chunk_id == b"fmt " && body.len() >= 16 {
                let mut fmt_tag = u16::from_le_bytes([body[0], body[1]]);
                channels = u16::from_le_bytes([body[2], body[3]]);
                sample_rate = u32::from_le_bytes([body[4], body[5], body[6], body[7]]);
                bits_per_sample = u16::from_le_bytes([body[14], body[15]]);
                if fmt_tag == 0xFFFE && body.len() >= 26 {
                    // WAVE_FORMAT_EXTENSIBLE subformat GUID first 2 bytes match PCM(1) or IEEE_FLOAT(3)
                    fmt_tag = u16::from_le_bytes([body[24], body[25]]);
                }
                audio_format = Some(fmt_tag);
            } else if chunk_id == b"data" {
                data_slice = Some(body);
            }
            // RIFF chunks are word-aligned (padded to even size).
            pos = body_end + (chunk_len & 1);
        }

        let fmt =
            audio_format.ok_or_else(|| WaywiError::AudioError("WAV missing fmt chunk".into()))?;
        let raw =
            data_slice.ok_or_else(|| WaywiError::AudioError("WAV missing data chunk".into()))?;
        if sample_rate == 0 || channels == 0 {
            return Err(WaywiError::AudioError(
                "WAV invalid rate or channel count".into(),
            ));
        }

        let samples: Vec<f32> = match (fmt, bits_per_sample) {
            (1, 8) => raw
                .iter()
                .map(|&b| (f32::from(b) - 128.0) / 128.0)
                .collect(),
            (1, 16) => raw
                .as_chunks::<2>()
                .0
                .iter()
                .map(|&c| f32::from(i16::from_le_bytes(c)) / 32768.0)
                .collect(),
            (1, 24) => raw
                .as_chunks::<3>()
                .0
                .iter()
                .map(|&[b0, b1, b2]| {
                    let sign = if b2 & 0x80 != 0 { 0xFF } else { 0x00 };
                    let s = i32::from_le_bytes([b0, b1, b2, sign]);
                    (s as f32) / 8_388_608.0
                })
                .collect(),
            (1, 32) => raw
                .as_chunks::<4>()
                .0
                .iter()
                .map(|&c| (i32::from_le_bytes(c) as f64 / 2_147_483_648.0) as f32)
                .collect(),
            (3, 32) => raw
                .as_chunks::<4>()
                .0
                .iter()
                .map(|&c| f32::from_le_bytes(c).clamp(-1.0, 1.0))
                .collect(),
            _ => {
                return Err(WaywiError::AudioError(format!(
                    "unsupported WAV format tag {fmt} with {bits_per_sample} bits/sample"
                )));
            }
        };

        Self::from_pcm_f32(sample_rate, channels, samples)
    }

    /// Synthesizes an anti-clicked sine tone burst in software (`48 kHz` stereo).
    #[must_use]
    pub fn tone(freq_hz: f32, duration: Duration, amplitude: f32) -> Self {
        let total_frames = ((duration.as_secs_f32() * MIXER_SAMPLE_RATE as f32).round() as usize)
            .clamp(1, 480_000);
        let amp = amplitude.clamp(0.0, 1.0);
        let fade = (total_frames / 10).clamp(1, 240);
        let mut out = Vec::with_capacity(total_frames * 2);
        for i in 0..total_frames {
            let t = i as f32 / MIXER_SAMPLE_RATE as f32;
            let env_in = if i < fade {
                i as f32 / fade as f32
            } else {
                1.0
            };
            let rem = total_frames - 1 - i;
            let env_out = if rem < fade {
                rem as f32 / fade as f32
            } else {
                1.0
            };
            let s = (TAU * freq_hz * t).sin() * amp * env_in.min(env_out);
            out.extend_from_slice(&[s, s]);
        }
        Self {
            sample_rate: MIXER_SAMPLE_RATE,
            channels: 2,
            samples: Arc::from(out),
        }
    }

    /// Generates a built-in procedural waveform for `preset`.
    #[must_use]
    pub fn from_preset(preset: SoundPreset) -> Self {
        match preset {
            SoundPreset::Click => synth_chirp(880.0, 380.0, Duration::from_millis(18), 0.32),
            SoundPreset::DragTick => synth_chirp(360.0, 220.0, Duration::from_millis(10), 0.22),
            SoundPreset::Hover => synth_chirp(660.0, 740.0, Duration::from_millis(10), 0.14),
            SoundPreset::Notification => synth_two_tone(1046.5, 1318.5, 90, 150, 0.26),
            SoundPreset::Warning => synth_two_tone(587.3, 440.0, 80, 130, 0.28),
        }
    }

    /// Resamples and normalizes this sound into `48 kHz` interleaved stereo `f32` samples.
    #[must_use]
    pub fn to_stereo_48k(&self) -> Vec<f32> {
        let ch = usize::from(self.channels.max(1));
        let src_frames = self.samples.len() / ch;
        if src_frames == 0 || self.sample_rate == 0 {
            return Vec::new();
        }
        if self.sample_rate == MIXER_SAMPLE_RATE && ch == 2 {
            return self.samples.to_vec();
        }
        self.resample_to_stereo_48k(src_frames, ch)
    }

    pub(crate) fn to_stereo_48k_arc(&self) -> Arc<[f32]> {
        let ch = usize::from(self.channels.max(1));
        let src_frames = self.samples.len() / ch;
        if src_frames == 0 || self.sample_rate == 0 {
            return Arc::from([]);
        }
        if self.sample_rate == MIXER_SAMPLE_RATE && ch == 2 {
            return Arc::clone(&self.samples);
        }
        Arc::from(self.resample_to_stereo_48k(src_frames, ch))
    }

    fn resample_to_stereo_48k(&self, src_frames: usize, ch: usize) -> Vec<f32> {
        let ratio = f64::from(self.sample_rate) / f64::from(MIXER_SAMPLE_RATE);
        let dst_frames = ((src_frames as f64) / ratio).ceil() as usize;
        let mut out = Vec::with_capacity(dst_frames * 2);

        for dst_i in 0..dst_frames {
            let src_pos = (dst_i as f64) * ratio;
            let idx0 = (src_pos.floor() as usize).min(src_frames - 1);
            let idx1 = (idx0 + 1).min(src_frames - 1);
            let frac = (src_pos - idx0 as f64) as f32;

            let (l0, r0) = frame_stereo(&self.samples, idx0, ch);
            let (l1, r1) = frame_stereo(&self.samples, idx1, ch);
            out.push(l0 + (l1 - l0) * frac);
            out.push(r0 + (r1 - r0) * frac);
        }
        out
    }
}

#[inline]
fn frame_stereo(samples: &[f32], frame_idx: usize, channels: usize) -> (f32, f32) {
    let base = frame_idx * channels;
    let l = samples.get(base).copied().unwrap_or(0.0);
    let r = if channels >= 2 {
        samples.get(base + 1).copied().unwrap_or(l)
    } else {
        l
    };
    (l, r)
}

fn synth_chirp(start_hz: f32, end_hz: f32, dur: Duration, amp: f32) -> SoundData {
    let frames = ((dur.as_secs_f32() * MIXER_SAMPLE_RATE as f32).round() as usize).max(1);
    let mut out = Vec::with_capacity(frames * 2);
    let mut phase = 0.0f32;
    for i in 0..frames {
        let u = i as f32 / frames as f32;
        let freq = start_hz + (end_hz - start_hz) * u;
        phase += TAU * freq / MIXER_SAMPLE_RATE as f32;
        let env = (1.0 - u).powi(2) * (i as f32 / 24.0).min(1.0);
        let sample = phase.sin() * amp * env;
        out.extend_from_slice(&[sample, sample]);
    }
    SoundData {
        sample_rate: MIXER_SAMPLE_RATE,
        channels: 2,
        samples: Arc::from(out),
    }
}

fn synth_two_tone(f1: f32, f2: f32, first_ms: u64, second_ms: u64, amp: f32) -> SoundData {
    let a = SoundData::tone(f1, Duration::from_millis(first_ms), amp);
    let b = SoundData::tone(f2, Duration::from_millis(second_ms), amp);
    let mut combined = Vec::with_capacity(a.samples.len() + b.samples.len());
    combined.extend_from_slice(&a.samples);
    combined.extend_from_slice(&b.samples);
    SoundData {
        sample_rate: MIXER_SAMPLE_RATE,
        channels: 2,
        samples: Arc::from(combined),
    }
}

#[inline]
fn is_riff_wave(bytes: &[u8]) -> bool {
    bytes.len() >= 12 && &bytes[0..4] == b"RIFF" && &bytes[8..12] == b"WAVE"
}

#[cfg(feature = "audio-playback")]
fn decode_via_rodio(bytes: Vec<u8>, hint: Option<&str>) -> Result<SoundData> {
    use rodio::{Decoder, Source};
    let cursor = std::io::Cursor::new(bytes);
    let mut builder = rodio::decoder::DecoderBuilder::new().with_data(cursor);
    if let Some(h) = hint {
        builder = builder.with_hint(h);
    }
    let decoder: Decoder<_> = builder
        .build()
        .map_err(|e| WaywiError::AudioError(format!("rodio audio decode failed: {e}")))?;
    let channels = decoder.channels().get();
    let sample_rate = decoder.sample_rate().get();
    let samples: Vec<f32> = decoder.collect();
    SoundData::from_pcm_f32(sample_rate, channels, samples)
}

/// Resolves a freedesktop sound theme name (e.g. `"message-new-instant"`, `"bell"`) into an
/// audio file path using `$XDG_DATA_HOME/sounds` and `$XDG_DATA_DIRS/sounds`.
#[must_use]
pub fn resolve_sound_theme_path(name: &str, theme_override: Option<&str>) -> Option<PathBuf> {
    resolve_sound_theme_path_in(name, theme_override, &default_sound_dirs())
}

/// Returns standard XDG sound theme base directories.
#[must_use]
pub fn default_sound_dirs() -> Vec<PathBuf> {
    crate::desktop::xdg::xdg_data_dirs_for("sounds")
}

/// Searches explicit sound theme directories for a sound name respecting `index.theme` inheritance.
#[must_use]
pub fn resolve_sound_theme_path_in(
    name: &str,
    theme_override: Option<&str>,
    sound_dirs: &[PathBuf],
) -> Option<PathBuf> {
    let trimmed = name.trim();
    if trimmed.is_empty() {
        return None;
    }
    let direct = Path::new(trimmed);
    if direct.is_absolute() && direct.is_file() {
        return Some(direct.to_path_buf());
    }

    let primary = theme_override
        .map(str::to_owned)
        .or_else(detect_active_sound_theme)
        .unwrap_or_else(|| "freedesktop".to_owned());
    let themes = build_sound_theme_chain(&primary, sound_dirs);
    let subdirs = ["stereo", "notifications", "actions", "alerts", ""];
    let exts: &[&str] = if cfg!(feature = "audio-playback") {
        &["oga", "ogg", "wav", "flac", "mp3"]
    } else {
        // Without `audio-playback` (`rodio`), only uncompressed WAV can be decoded natively.
        &["wav"]
    };

    let candidate_names = crate::desktop::xdg::dash_fallback_names(trimmed);

    for cand_name in &candidate_names {
        for theme in &themes {
            for base in sound_dirs {
                let root = base.join(theme);
                if !root.is_dir() {
                    continue;
                }
                for sub in subdirs {
                    let dir = if sub.is_empty() {
                        root.clone()
                    } else {
                        let d = root.join(sub);
                        if !d.is_dir() {
                            continue;
                        }
                        d
                    };
                    for ext in exts {
                        let p = dir.join(format!("{cand_name}.{ext}"));
                        if p.is_file() {
                            return Some(p);
                        }
                    }
                }
            }
        }
    }
    None
}

fn detect_active_sound_theme() -> Option<String> {
    if let Ok(t) = env::var("XDG_SOUND_THEME")
        && !t.trim().is_empty()
    {
        return Some(t.trim().to_owned());
    }
    crate::desktop::xdg::read_gtk_setting("gtk-sound-theme-name")
}

fn build_sound_theme_chain(primary: &str, dirs: &[PathBuf]) -> Vec<String> {
    crate::desktop::xdg::walk_theme_chain([primary, "freedesktop"], |name| {
        for base in dirs {
            let idx = base.join(name).join("index.theme");
            if let Ok(text) = fs::read_to_string(idx) {
                return crate::desktop::xdg::ini_key(&text, "Inherits")
                    .map(|val| crate::desktop::xdg::parse_csv_list(&val))
                    .unwrap_or_default();
            }
        }
        Vec::new()
    })
}

/// Polyphonic active-voice mixer outputting normalized 48 kHz interleaved stereo frames.
#[cfg(any(test, feature = "pipewire", feature = "pulseaudio", feature = "alsa"))]
#[derive(Default)]
pub(crate) struct PolyphonicMixer {
    voices: Vec<ActiveVoice>,
}

#[cfg(any(test, feature = "pipewire", feature = "pulseaudio", feature = "alsa"))]
struct ActiveVoice {
    stereo_48k: Arc<[f32]>,
    cursor: usize,
    gain: f32,
}

#[cfg(any(test, feature = "pipewire", feature = "pulseaudio", feature = "alsa"))]
impl PolyphonicMixer {
    pub(crate) fn push(&mut self, stereo_48k: Arc<[f32]>, gain: f32) {
        if stereo_48k.is_empty() || gain <= 0.0 {
            return;
        }
        if self.voices.len() >= 32 {
            self.voices.remove(0);
        }
        self.voices.push(ActiveVoice {
            stereo_48k,
            cursor: 0,
            gain: gain.clamp(0.0, 2.0),
        });
    }

    pub(crate) fn is_empty(&self) -> bool {
        self.voices.is_empty()
    }

    pub(crate) fn clear(&mut self) {
        self.voices.clear();
    }

    pub(crate) fn mix_f32(&mut self, out: &mut [f32]) -> bool {
        out.fill(0.0);
        if self.voices.is_empty() {
            return false;
        }
        for voice in &mut self.voices {
            let avail = voice.stereo_48k.len().saturating_sub(voice.cursor);
            let n = out.len().min(avail);
            for (dst, &src) in out[..n]
                .iter_mut()
                .zip(&voice.stereo_48k[voice.cursor..voice.cursor + n])
            {
                *dst += src * voice.gain;
            }
            voice.cursor += n;
        }
        for s in out.iter_mut() {
            *s = s.clamp(-1.0, 1.0);
        }
        self.voices.retain(|v| v.cursor < v.stereo_48k.len());
        true
    }
}

/// Shared asset map tracking decoded sounds by hash inside `AudioPlayer`.
#[derive(Default)]
pub(crate) struct SoundAssetStore {
    next_id: u64,
    by_hash: HashMap<u64, SoundAsset>,
    buffers: HashMap<AssetId, Arc<[f32]>>,
}

impl SoundAssetStore {
    pub(crate) fn insert_sound_data(&mut self, hash: u64, data: &SoundData) -> SoundAsset {
        if let Some(&existing) = self.by_hash.get(&hash) {
            return existing;
        }
        self.next_id += 1;
        let id = AssetId::new(self.next_id);
        let asset = SoundAsset {
            id,
            duration: data.duration(),
        };
        let stereo = data.to_stereo_48k_arc();
        self.by_hash.insert(hash, asset);
        self.buffers.insert(id, stereo);
        asset
    }

    pub(crate) fn get_by_hash(&self, hash: u64) -> Option<SoundAsset> {
        self.by_hash.get(&hash).copied()
    }

    pub(crate) fn get_stereo(&self, id: AssetId) -> Option<Arc<[f32]>> {
        self.buffers.get(&id).cloned()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn build_test_wav_16bit(sample_rate: u32, channels: u16, samples: &[i16]) -> Vec<u8> {
        let data_bytes = (samples.len() * 2) as u32;
        let byte_rate = sample_rate * u32::from(channels) * 2;
        let block_align = channels * 2;
        let riff_len = 36 + data_bytes;
        let mut out = Vec::with_capacity((riff_len + 8) as usize);
        out.extend_from_slice(b"RIFF");
        out.extend_from_slice(&riff_len.to_le_bytes());
        out.extend_from_slice(b"WAVE");
        out.extend_from_slice(b"fmt ");
        out.extend_from_slice(&16u32.to_le_bytes());
        out.extend_from_slice(&1u16.to_le_bytes()); // PCM
        out.extend_from_slice(&channels.to_le_bytes());
        out.extend_from_slice(&sample_rate.to_le_bytes());
        out.extend_from_slice(&byte_rate.to_le_bytes());
        out.extend_from_slice(&block_align.to_le_bytes());
        out.extend_from_slice(&16u16.to_le_bytes());
        out.extend_from_slice(b"data");
        out.extend_from_slice(&data_bytes.to_le_bytes());
        for s in samples {
            out.extend_from_slice(&s.to_le_bytes());
        }
        out
    }

    #[test]
    fn decodes_wav_resamples_and_mixes_polyphonically() {
        let wav = build_test_wav_16bit(24_000, 1, &[16384, -16384, 0, 16384]);
        let snd = SoundData::from_wav_bytes(&wav).expect("decode 16-bit mono WAV");
        assert_eq!(snd.sample_rate, 24_000);
        assert_eq!(snd.channels, 1);
        assert!((snd.samples[0] - 0.5).abs() < 0.002);

        // 24kHz -> 48kHz stereo doubles frame count and duplicates mono to L/R.
        let resampled = snd.to_stereo_48k();
        assert_eq!(resampled.len(), 4 * 2 * 2);
        assert!((resampled[0] - 0.5).abs() < 0.002);
        assert!((resampled[1] - 0.5).abs() < 0.002);

        let mut mixer = PolyphonicMixer::default();
        let v1: Arc<[f32]> = Arc::from([0.4f32, 0.4, 0.2, 0.2]);
        let v2: Arc<[f32]> = Arc::from([0.3f32, 0.3]);
        mixer.push(v1, 1.0);
        mixer.push(v2, 1.0);
        let mut out = [0.0f32; 4];
        assert!(mixer.mix_f32(&mut out));
        assert!((out[0] - 0.7).abs() < 0.001);
        assert!((out[2] - 0.2).abs() < 0.001);
        assert!(!mixer.mix_f32(&mut out));
        mixer.push(Arc::from([0.5f32, 0.5]), 1.0);
        mixer.clear();
        assert!(!mixer.mix_f32(&mut out));
    }

    #[test]
    fn resolves_xdg_sound_theme_with_inheritance_and_fallback() {
        let root = env::temp_dir().join(format!("waywi-sound-theme-{}", std::process::id()));
        let _ = fs::remove_dir_all(&root);
        let child = root.join("CustomTheme");
        let parent_dir = root.join("freedesktop/stereo");
        fs::create_dir_all(&child).unwrap();
        fs::create_dir_all(&parent_dir).unwrap();
        fs::write(
            child.join("index.theme"),
            "[Sound Theme]\nInherits=freedesktop\n",
        )
        .unwrap();
        let wav_path = parent_dir.join("message-new.wav");
        fs::write(&wav_path, b"RIFF....WAVE").unwrap();

        let resolved = resolve_sound_theme_path_in(
            "message-new-instant",
            Some("CustomTheme"),
            std::slice::from_ref(&root),
        );
        assert_eq!(resolved, Some(wav_path));

        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn synthesizes_presets_cleanly() {
        let click = SoundData::from_preset(SoundPreset::Click);
        assert_eq!(click.channels, 2);
        assert!(!click.samples.is_empty());
        let notif = SoundData::from_preset(SoundPreset::Notification);
        assert!(notif.duration() >= Duration::from_millis(200));
    }
}
