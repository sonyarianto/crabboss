//! Engine trait — common interface for audio backends.
//!
//! `crabui` codes against `Engine`; `CpalEngine` (cpal) is the backend.

use std::path::{Path, PathBuf};

use crate::audio::mixer::EQ_BAND_COUNT;
use crate::error::Result;

/// Information about the currently loaded track.
#[derive(Debug, Clone)]
pub struct TrackInfo {
    pub path: PathBuf,
    pub title: Option<String>,
    pub artist: Option<String>,
    pub duration_secs: Option<f64>,
}

/// Transport state.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PlayerState {
    Stopped,
    Playing,
    Paused,
    /// A `play` was accepted and the file is decoding on the background
    /// loader — the UI thread is never blocked waiting for it. The audio
    /// callback stays silent until the decoded deck lands (`Playing`),
    /// and a mid-load `stop` supersedes it (back to `Stopped`).
    Buffering,
}

/// Per-path loudness gain lookup (dB) used by normalization at decode time.
pub type LoudnessLookup = Box<dyn Fn(&Path) -> Option<f32>>;

/// Minimal transport + volume interface the audio backend provides.
/// NOTE: no Send+Sync bound — cpal::Stream is !Send/!Sync; the UI owns
/// the engine single-threaded (Rc/RefCell).
pub trait Engine {
    /// Start `path`, crossfading when something is already live.
    /// Returns as soon as the file is handed to the background decode
    /// loader — it must never block the caller on full-file decode +
    /// resample. An idle play is announced via `current_track` (state
    /// `Buffering`); a live play keeps `Playing` and its current label
    /// until the decoded deck lands as a crossfade (`Playing`).
    fn play(&self, path: &Path) -> Result<()>;
    /// Queue for end-of-track start (insert-after-current). Default degrades
    /// to immediate `play`; `CpalEngine` blends it in at the boundary.
    /// Same async contract as `play`: returns after enqueueing the job.
    fn queue(&self, path: &Path) -> Result<()> {
        self.play(path)
    }
    fn pause(&self);
    fn resume(&self);
    fn stop(&self);
    fn toggle_play_pause(&self);
    fn set_volume(&self, vol: f32);
    fn volume(&self) -> f32;
    fn state(&self) -> PlayerState;
    fn current_track(&self) -> Option<TrackInfo>;
    fn has_audio_device(&self) -> bool;
    fn is_finished(&self) -> bool;
    /// Human label of the opened output (device picker display).
    fn device_name(&self) -> String {
        "Default".to_string()
    }
    /// Dead-air alarm: true when output has been (near-)silent for longer
    /// than the threshold while Playing. `CpalEngine` measures the real
    /// mix bus.
    fn silence_alarm(&self) -> bool {
        false
    }
    /// Seconds of continuous silence before [`Engine::silence_alarm`] trips.
    fn set_silence_threshold_secs(&self, _secs: f32) {}
    /// Blend length for crossfades / queued takeovers.
    fn set_crossfade_secs(&self, _secs: f32) {}
    /// Enable the program EQ insert (12-band). No-op on backends without DSP.
    fn set_eq_enabled(&self, _on: bool) {}
    /// True when the EQ insert is active.
    fn eq_enabled(&self) -> bool {
        false
    }
    /// Set one EQ band's gain in dB (±12). No-op on backends without DSP.
    fn set_eq_band(&self, _band: usize, _gain_db: f32) {}
    /// Current EQ band gains in dB (index order = [`EQ_CENTER_HZ`]).
    fn eq_bands(&self) -> [f32; EQ_BAND_COUNT] {
        [0.0; EQ_BAND_COUNT]
    }
    /// Limiter ceiling (linear 0.1..1.0) on the program bus.
    fn set_limiter_ceiling(&self, _ceiling: f32) {}
    fn limiter_ceiling(&self) -> f32 {
        0.99
    }
    /// Live limiter gain reduction in dB (negative when working; UI meter).
    fn limiter_reduction_db(&self) -> f32 {
        0.0
    }
    /// Loudness normalization: install a per-path gain lookup (dB) used at
    /// decode time. Called once with `Some(...)` when the library is open;
    /// `None` clears it.
    fn set_loudness_lookup(&mut self, _lookup: Option<LoudnessLookup>) {}
    /// Enable/disable loudness normalization (lookup stays installed).
    fn set_loudness_enabled(&self, _on: bool) {}
    /// True when loudness normalization is active.
    fn loudness_enabled(&self) -> bool {
        false
    }
    /// Install the streaming (Icecast) config; applied on next start.
    fn set_stream_config(&mut self, _config: crate::stream::StreamConfig) {}
    /// Current streaming config.
    fn stream_config(&self) -> crate::stream::StreamConfig {
        crate::stream::StreamConfig::default()
    }
    /// Start streaming (encode the program bus + push to the server).
    /// Errors surface via [`Engine::stream_state`], not here, since the
    /// connection happens on the sender thread.
    fn stream_start(&mut self) -> Result<()> {
        Ok(())
    }
    /// Stop streaming.
    fn stream_stop(&mut self) {}
    /// Live streaming state (Off/Connecting/Live/Error).
    fn stream_state(&self) -> crate::stream::StreamState {
        crate::stream::StreamState::Off
    }
    /// Counters since this stream run started.
    fn stream_stats(&self) -> crate::stream::StreamStats {
        crate::stream::StreamStats::default()
    }
    /// Queue a now-playing metadata update for the stream.
    fn set_stream_title(&self, _title: &str) {}
    /// Install the mic/line-in config; device applies on next start,
    /// level + ducking apply live when running.
    fn set_mic_config(&mut self, _config: crate::audio::MicConfig) {}
    /// Current mic config.
    fn mic_config(&self) -> crate::audio::MicConfig {
        crate::audio::MicConfig::default()
    }
    /// Start the input stream (voice feeds the program bus + stream tap).
    /// Unlike streaming, open failures are synchronous, so they surface
    /// here AND via [`Engine::mic_state`].
    fn mic_start(&mut self) -> Result<()> {
        Ok(())
    }
    /// Stop the input stream.
    fn mic_stop(&mut self) {}
    /// Live mic state (Off/Live/Error).
    fn mic_state(&self) -> crate::audio::MicState {
        crate::audio::MicState::Off
    }
    /// Mic envelope in dBFS (floored; UI level meter).
    fn mic_level_db(&self) -> f32 {
        -99.0
    }
    /// True while the music bed is audibly ducked under the mic.
    fn mic_ducking(&self) -> bool {
        false
    }
    /// Seconds into the current track (`0.0` when nothing is playing).
    fn position_secs(&self) -> f64 {
        0.0
    }
    /// Queued-but-unheard decks (auto-DJ prefetch bookkeeping).
    fn pending_count(&self) -> usize {
        0
    }
    /// Decode jobs submitted but not yet installed (sitting in the loader
    /// channel or decoding). A `queue()` that hasn't installed yet still
    /// counts as outstanding prefetch — without this, a fast poll loop
    /// re-queues the same pick once per tick until the first decode lands,
    /// stacking duplicate decks behind the live one.
    fn load_inflight(&self) -> usize {
        0
    }
    /// True when `queue()` really defers to end-of-track (cpal).
    /// Prefetch must only run here — elsewhere it would cut tracks short.
    fn has_queue(&self) -> bool {
        false
    }
}

/// Prefetch policy for Auto-DJ: queue the next pick while the current track
/// still has `horizon_secs` left, and only when nothing is already pending.
pub fn needs_prefetch(
    position_secs: f64,
    duration_secs: Option<f64>,
    pending: usize,
    has_queue: bool,
    horizon_secs: f64,
) -> bool {
    if !has_queue || pending > 0 {
        return false;
    }
    match duration_secs {
        Some(d) if d > 0.0 => d - position_secs <= horizon_secs,
        _ => false,
    }
}

// CpalEngine is the backend; see cpal_engine.rs.

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn prefetch_only_when_useful() {
        assert!(needs_prefetch(50.0, Some(55.0), 0, true, 8.0));
        assert!(!needs_prefetch(10.0, Some(55.0), 0, true, 8.0));
        assert!(!needs_prefetch(50.0, Some(55.0), 1, true, 8.0));
        assert!(!needs_prefetch(50.0, Some(55.0), 0, false, 8.0));
        assert!(!needs_prefetch(0.0, None, 0, true, 8.0));
    }
}
