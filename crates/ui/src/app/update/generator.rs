//! Rotation generator panel (Home): fire several daypart rotations
//! per session into saved playlists. The rule engine (`generate`) is
//! done; this is the multi-preset UI over it. Presets are a fixed
//! daypart set for now — custom named/persisted presets are the
//! follow-up, not this PR.

use std::path::PathBuf;

use crabcore::library::{Library, TrackKind};
use crabcore::playlist::{GenConfig, PlaylistItem, PlaylistManager};

use super::super::App;
use crate::widgets::track_label;

/// One fireable daypart row: display name + default hour.
pub(crate) struct DaypartPreset {
    pub(crate) name: &'static str,
    pub(crate) hour: u8,
}

pub(crate) const DAYPARTS: [DaypartPreset; 4] = [
    DaypartPreset {
        name: "Morning",
        hour: 8,
    },
    DaypartPreset {
        name: "Midday",
        hour: 12,
    },
    DaypartPreset {
        name: "Evening",
        hour: 18,
    },
    DaypartPreset {
        name: "Night",
        hour: 22,
    },
];

/// Default tracks per rotation (session knob, not persisted).
pub(crate) const DEFAULT_GEN_COUNT: usize = 15;

/// One job: which daypart row with which rules.
pub(crate) struct FireJob {
    pub(crate) daypart: String,
    pub(crate) cfg: GenConfig,
}

/// One row of the Home "Saved Playlists" list (A1: playlists are no
/// longer write-only — this is what the list renders and fires).
pub(crate) struct SavedPlaylist {
    pub(crate) id: String,
    pub(crate) name: String,
    pub(crate) tracks: usize,
}

/// One expanded row of the manual builder (B): stored-order item with
/// its display label. `missing` marks files the fire path would skip.
pub(crate) struct PlaylistDetailItem {
    pub(crate) label: String,
    pub(crate) missing: bool,
}

/// One persisted rotation.
#[derive(Debug, PartialEq, Eq)]
pub(crate) struct FireResult {
    pub(crate) playlist: String,
    pub(crate) music: usize,
    pub(crate) jingles: usize,
}

/// Saved playlist name. The stamp is injected so tests pin it without
/// touching the clock.
pub(crate) fn playlist_name(daypart: &str, stamp: &str) -> String {
    format!("{daypart} {stamp}")
}

/// Generate + persist every job. Takes the managers so tests run it on
/// temp DBs without App/GUI. One bad job (empty library still saves an
/// empty rotation, like the scheduler path) never vetoes the rest.
pub(crate) fn fire_rotations(
    library: &Library,
    playlists: &PlaylistManager,
    jobs: &[FireJob],
    stamp: &str,
) -> Vec<Result<FireResult, String>> {
    jobs.iter()
        .map(|job| {
            let rotation = crabcore::playlist::generate(library, &job.cfg).unwrap_or_default();
            let music = rotation
                .iter()
                .filter(|t| t.kind == TrackKind::Music)
                .count();
            let jingles = rotation
                .iter()
                .filter(|t| t.kind == TrackKind::Jingle)
                .count();
            let name = playlist_name(&job.daypart, stamp);
            let pl = playlists
                .create(&name, Some("Generated rotation"))
                .map_err(|e| format!("{}: {e}", job.daypart))?;
            for t in &rotation {
                let _ = playlists.add_track(
                    &pl.id,
                    &t.id,
                    t.kind == TrackKind::Jingle,
                    t.kind == TrackKind::Ad,
                );
            }
            tracing::info!("Generated playlist '{name}' ({music} music + {jingles} jingles)");
            Ok(FireResult {
                playlist: name,
                music,
                jingles,
            })
        })
        .collect()
}

pub(crate) fn hour_step(state: &mut App, i: usize, up: bool) {
    if let Some(h) = state.gen_hours.get_mut(i) {
        *h = if up {
            h.saturating_add(1).min(23)
        } else {
            h.saturating_sub(1)
        };
    }
}

pub(crate) fn count_step(state: &mut App, i: usize, up: bool) {
    const STEP: usize = 5;
    if let Some(c) = state.gen_counts.get_mut(i) {
        *c = if up {
            c.saturating_add(STEP).min(60)
        } else {
            c.saturating_sub(STEP).max(STEP)
        };
    }
}

fn job_for(state: &App, i: usize) -> Option<FireJob> {
    let preset = DAYPARTS.get(i)?;
    Some(FireJob {
        daypart: preset.name.to_string(),
        cfg: GenConfig {
            target_tracks: state
                .gen_counts
                .get(i)
                .copied()
                .unwrap_or(DEFAULT_GEN_COUNT),
            hour: state.gen_hours.get(i).copied().unwrap_or(preset.hour),
            weekday: chrono::Local::now().format("%a").to_string(),
            ..Default::default()
        },
    })
}

fn apply_results(state: &mut App, results: Vec<Result<FireResult, String>>) {
    let mut saved = Vec::new();
    let mut failed = Vec::new();
    for r in results {
        match r {
            Ok(f) => saved.push(format!("{} ({}+{})", f.playlist, f.music, f.jingles)),
            Err(e) => failed.push(e),
        }
    }
    state.playlist_count = state.playlist_manager.list_all().unwrap_or_default().len();
    state.refresh_saved_playlists();
    state.gen_status = match (saved.len(), failed.len()) {
        (0, _) => format!("Nothing saved: {}", failed.join("; ")),
        (1, 0) => format!("Saved '{}'", saved[0]),
        _ => {
            let mut parts = vec![format!(
                "Saved {}/{} rotations",
                saved.len(),
                saved.len() + failed.len()
            )];
            parts.extend(saved);
            parts.extend(failed.iter().map(|e| format!("FAILED {e}")));
            parts.join("; ")
        }
    };
}

pub(crate) fn fire_one(state: &mut App, i: usize) {
    let Some(job) = job_for(state, i) else {
        return;
    };
    let stamp = chrono::Local::now().format("%H:%M").to_string();
    let results = fire_rotations(&state.library, &state.playlist_manager, &[job], &stamp);
    apply_results(state, results);
}

pub(crate) fn fire_all(state: &mut App) {
    let stamp = chrono::Local::now().format("%H:%M").to_string();
    let jobs: Vec<FireJob> = (0..DAYPARTS.len())
        .filter_map(|i| job_for(state, i))
        .collect();
    let results = fire_rotations(&state.library, &state.playlist_manager, &jobs, &stamp);
    apply_results(state, results);
}

/// One resolved playlist entry: (file path, track id, duration, label).
pub(crate) type ResolvedItem = (PathBuf, String, Option<f64>, String);

/// Resolve playlist items to playable files in stored order.
/// Pure over the managers so tests pin the ordering contract without
/// App/GUI. Returns `(ready, missing)` where each ready entry is
/// (path, track id, duration, display label).
pub(crate) fn resolve_playlist_order(
    library: &Library,
    items: &[PlaylistItem],
) -> (Vec<ResolvedItem>, usize) {
    let mut ready = Vec::new();
    let mut missing = 0usize;
    for item in items {
        match library.get_track(&item.track_id).ok().flatten() {
            Some(t) if PathBuf::from(&t.file_path).is_file() => {
                let label = track_label(&t);
                ready.push((PathBuf::from(&t.file_path), t.id, t.duration_secs, label));
            }
            _ => missing += 1,
        }
    }
    (ready, missing)
}

impl App {
    /// Rebuild the Home "Saved Playlists" list (id + name + item count,
    /// `list_all` order). Called from `refresh_counts` (boot/restore)
    /// and after every generator fire.
    pub(crate) fn refresh_saved_playlists(&mut self) {
        let mut out = Vec::new();
        if let Ok(lists) = self.playlist_manager.list_all() {
            for pl in lists {
                let n = self
                    .playlist_manager
                    .get_with_items(&pl.id)
                    .map(|p| p.map(|p| p.items.len()).unwrap_or(0))
                    .unwrap_or(0);
                out.push(SavedPlaylist {
                    id: pl.id,
                    name: pl.name,
                    tracks: n,
                });
            }
        }
        self.saved_playlists = out;
        // A deleted playlist must not stay expanded with stale rows.
        if let Some(sel) = self.playlist_selected.clone() {
            if !self.saved_playlists.iter().any(|p| p.id == sel) {
                self.playlist_selected = None;
                self.playlist_detail.clear();
                self.playlist_detail_missing = 0;
            } else {
                self.refresh_playlist_detail();
            }
        }
    }

    /// Rebuild the expanded detail for `playlist_selected` in stored
    /// order. Missing files stay visible with a flag (the fire path
    /// skips them with a count) so the operator can remove or
    /// re-import them deliberately.
    pub(crate) fn refresh_playlist_detail(&mut self) {
        self.playlist_detail.clear();
        self.playlist_detail_missing = 0;
        let Some(sel) = self.playlist_selected.clone() else {
            return;
        };
        let pl = match self.playlist_manager.get_with_items(&sel) {
            Ok(Some(p)) => p,
            _ => {
                self.playlist_selected = None;
                return;
            }
        };
        for item in &pl.items {
            match self.library.get_track(&item.track_id).ok().flatten() {
                Some(t) => {
                    let missing = !PathBuf::from(&t.file_path).is_file();
                    if missing {
                        self.playlist_detail_missing += 1;
                    }
                    self.playlist_detail.push(PlaylistDetailItem {
                        label: track_label(&t),
                        missing,
                    });
                }
                None => {
                    self.playlist_detail_missing += 1;
                    self.playlist_detail.push(PlaylistDetailItem {
                        label: "(track removed from library)".into(),
                        missing: true,
                    });
                }
            }
        }
    }
}

/// B: create an empty manual playlist from the Home input.
pub(crate) fn playlist_create(state: &mut App) {
    let name = state.playlist_new_name.trim().to_string();
    if name.is_empty() {
        state.gen_status = "Name the playlist first".into();
        return;
    }
    match state
        .playlist_manager
        .create(&name, Some("Manual playlist"))
    {
        Ok(pl) => {
            state.playlist_new_name.clear();
            state.playlist_count = state.playlist_manager.list_all().unwrap_or_default().len();
            state.refresh_saved_playlists();
            state.playlist_selected = Some(pl.id);
            state.refresh_playlist_detail();
            state.gen_status = format!("Created '{}'", name);
        }
        Err(e) => {
            state.gen_status = format!("Create failed: {e}");
        }
    }
}

/// B: expand/collapse a saved playlist to edit its order.
pub(crate) fn playlist_select(state: &mut App, playlist_id: String) {
    if state.playlist_selected.as_deref() == Some(playlist_id.as_str()) {
        state.playlist_selected = None;
        state.playlist_detail.clear();
        state.playlist_detail_missing = 0;
        state.playlist_rename.clear();
    } else {
        state.playlist_rename = state
            .saved_playlists
            .iter()
            .find(|p| p.id == playlist_id)
            .map(|p| p.name.clone())
            .unwrap_or_default();
        state.playlist_selected = Some(playlist_id);
        state.refresh_playlist_detail();
    }
}

/// B: delete a whole saved playlist (items go via FK cascade).
pub(crate) fn playlist_delete(state: &mut App, playlist_id: String) {
    let name = state
        .saved_playlists
        .iter()
        .find(|p| p.id == playlist_id)
        .map(|p| p.name.clone())
        .unwrap_or_default();
    match state.playlist_manager.delete(&playlist_id) {
        Ok(()) => {
            if state.playlist_selected.as_deref() == Some(playlist_id.as_str()) {
                state.playlist_selected = None;
                state.playlist_detail.clear();
                state.playlist_detail_missing = 0;
            }
            state.playlist_count = state.playlist_manager.list_all().unwrap_or_default().len();
            state.refresh_saved_playlists();
            state.gen_status = if name.is_empty() {
                "Playlist deleted".into()
            } else {
                format!("Deleted '{name}'")
            };
        }
        Err(e) => {
            state.gen_status = format!("Delete failed: {e}");
        }
    }
}

/// B: append the currently selected Library track to a playlist.
pub(crate) fn playlist_add_selected(state: &mut App, playlist_id: String) {
    let Some(i) = state.lib_selected else {
        state.gen_status = "Pick a track in Library first".into();
        return;
    };
    let Some(track) = state.lib_tracks.get(i).cloned() else {
        state.gen_status = "Selected track is gone — pick another".into();
        return;
    };
    if !PathBuf::from(&track.file_path).is_file() {
        state.gen_status = format!("'{}' file is missing, not added", track_label(&track));
        return;
    }
    let (is_jingle, is_ad) = match track.kind {
        TrackKind::Jingle => (true, false),
        TrackKind::Ad => (false, true),
        TrackKind::Music => (false, false),
    };
    match state
        .playlist_manager
        .add_track(&playlist_id, &track.id, is_jingle, is_ad)
    {
        Ok(()) => {
            state.refresh_saved_playlists();
            state.refresh_playlist_detail();
            state.gen_status = format!("Added '{}'", track_label(&track));
        }
        Err(e) => {
            state.gen_status = format!("Add failed: {e}");
        }
    }
}

/// B: remove one stored-order entry by its list index.
pub(crate) fn playlist_remove_item(state: &mut App, playlist_id: String, idx: usize) {
    let position = match state
        .playlist_manager
        .get_with_items(&playlist_id)
        .ok()
        .flatten()
    {
        Some(pl) => match pl.items.get(idx) {
            Some(item) => item.position,
            None => {
                state.gen_status = "Row is gone — reopen the playlist".into();
                return;
            }
        },
        None => {
            state.gen_status = "Playlist gone — pick another".into();
            return;
        }
    };
    match state.playlist_manager.remove_at(&playlist_id, position) {
        Ok(()) => {
            state.refresh_saved_playlists();
            state.refresh_playlist_detail();
            state.gen_status = "Removed 1 track".into();
        }
        Err(e) => {
            state.gen_status = format!("Remove failed: {e}");
        }
    }
}

/// B: nudge one entry up (`up = true`) or down in stored order.
pub(crate) fn playlist_move(state: &mut App, playlist_id: String, idx: usize, up: bool) {
    let len = state
        .playlist_manager
        .get_with_items(&playlist_id)
        .ok()
        .flatten()
        .map(|p| p.items.len())
        .unwrap_or(0);
    if len == 0 {
        state.gen_status = "Playlist gone — pick another".into();
        return;
    }
    if up && idx == 0 {
        return;
    }
    if !up && idx + 1 >= len {
        return;
    }
    let to = if up { idx - 1 } else { idx + 1 };
    match state.playlist_manager.move_item(&playlist_id, idx, to) {
        Ok(()) => {
            state.refresh_saved_playlists();
            state.refresh_playlist_detail();
        }
        Err(e) => {
            state.gen_status = format!("Move failed: {e}");
        }
    }
}

/// P0: rename the playlist (core `rename` existed with no UI —
/// blank names are rejected by the manager, and the id stays so the
/// expanded detail and scheduler targets by id keep working; scheduler
/// `load` by display name picks the new name up on next fire).
pub(crate) fn playlist_rename(state: &mut App, playlist_id: String) {
    let name = state.playlist_rename.trim().to_string();
    if name.is_empty() {
        state.gen_status = "Name the playlist first".into();
        return;
    }
    match state.playlist_manager.rename(&playlist_id, &name) {
        Ok(()) => {
            state.playlist_rename = name.clone();
            state.refresh_saved_playlists();
            state.refresh_playlist_detail();
            state.gen_status = format!("Renamed to '{name}'");
        }
        Err(e) => {
            state.gen_status = format!("Rename failed: {e}");
        }
    }
}

/// Filename-safe default for the m3u save dialog (the playlist name
/// may contain characters the OS refuses as a file name).
pub(crate) fn sanitize_m3u_filename(name: &str) -> String {
    let mut out: String = name
        .chars()
        .map(|c| match c {
            '\\' | '/' | ':' | '*' | '?' | '"' | '<' | '>' | '|' => '_',
            c => c,
        })
        .collect();
    if out.trim().is_empty() {
        out = "playlist".into();
    }
    out
}

/// Render stored-order file paths as an `.m3u8` document (UTF-8,
/// `#EXTM3U` header, one absolute path per line).
pub(crate) fn build_m3u(paths: &[String]) -> String {
    let mut out = String::from("#EXTM3U\n");
    for p in paths {
        out.push_str(p);
        out.push('\n');
    }
    out
}

/// Parse an `.m3u`/`.m3u8` document into raw entries in order:
/// trims whitespace/CR, skips blanks and `#` comment/directive lines
/// (incl. `#EXTM3U`/`#EXTINF`), strips a leading `file://` scheme.
pub(crate) fn parse_m3u_entries(content: &str) -> Vec<String> {
    content
        .lines()
        .map(|l| l.trim().trim_end_matches('\r'))
        .filter(|l| !l.trim().is_empty())
        .map(|l| l.trim().to_string())
        .filter(|l| !l.starts_with('#'))
        .map(|l| {
            l.strip_prefix("file://")
                .map(|s| {
                    // `file:///C:/...` -> `C:/...`; keep Unix paths intact.
                    s.strip_prefix('/')
                        .filter(|s| s.len() >= 2 && s.as_bytes()[1] == b':')
                        .map_or(s.to_string(), |s| s.to_string())
                })
                .unwrap_or(l)
        })
        .collect()
}

/// Resolve one m3u entry against the playlist file's directory:
/// absolute entries stay as-is, relative ones join the base dir.
pub(crate) fn resolve_m3u_entry(raw: &str, base_dir: &std::path::Path) -> PathBuf {
    let p = PathBuf::from(raw);
    if p.is_absolute() {
        p
    } else {
        base_dir.join(p)
    }
}

/// P0: export one saved playlist to `.m3u8` (stored order, absolute
/// paths). Read-only over the engine: no transport, queue, or
/// play-log side effects — the file is the only output.
pub(crate) fn playlist_export(state: &mut App, playlist_id: String) {
    let pl = match state.playlist_manager.get_with_items(&playlist_id) {
        Ok(Some(p)) => p,
        Ok(None) => {
            state.gen_status = "Playlist gone — pick another".into();
            return;
        }
        Err(e) => {
            state.gen_status = format!("Playlist read failed: {e}");
            return;
        }
    };
    if pl.items.is_empty() {
        state.gen_status = format!("'{}' is empty, nothing to export", pl.name);
        return;
    }
    let mut paths = Vec::new();
    let mut missing = 0usize;
    for item in &pl.items {
        match state.library.get_track(&item.track_id).ok().flatten() {
            Some(t) => paths.push(t.file_path),
            None => missing += 1,
        }
    }
    if paths.is_empty() {
        state.gen_status = format!("'{}': all tracks gone from library", pl.name);
        return;
    }
    let doc = build_m3u(&paths);
    let default_name = format!("{}.m3u8", sanitize_m3u_filename(&pl.name));
    let Some(dest) = rfd::FileDialog::new()
        .set_title("Export playlist (.m3u8)")
        .set_file_name(default_name)
        .add_filter("M3U playlist", &["m3u", "m3u8"])
        .save_file()
    else {
        return;
    };
    match std::fs::write(&dest, doc) {
        Ok(()) => {
            let mut msg = format!("Exported '{}' ({} tracks)", pl.name, paths.len());
            if missing > 0 {
                msg.push_str(&format!(", {missing} library-gone skipped"));
            }
            tracing::info!("{msg} -> {}", dest.display());
            state.gen_status = msg;
        }
        Err(e) => {
            tracing::error!("Playlist export failed: {e}");
            state.gen_status = format!("Export failed: {e}");
        }
    }
}

/// P0: import one `.m3u`/`.m3u8` file as a new saved playlist.
/// Only the library + playlist store are touched (no transport):
/// entries are matched to library tracks by path in file order —
/// unknown paths and missing files are skipped with a count, never
/// imported as dead rows.
pub(crate) fn playlist_import(state: &mut App) {
    let Some(src) = rfd::FileDialog::new()
        .set_title("Import playlist (.m3u / .m3u8)")
        .add_filter("M3U playlist", &["m3u", "m3u8"])
        .pick_file()
    else {
        return;
    };
    let content = match std::fs::read_to_string(&src) {
        Ok(c) => c,
        Err(e) => {
            state.gen_status = format!("Import failed: {e}");
            return;
        }
    };
    let base = src.parent().map(|p| p.to_path_buf()).unwrap_or_default();
    let entries = parse_m3u_entries(&content);
    if entries.is_empty() {
        state.gen_status = "No tracks in that playlist file".into();
        return;
    }
    let name = src
        .file_stem()
        .and_then(|s| s.to_str())
        .filter(|s| !s.trim().is_empty())
        .map(|s| s.trim().to_string())
        .unwrap_or_else(|| "Imported".into());
    let pl = match state.playlist_manager.create(&name, Some("Imported .m3u")) {
        Ok(p) => p,
        Err(e) => {
            state.gen_status = format!("Import failed: {e}");
            return;
        }
    };
    let mut added = 0usize;
    let mut unknown = 0usize;
    let mut missing = 0usize;
    for raw in &entries {
        let candidate = resolve_m3u_entry(raw, &base);
        let key = candidate.to_string_lossy().into_owned();
        let hit = state.library.find_by_path(&key).ok().flatten().or_else(|| {
            // Stale working-directory-relative entry: try the raw
            // string itself before giving up.
            if key != *raw {
                state.library.find_by_path(raw).ok().flatten()
            } else {
                None
            }
        });
        match hit {
            Some(t) if PathBuf::from(&t.file_path).is_file() => {
                let (is_jingle, is_ad) = match t.kind {
                    TrackKind::Jingle => (true, false),
                    TrackKind::Ad => (false, true),
                    TrackKind::Music => (false, false),
                };
                match state
                    .playlist_manager
                    .add_track(&pl.id, &t.id, is_jingle, is_ad)
                {
                    Ok(()) => added += 1,
                    Err(e) => {
                        tracing::warn!("Playlist import: add failed for {}: {e}", t.file_path);
                        unknown += 1;
                    }
                }
            }
            Some(_) => missing += 1,
            None => unknown += 1,
        }
    }
    if added == 0 {
        let _ = state.playlist_manager.delete(&pl.id);
        state.gen_status = format!("Nothing imported ({unknown} unknown, {missing} missing)");
        return;
    }
    state.playlist_count = state.playlist_manager.list_all().unwrap_or_default().len();
    state.refresh_saved_playlists();
    state.playlist_selected = Some(pl.id);
    state.refresh_playlist_detail();
    state.playlist_rename = name.clone();
    let mut parts = vec![format!("Imported '{name}' ({added} tracks)")];
    if unknown > 0 {
        parts.push(format!("{unknown} unknown skipped"));
    }
    if missing > 0 {
        parts.push(format!("{missing} missing skipped"));
    }
    state.gen_status = parts.join(", ");
}

/// A1 "Playlist to Air": fire a saved playlist in its stored order.
/// First track starts now (`play` crossfades when live, starts from
/// silence when idle — never a `stop` gap), the rest queue behind it in
/// order. Queued decks are play-logged by the tick reconciler on
/// promotion; only the first needs an explicit `record_play` here.
/// Continuity follows the Auto-DJ toggle, like the On Air gate.
pub(crate) fn playlist_to_air(state: &mut App, playlist_id: String) {
    match fire_playlist_to_air(state, &playlist_id) {
        Ok(f) => {
            state.gen_status = format!("On air: '{}' ({})", f.name, f.detail());
        }
        Err(msg) => {
            state.gen_status = msg;
        }
    }
}

/// Fired-playlist summary for status lines (Home and Scheduler phrase
/// it differently, so the message stays with the caller).
pub(crate) struct PlaylistFire {
    pub(crate) name: String,
    pub(crate) to_air: usize,
    pub(crate) missing: usize,
    pub(crate) held_back: bool,
}

impl PlaylistFire {
    pub(crate) fn detail(&self) -> String {
        let mut parts = vec![format!("{} to air", self.to_air)];
        if self.missing > 0 {
            parts.push(format!("{} missing skipped", self.missing));
        }
        if self.held_back {
            parts.push("queued deck plays first".into());
        }
        parts.join(", ")
    }
}

/// Look a saved playlist id up by its display name (what the scheduler
/// `load` target and the operator type). Pure over the manager.
pub(crate) fn find_playlist_id(playlists: &PlaylistManager, name: &str) -> Option<String> {
    playlists
        .list_all()
        .unwrap_or_default()
        .into_iter()
        .find(|p| p.name == name)
        .map(|p| p.id)
}

/// Shared fire engine behind the Home button and scheduler `load`.
/// On success the transport already reflects the new air state; the
/// caller only phrases the returned summary into its own status line.
pub(crate) fn fire_playlist_to_air(
    state: &mut App,
    playlist_id: &str,
) -> Result<PlaylistFire, String> {
    let pl = match state.playlist_manager.get_with_items(playlist_id) {
        Ok(Some(p)) => p,
        Ok(None) => return Err("Playlist gone — pick another".into()),
        Err(e) => return Err(format!("Playlist read failed: {e}")),
    };
    if pl.items.is_empty() {
        return Err(format!("'{}' is empty", pl.name));
    }
    // Resolve in stored order; missing files skip with a count.
    let (ready, missing) = resolve_playlist_order(&state.library, &pl.items);
    if ready.is_empty() {
        return Err(format!(
            "'{}': all {} tracks missing, nothing queued",
            pl.name,
            pl.items.len()
        ));
    }
    // A stale prefetch/queued deck (Auto-DJ, scheduler) still plays
    // before the playlist — say so instead of surprising the operator.
    let held_back = state.player.pending_count() + state.player.load_inflight() > 0;
    let (first_path, first_id, first_dur, first_label) = ready[0].clone();
    match state.player.play(&first_path) {
        Ok(()) => {
            let _ = state.library.record_play(&first_id, first_dur);
            let mut queued = 0usize;
            for (p, _, _, _) in ready.iter().skip(1) {
                match state.player.queue(p) {
                    Ok(()) => queued += 1,
                    Err(e) => tracing::warn!("Playlist queue failed for {}: {e}", p.display()),
                }
            }
            state.auto_continue = state.autodj;
            state.is_playing = true;
            state.now_title = first_label;
            state.now_artist = "Playlist".into();
            state.engine_track = Some(first_path);
            state.up_next = ready
                .get(1)
                .map(|(_, _, _, l)| l.clone())
                .unwrap_or_default();
            state.pending_source = Some("Playlist".into());
            let fire = PlaylistFire {
                name: pl.name.clone(),
                to_air: queued + 1,
                missing,
                held_back,
            };
            tracing::info!(
                "Playlist to air: '{}' ({} to air, {} missing skipped)",
                pl.name,
                queued + 1,
                missing
            );
            Ok(fire)
        }
        Err(e) => {
            tracing::error!("Playlist to air failed: {e}");
            Err(format!("Playlist to air failed: {e}"))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn test_dir(name: &str) -> std::path::PathBuf {
        let dir = std::env::temp_dir().join(format!("crabboss-genfire-{name}"));
        // Start clean: a leaked dir from an interrupted run must not
        // poison primary keys on re-seed.
        std::fs::remove_dir_all(&dir).ok();
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    /// Minimal silent WAV: `add_track` only reads tags + header, and a
    /// real file keeps the test on the public ingestion path (no
    /// `pub(crate)` backdoors, no new dev-dependencies).
    fn write_silent_wav(path: &std::path::Path) {
        let rate = 8000u32;
        let n = rate as usize; // 1 s mono 16-bit
        let data_bytes = (n * 2) as u32;
        let mut wav = Vec::with_capacity(44 + data_bytes as usize);
        wav.extend_from_slice(b"RIFF");
        wav.extend_from_slice(&(36 + data_bytes).to_le_bytes());
        wav.extend_from_slice(b"WAVEfmt ");
        wav.extend_from_slice(&16u32.to_le_bytes());
        wav.extend_from_slice(&1u16.to_le_bytes());
        wav.extend_from_slice(&1u16.to_le_bytes());
        wav.extend_from_slice(&rate.to_le_bytes());
        wav.extend_from_slice(&(rate * 2).to_le_bytes());
        wav.extend_from_slice(&2u16.to_le_bytes());
        wav.extend_from_slice(&16u16.to_le_bytes());
        wav.extend_from_slice(b"data");
        wav.extend_from_slice(&data_bytes.to_le_bytes());
        wav.extend(std::iter::repeat_n(0u8, data_bytes as usize));
        std::fs::write(path, wav).unwrap();
    }

    fn seed_music(lib: &Library, dir: &std::path::Path, n: usize) {
        for i in 0..n {
            // No jingle/ad keywords in the path: kind must stay Music.
            let f = dir.join(format!("song{i}.wav"));
            write_silent_wav(&f);
            lib.add_track(&f).unwrap();
        }
    }

    fn job(daypart: &str, target: usize) -> FireJob {
        FireJob {
            daypart: daypart.to_string(),
            cfg: GenConfig {
                target_tracks: target,
                jingles_every: 0,
                ..Default::default()
            },
        }
    }

    #[test]
    fn playlist_name_combines_daypart_and_stamp() {
        assert_eq!(playlist_name("Morning", "08:00"), "Morning 08:00");
    }

    #[test]
    fn find_playlist_id_matches_display_name() {
        let dir = test_dir("findname");
        let db = dir.join("station.db");
        let lib = Library::open(&db).unwrap();
        seed_music(&lib, &dir, 2);
        let pls = PlaylistManager::open(&db).unwrap();
        let pl = pls.create("Morning Mix", None).unwrap();
        assert_eq!(find_playlist_id(&pls, "Morning Mix"), Some(pl.id.clone()));
        assert_eq!(find_playlist_id(&pls, "Evening Mix"), None);
        assert_eq!(find_playlist_id(&pls, "morning mix"), None);
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn fire_detail_phrases_counts() {
        let f = PlaylistFire {
            name: "X".into(),
            to_air: 12,
            missing: 0,
            held_back: false,
        };
        assert_eq!(f.detail(), "12 to air");
        let f = PlaylistFire {
            name: "X".into(),
            to_air: 11,
            missing: 1,
            held_back: true,
        };
        assert_eq!(
            f.detail(),
            "11 to air, 1 missing skipped, queued deck plays first"
        );
    }

    #[test]
    fn fire_saves_one_playlist_per_job() {
        let dir = test_dir("multi");
        let db = dir.join("station.db");
        let lib = Library::open(&db).unwrap();
        seed_music(&lib, &dir, 6);
        let pls = PlaylistManager::open(&db).unwrap();
        let out = fire_rotations(&lib, &pls, &[job("Morning", 4), job("Night", 3)], "08:00");
        assert_eq!(out.len(), 2);
        let names: Vec<_> = pls
            .list_all()
            .unwrap()
            .into_iter()
            .map(|p| p.name)
            .collect();
        assert!(names.contains(&"Morning 08:00".to_string()));
        assert!(names.contains(&"Night 08:00".to_string()));
        for r in out {
            let f = r.expect("seeded library saves");
            assert!(f.music > 0, "rotation is not empty: {f:?}");
        }
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn fire_on_empty_library_saves_empty_rotation() {
        let dir = test_dir("empty");
        let db = dir.join("station.db");
        let lib = Library::open(&db).unwrap();
        let pls = PlaylistManager::open(&db).unwrap();
        let out = fire_rotations(&lib, &pls, &[job("Morning", 4)], "08:00");
        assert_eq!(
            out,
            vec![Ok(FireResult {
                playlist: "Morning 08:00".into(),
                music: 0,
                jingles: 0,
            })]
        );
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn resolve_keeps_stored_order_and_counts_missing() {
        use crabcore::playlist::PlaylistItem;

        let dir = test_dir("resolve");
        let db = dir.join("station.db");
        let lib = Library::open(&db).unwrap();
        seed_music(&lib, &dir, 3);
        let id_of = |f: &str| {
            lib.find_by_path(&dir.join(f).to_string_lossy())
                .unwrap()
                .expect("seeded")
                .id
        };
        let item = |track_id: String, position: i32| PlaylistItem {
            track_id,
            position,
            is_jingle: false,
            is_ad: false,
        };
        // Stored order song2, <gone>, song0 — plus a dangling id.
        let items = vec![
            item(id_of("song2.wav"), 0),
            item("dangling-id".into(), 1),
            item(id_of("song0.wav"), 2),
        ];
        let (ready, missing) = resolve_playlist_order(&lib, &items);
        let names: Vec<_> = ready
            .iter()
            .map(|(p, _, _, _)| p.file_name().unwrap().to_string_lossy().into_owned())
            .collect();
        assert_eq!(names, vec!["song2.wav", "song0.wav"]);
        assert_eq!(missing, 1);
        // A deleted file counts as missing too, order of the rest holds.
        std::fs::remove_file(dir.join("song2.wav")).ok();
        let (ready, missing) = resolve_playlist_order(&lib, &items);
        assert_eq!(ready.len(), 1);
        assert_eq!(missing, 2);
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn m3u_parse_skips_comments_and_blanks() {
        let doc = "#EXTM3U\n#EXTINF:123,Artist - Title\nC:/mix/song one.mp3\r\n\n  \n# a comment\nrel/song2.wav\n";
        assert_eq!(
            parse_m3u_entries(doc),
            vec![
                "C:/mix/song one.mp3".to_string(),
                "rel/song2.wav".to_string()
            ]
        );
        assert!(parse_m3u_entries("#EXTM3U\n# only comments\n").is_empty());
    }

    #[test]
    fn m3u_resolve_keeps_absolute_and_joins_relative() {
        use std::path::PathBuf;

        let base = PathBuf::from("/base/dir");
        assert_eq!(
            resolve_m3u_entry("C:/mix/a.mp3", &base),
            PathBuf::from("C:/mix/a.mp3")
        );
        assert_eq!(
            resolve_m3u_entry("sub/b.wav", &base),
            PathBuf::from("/base/dir/sub/b.wav")
        );
    }

    #[test]
    fn m3u_build_roundtrips_through_parse() {
        let paths = vec!["C:/a.mp3".to_string(), "/m/b song.wav".to_string()];
        let doc = build_m3u(&paths);
        assert!(doc.starts_with("#EXTM3U\n"));
        assert_eq!(parse_m3u_entries(&doc), paths);
    }

    #[test]
    fn m3u_filename_sanitizes_os_refused_chars() {
        assert_eq!(sanitize_m3u_filename("Morning: Mix/8?"), "Morning_ Mix_8_");
        assert_eq!(sanitize_m3u_filename("   "), "playlist");
    }
}
