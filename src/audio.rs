//! Playback engine.
//!
//! Decoding goes through Symphonia (pure Rust: MP3, FLAC, WAV, OGG/Vorbis, AAC/M4A, ALAC,
//! AIFF, CAF, MKV/WebM, ADPCM...). Anything Symphonia can't open is handed to `ffmpeg` when it
//! is on PATH (Opus, WMA, APE, WavPack, DSD, AC3, ...). Output and resampling are rodio/cpal.

use std::collections::VecDeque;
use std::fs::File;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::Duration;

use rodio::{OutputStream, OutputStreamHandle, Sink, Source};
use symphonia::core::audio::SampleBuffer;
use symphonia::core::codecs::{Decoder, DecoderOptions, CODEC_TYPE_NULL};
use symphonia::core::errors::Error as SymError;
use symphonia::core::formats::{FormatOptions, FormatReader, SeekMode, SeekTo};
use symphonia::core::io::MediaSourceStream;
use symphonia::core::meta::MetadataOptions;
use symphonia::core::probe::Hint;
use symphonia::core::units::Time;

/// How many mono samples the visualiser tap keeps around.
const TAP_LEN: usize = 4096;

pub const AUDIO_EXTENSIONS: &[&str] = &[
    "mp3", "mp2", "mp1", "flac", "wav", "wave", "w64", "ogg", "oga", "opus", "spx", "m4a", "m4b",
    "mp4", "aac", "adts", "alac", "aif", "aiff", "aifc", "caf", "mka", "mkv", "webm", "wv", "ape",
    "wma", "ac3", "eac3", "dts", "mpc", "tta", "dsf", "dff", "amr", "au", "snd", "voc", "3gp",
];

pub fn is_audio_file(p: &Path) -> bool {
    p.extension()
        .and_then(|e| e.to_str())
        .map(|e| AUDIO_EXTENSIONS.contains(&e.to_ascii_lowercase().as_str()))
        .unwrap_or(false)
}

pub fn ffmpeg_available() -> bool {
    static AVAILABLE: OnceLock<bool> = OnceLock::new();
    *AVAILABLE.get_or_init(|| {
        hidden_command("ffmpeg")
            .arg("-version")
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()
            .map(|s| s.success())
            .unwrap_or(false)
    })
}

/// A `Command` that doesn't flash a console window on Windows. A copy of `program` sitting
/// next to our own executable wins over PATH, so a portable folder can carry its own ffmpeg.
pub fn hidden_command(program: &str) -> Command {
    let bundled = std::env::current_exe()
        .ok()
        .and_then(|exe| exe.parent().map(|d| d.join(program).with_extension(std::env::consts::EXE_EXTENSION)))
        .filter(|p| p.is_file());
    #[allow(unused_mut)]
    let mut cmd = match bundled {
        Some(p) => Command::new(p),
        None => Command::new(program),
    };
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        const CREATE_NO_WINDOW: u32 = 0x0800_0000;
        cmd.creation_flags(CREATE_NO_WINDOW);
    }
    cmd
}

/// Ask ffprobe for a duration, for files only ffmpeg understands.
pub fn ffprobe_duration(path: &Path) -> Option<f64> {
    let out = hidden_command("ffprobe")
        .args(["-v", "error", "-show_entries", "format=duration", "-of", "csv=p=0"])
        .arg(path)
        .stderr(Stdio::null())
        .output()
        .ok()?;
    String::from_utf8_lossy(&out.stdout).trim().parse().ok()
}

// ---------------------------------------------------------------------------------------------
// Decoder backends

trait Backend: Send {
    fn next_chunk(&mut self) -> Option<Vec<f32>>;
}

struct SymphoniaBackend {
    format: Box<dyn FormatReader>,
    decoder: Box<dyn Decoder>,
    track_id: u32,
    channels: u16,
    sample_buf: Option<(u64, SampleBuffer<f32>)>,
}

impl SymphoniaBackend {
    /// Opens the file and decodes up to the first chunk so channel count and rate are known.
    fn open(path: &Path, start: f64) -> Result<(Self, Vec<f32>, u16, u32), String> {
        let file = File::open(path).map_err(|e| e.to_string())?;
        let mss = MediaSourceStream::new(Box::new(file), Default::default());
        let mut hint = Hint::new();
        if let Some(ext) = path.extension().and_then(|e| e.to_str()) {
            hint.with_extension(ext);
        }
        let probed = symphonia::default::get_probe()
            .format(
                &hint,
                mss,
                &FormatOptions { enable_gapless: true, ..Default::default() },
                &MetadataOptions::default(),
            )
            .map_err(|e| e.to_string())?;
        let mut format = probed.format;
        let track = format
            .tracks()
            .iter()
            .find(|t| t.codec_params.codec != CODEC_TYPE_NULL)
            .ok_or("no audio track")?;
        let track_id = track.id;
        let decoder = symphonia::default::get_codecs()
            .make(&track.codec_params, &DecoderOptions::default())
            .map_err(|e| e.to_string())?;

        if start > 0.0 {
            // Not every format can seek; playing from the top is a reasonable fallback.
            let _ = format.seek(
                SeekMode::Coarse,
                SeekTo::Time { time: Time::from(start), track_id: Some(track_id) },
            );
        }

        let mut backend =
            SymphoniaBackend { format, decoder, track_id, channels: 0, sample_buf: None };
        backend.decoder.reset();
        let (first, channels, rate) = backend.decode_next().ok_or("no decodable audio")?;
        backend.channels = channels;
        Ok((backend, first, channels, rate))
    }

    fn decode_next(&mut self) -> Option<(Vec<f32>, u16, u32)> {
        loop {
            let packet = match self.format.next_packet() {
                Ok(p) => p,
                Err(_) => return None, // end of stream or unrecoverable I/O
            };
            if packet.track_id() != self.track_id {
                continue;
            }
            match self.decoder.decode(&packet) {
                Ok(decoded) => {
                    let spec = *decoded.spec();
                    let frames = decoded.capacity() as u64;
                    if frames == 0 {
                        continue;
                    }
                    let needs_new = match &self.sample_buf {
                        Some((cap, _)) => *cap < frames,
                        None => true,
                    };
                    if needs_new {
                        self.sample_buf = Some((frames, SampleBuffer::new(frames, spec)));
                    }
                    let (_, sb) = self.sample_buf.as_mut().unwrap();
                    sb.copy_interleaved_ref(decoded);
                    if sb.samples().is_empty() {
                        continue;
                    }
                    return Some((sb.samples().to_vec(), spec.channels.count() as u16, spec.rate));
                }
                Err(SymError::DecodeError(_)) => continue, // skip corrupt packet
                Err(_) => return None,
            }
        }
    }
}

impl Backend for SymphoniaBackend {
    fn next_chunk(&mut self) -> Option<Vec<f32>> {
        let (samples, ch, _) = self.decode_next()?;
        Some(remix(samples, ch, self.channels))
    }
}

/// Some streams change channel count mid-way; rodio needs it constant.
fn remix(samples: Vec<f32>, from: u16, to: u16) -> Vec<f32> {
    if from == to || from == 0 {
        return samples;
    }
    let (from, to) = (from as usize, to as usize);
    let mut out = Vec::with_capacity(samples.len() / from * to);
    for frame in samples.chunks(from) {
        for c in 0..to {
            out.push(frame[c.min(frame.len() - 1)]);
        }
    }
    out
}

const FFMPEG_RATE: u32 = 48_000;

struct FfmpegBackend {
    child: Child,
    leftover: Vec<u8>,
}

impl FfmpegBackend {
    fn open(path: &Path, start: f64) -> Result<Self, String> {
        let child = hidden_command("ffmpeg")
            .args(["-nostdin", "-v", "error", "-ss", &format!("{start:.3}"), "-i"])
            .arg(path)
            .args(["-vn", "-f", "f32le", "-ac", "2", "-ar", &FFMPEG_RATE.to_string(), "-"])
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .map_err(|e| format!("ffmpeg: {e}"))?;
        Ok(FfmpegBackend { child, leftover: Vec::new() })
    }
}

impl Backend for FfmpegBackend {
    fn next_chunk(&mut self) -> Option<Vec<f32>> {
        let stdout = self.child.stdout.as_mut()?;
        let mut buf = vec![0u8; 16 * 1024];
        let n = stdout.read(&mut buf).ok()?;
        if n == 0 {
            return None;
        }
        self.leftover.extend_from_slice(&buf[..n]);
        let usable = self.leftover.len() / 4 * 4;
        let samples = self.leftover[..usable]
            .chunks_exact(4)
            .map(|b| f32::from_le_bytes([b[0], b[1], b[2], b[3]]))
            .collect();
        self.leftover.drain(..usable);
        Some(samples)
    }
}

impl Drop for FfmpegBackend {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

type Opened = (Box<dyn Backend>, Vec<f32>, u16, u32, bool);

/// Symphonia first; ffmpeg for whatever it can't handle. Returns
/// (backend, first decoded chunk, channels, sample rate, via ffmpeg).
fn open_backend(path: &Path, start: f64) -> Result<Opened, String> {
    match SymphoniaBackend::open(path, start) {
        Ok((b, first, ch, rate)) => Ok((Box::new(b), first, ch, rate, false)),
        Err(sym_err) => {
            if !ffmpeg_available() {
                return Err(format!(
                    "Can't decode {} ({sym_err}). Install ffmpeg for more formats.",
                    path.display()
                ));
            }
            let b = FfmpegBackend::open(path, start)?;
            Ok((Box::new(b), Vec::new(), 2, FFMPEG_RATE, true))
        }
    }
}

// ---------------------------------------------------------------------------------------------
// rodio source

/// Per-track playback state shared between the audio thread and the UI.
struct TrackState {
    frames_played: AtomicU64,
    sample_rate: AtomicU32,
    finished: AtomicBool,
}

struct StreamSource {
    backend: Box<dyn Backend>,
    buf: Vec<f32>,
    idx: usize,
    channels: u16,
    rate: u32,
    state: Arc<TrackState>,
    tap: Arc<Mutex<VecDeque<f32>>>,
    tap_pending: Vec<f32>,
    frame_acc: f32,
    frame_pos: u16,
}

impl Iterator for StreamSource {
    type Item = f32;

    fn next(&mut self) -> Option<f32> {
        while self.idx >= self.buf.len() {
            match self.backend.next_chunk() {
                Some(chunk) => {
                    self.buf = chunk;
                    self.idx = 0;
                }
                None => {
                    self.state.finished.store(true, Ordering::Release);
                    return None;
                }
            }
        }
        let s = self.buf[self.idx];
        self.idx += 1;

        self.frame_acc += s;
        self.frame_pos += 1;
        if self.frame_pos == self.channels {
            self.tap_pending.push(self.frame_acc / self.channels as f32);
            self.frame_acc = 0.0;
            self.frame_pos = 0;
            self.state.frames_played.fetch_add(1, Ordering::Relaxed);
            if self.tap_pending.len() >= 256 {
                if let Ok(mut tap) = self.tap.lock() {
                    tap.extend(self.tap_pending.drain(..));
                    let excess = tap.len().saturating_sub(TAP_LEN);
                    tap.drain(..excess);
                }
                self.tap_pending.clear();
            }
        }
        Some(s)
    }
}

impl Source for StreamSource {
    fn current_frame_len(&self) -> Option<usize> {
        None
    }
    fn channels(&self) -> u16 {
        self.channels
    }
    fn sample_rate(&self) -> u32 {
        self.rate
    }
    fn total_duration(&self) -> Option<Duration> {
        None
    }
}

// ---------------------------------------------------------------------------------------------
// Player

#[derive(Clone, Copy, Debug, Default)]
pub struct StreamInfo {
    pub channels: u16,
    pub sample_rate: u32,
    pub via_ffmpeg: bool,
}

pub struct Player {
    // Field order matters: the sink must drop before the stream.
    sink: Option<Sink>,
    handle: Option<OutputStreamHandle>,
    _stream: Option<OutputStream>,
    state: Arc<TrackState>,
    tap: Arc<Mutex<VecDeque<f32>>>,
    current: Option<PathBuf>,
    offset: f64,
    volume: f32,
    paused: bool,
    pub info: StreamInfo,
    pub last_error: Option<String>,
}

impl Player {
    pub fn new() -> Self {
        let (stream, handle, err) = match OutputStream::try_default() {
            Ok((s, h)) => (Some(s), Some(h), None),
            Err(e) => (None, None, Some(format!("No audio output device: {e}"))),
        };
        Player {
            sink: None,
            handle,
            _stream: stream,
            state: Arc::new(TrackState {
                frames_played: AtomicU64::new(0),
                sample_rate: AtomicU32::new(44_100),
                finished: AtomicBool::new(false),
            }),
            tap: Arc::new(Mutex::new(VecDeque::with_capacity(TAP_LEN))),
            current: None,
            offset: 0.0,
            volume: 0.05,
            paused: false,
            info: StreamInfo::default(),
            last_error: err,
        }
    }

    pub fn load(&mut self, path: &Path, start: f64, paused: bool) -> Result<(), String> {
        self.stop();
        let handle = self.handle.as_ref().ok_or("no audio output device")?;

        let (backend, first, channels, rate, via_ffmpeg) = open_backend(path, start)?;

        let state = Arc::new(TrackState {
            frames_played: AtomicU64::new(0),
            sample_rate: AtomicU32::new(rate),
            finished: AtomicBool::new(false),
        });
        let source = StreamSource {
            backend,
            buf: first,
            idx: 0,
            channels: channels.max(1),
            rate,
            state: state.clone(),
            tap: self.tap.clone(),
            tap_pending: Vec::with_capacity(256),
            frame_acc: 0.0,
            frame_pos: 0,
        };

        let sink = Sink::try_new(handle).map_err(|e| e.to_string())?;
        sink.set_volume(self.volume);
        if paused {
            sink.pause();
        }
        sink.append(source);

        self.sink = Some(sink);
        self.state = state;
        self.current = Some(path.to_path_buf());
        self.offset = start.max(0.0);
        self.paused = paused;
        self.info = StreamInfo { channels, sample_rate: rate, via_ffmpeg };
        self.last_error = None;
        Ok(())
    }

    pub fn stop(&mut self) {
        if let Some(sink) = self.sink.take() {
            sink.stop();
        }
        self.current = None;
        self.offset = 0.0;
        self.paused = false;
        if let Ok(mut t) = self.tap.lock() {
            t.clear();
        }
    }

    pub fn seek(&mut self, to: f64) {
        if let Some(path) = self.current.clone() {
            if let Err(e) = self.load(&path, to.max(0.0), self.paused) {
                self.last_error = Some(e);
            }
        }
    }

    pub fn set_paused(&mut self, paused: bool) {
        if let Some(sink) = &self.sink {
            if paused {
                sink.pause()
            } else {
                sink.play()
            }
            self.paused = paused;
        }
    }

    pub fn set_volume(&mut self, v: f32) {
        self.volume = v.clamp(0.0, 1.0);
        if let Some(sink) = &self.sink {
            sink.set_volume(self.volume);
        }
    }

    pub fn volume(&self) -> f32 {
        self.volume
    }

    pub fn is_loaded(&self) -> bool {
        self.sink.is_some()
    }

    pub fn is_playing(&self) -> bool {
        self.sink.is_some() && !self.paused
    }

    pub fn is_paused(&self) -> bool {
        self.sink.is_some() && self.paused
    }

    /// True once the decoder ran dry and the output drained everything it was given.
    pub fn track_finished(&self) -> bool {
        match &self.sink {
            Some(sink) => self.state.finished.load(Ordering::Acquire) && sink.empty(),
            None => false,
        }
    }

    pub fn position(&self) -> f64 {
        if self.sink.is_none() {
            return 0.0;
        }
        let frames = self.state.frames_played.load(Ordering::Relaxed) as f64;
        let rate = self.state.sample_rate.load(Ordering::Relaxed).max(1) as f64;
        self.offset + frames / rate
    }

    /// The most recent mono samples heard, for visualisers.
    pub fn recent_samples(&self, n: usize) -> Vec<f32> {
        let tap = match self.tap.lock() {
            Ok(t) => t,
            Err(_) => return Vec::new(),
        };
        let skip = tap.len().saturating_sub(n);
        tap.iter().skip(skip).copied().collect()
    }
}

// ---------------------------------------------------------------------------------------------
// Spectrum

/// In-place radix-2 FFT. `re.len()` must be a power of two.
fn fft(re: &mut [f32], im: &mut [f32]) {
    let n = re.len();
    let mut j = 0;
    for i in 1..n {
        let mut bit = n >> 1;
        while j & bit != 0 {
            j ^= bit;
            bit >>= 1;
        }
        j |= bit;
        if i < j {
            re.swap(i, j);
            im.swap(i, j);
        }
    }
    let mut len = 2;
    while len <= n {
        let ang = -2.0 * std::f32::consts::PI / len as f32;
        let (wr, wi) = (ang.cos(), ang.sin());
        for start in (0..n).step_by(len) {
            let (mut cr, mut ci) = (1.0f32, 0.0f32);
            for k in 0..len / 2 {
                let (a, b) = (start + k, start + k + len / 2);
                let tr = re[b] * cr - im[b] * ci;
                let ti = re[b] * ci + im[b] * cr;
                re[b] = re[a] - tr;
                im[b] = im[a] - ti;
                re[a] += tr;
                im[a] += ti;
                let ncr = cr * wr - ci * wi;
                ci = cr * wi + ci * wr;
                cr = ncr;
            }
        }
        len <<= 1;
    }
}

/// Log-spaced band levels in 0..=1 from the latest 1024 samples.
pub fn spectrum(samples: &[f32], bands: usize, sample_rate: u32) -> Vec<f32> {
    const N: usize = 1024;
    if samples.len() < N || bands == 0 {
        return vec![0.0; bands];
    }
    let src = &samples[samples.len() - N..];
    let mut re: Vec<f32> = src
        .iter()
        .enumerate()
        .map(|(i, s)| {
            let w = 0.5 - 0.5 * (2.0 * std::f32::consts::PI * i as f32 / (N - 1) as f32).cos();
            s * w
        })
        .collect();
    let mut im = vec![0.0; N];
    fft(&mut re, &mut im);

    let bin_hz = sample_rate as f32 / N as f32;
    let (lo, hi) = (50.0f32, 16_000.0f32.min(sample_rate as f32 / 2.0));
    (0..bands)
        .map(|b| {
            let f0 = lo * (hi / lo).powf(b as f32 / bands as f32);
            let f1 = lo * (hi / lo).powf((b + 1) as f32 / bands as f32);
            let i0 = ((f0 / bin_hz) as usize).clamp(1, N / 2 - 1);
            let i1 = ((f1 / bin_hz) as usize).clamp(i0 + 1, N / 2);
            let peak = (i0..i1)
                .map(|i| (re[i] * re[i] + im[i] * im[i]).sqrt())
                .fold(0.0f32, f32::max);
            // ~60 dB range mapped to 0..1, with a gentle tilt so highs aren't always flat.
            let db = 20.0 * (peak / (N as f32 / 4.0) + 1e-9).log10() + 4.0 * b as f32 / bands as f32 * 3.0;
            ((db + 60.0) / 60.0).clamp(0.0, 1.0)
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn spectrum_finds_a_tone() {
        let rate = 44_100;
        let tone: Vec<f32> = (0..2048).map(|i| (i as f32 * 2.0 * std::f32::consts::PI * 1000.0 / rate as f32).sin()).collect();
        let bands = spectrum(&tone, 16, rate);
        let loudest = bands.iter().enumerate().max_by(|a, b| a.1.total_cmp(b.1)).unwrap().0;
        // 1 kHz sits around the middle of a 50 Hz..16 kHz log scale
        assert!((6..=10).contains(&loudest), "loudest band {loudest}: {bands:?}");
    }

    #[test]
    fn remix_mono_to_stereo() {
        assert_eq!(remix(vec![0.1, 0.2], 1, 2), vec![0.1, 0.1, 0.2, 0.2]);
    }

    /// Decodes a second of every audio file in $JAMP_TEST_MUSIC: `cargo test -- --ignored`
    #[test]
    #[ignore]
    fn decodes_every_file_in_test_dir() {
        let dir = std::env::var("JAMP_TEST_MUSIC").expect("set JAMP_TEST_MUSIC");
        let mut n = 0;
        for e in std::fs::read_dir(dir).unwrap().flatten() {
            let path = e.path();
            if !is_audio_file(&path) {
                continue;
            }
            let (mut b, first, ch, rate, ff) = open_backend(&path, 30.0).unwrap_or_else(|e| panic!("{e}"));
            let mut got = first.len();
            while got < (rate as usize) * ch as usize {
                got += b.next_chunk().expect("stream ended early").len();
            }
            println!("{}: {ch} ch @ {rate} Hz{}", path.display(), if ff { " (ffmpeg)" } else { "" });
            n += 1;
        }
        assert!(n > 0);
    }
}
