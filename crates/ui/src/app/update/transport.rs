//! Transport: play/pause/stop/next/prev, volume, and the Auto-DJ
//! switch plus its pick/play helpers.

use std::path::PathBuf;

use crabcore::audio::PlayerState;
use crabcore::library::Track;

use super::super::App;
use crate::widgets::track_label;

pub(crate) fn play(state: &mut App) {
    match state.player.state() {
        PlayerState::Paused => {
            state.player.resume();
            state.is_playing = true;
        }
        PlayerState::Playing | PlayerState::Buffering => {
            state.is_playing = true;
        }
        PlayerState::Stopped => {
            // Cold start: a bare Play with nothing loaded was a
            // silent no-op (the tick overwrote is_playing right
            // back). Play the selected track, else an Auto-DJ pick;
            // continuity follows the toggle, not the button.
            if let Some(track) = state
                .lib_selected
                .and_then(|i| state.lib_tracks.get(i))
                .cloned()
            {
                let path = PathBuf::from(&track.file_path);
                match state.player.play(&path) {
                    Ok(()) => {
                        let _ = state.library.record_play(&track.id, track.duration_secs);
                        state.auto_continue = state.autodj;
                        state.is_playing = true;
                        state.now_title = track_label(&track);
                        state.now_artist = track.artist.clone().unwrap_or_default();
                        state.engine_track = Some(path);
                    }
                    Err(e) => {
                        tracing::error!("Failed to play: {}", e);
                        state.lib_status = format!("Play failed: {}", e);
                    }
                }
            } else {
                state.autodj_play_now();
                state.auto_continue = state.autodj;
            }
        }
    }
}

pub(crate) fn pause(state: &mut App) {
    state.player.pause();
    state.is_playing = false;
}

pub(crate) fn stop(state: &mut App) {
    state.player.stop();
    state.auto_continue = false;
    state.is_playing = false;
    state.now_title = "No track loaded".into();
    state.now_artist.clear();
    state.up_next.clear();
    state.engine_track = None;
    state.pending_source = None;
}

pub(crate) fn next(state: &mut App) {
    state.auto_continue = true;
    state.autodj_play_now();
}

pub(crate) fn prev(state: &mut App) {
    if let Some(cur) = state.player.current_track().map(|t| t.path) {
        match state.player.play(&cur) {
            Ok(()) => {
                state.auto_continue = true;
                state.is_playing = true;
                state.engine_track = Some(cur);
                state.up_next.clear();
                state.pending_source = None;
            }
            Err(e) => tracing::error!("Prev failed: {}", e),
        }
    }
}

pub(crate) fn set_volume(state: &mut App, v: f32) {
    state.volume = v.clamp(0.0, 1.0);
    state.player.set_volume(state.volume);
}

pub(crate) fn toggle_autodj(state: &mut App, en: bool) {
    state.autodj = en;
    state.settings.autodj = en;
    state.save_settings();
    tracing::info!("Auto-DJ {}", if en { "ON" } else { "OFF" });
    if !en {
        state.up_next.clear();
    } else if state.player.state() == PlayerState::Stopped {
        // Kick off playback: without a first track there is never
        // an EOF transition for continuity to continue from.
        state.autodj_play_now();
        state.auto_continue = true;
    }
}

impl App {
    // -- Auto-DJ ------------------------------------------------------------
    pub(crate) fn autodj_pick(&mut self) -> Option<Track> {
        crabcore::playlist::generate_next(
            &self.library,
            &self.autodj_cfg(),
            &mut self.autodj_history,
        )
        .ok()
        .flatten()
    }

    /// One-pick config for Auto-DJ rotation (current hour/weekday).
    pub(crate) fn autodj_cfg(&self) -> crabcore::playlist::GenConfig {
        let now = chrono::Local::now();
        crabcore::playlist::GenConfig {
            target_tracks: 1,
            hour: now.format("%H").to_string().parse().unwrap_or(12),
            weekday: now.format("%a").to_string(),
            ..Default::default()
        }
    }

    pub(crate) fn autodj_play_now(&mut self) {
        let pick = self.autodj_pick();
        let Some(pick) = pick else {
            tracing::warn!("Auto-DJ: library is empty");
            return;
        };
        let path = PathBuf::from(&pick.file_path);
        if !path.is_file() {
            tracing::warn!("Auto-DJ: file missing: {}", pick.file_path);
            return;
        }
        match self.player.play(&path) {
            Ok(()) => {
                let _ = self.library.record_play(&pick.id, pick.duration_secs);
                let label = track_label(&pick);
                tracing::info!("Auto-DJ playing: {}", label);
                self.is_playing = true;
                self.now_title = label;
                self.now_artist = pick
                    .artist
                    .clone()
                    .filter(|a| !a.trim().is_empty())
                    .unwrap_or_else(|| "Auto-DJ".into());
                self.up_next.clear();
                self.engine_track = Some(path);
                // Direct play discards any pending queue (and its source).
                // (History already advanced inside generate_next.)
                self.pending_source = None;
            }
            Err(e) => tracing::error!("Auto-DJ play failed: {}", e),
        }
    }
}
