//! cpal-backed engine (ROADMAP: rodio → cpal).
//!
//! Status: stereo symphonia decode → rubato resample to device rate →
//! dual-cursor equal-power/linear crossfade through `Mixer` in the callback,
//! with 12-band EQ insert and limiter on the program bus. Mic/line-in
//! (§1.6) sums into the program bus ahead of the limiter + stream tap
//! with voice-activated ducking of the music bed.

use std::collections::VecDeque;
use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use rtrb::RingBuffer;

use crate::audio::engine::Engine;
use crate::audio::mic::{MicConfig, MicResampler, MicState, MIC_RING_SAMPLES};
use crate::audio::mixer::{Frame, Mixer, EQ_BAND_COUNT};
use crate::audio::player::{PlayerState, TrackInfo};
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

        Self {
            _stream: stream,
            device_rate,
            device_name,
            xfade,
            crossfade_secs: Arc::new(Mutex::new(3.0)),
            silence,
            state,
            current_track,
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
        }
    }

    pub fn device_rate(&self) -> u32 {
        self.device_rate
    }

    /// The device actually opened (may differ from the request on fallback).
    pub fn device_name(&self) -> String {
        self.device_name.clone()
    }

    fn xfade_secs(&self) -> f32 {
        *self.crossfade_secs.lock().unwrap()
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

    fn decode_resampled(&self, path: &Path) -> Result<PlaybackCursor> {
        let (mut samples, file_rate) = decode_to_stereo(path)?;
        // Loudness normalization: per-deck gain from the library analysis
        // (ReplayGain-style toward the R128 target). Missing analysis → 0 dB.
        let gain_db = self
            .loudness_lookup
            .as_ref()
            .and_then(|f| f(path))
            .unwrap_or(0.0)
            .clamp(-MAX_GAIN_DB, MAX_GAIN_DB);
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
        let samples = if file_rate != self.device_rate {
            tracing::info!("Resampling {} Hz → {} Hz", file_rate, self.device_rate);
            resample_stereo(samples, file_rate, self.device_rate)?
        } else {
            samples
        };
        Ok(PlaybackCursor {
            samples,
            pos_frames: 0,
        })
    }

    /// Queue a file to start at the current deck's end (insert-after).
    /// Appends behind anything already pending.
    pub fn queue_file(&self, path: &Path) -> Result<()> {
        if !path.exists() {
            return Err(CrabError::FileNotFound {
                path: path.to_path_buf(),
            });
        }
        let new = self.decode_resampled(path)?;
        let mut xf = self.xfade.lock().unwrap();
        xf.auto_len = (self.xfade_secs() * self.device_rate as f32) as usize;
        if xf.current.as_ref().is_some_and(|c| !c.is_done()) || !xf.next.is_empty() {
            xf.next.push_back(new);
            tracing::info!("CpalEngine queued: {}", path.display());
        } else {
            xf.current = Some(new);
            xf.pos = 0;
            xf.len = 0;
        }
        drop(xf);
        self.silence.lock().unwrap().reset();
        *self.state.lock().unwrap() = PlayerState::Playing;
        *self.current_track.lock().unwrap() = Some(TrackInfo {
            path: path.to_path_buf(),
            title: None,
            artist: None,
            duration_secs: self.read_duration(path),
        });
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

    fn read_duration(&self, path: &Path) -> Option<f64> {
        lofty::read_from_path(path).ok().map(|f| {
            use lofty::file::AudioFile;
            f.properties().duration().as_secs_f64()
        })
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
        let duration = self.read_duration(path);
        let new = self.decode_resampled(path)?;
        let mut xf = self.xfade.lock().unwrap();
        xf.auto_len = (self.xfade_secs() * self.device_rate as f32) as usize;
        let live = *self.state.lock().unwrap() == PlayerState::Playing
            && xf.current.as_ref().is_some_and(|c| !c.is_done());
        if live {
            // Crossfade: blend out of the current deck into the new one,
            // replacing anything pending (immediate intent wins).
            let remaining = xf
                .current
                .as_ref()
                .map(|c| c.remaining_frames())
                .unwrap_or(0);
            let want = (self.xfade_secs() * self.device_rate as f32) as usize;
            xf.len = want.min(remaining).min(new.remaining_frames()).max(1);
            xf.pos = 0;
            xf.next = VecDeque::from([new]);
            tracing::info!(
                "CpalEngine crossfading ({} frames): {}",
                xf.len,
                path.display()
            );
        } else {
            xf.current = Some(new);
            xf.next.clear();
            xf.pos = 0;
            xf.len = 0;
            tracing::info!("CpalEngine playing: {}", path.display());
        }
        drop(xf);
        self.silence.lock().unwrap().reset();
        *self.state.lock().unwrap() = PlayerState::Playing;
        *self.current_track.lock().unwrap() = Some(TrackInfo {
            path: path.to_path_buf(),
            title: None,
            artist: None,
            duration_secs: duration,
        });
        Ok(())
    }

    fn pause(&self) {
        *self.state.lock().unwrap() = PlayerState::Paused;
    }

    fn resume(&self) {
        // Only resume if there is something loaded.
        if !self.xfade.lock().unwrap().is_done() {
            *self.state.lock().unwrap() = PlayerState::Playing;
        }
    }

    fn stop(&self) {
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
}
