//! cpal-backed engine scaffold (ROADMAP: rodio → cpal).
//!
//! Status: MVP — device open + symphonia decode + mixer in callback.
//! TODO: rubato resampling to device rate, gapless crossfade between
//! tracks, EQ insert, mic input, Icecast tee.

use std::path::Path;
use std::sync::{Arc, Mutex};

use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};

use crate::audio::engine::Engine;
use crate::audio::mixer::Mixer;
use crate::audio::player::{PlayerState, TrackInfo};
use crate::error::{CrabError, Result};

struct PlaybackCursor {
    samples_mono: Vec<f32>,
    pos: usize,
    #[allow(dead_code)]
    file_rate: u32,
}

impl PlaybackCursor {
    fn next_sample(&mut self) -> Option<f32> {
        if self.pos >= self.samples_mono.len() {
            return None;
        }
        let s = self.samples_mono[self.pos];
        self.pos += 1;
        Some(s)
    }

    fn is_done(&self) -> bool {
        self.pos >= self.samples_mono.len()
    }
}

/// Low-level engine. Keeps the cpal `Stream` alive; callback pulls
/// decoded mono samples through `Mixer`.
pub struct CpalEngine {
    _stream: Option<cpal::Stream>,
    device_rate: u32,
    cursor: Arc<Mutex<Option<PlaybackCursor>>>,
    state: Arc<Mutex<PlayerState>>,
    current_track: Arc<Mutex<Option<TrackInfo>>>,
    volume: Arc<Mutex<f32>>,
    mixer: Arc<Mutex<Mixer>>,
}

impl CpalEngine {
    /// Open default output. Never panics — falls back to headless
    /// (`_stream: None`, still tracks state) when no device exists.
    pub fn new() -> Self {
        let cursor: Arc<Mutex<Option<PlaybackCursor>>> = Arc::new(Mutex::new(None));
        let state = Arc::new(Mutex::new(PlayerState::Stopped));
        let current_track = Arc::new(Mutex::new(None));
        let volume = Arc::new(Mutex::new(1.0));
        let mixer = Arc::new(Mutex::new(Mixer::default()));

        let (stream, device_rate) = match Self::open_silent_stream(
            cursor.clone(),
            state.clone(),
            volume.clone(),
            mixer.clone(),
        ) {
            Ok((s, rate)) => {
                tracing::info!("CpalEngine: output opened @ {} Hz", rate);
                (Some(s), rate)
            }
            Err(e) => {
                tracing::warn!("CpalEngine: no audio device ({}). Headless.", e);
                (None, 48000)
            }
        };

        Self {
            _stream: stream,
            device_rate,
            cursor,
            state,
            current_track,
            volume,
            mixer,
        }
    }

    pub fn device_rate(&self) -> u32 {
        self.device_rate
    }

    /// List output devices (for Settings screen later).
    pub fn list_output_devices() -> Vec<String> {
        cpal::default_host()
            .output_devices()
            .map(|devs| {
                devs
                    .filter_map(|d| d.name().ok())
                    .collect::<Vec<_>>()
            })
            .unwrap_or_default()
    }

    fn open_silent_stream(
        cursor: Arc<Mutex<Option<PlaybackCursor>>>,
        state: Arc<Mutex<PlayerState>>,
        volume: Arc<Mutex<f32>>,
        mixer: Arc<Mutex<Mixer>>,
    ) -> std::result::Result<(cpal::Stream, u32), String> {
        let host = cpal::default_host();
        let device = host
            .default_output_device()
            .ok_or_else(|| "no default output device".to_string())?;
        let config = device
            .default_output_config()
            .map_err(|e| e.to_string())?;
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
                    let mut cur = cursor.lock().unwrap();
                    let playing =
                        *state.lock().unwrap() == PlayerState::Playing;

                    for frame in data.chunks_mut(channels) {
                        let src = if playing {
                            cur.as_mut().and_then(|c| c.next_sample())
                        } else {
                            None
                        };
                        // Mono through mixer (gain+limiter); duplicate to channels.
                        let out = mx.process(src, None);
                        let v = out.l * vol;
                        for s in frame.iter_mut() {
                            *s = v;
                        }
                    }
                    // Auto-stop at EOF.
                    if let Some(c) = cur.as_ref() {
                        if c.is_done() {
                            *state.lock().unwrap() = PlayerState::Stopped;
                        }
                    }
                },
                err_fn,
                None,
            )
            .map_err(|e| e.to_string())?;
        stream.play().map_err(|e| e.to_string())?;
        Ok((stream, sample_rate))
    }

    fn read_duration(&self, path: &Path) -> Option<f64> {
        lofty::read_from_path(path).ok().and_then(|f| {
            use lofty::file::AudioFile;
            Some(f.properties().duration().as_secs_f64())
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
        let samples = decode_to_mono(path)?;
        let file_rate = probe_sample_rate(path).unwrap_or(self.device_rate);
        if file_rate != self.device_rate {
            tracing::warn!(
                "Rate mismatch file={} device={} — rubato resample TODO, playing at file speed",
                file_rate,
                self.device_rate
            );
        }
        *self.cursor.lock().unwrap() = Some(PlaybackCursor {
            samples_mono: samples,
            pos: 0,
            file_rate,
        });
        *self.state.lock().unwrap() = PlayerState::Playing;
        *self.current_track.lock().unwrap() = Some(TrackInfo {
            path: path.to_path_buf(),
            title: None,
            artist: None,
            duration_secs: duration,
        });
        tracing::info!("CpalEngine playing: {}", path.display());
        Ok(())
    }

    fn pause(&self) {
        *self.state.lock().unwrap() = PlayerState::Paused;
    }

    fn resume(&self) {
        // Only resume if there is something loaded.
        if self.cursor.lock().unwrap().is_some() {
            *self.state.lock().unwrap() = PlayerState::Playing;
        }
    }

    fn stop(&self) {
        *self.cursor.lock().unwrap() = None;
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

    fn is_finished(&self) -> bool {
        match *self.state.lock().unwrap() {
            PlayerState::Stopped => true,
            _ => self
                .cursor
                .lock()
                .unwrap()
                .as_ref()
                .is_none_or(|c| c.is_done()),
        }
    }
}

/// Decode any symphonia-supported file to mono f32.
fn decode_to_mono(path: &Path) -> Result<Vec<f32>> {
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
        .format(&hint, mss, &FormatOptions::default(), &MetadataOptions::default())
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

    let mut mono: Vec<f32> = Vec::new();
    loop {
        let packet = match format.next_packet() {
            Ok(p) => p,
            Err(_) => break,
        };
        if packet.track_id() != track_id {
            continue;
        }
        let decoded = match decoder.decode(&packet) {
            Ok(d) => d,
            Err(_) => continue,
        };
        match decoded {
            AudioBufferRef::F32(buf) => {
                if buf.spec().channels.count() == 1 {
                    mono.extend(buf.chan(0));
                } else {
                    let (c0, c1) = (buf.chan(0), buf.chan(1.min(buf.spec().channels.count() - 1)));
                    mono.extend(c0.iter().zip(c1.iter()).map(|(a, b)| (a + b) * 0.5));
                }
            }
            AudioBufferRef::S16(buf) => {
                let n = buf.frames() as usize;
                let ch = buf.spec().channels.count();
                for i in 0..n {
                    let mut acc = 0i32;
                    for c in 0..ch {
                        acc += buf.chan(c)[i] as i32;
                    }
                    mono.push((acc as f32 / ch as f32) / i16::MAX as f32);
                }
                let _ = buf.spec();
            }
            AudioBufferRef::S32(buf) => {
                let n = buf.frames() as usize;
                let ch = buf.spec().channels.count();
                for i in 0..n {
                    let mut acc = 0i64;
                    for c in 0..ch {
                        acc += buf.chan(c)[i] as i64;
                    }
                    mono.push((acc as f32 / ch as f32) / i32::MAX as f32);
                }
            }
            _ => {
                // Other sample formats: convert via intermediate is overkill for scaffold.
                return Err(CrabError::Audio("unsupported sample format (scaffold)".into()));
            }
        }
    }
    if mono.is_empty() {
        return Err(CrabError::Audio("decoded 0 samples".into()));
    }
    Ok(mono)
}

fn probe_sample_rate(path: &Path) -> Option<u32> {
    lofty::read_from_path(path).ok().and_then(|f| {
        use lofty::file::AudioFile;
        f.properties().sample_rate()
    })
}
