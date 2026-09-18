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
    }
}

/// A1 "Playlist to Air": fire a saved playlist in its stored order.
/// First track starts now (`play` crossfades when live, starts from
/// silence when idle — never a `stop` gap), the rest queue behind it in
/// order. Queued decks are play-logged by the tick reconciler on
/// promotion; only the first needs an explicit `record_play` here.
/// Continuity follows the Auto-DJ toggle, like the On Air gate.
pub(crate) fn playlist_to_air(state: &mut App, playlist_id: String) {
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
        state.gen_status = format!("'{}' is empty", pl.name);
        return;
    }
    // Resolve in stored order; missing files skip with a count.
    let (ready, missing) = resolve_playlist_order(&state.library, &pl.items);
    if ready.is_empty() {
        state.gen_status = format!(
            "'{}': all {} tracks missing, nothing queued",
            pl.name,
            pl.items.len()
        );
        return;
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
            let mut detail = vec![format!("{} to air", queued + 1)];
            if missing > 0 {
                detail.push(format!("{missing} missing skipped"));
            }
            if held_back {
                detail.push("queued deck plays first".into());
            }
            state.gen_status = format!("On air: '{}' ({})", pl.name, detail.join(", "));
            tracing::info!(
                "Playlist to air: '{}' ({} to air, {} missing skipped)",
                pl.name,
                queued + 1,
                missing
            );
        }
        Err(e) => {
            tracing::error!("Playlist to air failed: {e}");
            state.gen_status = format!("Playlist to air failed: {e}");
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
}
