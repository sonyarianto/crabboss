//! cpal-backed engine (ROADMAP: rodio → cpal).
//!
//! Status: stereo symphonia decode → rubato resample to device rate →
//! dual-cursor equal-power/linear crossfade through `Mixer` in the callback.
//! TODO: EQ insert, mic input, Icecast tee.

use std::collections::VecDeque;
use std::path::Path;
use std::sync::{Arc, Mutex};

use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};

use crate::audio::engine::Engine;
use crate::audio::mixer::{Frame, Mixer};
use crate::audio::player::{PlayerState, TrackInfo};
use crate::audio::silence::SilenceMonitor;
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

        let (stream, device_rate, device_name) = match Self::open_silent_stream(
            xfade.clone(),
            state.clone(),
            volume.clone(),
            mixer.clone(),
            silence.clone(),
            want,
        ) {
            Ok((s, rate, name)) => {
                tracing::info!("CpalEngine: output '{}' @ {} Hz", name, rate);
                *silence.lock().unwrap() = SilenceMonitor::new(rate, 10.0);
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

    fn decode_resampled(&self, path: &Path) -> Result<PlaybackCursor> {
        let (samples, file_rate) = decode_to_stereo(path)?;
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

    fn open_silent_stream(
        xfade: Arc<Mutex<XfadeState>>,
        state: Arc<Mutex<PlayerState>>,
        volume: Arc<Mutex<f32>>,
        mixer: Arc<Mutex<Mixer>>,
        silence: Arc<Mutex<SilenceMonitor>>,
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
                    let mx = mixer.lock().unwrap();
                    let mut xf = xfade.lock().unwrap();
                    let mut sil = silence.lock().unwrap();
                    let playing = *state.lock().unwrap() == PlayerState::Playing;

                    for frame in data.chunks_mut(channels) {
                        let req = if playing { xf.pull() } else { None };
                        let (l, r) = match req {
                            Some((a, b, x)) => {
                                let f = mx.process_x(Some(a), b, x);
                                (f.l * vol, f.r * vol)
                            }
                            None => (0.0, 0.0),
                        };
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

    fn silence_alarm(&self) -> bool {
        *self.state.lock().unwrap() == PlayerState::Playing && self.silence.lock().unwrap().alarm()
    }

    fn set_silence_threshold_secs(&self, secs: f32) {
        self.silence.lock().unwrap().set_threshold_secs(secs);
    }

    fn set_crossfade_secs(&self, secs: f32) {
        *self.crossfade_secs.lock().unwrap() = secs.clamp(0.0, 30.0);
    }
}

/// Decode any symphonia-supported file to stereo-interleaved f32.
/// Returns `(samples, source_sample_rate)`. Mono is duplicated to both ears.
fn decode_to_stereo(path: &Path) -> Result<(Vec<f32>, u32)> {
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
