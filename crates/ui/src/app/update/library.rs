//! Library: browsing, import, health, and the loudness background
//! scan plus its per-tick pump.

use std::collections::VecDeque;
use std::path::PathBuf;
use std::sync::mpsc::{self, TryRecvError};

use crabcore::audio::{MAX_GAIN_DB, TARGET_MAX_LUFS, TARGET_MIN_LUFS};

use super::super::{App, Message};
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

pub(crate) fn dupes_toggled(state: &mut App, only: bool) {
    state.lib_dupes_only = only;
    state.lib_selected = None;
    state.refresh_library();
    if only {
        let n = state.lib_dupe_groups;
        let shown = state.lib_tracks.len();
        state.lib_status = if n == 0 {
            "No possible duplicates found".into()
        } else {
            format!(
                "{n} possible duplicate group{} ({shown} tracks) - review only, nothing auto-deleted",
                if n == 1 { "" } else { "s" }
            )
        };
    }
}

pub(crate) fn track_selected(state: &mut App, i: usize) {
    state.lib_selected = Some(i);
}

/// Cue (PFL) preview on the private bus (B2 Phase 3): program untouched,
/// no `record_play` (preview must not pollute airplay reports), no
/// `auto_continue` / `engine_track` / `up_next` side effects. Failures
/// surface inline in `lib_status` with an actionable hint.
pub(crate) fn cue_play(state: &mut App, i: usize) {
    let Some(track) = state.lib_tracks.get(i).cloned() else {
        return;
    };
    let path = PathBuf::from(&track.file_path);
    if !path.is_file() {
        state.lib_status = format!("Cue failed: file missing: {}", track.file_path);
        return;
    }
    match state.player.cue_play(&path) {
        Ok(()) => {
            tracing::info!("Cue preview: {:?}", path);
            state.lib_selected = Some(i);
            state.cue_status = state.player.cue_state().label();
            state.lib_status = format!("Cue: {} (headphones, program untouched)", track_label(&track));
        }
        Err(e) => {
            let msg = e.to_string();
            tracing::warn!("Cue failed: {msg}");
            state.cue_status = state.player.cue_state().label();
            state.lib_status = if msg.contains("no cue device") || msg.contains("unavailable") {
                "Cue: no device — pick headphones in Settings > Audio Device".into()
            } else {
                format!("Cue failed: {msg}")
            };
        }
    }
}

pub(crate) fn cue_stop(state: &mut App) {
    state.player.cue_stop();
    state.cue_status = state.player.cue_state().label();
    if state.lib_status.starts_with("Cue:") || state.lib_status.starts_with("Cue ") {
        state.lib_status.clear();
    }
}

pub(crate) fn import_files(state: &mut App) -> iced::Task<Message> {
    if state.import_active {
        state.lib_status = "Import already running...".into();
        return iced::Task::none();
    }
    // Async dialog: `update` must return immediately so the iced event
    // loop keeps pumping while the native dialog enumerates folders.
    // The old sync `pick_files()` blocked the UI thread here, which on
    // Windows stalls COM + makes the dialog sit on "Working on it...".
    let mut dialog = rfd::AsyncFileDialog::new()
        .set_title("Import audio files")
        .add_filter(
            "Audio",
            &[
                "mp3", "flac", "wav", "ogg", "oga", "aac", "m4a", "opus", "aiff", "wv",
            ],
        );
    if let Some(dir) = import_start_dir(state) {
        dialog = dialog.set_directory(dir);
    }
    state.lib_status = "Choose audio files in the dialog...".into();
    iced::Task::perform(dialog.pick_files(), |handles| {
        let files: Vec<PathBuf> = handles
            .map(|hs| hs.into_iter().map(|h| h.path().to_path_buf()).collect())
            .unwrap_or_default();
        Message::ImportFilesPicked(files)
    })
}

pub(crate) fn import_files_picked(state: &mut App, files: Vec<PathBuf>) {
    if files.is_empty() {
        // Cancelled: only clear our own "choose..." hint, never wipe a
        // real status (e.g. an import that started via auto-sync meanwhile).
        if state.lib_status == "Choose audio files in the dialog..." {
            state.lib_status.clear();
        }
        return;
    }
    if let Some(parent) = files[0].parent() {
        if parent.is_dir() {
            state.last_import_dir = Some(parent.to_path_buf());
        }
    }
    tracing::info!("Importing {} files...", files.len());
    if state.import_active {
        // Auto-sync (or another pick) started while the dialog was open:
        // append instead of resetting counters.
        state.import_total += files.len();
        state.import_pending.extend(files);
        let done = state.import_total - state.import_pending.len();
        state.lib_status = format!("Importing {done}/{}...", state.import_total);
        return;
    }
    state.import_active = true;
    state.import_total = files.len();
    state.import_added = 0;
    state.import_skipped = 0;
    state.import_pending = VecDeque::from(files);
    state.lib_status = format!("Importing 0/{}...", state.import_total);
}

/// Folder the import dialog should open in. Never returns Quick Access /
/// This PC: an explicit existing dir keeps Windows enumeration fast
/// (the "Working on it..." hang is almost always a virtual-folder /
/// disconnected-network-drive enumeration).
fn import_start_dir(state: &App) -> Option<PathBuf> {
    if let Some(d) = &state.last_import_dir {
        if d.is_dir() {
            return Some(d.clone());
        }
    }
    for f in &state.settings.watch_folders {
        if f.is_dir() {
            return Some(f.clone());
        }
    }
    if let Some(t) = state.lib_tracks.first() {
        let p = PathBuf::from(&t.file_path);
        if let Some(parent) = p.parent() {
            if parent.is_dir() {
                return Some(parent.to_path_buf());
            }
        }
    }
    if let Ok(profile) = std::env::var("USERPROFILE") {
        let music = PathBuf::from(&profile).join("Music");
        if music.is_dir() {
            return Some(music);
        }
        let p = PathBuf::from(&profile);
        if p.is_dir() {
            return Some(p);
        }
    }
    std::env::current_dir().ok().filter(|p| p.is_dir())
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

pub(crate) fn autosync_toggled(state: &mut App, on: bool) {
    state.settings.auto_sync_enabled = on;
    state.save_settings();
    if on {
        // Fire promptly on the next tick when folders are watched.
        state.last_auto_sync = None;
        state.lib_status = if state.settings.watch_folders.is_empty() {
            "Auto-sync on: add a watch folder first".into()
        } else {
            format!(
                "Auto-sync on: every {} min",
                state.settings.auto_sync_interval_mins
            )
        };
    } else {
        state.lib_status = "Auto-sync off".into();
    }
}

pub(crate) fn autosync_interval_step(state: &mut App, up: bool) {
    const STEP_MINS: u32 = 15;
    let cur = state.settings.auto_sync_interval_mins;
    let next = if up {
        cur.saturating_add(STEP_MINS)
    } else {
        cur.saturating_sub(STEP_MINS)
    };
    state.settings.auto_sync_interval_mins = next.clamp(
        crabcore::settings::AUTO_SYNC_MIN_MINUTES,
        crabcore::settings::AUTO_SYNC_MAX_MINUTES,
    );
    state.save_settings();
    state.lib_status = format!(
        "Auto-sync every {} min",
        state.settings.auto_sync_interval_mins
    );
}

pub(crate) fn watch_folder_add(state: &mut App) {
    let Some(folder) = rfd::FileDialog::new()
        .set_title("Watch folder for new audio")
        .pick_folder()
    else {
        return;
    };
    if state.settings.watch_folders.iter().any(|f| f == &folder) {
        state.lib_status = "Folder already watched".into();
        return;
    }
    state.settings.watch_folders.push(folder);
    // Reuse the load-time cleanup (dedupe/order), no filesystem checks.
    state.settings = std::mem::take(&mut state.settings).sanitized();
    state.save_settings();
    let n = state.settings.watch_folders.len();
    state.lib_status = format!("Watching {} folder{}", n, if n == 1 { "" } else { "s" });
}

pub(crate) fn watch_folder_remove(state: &mut App, i: usize) {
    if i < state.settings.watch_folders.len() {
        state.settings.watch_folders.remove(i);
        state.save_settings();
    }
    let n = state.settings.watch_folders.len();
    state.lib_status = if n == 0 {
        "No watch folders".into()
    } else {
        format!("Watching {} folder{}", n, if n == 1 { "" } else { "s" })
    };
}

/// Timer fire (called from `on_tick` after the pumps): start one
/// background walk when due and idle. The walk never touches the
/// database; results come back over `sync_rx` for [`App::pump_autosync`].
pub(crate) fn maybe_autosync(state: &mut App) {
    let elapsed = state.last_auto_sync.map(|t| t.elapsed().as_secs());
    let busy = state.import_active || state.scanning || state.syncing;
    let s = &state.settings;
    if !crate::rules::autosync_due(
        s.auto_sync_enabled,
        !s.watch_folders.is_empty(),
        busy,
        elapsed,
        u64::from(s.auto_sync_interval_mins) * 60,
    ) {
        return;
    }
    let folders = s.watch_folders.clone();
    let (tx, rx) = mpsc::channel();
    state.sync_rx = Some(rx);
    state.syncing = true;
    state.last_auto_sync = Some(std::time::Instant::now());
    tracing::info!("Auto-sync pass started ({} folders)", folders.len());
    if std::thread::Builder::new()
        .name("folder-sync".into())
        .spawn(move || {
            let (scanned, paths) = crabcore::library::collect_audio_files(&folders);
            let _ = tx.send(crate::widgets::SyncFound {
                folders: scanned,
                paths,
            });
        })
        .is_err()
    {
        tracing::error!("Auto-sync: failed to spawn worker thread");
        state.syncing = false;
        state.sync_rx = None;
        state.lib_status = "Auto-sync failed to start".into();
    }
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
                if self.lib_dupes_only {
                    // Duplicates are a library-wide property: group the
                    // full set, then narrow the already-filtered list to
                    // members. Grouping runs only while the filter is on.
                    let groups = crabcore::library::find_duplicate_groups(
                        &all,
                        crabcore::library::DURATION_TOLERANCE_SECS,
                    );
                    self.lib_dupe_groups = groups.len();
                    let ids = crabcore::library::duplicate_ids(&groups);
                    tracks.retain(|t| ids.contains(&t.id));
                } else {
                    self.lib_dupe_groups = 0;
                }
            }
            Err(e) => {
                let msg = format!("Library count failed: {e}");
                tracing::warn!("{msg}");
                self.lib_status = msg;
                self.lib_dupe_groups = 0;
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

    // -- Auto-sync reap: file a finished walk into the import queue --------
    pub(crate) fn pump_autosync(&mut self) {
        if !self.syncing {
            return;
        }
        let msg = match self.sync_rx.as_mut() {
            Some(rx) => match rx.try_recv() {
                Ok(m) => m,
                Err(TryRecvError::Empty) => return,
                Err(TryRecvError::Disconnected) => {
                    self.syncing = false;
                    self.sync_rx = None;
                    return;
                }
            },
            None => return,
        };
        self.syncing = false;
        self.sync_rx = None;
        // Known paths = library + anything already queued, so the counts
        // stay honest (`add_track` itself is INSERT OR IGNORE and would
        // otherwise report re-queued files as fresh imports).
        let mut known: std::collections::HashSet<String> = self
            .library
            .get_all_tracks()
            .unwrap_or_default()
            .into_iter()
            .map(|t| t.file_path)
            .collect();
        known.extend(
            self.import_pending
                .iter()
                .map(|p| p.to_string_lossy().into_owned()),
        );
        let mut fresh = Vec::new();
        for p in &msg.paths {
            if known.insert(p.to_string_lossy().into_owned()) {
                fresh.push(p.clone());
            }
        }
        if fresh.is_empty() {
            self.lib_status = format!("Auto-sync: no new files ({} checked)", msg.paths.len());
            tracing::info!("Auto-sync pass: {} checked, nothing new", msg.paths.len());
            return;
        }
        let n = fresh.len();
        self.import_pending.extend(fresh);
        if !self.import_active {
            self.import_active = true;
            self.import_total = n;
            self.import_added = 0;
            self.import_skipped = 0;
        } else {
            self.import_total += n;
        }
        self.lib_status = format!(
            "Auto-sync found {n} new file{}...",
            if n == 1 { "" } else { "s" }
        );
        tracing::info!(
            "Auto-sync pass: {n} new files queued ({} folders)",
            msg.folders
        );
    }
}
