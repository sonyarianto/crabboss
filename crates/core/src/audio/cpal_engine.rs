//! cpal audio engine: stereo symphonia decode → rubato resample to the
//! device rate → dual-cursor equal-power/linear crossfade through `Mixer`
//! in the callback, with 12-band EQ insert and limiter on the program bus.
//! Mic/line-in (§1.6) sums into the program bus ahead of the limiter +
//! stream tap with voice-activated ducking of the music bed.

use std::collections::VecDeque;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{mpsc, Arc, Mutex};

use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use rtrb::RingBuffer;

use crate::audio::engine::{Engine, PlayerState, TrackInfo};
use crate::audio::mic::{MicConfig, MicResampler, MicState, MIC_RING_SAMPLES};
use crate::audio::mixer::{Frame, Mixer, EQ_BAND_COUNT};
use crate::audio::silence::SilenceMonitor;
use crate::audio::MAX_GAIN_DB;
use crate::error::{CrabError, Result};

/// Decoded track: stereo-interleaved f32 at device rate.
struct PlaybackCursor {
    samples: Vec<f32>,
    /// Position in frames (one frame = L+R).
    pos_frames: usize,
}

impl PlaybackCursor {
    fn next_stereo(&mut self) -> Option<(f32, f32)> {
        let i = self.pos_frames * 2;
        if i + 1 >= self.samples.len() {
            return None;
        }
        self.pos_frames += 1;
        Some((self.samples[i], self.samples[i + 1]))
    }

    fn remaining_frames(&self) -> usize {
        self.samples.len() / 2 - self.pos_frames.min(self.samples.len() / 2)
    }

    fn is_done(&self) -> bool {
        self.remaining_frames() == 0
    }
}

/// Dual-cursor crossfade state, owned by the audio callback.
struct XfadeState {
    current: Option<PlaybackCursor>,
    /// Pending decks in play order (queue primitive: ad chains, auto-DJ).
    next: VecDeque<PlaybackCursor>,
    /// Frames elapsed in the active blend; `len == 0` means no blend.
    pos: usize,
    len: usize,
    /// End-of-track auto-blend length for queued decks (`0` = hard takeover).
    auto_len: usize,
}

impl XfadeState {
    /// Pull one frame: `(current, incoming, blend 0..1)`.
    /// `None` = nothing left to play.
    fn pull(&mut self) -> Option<(Frame, Option<Frame>, f32)> {
        // Promote when the current deck is exhausted.
        if self.current.as_ref().is_none_or(|c| c.is_done()) {
            let n = self.next.pop_front()?;
            self.current = Some(n);
            self.pos = 0;
            self.len = 0;
        }
        let cur = self.current.as_mut().unwrap();
        let (l, r) = match cur.next_stereo() {
            Some(v) => v,
            None => {
                // Hit EOF exactly on this pull: hand over to next if any.
                let n = self.next.pop_front()?;
                self.current = Some(n);
                self.pos = 0;
                self.len = 0;
                match self.current.as_mut().unwrap().next_stereo() {
                    Some((l, r)) => (l, r),
                    None => return None,
                }
            }
        };
        let a = Frame { l, r };
        // Queued deck waiting and current nearly done: ease into the blend.
        if self.len == 0 && self.auto_len > 0 {
            if let (Some(cur), Some(nxt)) = (self.current.as_ref(), self.next.front()) {
                let remaining = cur.remaining_frames();
                if remaining > 0 && remaining <= self.auto_len && nxt.remaining_frames() > 0 {
                    self.len = self
                        .auto_len
                        .min(remaining)
                        .min(nxt.remaining_frames())
                        .max(1);
                    self.pos = 0;
                }
            }
        }
        if self.len > 0 {
            if let Some(nxt) = self.next.front_mut() {
                match nxt.next_stereo() {
                    Some((l, r)) => {
                        let x = (self.pos as f32 / self.len as f32).min(1.0);
                        self.pos += 1;
                        if self.pos >= self.len {
                            // Blend finished: incoming deck takes over.
                            self.current = self.next.pop_front();
                            self.pos = 0;
                            self.len = 0;
                        }
                        return Some((a, Some(Frame { l, r }), x));
                    }
                    // Front clip exhausted mid-blend: drop it, keep current.
                    None => {
                        self.next.pop_front();
                        self.pos = 0;
                        self.len = 0;
                    }
                }
            } else {
                self.len = 0;
            }
        }
        Some((a, None, 0.0))
    }

    fn is_done(&self) -> bool {
        self.current.as_ref().is_none_or(|c| c.is_done()) && self.next.is_empty()
    }
}

/// One decode unit for the background loader thread. A single FIFO
/// worker executes these in submit order, so chained sequences
/// (ad intro → spot → outro, Auto-DJ prefetch → next pick) keep playing
/// in the order the UI requested them.
enum LoadJob {
    /// Start `path` now-ish (crossfade when `was_live` was true at submit).
    /// Carries a generation: a later `play`/`stop` supersedes it.
    Play {
        path: PathBuf,
        gen: u64,
        was_live: bool,
        gain_db: f32,
    },
    /// Append `path` behind the live deck (insert-after-current).
    /// Keeps its submit-time generation: a later `play`/`stop` discards it.
    Queue {
        path: PathBuf,
        gen: u64,
        gain_db: f32,
    },
}

impl LoadJob {
    fn gen(&self) -> u64 {
        match self {
            LoadJob::Play { gen, .. } => *gen,
            LoadJob::Queue { gen, .. } => *gen,
        }
    }
}

/// Transport handles shared with the loader thread. Everything is
/// lock-protected and installed sequentially (never nested), matching the
/// audio callback's lock order — the engine itself stays `!Send` behind
/// `Rc` in the UI, only these `Arc`s cross threads.
#[derive(Clone)]
struct LoaderShared {
    xfade: Arc<Mutex<XfadeState>>,
    state: Arc<Mutex<PlayerState>>,
    current_track: Arc<Mutex<Option<TrackInfo>>>,
    silence: Arc<Mutex<SilenceMonitor>>,
    crossfade_secs: Arc<Mutex<f32>>,
    load_gen: Arc<AtomicU64>,
    /// True while a `play` decode is in flight — the resume target when a
    /// pause lands mid-load (back to `Buffering`, so the landing deck still
    /// advances instead of stranding the transport in `Paused`).
    loading: Arc<AtomicBool>,
    device_rate: u32,
}

impl LoaderShared {
    fn xfade_secs(&self) -> f32 {
        *self.crossfade_secs.lock().unwrap()
    }

    /// Install a decoded `play` job (mirrors the old synchronous branches:
    /// crossfade when something was live at submit, hard switch otherwise).
    /// A pause pressed mid-load wins: only `Buffering` auto-advances.
    fn install_play(
        &self,
        path: PathBuf,
        gen: u64,
        cursor: PlaybackCursor,
        duration: Option<f64>,
        was_live: bool,
    ) {
        let mut xf = self.xfade.lock().unwrap();
        let secs = self.xfade_secs();
        xf.auto_len = (secs * self.device_rate as f32) as usize;
        if was_live {
            let remaining = xf
                .current
                .as_ref()
                .map(|c| c.remaining_frames())
                .unwrap_or(0);
            let want = (secs * self.device_rate as f32) as usize;
            xf.len = want.min(remaining).min(cursor.remaining_frames()).max(1);
            xf.pos = 0;
            xf.next = VecDeque::from([cursor]);
            tracing::info!(
                "CpalEngine crossfading ({} frames): {}",
                xf.len,
                path.display()
            );
        } else {
            xf.current = Some(cursor);
            xf.next.clear();
            xf.pos = 0;
            xf.len = 0;
            tracing::info!("CpalEngine playing: {}", path.display());
        }
        drop(xf);
        if self.load_gen.load(Ordering::SeqCst) == gen {
            self.loading.store(false, Ordering::SeqCst);
        }
        self.silence.lock().unwrap().reset();
        *self.current_track.lock().unwrap() = Some(TrackInfo {
            path,
            title: None,
            artist: None,
            duration_secs: duration,
        });
        let mut st = self.state.lock().unwrap();
        // A pause pressed mid-load wins (stays `Paused` for an explicit
        // resume); anything else advances to `Playing` — including a live
        // handoff whose old deck ran out while the new one was decoding.
        if *st != PlayerState::Paused {
            *st = PlayerState::Playing;
        }
    }

    /// Install a decoded `queue` job (same end-of-track semantics as the
    /// old synchronous `queue_file`: append behind live audio, take over
    /// directly when idle).
    fn install_queue(&self, path: PathBuf, cursor: PlaybackCursor, duration: Option<f64>) {
        let label = path.display().to_string();
        let mut xf = self.xfade.lock().unwrap();
        xf.auto_len = (self.xfade_secs() * self.device_rate as f32) as usize;
        if xf.current.as_ref().is_some_and(|c| !c.is_done()) || !xf.next.is_empty() {
            xf.next.push_back(cursor);
            tracing::info!("CpalEngine queued: {}", label);
        } else {
            xf.current = Some(cursor);
            xf.pos = 0;
            xf.len = 0;
        }
        drop(xf);
        self.silence.lock().unwrap().reset();
        *self.state.lock().unwrap() = PlayerState::Playing;
        *self.current_track.lock().unwrap() = Some(TrackInfo {
            path,
            title: None,
            artist: None,
            duration_secs: duration,
        });
    }

    /// A decode failed: park the transport back to `Stopped` (clearing the
    /// announced track) unless something newer already took over.
    fn fail_load(&self, path: &Path, gen: u64, was_live: bool, err: &str) {
        tracing::warn!("Load failed for {}: {}", path.display(), err);
        if self.load_gen.load(Ordering::SeqCst) != gen {
            return;
        }
        self.loading.store(false, Ordering::SeqCst);
        // A failed live handoff keeps the old deck (its label was retained
        // at submit) — only an idle load parks the transport to `Stopped`.
        if was_live {
            return;
        }
        let mut st = self.state.lock().unwrap();
        if *st == PlayerState::Buffering {
            *st = PlayerState::Stopped;
            *self.current_track.lock().unwrap() = None;
        }
    }
}

/// Worker loop: decode jobs FIFO, skipping anything superseded while
/// queued and discarding anything superseded mid-decode — stale audio is
/// never installed over a newer request. Returns false when the thread
/// failed to spawn (callers fall back to synchronous decode).
fn spawn_loader(shared: LoaderShared, rx: mpsc::Receiver<LoadJob>) -> bool {
    let res = std::thread::Builder::new()
        .name("crabboss-loader".into())
        .spawn(move || {
            while let Ok(job) = rx.recv() {
                if job.gen() != shared.load_gen.load(Ordering::SeqCst) {
                    continue;
                }
                let (path, gain_db) = match &job {
                    LoadJob::Play { path, gain_db, .. } => (path.clone(), *gain_db),
                    LoadJob::Queue { path, gain_db, .. } => (path.clone(), *gain_db),
                };
                match decode_load(&path, gain_db, shared.device_rate) {
                    Ok((cursor, duration)) => {
                        let gen = job.gen();
                        if gen != shared.load_gen.load(Ordering::SeqCst) {
                            continue;
                        }
                        match job {
                            LoadJob::Play { was_live, .. } => {
                                shared.install_play(path, gen, cursor, duration, was_live);
                            }
                            LoadJob::Queue { .. } => {
                                shared.install_queue(path, cursor, duration);
                            }
                        }
                    }
                    Err(e) => {
                        let (gen, was_live) = match &job {
                            LoadJob::Play { gen, was_live, .. } => (*gen, *was_live),
                            // Queues never own the transport state — a failed
                            // queue must not park it.
                            LoadJob::Queue { gen, .. } => (*gen, true),
                        };
                        shared.fail_load(&path, gen, was_live, &e.to_string());
                    }
                }
            }
        });
    match res {
        Ok(_) => true,
        Err(e) => {
            tracing::error!("Failed to spawn decode loader thread: {e}");
            false
        }
    }
}

/// Decode + normalize + resample one file for the loader thread.
/// Pure function of (path, gain, rate): safe to run off the UI thread.
fn decode_load(
    path: &Path,
    gain_db: f32,
    device_rate: u32,
) -> Result<(PlaybackCursor, Option<f64>)> {
    let (mut samples, file_rate) = decode_to_stereo(path)?;
    if gain_db != 0.0 {
        let g = 10f32.powf(gain_db / 20.0);
        for s in samples.iter_mut() {
            *s *= g;
        }
        tracing::debug!(
            "Loudness gain {gain_db:+.1} dB applied to {}",
            path.display()
        );
    }
    let samples = if file_rate != device_rate {
        tracing::info!("Resampling {} Hz → {} Hz", file_rate, device_rate);
        resample_stereo(samples, file_rate, device_rate)?
    } else {
        samples
    };
    let duration = read_duration(path);
    Ok((
        PlaybackCursor {
            samples,
            pos_frames: 0,
        },
        duration,
    ))
}

fn read_duration(path: &Path) -> Option<f64> {
    lofty::read_from_path(path).ok().map(|f| {
        use lofty::file::AudioFile;
        f.properties().duration().as_secs_f64()
    })
}

/// Low-level engine. Keeps the cpal `Stream` alive; callback pulls
/// decoded stereo samples through `Mixer`.
pub struct CpalEngine {
    _stream: Option<cpal::Stream>,
    device_rate: u32,
    device_name: String,
    xfade: Arc<Mutex<XfadeState>>,
    crossfade_secs: Arc<Mutex<f32>>,
    silence: Arc<Mutex<SilenceMonitor>>,
    state: Arc<Mutex<PlayerState>>,
    current_track: Arc<Mutex<Option<TrackInfo>>>,
    volume: Arc<Mutex<f32>>,
    mixer: Arc<Mutex<Mixer>>,
    /// Loudness normalization: enabled flag + per-path gain lookup (dB).
    loudness_lookup: Option<crate::audio::engine::LoudnessLookup>,
    loudness_enabled: std::cell::Cell<bool>,
    /// Icecast streaming: manager + live tap for the audio callback.
    stream: Arc<Mutex<crate::stream::StreamManager>>,
    stream_tap: Arc<Mutex<Option<crate::stream::StreamTap>>>,
    /// Mic/line-in (§1.6): input stream (rebuilt on device change) +
    /// consumer swapped in/out by start/stop for the output callback.
    _mic_stream: Option<cpal::Stream>,
    mic_consumer: Arc<Mutex<Option<rtrb::Consumer<f32>>>>,
    mic_live: Arc<AtomicBool>,
    mic_config: Arc<Mutex<MicConfig>>,
    mic_state: Arc<Mutex<MicState>>,
    /// Background decode loader: `play`/`queue` only enqueue here and
    /// return instantly, so the UI thread never waits on full-file decode
    /// + sinc resample (seconds of frozen window on real songs).
    load_tx: mpsc::Sender<LoadJob>,
    /// Monotonic generation: every `play`/`stop` invalidates older jobs.
    load_gen: Arc<AtomicU64>,
    /// True while a `play` decode is in flight (resume target). See
    /// [`LoaderShared::loading`].
    loading: Arc<AtomicBool>,
    /// False only when the loader thread failed to spawn — `play`/`queue`
    /// then fall back to synchronous decode (blocks, but audio works).
    loader_ok: bool,
}

impl CpalEngine {
    /// Open default output. Never panics — falls back to headless
    /// (`_stream: None`, still tracks state) when no device exists.
    pub fn new() -> Self {
        Self::with_device(None)
    }

    /// Open a named output device (falls back to default with a warning
    /// when unplugged/missing, so a stale setting never kills audio).
    pub fn open_named(name: &str) -> Self {
        Self::with_device(Some(name.to_string()))
    }

    fn with_device(want: Option<String>) -> Self {
        let xfade: Arc<Mutex<XfadeState>> = Arc::new(Mutex::new(XfadeState {
            current: None,
            next: VecDeque::new(),
            pos: 0,
            len: 0,
            auto_len: 0,
        }));
        let state = Arc::new(Mutex::new(PlayerState::Stopped));
        let current_track = Arc::new(Mutex::new(None));
        let crossfade_secs = Arc::new(Mutex::new(3.0));
        let volume = Arc::new(Mutex::new(1.0));
        let mixer = Arc::new(Mutex::new(Mixer::default()));
        let silence = Arc::new(Mutex::new(SilenceMonitor::new(48000, 10.0)));
        let stream_tap: Arc<Mutex<Option<crate::stream::StreamTap>>> = Arc::new(Mutex::new(None));
        let mic_consumer: Arc<Mutex<Option<rtrb::Consumer<f32>>>> = Arc::new(Mutex::new(None));
        let mic_live = Arc::new(AtomicBool::new(false));
        let mic_config = Arc::new(Mutex::new(MicConfig::default()));
        let mic_state: Arc<Mutex<MicState>> = Arc::new(Mutex::new(MicState::Off));

        let (stream, device_rate, device_name) = match Self::open_silent_stream(
            xfade.clone(),
            state.clone(),
            volume.clone(),
            mixer.clone(),
            silence.clone(),
            stream_tap.clone(),
            mic_consumer.clone(),
            mic_live.clone(),
            mic_config.clone(),
            want,
        ) {
            Ok((s, rate, name)) => {
                tracing::info!("CpalEngine: output '{}' @ {} Hz", name, rate);
                *silence.lock().unwrap() = SilenceMonitor::new(rate, 10.0);
                mixer.lock().unwrap().set_eq_rate(rate);
                mixer.lock().unwrap().limiter.reset();
                (Some(s), rate, name)
            }
            Err(e) => {
                tracing::warn!("CpalEngine: no audio device ({}). Headless.", e);
                (None, 48000, "None (headless)".to_string())
            }
        };

        // Background decode loader (decoding needs no device — headless
        // engines and tests get one too).
        let load_gen = Arc::new(AtomicU64::new(0));
        let loading = Arc::new(AtomicBool::new(false));
        let (load_tx, load_rx) = mpsc::channel();
        let loader_ok = spawn_loader(
            LoaderShared {
                xfade: xfade.clone(),
                state: state.clone(),
                current_track: current_track.clone(),
                silence: silence.clone(),
                crossfade_secs: crossfade_secs.clone(),
                load_gen: load_gen.clone(),
                loading: loading.clone(),
                device_rate,
            },
            load_rx,
        );

        Self {
            _stream: stream,
            device_rate,
            device_name,
            xfade: xfade.clone(),
            crossfade_secs: crossfade_secs.clone(),
            silence: silence.clone(),
            state: state.clone(),
            current_track: current_track.clone(),
            volume,
            mixer,
            loudness_lookup: None,
            loudness_enabled: std::cell::Cell::new(false),
            stream: Arc::new(Mutex::new(crate::stream::StreamManager::new(
                crate::stream::StreamConfig::default(),
            ))),
            stream_tap: Arc::new(Mutex::new(None)),
            _mic_stream: None,
            mic_consumer,
            mic_live,
            mic_config,
            mic_state,
            load_tx,
            load_gen,
            loading,
            loader_ok,
        }
    }

    /// Transport handles for the loader thread / sync fallback.
    fn loader_shared(&self) -> LoaderShared {
        LoaderShared {
            xfade: self.xfade.clone(),
            state: self.state.clone(),
            current_track: self.current_track.clone(),
            silence: self.silence.clone(),
            crossfade_secs: self.crossfade_secs.clone(),
            load_gen: self.load_gen.clone(),
            loading: self.loading.clone(),
            device_rate: self.device_rate,
        }
    }

    pub fn device_rate(&self) -> u32 {
        self.device_rate
    }

    /// The device actually opened (may differ from the request on fallback).
    pub fn device_name(&self) -> String {
        self.device_name.clone()
    }

    /// Push level + ducking prefs into the mixer's ducker (live, no
    /// stream rebuild — the callback reads `mic_level` per invocation).
    fn apply_mic_dsp(&self) {
        let config = self.mic_config.lock().unwrap().clone();
        let mut mx = self.mixer.lock().unwrap();
        mx.ducker.set_enabled(config.duck_enabled);
        mx.ducker.configure(
            config.duck_threshold_db,
            config.duck_depth_db,
            config.attack_ms,
            config.release_ms,
        );
    }

    /// ReplayGain-style deck gain for a path (dB, clamped). Resolved on
    /// the caller thread at submit time so the loader stays dependency-free.
    fn gain_for(&self, path: &Path) -> f32 {
        if !self.loudness_enabled.get() {
            return 0.0;
        }
        self.loudness_lookup
            .as_ref()
            .and_then(|f| f(path))
            .unwrap_or(0.0)
            .clamp(-MAX_GAIN_DB, MAX_GAIN_DB)
    }

    /// Queue a file to start at the current deck's end (insert-after).
    /// Appends behind anything already pending. Async like `play`: the
    /// job decodes on the loader thread, so Auto-DJ prefetch on the UI
    /// tick never freezes the window either.
    pub fn queue_file(&self, path: &Path) -> Result<()> {
        if !path.exists() {
            return Err(CrabError::FileNotFound {
                path: path.to_path_buf(),
            });
        }
        let path = path.to_path_buf();
        let gain_db = self.gain_for(&path);
        if !self.loader_ok {
            // Degraded path: decode inline (blocks, but audio still works).
            let (cursor, duration) = decode_load(&path, gain_db, self.device_rate)?;
            self.loader_shared().install_queue(path, cursor, duration);
            return Ok(());
        }
        // Queues keep their submit-time generation: a later `play`/`stop`
        // supersedes them before they ever decode or install.
        let job = LoadJob::Queue {
            path,
            gen: self.load_gen.load(Ordering::SeqCst),
            gain_db,
        };
        self.load_tx
            .send(job)
            .map_err(|e| CrabError::Audio(format!("decode loader gone: {e}")))?;
        Ok(())
    }

    /// List output devices (for Settings screen later).
    pub fn list_output_devices() -> Vec<String> {
        cpal::default_host()
            .output_devices()
            .map(|devs| devs.filter_map(|d| d.name().ok()).collect::<Vec<_>>())
            .unwrap_or_default()
    }

    #[allow(clippy::too_many_arguments)]
    fn open_silent_stream(
        xfade: Arc<Mutex<XfadeState>>,
        state: Arc<Mutex<PlayerState>>,
        volume: Arc<Mutex<f32>>,
        mixer: Arc<Mutex<Mixer>>,
        silence: Arc<Mutex<SilenceMonitor>>,
        stream_tap: Arc<Mutex<Option<crate::stream::StreamTap>>>,
        mic_consumer: Arc<Mutex<Option<rtrb::Consumer<f32>>>>,
        mic_live: Arc<AtomicBool>,
        mic_config: Arc<Mutex<MicConfig>>,
        want: Option<String>,
    ) -> std::result::Result<(cpal::Stream, u32, String), String> {
        let host = cpal::default_host();
        let named = want.as_deref().and_then(|n| {
            host.output_devices()
                .ok()
                .and_then(|mut devs| devs.find(|d| d.name().is_ok_and(|dn| dn == n)))
        });
        if want.is_some() && named.is_none() {
            tracing::warn!(
                "Output device '{}' not found, falling back to default",
                want.as_deref().unwrap_or_default()
            );
        }
        let device = named
            .or_else(|| host.default_output_device())
            .ok_or_else(|| "no output device".to_string())?;
        let name = device.name().unwrap_or_else(|_| "Default".to_string());
        let config = device.default_output_config().map_err(|e| e.to_string())?;
        let sample_rate = config.sample_rate().0;
        let channels = config.channels() as usize;
        let stream_config: cpal::StreamConfig = config.into();

        let err_fn = |err| tracing::error!("cpal stream error: {}", err);
        let stream = device
            .build_output_stream(
                &stream_config,
                move |data: &mut [f32], _| {
                    let vol = *volume.lock().unwrap();
                    let mut mx = mixer.lock().unwrap();
                    let mut xf = xfade.lock().unwrap();
                    let mut sil = silence.lock().unwrap();
                    let playing = *state.lock().unwrap() == PlayerState::Playing;
                    // Program-bus tap (streaming): cloned once per callback.
                    let tap = stream_tap.lock().unwrap().clone();
                    let mut tap_buf = [0.0f32; 8192];
                    let mut tap_n = 0usize;
                    // Mic drain: locked once per callback, popped per frame.
                    // Input and output devices drift apart over hours, so
                    // bound the buffered latency — discard the oldest down
                    // to 1/4 ring when more than 1/2 ring is buffered.
                    let mic_on = mic_live.load(Ordering::Relaxed);
                    let mic_level = mic_config.lock().unwrap().level;
                    let mut mic_con = mic_consumer.lock().unwrap();
                    if mic_on {
                        if let Some(con) = mic_con.as_mut() {
                            let buffered = con.slots();
                            if buffered > MIC_RING_SAMPLES / 2 {
                                let mut drop_n = (buffered - MIC_RING_SAMPLES / 4) & !1;
                                while drop_n > 0 {
                                    if con.pop().is_err() {
                                        break;
                                    }
                                    drop_n -= 1;
                                }
                            }
                        }
                    }

                    for frame in data.chunks_mut(channels) {
                        let req = if playing { xf.pull() } else { None };
                        // Mic sums into the program bus ahead of the limiter
                        // + stream tap (voice goes out over the broadcast
                        // feed too), independent of transport state so talk
                        // breaks work over a silent bed.
                        let mic_frame = if mic_on {
                            mic_con.as_mut().and_then(|con| {
                                con.pop().ok().map(|l| {
                                    let r = con.pop().unwrap_or(l);
                                    Frame { l, r }
                                })
                            })
                        } else {
                            None
                        };
                        let (l, r) = match req {
                            Some((a, b, x)) => {
                                let f = mx.process_x_mic(Some(a), b, x, mic_frame, mic_level);
                                (f.l, f.r)
                            }
                            None => {
                                let f = mx.process_x_mic(None, None, 0.0, mic_frame, mic_level);
                                (f.l, f.r)
                            }
                        };
                        // Tap post-DSP, pre-monitor-volume: the broadcast
                        // feed carries full program level regardless of the
                        // operator's local listening volume.
                        if tap.is_some() {
                            if tap_n + 2 > tap_buf.len() {
                                if let Some(t) = &tap {
                                    t.push(&tap_buf[..tap_n]);
                                }
                                tap_n = 0;
                            }
                            tap_buf[tap_n] = l;
                            tap_buf[tap_n + 1] = r;
                            tap_n += 2;
                        }
                        let (l, r) = (l * vol, r * vol);
                        sil.push_frame(playing, l, r);
                        if channels == 1 {
                            frame[0] = (l + r) * 0.5;
                        } else {
                            frame[0] = l;
                            if channels > 1 {
                                frame[1] = r;
                            }
                            for s in frame.iter_mut().skip(2) {
                                *s = 0.0;
                            }
                        }
                    }
                    // Flush the tap buffer for this callback invocation.
                    if let Some(t) = &tap {
                        t.push(&tap_buf[..tap_n]);
                    }
                    // Auto-stop at EOF.
                    if playing && xf.is_done() {
                        *state.lock().unwrap() = PlayerState::Stopped;
                    }
                },
                err_fn,
                None,
            )
            .map_err(|e| e.to_string())?;
        stream.play().map_err(|e| e.to_string())?;
        Ok((stream, sample_rate, name))
    }

    /// List input devices (mic picker on the Settings screen).
    pub fn list_input_devices() -> Vec<String> {
        cpal::default_host()
            .input_devices()
            .map(|devs| devs.filter_map(|d| d.name().ok()).collect::<Vec<_>>())
            .unwrap_or_default()
    }

    /// Open the mic/line-in stream at (or resampled to) the output rate.
    /// The input callback converts channels → stereo, resamples, and
    /// pushes into `ring`; the output callback drains it. Returns the
    /// stream, the device name, and the native input rate.
    fn open_input_stream(
        want: Option<String>,
        device_rate: u32,
        ring: rtrb::Producer<f32>,
    ) -> std::result::Result<(cpal::Stream, String), String> {
        use cpal::SampleFormat;
        let host = cpal::default_host();
        let named = want.as_deref().and_then(|n| {
            host.input_devices()
                .ok()
                .and_then(|mut devs| devs.find(|d| d.name().is_ok_and(|dn| dn == n)))
        });
        if want.is_some() && named.is_none() {
            tracing::warn!(
                "Input device '{}' not found, falling back to default",
                want.as_deref().unwrap_or_default()
            );
        }
        let device = named
            .or_else(|| host.default_input_device())
            .ok_or_else(|| "no input device".to_string())?;
        let name = device.name().unwrap_or_else(|_| "Default".to_string());
        // Prefer an f32 config at the output rate (zero resampling);
        // otherwise take the default input config and resample in the
        // callback. Non-f32-only devices are rejected (rare on desktop).
        let supported = device
            .supported_input_configs()
            .map_err(|e| e.to_string())?
            .collect::<Vec<_>>();
        let at_rate = supported.iter().find(|c| {
            c.sample_format() == SampleFormat::F32
                && c.channels() >= 1
                && c.min_sample_rate().0 <= device_rate
                && device_rate <= c.max_sample_rate().0
        });
        let (stream_config, input_rate) = match at_rate {
            Some(c) => {
                let channels = c.channels().min(8);
                let cfg = (*c)
                    .with_sample_rate(cpal::SampleRate(device_rate))
                    .config();
                (cpal::StreamConfig { channels, ..cfg }, device_rate)
            }
            None => {
                let def = device.default_input_config().map_err(|e| e.to_string())?;
                if def.sample_format() != SampleFormat::F32 {
                    return Err(format!("input '{name}' offers no f32 capture config"));
                }
                let rate = def.sample_rate().0;
                (def.config(), rate)
            }
        };
        if input_rate != device_rate {
            tracing::info!("Mic resampling {input_rate} Hz → {device_rate} Hz");
        }
        let channels = stream_config.channels as usize;
        let err_fn = |err| tracing::error!("mic input stream error: {}", err);
        let mut producer = ring;
        let mut resampler = MicResampler::new(input_rate, device_rate);
        let mut scratch: Vec<(f32, f32)> = Vec::with_capacity(2048);
        let stream = device
            .build_input_stream(
                &stream_config,
                move |data: &[f32], _: &_| {
                    scratch.clear();
                    for frame in data.chunks(channels.max(1)) {
                        let (l, r) = match frame {
                            [s] => (*s, *s),
                            [l, r, ..] => (*l, *r),
                            [] => continue,
                        };
                        resampler.feed(l, r, &mut scratch);
                    }
                    // Drop newest when the ring is full — a glitch beats
                    // ever-growing monitoring latency. The `slots` check
                    // makes the pair-push atomic: this is the only writer
                    // and the consumer only ever frees slots.
                    for (l, r) in scratch.drain(..) {
                        if producer.slots() < 2 {
                            break;
                        }
                        let _ = producer.push(l);
                        let _ = producer.push(r);
                    }
                },
                err_fn,
                None,
            )
            .map_err(|e| e.to_string())?;
        stream.play().map_err(|e| e.to_string())?;
        Ok((stream, name))
    }
}

impl Default for CpalEngine {
    fn default() -> Self {
        Self::new()
    }
}

impl Engine for CpalEngine {
    fn play(&self, path: &Path) -> Result<()> {
        if !path.exists() {
            return Err(CrabError::FileNotFound {
                path: path.to_path_buf(),
            });
        }
        // Async handoff: decode on the loader thread; the UI thread returns
        // in microseconds — full symphonia decode + sinc resample of a real
        // song takes seconds. An idle play announces the track + enters
        // `Buffering`; a live play keeps `Playing` (old deck sounds on, old
        // label stays) until the new deck lands as a crossfade.
        // Locks are taken sequentially (never nested) to match the audio
        // callback's lock order.
        let gen = self.load_gen.fetch_add(1, Ordering::SeqCst) + 1;
        let live_state = *self.state.lock().unwrap();
        let live_cursor = self
            .xfade
            .lock()
            .unwrap()
            .current
            .as_ref()
            .is_some_and(|c| !c.is_done());
        let was_live = live_state == PlayerState::Playing && live_cursor;
        let path = path.to_path_buf();
        let gain_db = self.gain_for(&path);
        if !self.loader_ok {
            // Degraded path: decode inline first (today's old order), then
            // install — blocks, but audio still works.
            let (cursor, duration) = decode_load(&path, gain_db, self.device_rate)?;
            self.loader_shared()
                .install_play(path, gen, cursor, duration, was_live);
            *self.state.lock().unwrap() = PlayerState::Playing;
            return Ok(());
        }
        self.loading.store(true, Ordering::SeqCst);
        if was_live {
            // Live handoff: keep `Playing` so the callback keeps pulling the
            // old deck through the decode — no on-air gap. Its label stays
            // until the new deck lands (no 00:00 progress flicker).
            self.silence.lock().unwrap().reset();
            self.load_tx
                .send(LoadJob::Play {
                    path,
                    gen,
                    was_live,
                    gain_db,
                })
                .map_err(|e| CrabError::Audio(format!("decode loader gone: {e}")))?;
            return Ok(());
        }
        *self.current_track.lock().unwrap() = Some(TrackInfo {
            path: path.clone(),
            title: None,
            artist: None,
            duration_secs: None,
        });
        *self.state.lock().unwrap() = PlayerState::Buffering;
        self.silence.lock().unwrap().reset();
        self.load_tx
            .send(LoadJob::Play {
                path,
                gen,
                was_live,
                gain_db,
            })
            .map_err(|e| CrabError::Audio(format!("decode loader gone: {e}")))?;
        Ok(())
    }

    fn pause(&self) {
        *self.state.lock().unwrap() = PlayerState::Paused;
    }

    fn resume(&self) {
        // Only resume if there is something loaded — or still loading (a
        // pause pressed mid-load returns to `Buffering`, so the landing deck
        // advances instead of stranding the transport in `Paused`).
        if !self.xfade.lock().unwrap().is_done() {
            *self.state.lock().unwrap() = PlayerState::Playing;
        } else if self.loading.load(Ordering::SeqCst) {
            *self.state.lock().unwrap() = PlayerState::Buffering;
        }
    }

    fn stop(&self) {
        // Invalidate anything still decoding — a late install must never
        // resurrect playback after an explicit stop.
        self.load_gen.fetch_add(1, Ordering::SeqCst);
        self.loading.store(false, Ordering::SeqCst);
        let mut xf = self.xfade.lock().unwrap();
        xf.current = None;
        xf.next.clear();
        xf.pos = 0;
        xf.len = 0;
        drop(xf);
        self.silence.lock().unwrap().reset();
        *self.state.lock().unwrap() = PlayerState::Stopped;
        *self.current_track.lock().unwrap() = None;
    }

    fn toggle_play_pause(&self) {
        match *self.state.lock().unwrap() {
            PlayerState::Playing => self.pause(),
            PlayerState::Paused => self.resume(),
            PlayerState::Stopped => {}
            // Pausing mid-load sticks: the loader keeps `Paused` on install.
            PlayerState::Buffering => self.pause(),
        }
    }

    fn set_volume(&self, vol: f32) {
        let clamped = vol.clamp(0.0, 1.5);
        *self.volume.lock().unwrap() = clamped;
        self.mixer.lock().unwrap().set_gain(clamped);
    }

    fn volume(&self) -> f32 {
        *self.volume.lock().unwrap()
    }

    fn state(&self) -> PlayerState {
        *self.state.lock().unwrap()
    }

    fn current_track(&self) -> Option<TrackInfo> {
        self.current_track.lock().unwrap().clone()
    }

    fn has_audio_device(&self) -> bool {
        self._stream.is_some()
    }

    fn device_name(&self) -> String {
        self.device_name()
    }

    fn is_finished(&self) -> bool {
        // A decode in flight is not an EOF — Auto-DJ must not fire over it.
        if *self.state.lock().unwrap() == PlayerState::Buffering {
            return false;
        }
        match *self.state.lock().unwrap() {
            PlayerState::Stopped => true,
            _ => self.xfade.lock().unwrap().is_done(),
        }
    }

    fn queue(&self, path: &Path) -> Result<()> {
        self.queue_file(path)
    }

    fn position_secs(&self) -> f64 {
        let xf = self.xfade.lock().unwrap();
        xf.current
            .as_ref()
            .map(|c| c.pos_frames as f64 / self.device_rate.max(1) as f64)
            .unwrap_or(0.0)
    }

    fn pending_count(&self) -> usize {
        self.xfade.lock().unwrap().next.len()
    }

    fn has_queue(&self) -> bool {
        true
    }

    fn silence_alarm(&self) -> bool {
        *self.state.lock().unwrap() == PlayerState::Playing && self.silence.lock().unwrap().alarm()
    }

    fn set_silence_threshold_secs(&self, secs: f32) {
        self.silence.lock().unwrap().set_threshold_secs(secs);
    }

    fn set_crossfade_secs(&self, secs: f32) {
        *self.crossfade_secs.lock().unwrap() = secs.clamp(0.0, 30.0);
    }

    fn set_eq_enabled(&self, on: bool) {
        self.mixer.lock().unwrap().set_eq_enabled(on);
    }

    fn eq_enabled(&self) -> bool {
        self.mixer.lock().unwrap().eq_enabled()
    }

    fn set_eq_band(&self, band: usize, gain_db: f32) {
        self.mixer.lock().unwrap().set_eq_band(band, gain_db);
    }

    fn eq_bands(&self) -> [f32; EQ_BAND_COUNT] {
        self.mixer.lock().unwrap().eq.bands().map(|b| b.gain_db)
    }

    fn set_limiter_ceiling(&self, ceiling: f32) {
        self.mixer.lock().unwrap().set_ceiling(ceiling);
    }

    fn limiter_ceiling(&self) -> f32 {
        self.mixer.lock().unwrap().limiter.ceiling()
    }

    fn limiter_reduction_db(&self) -> f32 {
        self.mixer.lock().unwrap().limiter_reduction_db()
    }

    fn set_loudness_lookup(&mut self, lookup: Option<crate::audio::engine::LoudnessLookup>) {
        self.loudness_lookup = lookup;
    }

    fn set_loudness_enabled(&self, on: bool) {
        self.loudness_enabled.set(on);
    }

    fn loudness_enabled(&self) -> bool {
        self.loudness_enabled.get()
    }

    fn set_stream_config(&mut self, config: crate::stream::StreamConfig) {
        self.stream.lock().unwrap().set_config(config);
    }

    fn stream_config(&self) -> crate::stream::StreamConfig {
        self.stream.lock().unwrap().config()
    }

    fn stream_start(&mut self) -> Result<()> {
        let tap = self.stream.lock().unwrap().start(self.device_rate);
        *self.stream_tap.lock().unwrap() = tap;
        Ok(())
    }

    fn stream_stop(&mut self) {
        self.stream.lock().unwrap().stop();
        *self.stream_tap.lock().unwrap() = None;
    }

    fn stream_state(&self) -> crate::stream::StreamState {
        self.stream.lock().unwrap().state()
    }

    fn stream_stats(&self) -> crate::stream::StreamStats {
        self.stream.lock().unwrap().stats()
    }

    fn set_stream_title(&self, title: &str) {
        self.stream.lock().unwrap().set_title(title);
    }

    fn set_mic_config(&mut self, config: MicConfig) {
        let config = config.sanitized();
        let device_changed = self.mic_config.lock().unwrap().device != config.device;
        *self.mic_config.lock().unwrap() = config;
        self.apply_mic_dsp();
        // A live mic follows device switches without a restart dance.
        if device_changed && self.mic_live.load(Ordering::Relaxed) {
            if let Err(e) = self.mic_start() {
                tracing::warn!("Mic device switch failed: {e}");
            }
        }
    }

    fn mic_config(&self) -> MicConfig {
        self.mic_config.lock().unwrap().clone()
    }

    fn mic_start(&mut self) -> Result<()> {
        // Tear down first: start is an idempotent (re)open.
        self.mic_stop();
        let config = self.mic_config.lock().unwrap().clone().sanitized();
        let (producer, consumer) = RingBuffer::new(MIC_RING_SAMPLES);
        match Self::open_input_stream(config.device.clone(), self.device_rate, producer) {
            Ok((stream, name)) => {
                *self.mic_consumer.lock().unwrap() = Some(consumer);
                self.apply_mic_dsp();
                self.mixer.lock().unwrap().ducker.reset();
                self._mic_stream = Some(stream);
                self.mic_live.store(true, Ordering::Relaxed);
                *self.mic_state.lock().unwrap() = MicState::Live;
                tracing::info!("Mic live: input '{name}'");
                Ok(())
            }
            Err(msg) => {
                *self.mic_state.lock().unwrap() = MicState::Error(msg.clone());
                tracing::warn!("Mic start failed: {msg}");
                Err(CrabError::Audio(msg))
            }
        }
    }

    fn mic_stop(&mut self) {
        self.mic_live.store(false, Ordering::Relaxed);
        *self.mic_consumer.lock().unwrap() = None;
        self._mic_stream = None;
        self.mixer.lock().unwrap().ducker.reset();
        *self.mic_state.lock().unwrap() = MicState::Off;
    }

    fn mic_state(&self) -> MicState {
        // A dropped error stream reports Off once stopped; Live only while
        // the flag is set so a dead input can't masquerade as running.
        if self.mic_live.load(Ordering::Relaxed) {
            MicState::Live
        } else {
            self.mic_state.lock().unwrap().clone()
        }
    }

    fn mic_level_db(&self) -> f32 {
        if self.mic_live.load(Ordering::Relaxed) {
            self.mixer.lock().unwrap().mic_level_db()
        } else {
            -99.0
        }
    }

    fn mic_ducking(&self) -> bool {
        self.mic_live.load(Ordering::Relaxed) && self.mixer.lock().unwrap().ducking()
    }
}

/// Decode any symphonia-supported file to stereo-interleaved f32.
/// Returns `(samples, source_sample_rate)`. Mono is duplicated to both ears.
pub(crate) fn decode_to_stereo(path: &Path) -> Result<(Vec<f32>, u32)> {
    use symphonia::core::audio::{AudioBufferRef, Signal};
    use symphonia::core::codecs::DecoderOptions;
    use symphonia::core::formats::FormatOptions;
    use symphonia::core::io::MediaSourceStream;
    use symphonia::core::meta::MetadataOptions;
    use symphonia::core::probe::Hint;

    let file = std::fs::File::open(path).map_err(|_| CrabError::FileNotFound {
        path: path.to_path_buf(),
    })?;
    let mss = MediaSourceStream::new(Box::new(file), Default::default());
    let mut hint = Hint::new();
    if let Some(ext) = path.extension().and_then(|e| e.to_str()) {
        hint.with_extension(ext);
    }
    let probed = symphonia::default::get_probe()
        .format(
            &hint,
            mss,
            &FormatOptions::default(),
            &MetadataOptions::default(),
        )
        .map_err(|e| CrabError::Audio(e.to_string()))?;
    let mut format = probed.format;
    let track = format
        .tracks()
        .iter()
        .find(|t| t.codec_params.codec != symphonia::core::codecs::CODEC_TYPE_NULL)
        .ok_or_else(|| CrabError::Audio("no audio track".into()))?;
    let track_id = track.id;
    let mut decoder = symphonia::default::get_codecs()
        .make(&track.codec_params, &DecoderOptions::default())
        .map_err(|e| CrabError::Audio(e.to_string()))?;

    let mut stereo: Vec<f32> = Vec::new();
    let mut src_rate: Option<u32> = None;
    while let Ok(packet) = format.next_packet() {
        if packet.track_id() != track_id {
            continue;
        }
        let decoded = match decoder.decode(&packet) {
            Ok(d) => d,
            Err(_) => continue,
        };
        match decoded {
            AudioBufferRef::F32(buf) => {
                src_rate = src_rate.or(Some(buf.spec().rate));
                let ch = buf.spec().channels.count();
                if ch == 1 {
                    for s in buf.chan(0) {
                        stereo.push(*s);
                        stereo.push(*s);
                    }
                } else {
                    let (c0, c1) = (buf.chan(0), buf.chan(1.min(ch - 1)));
                    for (a, b) in c0.iter().zip(c1.iter()) {
                        stereo.push(*a);
                        stereo.push(*b);
                    }
                }
            }
            AudioBufferRef::S16(buf) => {
                src_rate = src_rate.or(Some(buf.spec().rate));
                let ch = buf.spec().channels.count();
                let n = buf.frames();
                for i in 0..n {
                    let l = buf.chan(0)[i] as f32 / i16::MAX as f32;
                    let r = if ch > 1 {
                        buf.chan(1)[i] as f32 / i16::MAX as f32
                    } else {
                        l
                    };
                    stereo.push(l);
                    stereo.push(r);
                }
            }
            AudioBufferRef::S32(buf) => {
                src_rate = src_rate.or(Some(buf.spec().rate));
                let ch = buf.spec().channels.count();
                let n = buf.frames();
                for i in 0..n {
                    let l = buf.chan(0)[i] as f32 / i32::MAX as f32;
                    let r = if ch > 1 {
                        buf.chan(1)[i] as f32 / i32::MAX as f32
                    } else {
                        l
                    };
                    stereo.push(l);
                    stereo.push(r);
                }
            }
            _ => {
                // Other sample formats: convert via intermediate is overkill for scaffold.
                return Err(CrabError::Audio(
                    "unsupported sample format (scaffold)".into(),
                ));
            }
        }
    }
    if stereo.is_empty() {
        return Err(CrabError::Audio("decoded 0 samples".into()));
    }
    Ok((stereo, src_rate.unwrap_or(44100)))
}

/// Resample stereo-interleaved f32 from one rate to another (rubato sinc).
/// Output is trimmed to the exact expected frame count.
fn resample_stereo(interleaved: Vec<f32>, from: u32, to: u32) -> Result<Vec<f32>> {
    if from == to || interleaved.is_empty() {
        return Ok(interleaved);
    }
    use rubato::{
        Resampler, SincFixedIn, SincInterpolationParameters, SincInterpolationType, WindowFunction,
    };
    let params = SincInterpolationParameters {
        sinc_len: 64,
        f_cutoff: 0.95,
        interpolation: SincInterpolationType::Linear,
        oversampling_factor: 128,
        window: WindowFunction::BlackmanHarris2,
    };
    let ratio = to as f64 / from as f64;
    let mut resampler = SincFixedIn::<f32>::new(ratio, 2.0, params, 1024, 2)
        .map_err(|e| CrabError::Audio(format!("resampler setup: {}", e)))?;
    let frames = interleaved.len() / 2;
    let mut waves: Vec<Vec<f32>> = (0..2).map(|_| Vec::with_capacity(frames)).collect();
    for pair in interleaved.as_chunks::<2>().0 {
        waves[0].push(pair[0]);
        waves[1].push(pair[1]);
    }
    let mut out: Vec<Vec<f32>> = vec![Vec::new(); 2];
    let mut pos = 0;
    while pos < frames {
        let need = resampler.input_frames_next();
        let take = need.min(frames - pos);
        let chunk: Vec<Vec<f32>> = waves
            .iter()
            .map(|w| {
                let mut c = Vec::with_capacity(need);
                c.extend_from_slice(&w[pos..pos + take]);
                c.resize(need, 0.0);
                c
            })
            .collect();
        let rendered = resampler
            .process(&chunk, None)
            .map_err(|e| CrabError::Audio(format!("resample failed: {}", e)))?;
        out[0].extend_from_slice(&rendered[0]);
        out[1].extend_from_slice(&rendered[1]);
        pos += take;
    }
    let want = (frames as f64 * ratio).round() as usize;
    out[0].truncate(want);
    out[1].truncate(want);
    let mut stereo = Vec::with_capacity(want * 2);
    for (l, r) in out[0].iter().zip(out[1].iter()) {
        stereo.push(*l);
        stereo.push(*r);
    }
    Ok(stereo)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::VecDeque;

    fn ramp_stereo(frames: usize) -> Vec<f32> {
        let mut v = Vec::with_capacity(frames * 2);
        for i in 0..frames {
            let t = i as f32 / frames as f32;
            v.push(t);
            v.push(1.0 - t);
        }
        v
    }

    #[test]
    fn resample_passthrough_when_rates_match() {
        let v = ramp_stereo(100);
        assert_eq!(resample_stereo(v.clone(), 48000, 48000).unwrap(), v);
    }

    #[test]
    fn resample_up_produces_expected_frame_count() {
        let out = resample_stereo(ramp_stereo(4410), 44100, 48000).unwrap();
        assert_eq!(out.len(), 4800 * 2);
        // Monotonic-ish ramp preserved on L (allow filter ripple at edges).
        assert!(out[200] > 0.0 && out[200] < 0.2);
        assert!(out[out.len() - 200] > 0.8);
    }

    #[test]
    fn resample_down_produces_expected_frame_count() {
        let out = resample_stereo(ramp_stereo(4800), 48000, 44100).unwrap();
        assert_eq!(out.len(), 4410 * 2);
    }

    #[test]
    fn xfade_pull_blends_and_promotes() {
        let mut xf = XfadeState {
            current: Some(PlaybackCursor {
                samples: vec![1.0, 1.0, 1.0, 1.0],
                pos_frames: 0,
            }),
            next: VecDeque::from([PlaybackCursor {
                samples: vec![0.0, 0.0, 0.0, 0.0, 0.0, 0.0],
                pos_frames: 0,
            }]),
            pos: 0,
            len: 2,
            auto_len: 0,
        };
        // Frame 0: full current.
        let (a, b, x) = xf.pull().unwrap();
        assert_eq!((a.l, x), (1.0, 0.0));
        assert!(b.is_some());
        // Frame 1: midpoint, then incoming takes over.
        let (_a, b, x) = xf.pull().unwrap();
        assert_eq!(x, 0.5);
        assert!(b.is_some());
        assert!(xf.next.is_empty(), "blend finished, deck promoted");
        // Continues from incoming deck at full volume.
        let (a, _, x) = xf.pull().unwrap();
        assert_eq!((a.l, x), (0.0, 0.0));
    }

    #[test]
    fn xfade_short_incoming_keeps_current() {
        let mut xf = XfadeState {
            current: Some(PlaybackCursor {
                samples: vec![1.0, 1.0, 1.0, 1.0, 1.0, 1.0, 1.0, 1.0],
                pos_frames: 0,
            }),
            next: VecDeque::from([PlaybackCursor {
                samples: vec![0.0, 0.0],
                pos_frames: 0,
            }]),
            pos: 0,
            len: 4,
            auto_len: 0,
        };
        xf.pull().unwrap(); // consumes the single incoming frame
        let (_, b, _) = xf.pull().unwrap(); // incoming exhausted → blend ends
        assert!(b.is_none());
        assert!(xf.next.is_empty());
        assert!(!xf.is_done(), "current deck keeps playing");
    }

    #[test]
    fn queued_deck_promotes_at_eof_without_blend() {
        let mut xf = XfadeState {
            current: Some(PlaybackCursor {
                samples: vec![1.0, 1.0, 1.0, 1.0],
                pos_frames: 0,
            }),
            next: VecDeque::from([PlaybackCursor {
                samples: vec![2.0, 2.0, 2.0, 2.0],
                pos_frames: 0,
            }]),
            pos: 0,
            len: 0,
            auto_len: 0,
        };
        assert_eq!(xf.pull().unwrap().0.l, 1.0);
        assert_eq!(xf.pull().unwrap().0.l, 1.0);
        // Current exhausted → queued deck takes over at full volume, no blend.
        let (a, b, x) = xf.pull().unwrap();
        assert_eq!((a.l, x), (2.0, 0.0));
        assert!(b.is_none());
    }

    #[test]
    fn queued_deck_auto_blends_at_track_end() {
        let mut xf = XfadeState {
            current: Some(PlaybackCursor {
                samples: vec![1.0, 1.0, 1.0, 1.0, 1.0, 1.0, 1.0, 1.0],
                pos_frames: 0,
            }),
            next: VecDeque::from([PlaybackCursor {
                samples: vec![0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0],
                pos_frames: 0,
            }]),
            pos: 0,
            len: 0,
            auto_len: 2,
        };
        // remaining 3 → full current; remaining 2 → blend x=0, then x=0.5 + promote.
        assert_eq!(xf.pull().unwrap().2, 0.0);
        let (_, b, x) = xf.pull().unwrap();
        assert!(b.is_some() && x == 0.0);
        let (_, b, x) = xf.pull().unwrap();
        assert!(b.is_some() && x == 0.5);
        assert!(xf.next.is_empty(), "blend finished, queued deck promoted");
        assert!(!xf.is_done());
    }

    #[test]
    fn pending_chain_plays_in_order() {
        // intro → spot → outro queued behind one live frame.
        let mut xf = XfadeState {
            current: Some(PlaybackCursor {
                samples: vec![1.0, 1.0],
                pos_frames: 0,
            }),
            next: VecDeque::from([
                PlaybackCursor {
                    samples: vec![2.0, 2.0],
                    pos_frames: 0,
                },
                PlaybackCursor {
                    samples: vec![3.0, 3.0],
                    pos_frames: 0,
                },
            ]),
            pos: 0,
            len: 0,
            auto_len: 0,
        };
        let heard: Vec<f32> = (0..3).map(|_| xf.pull().unwrap().0.l).collect();
        assert_eq!(heard, vec![1.0, 2.0, 3.0]);
        assert!(xf.is_done());
    }

    /// Minimal 16-bit mono PCM WAV (symphonia reads it natively) for
    /// loader tests — real files through the real decode path.
    fn write_test_wav(path: &std::path::Path, secs: f32, rate: u32) {
        let n = (secs * rate as f32) as usize;
        let data_bytes = (n * 2) as u32;
        let mut wav = Vec::with_capacity(44 + data_bytes as usize);
        wav.extend_from_slice(b"RIFF");
        wav.extend_from_slice(&(36 + data_bytes).to_le_bytes());
        wav.extend_from_slice(b"WAVEfmt ");
        wav.extend_from_slice(&16u32.to_le_bytes());
        wav.extend_from_slice(&1u16.to_le_bytes()); // PCM
        wav.extend_from_slice(&1u16.to_le_bytes()); // mono
        wav.extend_from_slice(&rate.to_le_bytes());
        wav.extend_from_slice(&(rate * 2).to_le_bytes()); // byte rate
        wav.extend_from_slice(&2u16.to_le_bytes()); // block align
        wav.extend_from_slice(&16u16.to_le_bytes()); // bits per sample
        wav.extend_from_slice(b"data");
        wav.extend_from_slice(&data_bytes.to_le_bytes());
        for i in 0..n {
            let t = i as f32 / rate as f32;
            let s = (t * 440.0 * 2.0 * std::f32::consts::PI).sin() * 0.4;
            wav.extend_from_slice(&((s * i16::MAX as f32) as i16).to_le_bytes());
        }
        std::fs::write(path, wav).unwrap();
    }

    fn loader_test_dir(name: &str) -> std::path::PathBuf {
        let dir =
            std::env::temp_dir().join(format!("crabboss-load-{name}-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    /// Spin until `cond` holds (the loader runs async) or time out.
    fn wait_for(msg: &str, mut cond: impl FnMut() -> bool) {
        let start = std::time::Instant::now();
        while !cond() {
            assert!(
                start.elapsed() < std::time::Duration::from_secs(10),
                "timed out waiting: {msg}"
            );
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
    }

    #[test]
    fn play_announces_instantly_and_starts_after_decode() {
        let dir = loader_test_dir("play");
        let f = dir.join("a.wav");
        write_test_wav(&f, 0.5, 44100);
        let eng = CpalEngine::new();
        eng.play(&f).unwrap();
        // Returns at once: track announced, transport buffering (or
        // already playing on a fast machine — never still stopped).
        assert_eq!(eng.current_track().unwrap().path, f);
        assert!(matches!(
            eng.state(),
            PlayerState::Buffering | PlayerState::Playing
        ));
        assert!(!eng.is_finished(), "in-flight decode is not an EOF");
        // The decoded deck lands with no further calls.
        wait_for("playback starts", || eng.state() == PlayerState::Playing);
        let cur = eng.current_track().unwrap();
        assert!((cur.duration_secs.unwrap_or(0.0) - 0.5).abs() < 0.1);
        eng.stop();
        assert_eq!(eng.state(), PlayerState::Stopped);
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn rapid_replay_supersedes_the_older_load() {
        let dir = loader_test_dir("supersede");
        let (a, b) = (dir.join("a.wav"), dir.join("b.wav"));
        write_test_wav(&a, 0.5, 44100);
        write_test_wav(&b, 0.5, 44100);
        let eng = CpalEngine::new();
        eng.play(&a).unwrap();
        eng.play(&b).unwrap();
        // Whatever interleaving the loader hits, the latest pick wins.
        wait_for("latest pick wins", || {
            eng.state() == PlayerState::Playing && eng.current_track().is_some_and(|t| t.path == b)
        });
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn stop_during_load_never_resurrects_playback() {
        let dir = loader_test_dir("stop");
        let f = dir.join("a.wav");
        write_test_wav(&f, 1.0, 44100);
        let eng = CpalEngine::new();
        eng.play(&f).unwrap();
        eng.stop();
        // Let any in-flight decode finish: the stale install must be
        // discarded, never reviving playback after an explicit stop.
        std::thread::sleep(std::time::Duration::from_millis(500));
        assert_eq!(eng.state(), PlayerState::Stopped);
        assert!(eng.current_track().is_none());
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn idle_queue_starts_playback_after_decode() {
        let dir = loader_test_dir("queue");
        let f = dir.join("q.wav");
        write_test_wav(&f, 0.5, 48000);
        let eng = CpalEngine::new();
        eng.queue(&f).unwrap();
        // Idle queue takes over directly (old `queue_file` semantics),
        // just asynchronously.
        wait_for("queued deck starts", || {
            eng.state() == PlayerState::Playing && eng.current_track().is_some_and(|t| t.path == f)
        });
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn live_play_keeps_old_deck_until_handoff() {
        let dir = loader_test_dir("live");
        let (a, b) = (dir.join("a.wav"), dir.join("b.wav"));
        write_test_wav(&a, 3.0, 44100);
        write_test_wav(&b, 0.5, 44100);
        let eng = CpalEngine::new();
        eng.play(&a).unwrap();
        wait_for("first deck live", || {
            eng.state() == PlayerState::Playing && eng.current_track().is_some_and(|t| t.path == a)
        });
        // Live handoff: transport stays `Playing` with the old label — the
        // callback keeps pulling the old deck, so there is no on-air gap.
        eng.play(&b).unwrap();
        assert_eq!(eng.state(), PlayerState::Playing);
        assert!(!eng.is_finished(), "old deck still live through the decode");
        // The latest pick still wins, landing as a crossfade.
        wait_for("handoff lands", || {
            eng.state() == PlayerState::Playing && eng.current_track().is_some_and(|t| t.path == b)
        });
        eng.stop();
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn live_load_failure_keeps_old_deck_playing() {
        let dir = loader_test_dir("livefail");
        let (a, bad) = (dir.join("a.wav"), dir.join("bad.wav"));
        write_test_wav(&a, 3.0, 44100);
        std::fs::write(&bad, b"not audio at all").unwrap();
        let eng = CpalEngine::new();
        eng.play(&a).unwrap();
        wait_for("first deck live", || {
            eng.state() == PlayerState::Playing && eng.current_track().is_some_and(|t| t.path == a)
        });
        eng.play(&bad).unwrap();
        assert_eq!(eng.state(), PlayerState::Playing);
        // The failed decode never parks the transport mid-show.
        std::thread::sleep(std::time::Duration::from_millis(800));
        assert_eq!(eng.state(), PlayerState::Playing);
        assert!(eng.current_track().is_some_and(|t| t.path == a));
        assert!(!eng.is_finished());
        eng.stop();
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn pause_then_resume_mid_load_still_starts() {
        let dir = loader_test_dir("resumeload");
        let f = dir.join("a.wav");
        write_test_wav(&f, 1.0, 48000);
        let eng = CpalEngine::new();
        eng.play(&f).unwrap();
        eng.pause();
        eng.resume();
        // Either the deck already landed (Playing) or the resume re-armed
        // the in-flight load (Buffering) — never stranded in Paused.
        assert!(matches!(
            eng.state(),
            PlayerState::Playing | PlayerState::Buffering
        ));
        wait_for("deck lands after resume", || {
            eng.state() == PlayerState::Playing && eng.current_track().is_some_and(|t| t.path == f)
        });
        eng.stop();
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn loudness_toggle_gates_decode_gain() {
        let dir = loader_test_dir("loudgate");
        let mut eng = CpalEngine::new();
        eng.set_loudness_lookup(Some(Box::new(|_: &Path| Some(6.0))));
        // Default OFF: lookup installed but ignored.
        assert!(!eng.loudness_enabled());
        assert_eq!(eng.gain_for(Path::new("/m/a.mp3")), 0.0);
        eng.set_loudness_enabled(true);
        assert!(eng.loudness_enabled());
        assert!((eng.gain_for(Path::new("/m/a.mp3")) - 6.0).abs() < 1e-6);
        // Clamped to the shared ceiling, and OFF wins again afterwards.
        eng.set_loudness_lookup(Some(Box::new(|_: &Path| Some(99.0))));
        assert!((eng.gain_for(Path::new("/m/a.mp3")) - MAX_GAIN_DB).abs() < 1e-6);
        eng.set_loudness_enabled(false);
        assert_eq!(eng.gain_for(Path::new("/m/a.mp3")), 0.0);
        std::fs::remove_dir_all(&dir).ok();
    }
}
