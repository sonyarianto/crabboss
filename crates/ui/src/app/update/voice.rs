//! Voice tracking (§1.4): record mic takes, fire them now or queue next.
//!
//! Takes live outside the library on purpose: no music reports, no
//! Auto-DJ rotations, no loudness scans. Playback reuses the normal
//! `play`/`queue` program paths (crossfading included); only the
//! labels are voice-aware (see the tick reconciler).

use std::path::PathBuf;

use crabcore::audio::MicState;
use crabcore::voice;

use super::super::App;

/// Rebuild the take list + recording mirror (boot, restore, every
/// take lifecycle event).
impl App {
    pub(crate) fn refresh_voice(&mut self) {
        self.voice_list = self.voice_manager.list_all().unwrap_or_default();
        self.voice_recording = self.player.voice_recording();
        if !self.voice_recording {
            self.voice_rec_elapsed = 0.0;
        }
    }
}

pub(crate) fn record_toggle(state: &mut App) {
    if state.player.voice_recording() {
        stop_take(state, false);
        return;
    }
    if !state.player.has_audio_device() {
        state.voice_status = "Record: no output device".into();
        return;
    }
    if state.player.mic_state() != MicState::Live {
        state.voice_status = "Record: mic is off — start it in Settings → Microphone".into();
        return;
    }
    if let Err(e) = std::fs::create_dir_all(&state.voice_dir) {
        state.voice_status = format!("Record: cannot use take folder: {e}");
        return;
    }
    // Unique filename (a second take within the same second gets -2…).
    let now = chrono::Local::now();
    let stem = voice::suggest_filename(now)
        .strip_suffix(".wav")
        .unwrap_or("voice")
        .to_string();
    let mut n = 0u32;
    let path = loop {
        let file = if n == 0 {
            format!("{stem}.wav")
        } else {
            format!("{stem}-{n}.wav")
        };
        let p = state.voice_dir.join(file);
        if !p.exists() {
            break p;
        }
        n += 1;
    };
    match state.player.voice_record_start(&path) {
        Ok(()) => {
            state.voice_take_path = Some(path.clone());
            state.voice_recording = true;
            state.voice_rec_elapsed = 0.0;
            state.voice_status = format!("● Recording to {} …", path.display());
            tracing::info!("Voice record started: {}", path.display());
        }
        Err(e) => {
            state.voice_status = format!("Record failed: {e}");
        }
    }
}

/// Stop the rolling take and file it. `auto` marks the 10-minute
/// budget stop so the status reads truthfully.
pub(crate) fn stop_take(state: &mut App, auto: bool) {
    let take_path = state.voice_take_path.clone();
    match state.player.voice_record_stop() {
        Ok(take) => {
            let now = chrono::Local::now();
            match take_path {
                Some(path) => {
                    let name = voice::suggest_name(now);
                    match state.voice_manager.create(
                        &name,
                        &path.to_string_lossy(),
                        take.duration_secs,
                        take.sample_rate,
                    ) {
                        Ok(_) => {
                            state.voice_status = if auto {
                                format!(
                                    "Take auto-stopped at 10:00 (limit), saved: {name} ({:.1}s)",
                                    take.duration_secs
                                )
                            } else {
                                format!("Take saved: {name} ({:.1}s)", take.duration_secs)
                            };
                        }
                        Err(e) => {
                            state.voice_status = format!("Take recorded but not filed: {e}");
                        }
                    }
                }
                None => {
                    state.voice_status = "Take stopped but its path was lost".into();
                }
            }
        }
        Err(e) => {
            state.voice_status = format!("Stop failed: {e}");
        }
    }
    state.voice_take_path = None;
    state.refresh_voice();
}

pub(crate) fn fire_now(state: &mut App, id: String) {
    let Some(v) = state.voice_list.iter().find(|v| v.id == id).cloned() else {
        state.voice_status = "Take is gone — list refreshed".into();
        state.refresh_voice();
        return;
    };
    let path = PathBuf::from(&v.file_path);
    if !path.is_file() {
        state.voice_status = format!("Take file missing: {}", v.file_path);
        return;
    }
    match state.player.play(&path) {
        Ok(()) => {
            // Voice is not a library track: no `record_play` (takes
            // must not pollute music reports), labels set directly.
            state.auto_continue = state.autodj;
            state.is_playing = true;
            state.now_title = format!("🎙 {}", v.name);
            state.now_artist = "Voice track".into();
            state.engine_track = Some(path.clone());
            state.up_next.clear();
            state.pending_source = None;
            state.voice_live = Some((path, v.name.clone()));
            state.voice_queued = None;
            state.player.set_stream_title(&v.name);
            state.voice_status = format!("On air: {}", v.name);
            tracing::info!("Voice on air now: {}", v.name);
        }
        Err(e) => {
            state.voice_status = format!("Voice play failed: {e}");
        }
    }
}

pub(crate) fn queue_next(state: &mut App, id: String) {
    let Some(v) = state.voice_list.iter().find(|v| v.id == id).cloned() else {
        state.voice_status = "Take is gone — list refreshed".into();
        state.refresh_voice();
        return;
    };
    let path = PathBuf::from(&v.file_path);
    if !path.is_file() {
        state.voice_status = format!("Take file missing: {}", v.file_path);
        return;
    }
    match state.player.queue(&path) {
        Ok(()) => {
            state.pending_source = Some("Voice".into());
            state.voice_queued = Some((path, v.name.clone()));
            state.up_next = format!("🎙 {}", v.name);
            state.voice_status = format!("Queued next: {}", v.name);
            tracing::info!("Voice queued next: {}", v.name);
        }
        Err(e) => {
            state.voice_status = format!("Voice queue failed: {e}");
        }
    }
}

/// Call on every direct music-deck install (and stop): a superseded
/// deck takes its voice labels with it. Deferred `queue` paths never
/// call this — a queued voice ahead in line still promotes normally.
pub(crate) fn clear_voice_labels(state: &mut App) {
    state.voice_live = None;
    state.voice_queued = None;
}

pub(crate) fn delete_take(state: &mut App, id: String) {
    match state.voice_manager.delete(&id) {
        Ok(()) => {
            state.voice_status = "Take deleted".into();
        }
        Err(e) => {
            state.voice_status = format!("Delete failed: {e}");
        }
    }
    state.refresh_voice();
}
