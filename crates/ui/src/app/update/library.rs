//! Library: browsing, import, health, and the loudness background
//! scan plus its per-tick pump.

use std::collections::VecDeque;
use std::path::PathBuf;
use std::sync::mpsc::{self, TryRecvError};

use crabcore::audio::{MAX_GAIN_DB, TARGET_MAX_LUFS, TARGET_MIN_LUFS};

use super::super::App;
use crate::widgets::track_label;

pub(crate) fn search_changed(state: &mut App, q: String) {
    state.lib_search = q;
    state.lib_selected = None;
    state.refresh_library();
}

pub(crate) fn kind_changed(state: &mut App, kind: Option<crabcore::library::TrackKind>) {
    state.lib_kind = kind;
    state.lib_selected = None;
    state.refresh_library();
}

pub(crate) fn missing_toggled(state: &mut App, only: bool) {
    state.lib_missing_only = only;
    state.lib_selected = None;
    state.refresh_library();
}

pub(crate) fn track_selected(state: &mut App, i: usize) {
    state.lib_selected = Some(i);
}

pub(crate) fn track_play(state: &mut App, i: usize) {
    let track = state.lib_tracks.get(i).cloned();
    if let Some(track) = track {
        let path = PathBuf::from(&track.file_path);
        tracing::info!("Playing track: {:?}", path);
        match state.player.play(&path) {
            Ok(()) => {
                let _ = state.library.record_play(&track.id, track.duration_secs);
                state.auto_continue = true;
                state.lib_selected = Some(i);
                state.is_playing = true;
                state.now_title = track_label(&track);
                state.now_artist = track.artist.clone().unwrap_or_default();
                state.engine_track = Some(path);
                // A manual play discards any prefetched deck, so its
                // "Up next" label dies with it.
                state.up_next.clear();
                state.pending_source = None;
            }
            Err(e) => {
                tracing::error!("Failed to play: {}", e);
                state.lib_status = format!("Play failed: {}", e);
            }
        }
    }
}

pub(crate) fn import_files(state: &mut App) {
    if state.import_active {
        state.lib_status = "Import already running...".into();
        return;
    }
    let files = rfd::FileDialog::new()
        .set_title("Import audio files")
        .add_filter(
            "Audio",
            &[
                "mp3", "flac", "wav", "ogg", "oga", "aac", "m4a", "opus", "aiff", "wv",
            ],
        )
        .pick_files();
    let Some(files) = files else {
        return;
    };
    if files.is_empty() {
        return;
    }
    tracing::info!("Importing {} files...", files.len());
    state.import_active = true;
    state.import_total = files.len();
    state.import_added = 0;
    state.import_skipped = 0;
    state.import_pending = VecDeque::from(files);
    state.lib_status = format!("Importing 0/{}...", state.import_total);
}

pub(crate) fn health_check(state: &mut App) {
    tracing::info!("Manual library health scan");
    let fixed = state.library.reclassify_all().unwrap_or(0);
    let missing = state.library.missing_files().unwrap_or_default();
    let pending = state.library.count_missing_loudness().unwrap_or(0);
    for t in &missing {
        tracing::warn!("Missing file: {}", t.file_path);
    }
    let mut parts = if missing.is_empty() {
        vec!["All files OK".to_string()]
    } else {
        vec![format!("{} files missing (see log)", missing.len())]
    };
    if fixed > 0 {
        parts.push(format!("re-labeled {fixed}"));
    }
    if pending > 0 {
        parts.push(format!("{} to analyze", pending));
    }
    state.lib_status = parts.join(" - ");
    state.refresh_library();
}

pub(crate) fn loudness_scan(state: &mut App) {
    let target = state.settings.loudness_target_lufs;
    state.start_loudness_scan(true);
    let _ = target;
}

impl App {
    pub(crate) fn refresh_library(&mut self) {
        // Live-state screen: on a read failure keep the last-known list
        // and say so, instead of showing an empty library as if valid.
        let mut tracks = match if self.lib_search.trim().is_empty() {
            self.library.get_all_tracks()
        } else {
            self.library.search(self.lib_search.trim())
        } {
            Ok(t) => t,
            Err(e) => {
                let msg = format!("Library read failed: {e}");
                tracing::warn!("{msg}");
                self.lib_status = msg;
                return;
            }
        };
        if let Some(kind) = self.lib_kind {
            tracks.retain(|t| t.kind == kind);
        }
        if self.lib_missing_only {
            tracks.retain(|t| !PathBuf::from(&t.file_path).is_file());
        }
        match self.library.get_all_tracks() {
            Ok(all) => {
                self.lib_total = all.len();
            }
            Err(e) => {
                let msg = format!("Library count failed: {e}");
                tracing::warn!("{msg}");
                self.lib_status = msg;
            }
        }
        self.lib_tracks = tracks;
        self.track_count = self.lib_total;
        if let Some(sel) = self.lib_selected {
            if sel >= self.lib_tracks.len() {
                self.lib_selected = None;
            }
        }
    }

    // -- Loudness scan -------------------------------------------------------
    pub(crate) fn start_loudness_scan(&mut self, announce_empty: bool) {
        if self.scanning {
            return;
        }
        let jobs: Vec<(String, String, String)> = self
            .library
            .tracks_missing_loudness(usize::MAX)
            .unwrap_or_default()
            .into_iter()
            .map(|t| (t.id, t.file_path, t.file_name))
            .collect();
        if jobs.is_empty() {
            if announce_empty {
                self.lib_status = "All tracks analyzed".into();
            }
            return;
        }
        let total = jobs.len();
        let target_lufs = self
            .settings
            .loudness_target_lufs
            .clamp(TARGET_MIN_LUFS, TARGET_MAX_LUFS);
        self.scanning = true;
        self.scan_done = 0;
        self.scan_total = total;
        let (tx, rx) = mpsc::channel();
        self.scan_rx = Some(rx);
        self.lib_status = format!("Loudness scan starting... ({total} to go)");
        tracing::info!("Loudness scan started ({total} tracks, background thread)");
        if std::thread::Builder::new()
            .name("loudness-scan".into())
            .spawn(move || {
                for (id, path, file_name) in jobs {
                    let p = PathBuf::from(&path);
                    let (lufs, gain_db) = if !p.is_file() {
                        (-70.0, 0.0)
                    } else {
                        match crabcore::audio::analyze_file(&p) {
                            Ok(a) => (
                                a.integrated_lufs,
                                (target_lufs - a.integrated_lufs).clamp(-MAX_GAIN_DB, MAX_GAIN_DB),
                            ),
                            Err(e) => {
                                tracing::warn!("Loudness failed for {file_name}: {e}");
                                (-70.0, 0.0)
                            }
                        }
                    };
                    if tx
                        .send(crate::widgets::LoudnessDone {
                            id,
                            file_name,
                            lufs,
                            gain_db,
                        })
                        .is_err()
                    {
                        break;
                    }
                }
            })
            .is_err()
        {
            tracing::error!("Loudness scan: failed to spawn worker thread");
            self.scanning = false;
            self.scan_rx = None;
            self.lib_status = "Loudness scan failed to start".into();
        }
    }

    pub(crate) fn pump_loudness(&mut self) {
        if !self.scanning {
            return;
        }
        let (mut batch, mut worker_gone) = (Vec::new(), false);
        if let Some(rx) = self.scan_rx.as_mut() {
            loop {
                match rx.try_recv() {
                    Ok(m) => batch.push(m),
                    Err(TryRecvError::Empty) => break,
                    Err(TryRecvError::Disconnected) => {
                        worker_gone = true;
                        break;
                    }
                }
            }
        } else {
            return;
        }
        if batch.is_empty() && !worker_gone {
            return;
        }
        for m in &batch {
            let _ = self.library.set_loudness(&m.id, m.lufs, m.gain_db);
            tracing::info!(
                "Loudness {}: {:.1} LUFS -> {:+.1} dB",
                m.file_name,
                m.lufs,
                m.gain_db
            );
        }
        self.scan_done += batch.len();
        if self.scan_done >= self.scan_total || worker_gone {
            self.scanning = false;
            self.scan_rx = None;
            self.refresh_library();
            self.lib_status = format!("Loudness scan complete ({} analyzed)", self.scan_done);
            tracing::info!(
                "Loudness scan complete: {}/{} analyzed",
                self.scan_done,
                self.scan_total
            );
        } else {
            self.lib_status = format!(
                "Analyzing loudness... {}/{}",
                self.scan_done, self.scan_total
            );
        }
    }

    // -- Import pump (one file per tick so the UI never freezes) -------------
    pub(crate) fn pump_import(&mut self) {
        if !self.import_active {
            return;
        }
        let Some(f) = self.import_pending.pop_front() else {
            self.import_active = false;
            self.refresh_library();
            self.lib_status = if self.import_skipped > 0 {
                format!(
                    "Imported {}, skipped {}",
                    self.import_added, self.import_skipped
                )
            } else {
                format!("Imported {}", self.import_added)
            };
            tracing::info!(
                "Import complete: {} added, {} skipped",
                self.import_added,
                self.import_skipped
            );
            if self.import_added > 0 {
                self.start_loudness_scan(false);
            }
            return;
        };
        let done = self.import_total - self.import_pending.len();
        match self.library.add_track(&f) {
            Ok(t) => {
                tracing::info!("Imported {} as {:?}", f.display(), t.kind);
                self.import_added += 1;
            }
            Err(e) => {
                tracing::warn!("Skipping {}: {}", f.display(), e);
                self.import_skipped += 1;
            }
        }
        self.lib_status = format!("Importing {done}/{}...", self.import_total);
    }
}
