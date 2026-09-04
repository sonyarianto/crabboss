//! Engine trait — common interface for audio backends.
//!
//! Lets `crabui` code against `Engine` while we migrate
//! legacy `Player` (rodio) → `CpalEngine` (cpal, see ROADMAP).

use std::path::Path;

use crate::audio::player::{PlayerState, TrackInfo};
use crate::error::Result;

/// Minimal transport + volume interface every backend must provide.
/// NOTE: no Send+Sync bound — cpal::Stream and rodio::OutputStream are
/// !Send/!Sync; UI owns the engine single-threaded (Rc/RefCell).
pub trait Engine {
    fn play(&self, path: &Path) -> Result<()>;
    /// Queue for end-of-track start (insert-after-current). Default degrades
    /// to immediate `play`; `CpalEngine` blends it in at the boundary.
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
    /// than the threshold while Playing. Backends without output metering
    /// (rodio) report false; `CpalEngine` measures the real mix bus.
    fn silence_alarm(&self) -> bool {
        false
    }
    /// Seconds of continuous silence before [`Engine::silence_alarm`] trips.
    fn set_silence_threshold_secs(&self, _secs: f32) {}
    /// Blend length for crossfades / queued takeovers.
    fn set_crossfade_secs(&self, _secs: f32) {}
}

// Legacy rodio Player already satisfies the interface.
impl Engine for crate::audio::player::Player {
    fn play(&self, path: &Path) -> Result<()> {
        crate::audio::player::Player::play(self, path)
    }
    fn pause(&self) {
        crate::audio::player::Player::pause(self)
    }
    fn resume(&self) {
        crate::audio::player::Player::resume(self)
    }
    fn stop(&self) {
        crate::audio::player::Player::stop(self)
    }
    fn toggle_play_pause(&self) {
        crate::audio::player::Player::toggle_play_pause(self)
    }
    fn set_volume(&self, vol: f32) {
        crate::audio::player::Player::set_volume(self, vol)
    }
    fn volume(&self) -> f32 {
        crate::audio::player::Player::volume(self)
    }
    fn state(&self) -> PlayerState {
        crate::audio::player::Player::state(self)
    }
    fn current_track(&self) -> Option<TrackInfo> {
        crate::audio::player::Player::current_track(self)
    }
    fn has_audio_device(&self) -> bool {
        crate::audio::player::Player::has_audio_device(self)
    }
    fn is_finished(&self) -> bool {
        crate::audio::player::Player::is_finished(self)
    }
}
