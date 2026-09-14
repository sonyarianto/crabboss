//! Playlist rotation generator (RadioBOSS-style Playlist Generator Pro MVP).
//!
//! Deterministic rule engine over the [`Library`]:
//! daypart eligibility → playcount priority → no-repeat windows
//! (artist/title/album) + genre separation → jingle slots.
//! Pure logic over [`Track`]s; persistence of the result is the caller's job.

use std::collections::VecDeque;

use crate::error::Result;
use crate::library::{Library, Track, TrackKind};

/// Which end of the playcount spectrum to favor.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum PlaycountPriority {
    /// Surface under-played tracks first (MIN-style).
    #[default]
    LeastPlayed,
    /// Favor hits (MAX-style).
    MostPlayed,
}

/// Rotation rules for one generated playlist.
#[derive(Debug, Clone)]
pub struct GenConfig {
    /// How many tracks to pick (music + jingles combined).
    pub target_tracks: usize,
    /// Same artist must not repeat within this many previous tracks.
    pub artist_window: usize,
    /// Same title must not repeat within this many previous tracks.
    pub title_window: usize,
    /// Same album must not repeat within this many previous tracks.
    pub album_window: usize,
    /// Same genre must not appear within this many previous tracks.
    pub genre_gap: usize,
    /// Insert a jingle after every N music tracks (`0` = none).
    pub jingles_every: usize,
    pub priority: PlaycountPriority,
    /// Daypart context: hour 0-23 and weekday (`Mon`..`Sun`).
    pub hour: u8,
    pub weekday: String,
}

impl Default for GenConfig {
    fn default() -> Self {
        Self {
            target_tracks: 15,
            artist_window: 4,
            title_window: 8,
            album_window: 4,
            genre_gap: 2,
            jingles_every: 3,
            priority: PlaycountPriority::LeastPlayed,
            hour: 12,
            weekday: "Mon".to_string(),
        }
    }
}

fn title_key(t: &Track) -> String {
    format!(
        "{}\x00{}",
        t.title.clone().unwrap_or_else(|| t.file_name.clone()),
        t.artist.clone().unwrap_or_default()
    )
}

/// No-repeat windows carried ACROSS `generate` calls. `generate` itself
/// starts empty every time (right for one-shot rotations); one-at-a-time
/// flows like Auto-DJ thread a single `RuleHistory` through successive
/// picks so separation actually bites — otherwise every pick sees virgin
/// windows and the rules never fire.
///
/// The jingle-slot counters live here too, so the live picker keeps the
/// same cadence as batch rotations (see `generate_next`).
#[derive(Debug, Clone, Default)]
pub struct RuleHistory {
    artists: VecDeque<String>,
    titles: VecDeque<String>,
    albums: VecDeque<String>,
    genres: VecDeque<String>,
    /// Music picks since the last jingle (`jingles_every` counts these).
    music_since_jingle: usize,
    /// Round-robin cursor into the eligible jingles.
    jingle_cursor: usize,
}

impl RuleHistory {
    /// Advance the windows for a pick so later picks separate from it.
    /// Empty keys never match (`within`), so untagged files can't poison
    /// the windows.
    pub fn push_track(&mut self, t: &Track, cfg: &GenConfig) {
        push_key(
            &mut self.artists,
            t.artist.clone().unwrap_or_default(),
            cfg.artist_window,
        );
        push_key(&mut self.titles, title_key(t), cfg.title_window);
        push_key(
            &mut self.albums,
            t.album.clone().unwrap_or_default(),
            cfg.album_window,
        );
        push_key(
            &mut self.genres,
            t.genre.clone().unwrap_or_default(),
            cfg.genre_gap,
        );
    }

    /// True when `t` clears every window (same predicate `generate` uses
    /// per slot, so single picks and rotations agree).
    fn allows(&self, t: &Track) -> bool {
        !within(&self.artists, &t.artist.clone().unwrap_or_default())
            && !within(&self.titles, &title_key(t))
            && !within(&self.albums, &t.album.clone().unwrap_or_default())
            && !within(&self.genres, &t.genre.clone().unwrap_or_default())
    }
}

/// Shared jingle-slot selection for `generate` and `generate_next`:
/// round-robin cursor over the eligible jingles, skipping one whose title
/// just played (a lone jingle always plays). Advances the cursor past the
/// pick; the caller owns its music/jingle counters.
fn pick_jingle(jingles: &[Track], cursor: &mut usize, last_title: &str) -> Option<Track> {
    let mut picked = None;
    for i in 0..jingles.len() {
        let idx = (*cursor + i) % jingles.len();
        if jingles.len() == 1 || title_key(&jingles[idx]) != last_title {
            picked = Some(idx);
            break;
        }
    }
    picked.map(|idx| {
        *cursor = idx + 1;
        jingles[idx].clone()
    })
}

/// Build a rotation. Always terminates: when no candidate satisfies every
/// rule, the best-ranked candidate is taken (rules relax, never block).
pub fn generate(library: &Library, cfg: &GenConfig) -> Result<Vec<Track>> {
    let mut music: Vec<Track> = library
        .list_by_kind(TrackKind::Music)?
        .into_iter()
        .filter(|t| t.eligible_at(cfg.hour, &cfg.weekday))
        .collect();
    let mut jingles: Vec<Track> = library
        .list_by_kind(TrackKind::Jingle)?
        .into_iter()
        .filter(|t| t.eligible_at(cfg.hour, &cfg.weekday))
        .collect();
    if music.is_empty() {
        return Ok(Vec::new());
    }
    sort_by_priority(&mut music, cfg.priority);
    sort_by_priority(&mut jingles, cfg.priority);

    let mut out: Vec<Track> = Vec::new();
    let mut history = RuleHistory::default();

    while out.len() < cfg.target_tracks {
        // Jingle slot (round-robin, skip if it just played).
        if cfg.jingles_every > 0
            && history.music_since_jingle >= cfg.jingles_every
            && !jingles.is_empty()
        {
            let last_title = history.titles.back().cloned().unwrap_or_default();
            if let Some(j) = pick_jingle(&jingles, &mut history.jingle_cursor, &last_title) {
                history.push_track(&j, cfg);
                history.music_since_jingle = 0;
                out.push(j);
                continue;
            }
        }
        // Music slot: first ranked candidate satisfying every rule, else relax.
        let choice = music
            .iter()
            .find(|t| history.allows(t))
            .or_else(|| music.first())
            .cloned();
        match choice {
            Some(t) => {
                history.push_track(&t, cfg);
                out.push(t);
                history.music_since_jingle += 1;
            }
            None => break,
        }
    }
    Ok(out)
}

/// One next pick honoring `history` across calls — music AND jingle slots,
/// same cadence as `generate`. This is the Auto-DJ primitive: the caller
/// threads one `RuleHistory` through successive picks; windows AND the
/// jingle counters advance here per pick (a pick that never sounds leaves
/// a harmless ghost in soft windows — far cheaper than split contracts).
/// Same ranking (daypart → playcount priority) and relax fallback as
/// `generate`; `None` only when the library has no eligible music.
pub fn generate_next(
    library: &Library,
    cfg: &GenConfig,
    history: &mut RuleHistory,
) -> Result<Option<Track>> {
    let mut music: Vec<Track> = library
        .list_by_kind(TrackKind::Music)?
        .into_iter()
        .filter(|t| t.eligible_at(cfg.hour, &cfg.weekday))
        .collect();
    let mut jingles: Vec<Track> = library
        .list_by_kind(TrackKind::Jingle)?
        .into_iter()
        .filter(|t| t.eligible_at(cfg.hour, &cfg.weekday))
        .collect();
    if music.is_empty() {
        return Ok(None);
    }
    sort_by_priority(&mut music, cfg.priority);
    sort_by_priority(&mut jingles, cfg.priority);
    // Jingle slot first, exactly like the batch path.
    if cfg.jingles_every > 0
        && history.music_since_jingle >= cfg.jingles_every
        && !jingles.is_empty()
    {
        let last_title = history.titles.back().cloned().unwrap_or_default();
        if let Some(j) = pick_jingle(&jingles, &mut history.jingle_cursor, &last_title) {
            history.push_track(&j, cfg);
            history.music_since_jingle = 0;
            return Ok(Some(j));
        }
    }
    let choice = music
        .iter()
        .find(|t| history.allows(t))
        .or_else(|| music.first())
        .cloned();
    if let Some(ref t) = choice {
        // Windows advance here too (same as the jingle arm above): one
        // state object, one updater — a pick that never sounds leaves a
        // harmless ghost in soft windows, far cheaper than split contracts.
        history.push_track(t, cfg);
        history.music_since_jingle += 1;
    }
    Ok(choice)
}

/// Forecast the next `n` picks without touching playcounts: simulate
/// successive `generate_next` calls on a CLONE of `history`, pushing each
/// simulated pick so the forecast itself separates. This is a prediction
/// for display (an "up next" list), not a commitment — manual overrides,
/// scheduler fires, and fresh playcounts will diverge it. The caller owns
/// refreshing when the world changes.
pub fn forecast_up_next(
    library: &Library,
    cfg: &GenConfig,
    history: &RuleHistory,
    n: usize,
) -> Vec<Track> {
    let mut hist = history.clone();
    let mut out = Vec::new();
    for _ in 0..n {
        // Windows + counters advance inside generate_next; nothing to
        // record here (and the clone is discarded anyway).
        match generate_next(library, cfg, &mut hist) {
            Ok(Some(t)) => out.push(t),
            _ => break,
        }
    }
    out
}

fn sort_by_priority(tracks: &mut [Track], priority: PlaycountPriority) {
    tracks.sort_by(|a, b| {
        let ord = match priority {
            PlaycountPriority::LeastPlayed => a.play_count.cmp(&b.play_count),
            PlaycountPriority::MostPlayed => b.play_count.cmp(&a.play_count),
        };
        ord.then_with(|| match (&a.last_played_at, &b.last_played_at) {
            (None, None) => std::cmp::Ordering::Equal,
            (None, Some(_)) => std::cmp::Ordering::Less,
            (Some(_), None) => std::cmp::Ordering::Greater,
            (Some(x), Some(y)) => x.cmp(y),
        })
        .then_with(|| a.file_name.cmp(&b.file_name))
    });
}

fn push_key(hist: &mut VecDeque<String>, key: String, window: usize) {
    if window == 0 {
        return;
    }
    hist.push_back(key);
    while hist.len() > window {
        hist.pop_front();
    }
}

fn within(hist: &VecDeque<String>, key: &str) -> bool {
    !key.is_empty() && hist.contains(&key.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn mem_lib(tracks: &[(&str, &str, &str, i32)]) -> Library {
        // (file, artist, genre, play_count)
        let lib = Library::open(std::path::Path::new(":memory:")).unwrap();
        for (i, (file, artist, genre, plays)) in tracks.iter().enumerate() {
            lib.conn()
                .execute(
                    "INSERT INTO tracks
                     (id, file_path, file_name, artist, genre, added_at, play_count, kind)
                     VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, 'music')",
                    rusqlite::params![
                        format!("id-{}", i),
                        format!("/m/{}", file),
                        file,
                        artist,
                        genre,
                        chrono::Utc::now().to_rfc3339(),
                        plays,
                    ],
                )
                .unwrap();
        }
        lib
    }

    #[test]
    fn respects_artist_window_when_possible() {
        let lib = mem_lib(&[
            ("a1.mp3", "A", "Rock", 0),
            ("a2.mp3", "A", "Rock", 0),
            ("b1.mp3", "B", "Pop", 0),
            ("c1.mp3", "C", "Jazz", 0),
        ]);
        let cfg = GenConfig {
            target_tracks: 4,
            artist_window: 2,
            genre_gap: 0,
            jingles_every: 0,
            ..Default::default()
        };
        let out = generate(&lib, &cfg).unwrap();
        assert_eq!(out.len(), 4);
        let artists: Vec<_> = out.iter().map(|t| t.artist.clone().unwrap()).collect();
        for (i, a) in artists.iter().enumerate() {
            assert!(
                !artists[i.saturating_sub(2)..i].contains(a),
                "artist repeated too soon: {:?}",
                artists
            );
        }
    }

    #[test]
    fn least_played_first() {
        let lib = mem_lib(&[
            ("hit.mp3", "Star", "Pop", 100),
            ("deep.mp3", "Nobody", "Rock", 0),
        ]);
        let cfg = GenConfig {
            target_tracks: 1,
            artist_window: 0,
            title_window: 0,
            album_window: 0,
            genre_gap: 0,
            jingles_every: 0,
            ..Default::default()
        };
        let out = generate(&lib, &cfg).unwrap();
        assert_eq!(out[0].file_name, "deep.mp3");
    }

    #[test]
    fn daypart_filters_ineligible() {
        let lib = Library::open(std::path::Path::new(":memory:")).unwrap();
        lib.conn()
            .execute(
                "INSERT INTO tracks
                 (id, file_path, file_name, artist, added_at, play_count, kind,
                  daypart_start, daypart_end, daypart_days)
                 VALUES ('n1', '/m/night.mp3', 'night.mp3', 'Owl',
                         '2024-01-01T00:00:00Z', 0, 'music', 22, 6, 'Daily')",
                [],
            )
            .unwrap();
        lib.conn()
            .execute(
                "INSERT INTO tracks
                 (id, file_path, file_name, artist, added_at, play_count, kind)
                 VALUES ('d1', '/m/day.mp3', 'day.mp3', 'Lark',
                         '2024-01-01T00:00:00Z', 0, 'music')",
                [],
            )
            .unwrap();
        let mut cfg = GenConfig {
            target_tracks: 5,
            jingles_every: 0,
            ..Default::default()
        };
        cfg.hour = 10;
        let out = generate(&lib, &cfg).unwrap();
        assert!(out.iter().all(|t| t.file_name == "day.mp3"));
        cfg.hour = 23;
        let out = generate(&lib, &cfg).unwrap();
        assert!(out.iter().any(|t| t.file_name == "night.mp3"));
    }

    #[test]
    fn relaxes_instead_of_stalling() {
        // One track, tight windows: must still fill by relaxing.
        let lib = mem_lib(&[("only.mp3", "Solo", "Rock", 0)]);
        let cfg = GenConfig {
            target_tracks: 3,
            jingles_every: 0,
            ..Default::default()
        };
        let out = generate(&lib, &cfg).unwrap();
        assert_eq!(out.len(), 3);
    }

    #[test]
    fn empty_library_gives_empty() {
        let lib = Library::open(std::path::Path::new(":memory:")).unwrap();
        assert!(generate(&lib, &GenConfig::default()).unwrap().is_empty());
    }

    fn next_cfg() -> GenConfig {
        // Isolate the artist rule: everything else wide open.
        GenConfig {
            target_tracks: 1,
            artist_window: 2,
            title_window: 0,
            album_window: 0,
            genre_gap: 0,
            jingles_every: 0,
            ..Default::default()
        }
    }

    #[test]
    fn generate_next_honors_seeded_history() {
        let lib = mem_lib(&[
            ("a1.mp3", "A", "Rock", 0),
            ("a2.mp3", "A", "Rock", 0),
            ("b1.mp3", "B", "Pop", 0),
        ]);
        let cfg = next_cfg();
        let mut history = RuleHistory::default();
        // Fresh history: filename order wins.
        let first = generate_next(&lib, &cfg, &mut history).unwrap().unwrap();
        assert_eq!(first.file_name, "a1.mp3");
        // Once A played, the window excludes it: B jumps the queue even
        // though A has a second unplayed track.
        let second = generate_next(&lib, &cfg, &mut history).unwrap().unwrap();
        assert_eq!(second.file_name, "b1.mp3");
        // Window full (A, B): relaxes to the top-ranked instead of stalling.
        let third = generate_next(&lib, &cfg, &mut history).unwrap().unwrap();
        assert_eq!(third.file_name, "a1.mp3");
    }

    #[test]
    fn generate_next_empty_library_is_none() {
        let lib = Library::open(std::path::Path::new(":memory:")).unwrap();
        let mut history = RuleHistory::default();
        assert!(generate_next(&lib, &next_cfg(), &mut history)
            .unwrap()
            .is_none());
    }

    #[test]
    fn forecast_lists_successive_picks_with_separation() {
        let lib = mem_lib(&[
            ("a1.mp3", "A", "Rock", 0),
            ("a2.mp3", "A", "Rock", 0),
            ("b1.mp3", "B", "Pop", 0),
        ]);
        let cfg = next_cfg();
        let history = RuleHistory::default();
        // a1, then B (separated); once both artists sit in the window the
        // relax fallback kicks in exactly like successive real picks.
        // (Simulation never touches playcounts, so the tail repeats a1
        // where live play would have re-ranked by count — forecasts are
        // approximate by design.)
        let out = forecast_up_next(&lib, &cfg, &history, 5);
        let names: Vec<_> = out.iter().map(|t| t.file_name.as_str()).collect();
        assert_eq!(
            names,
            vec!["a1.mp3", "b1.mp3", "a1.mp3", "a1.mp3", "b1.mp3"]
        );
        // Empty library forecasts nothing.
        let empty = Library::open(std::path::Path::new(":memory:")).unwrap();
        assert!(forecast_up_next(&empty, &cfg, &history, 5).is_empty());
    }

    fn jingle_cfg() -> GenConfig {
        GenConfig {
            target_tracks: 6,
            jingles_every: 2,
            ..Default::default()
        }
    }

    fn lib_with_jingle() -> Library {
        let lib = mem_lib(&[
            ("m1.mp3", "A", "Rock", 0),
            ("m2.mp3", "B", "Pop", 0),
            ("m3.mp3", "C", "Jazz", 0),
        ]);
        lib.conn()
            .execute(
                "INSERT INTO tracks
                 (id, file_path, file_name, artist, genre, added_at, play_count, kind)
                 VALUES ('j1', '/m/j1.mp3', 'j1.mp3', 'Station', 'ID', '2024-01-01T00:00:00Z', 0, 'jingle')",
                [],
            )
            .unwrap();
        lib
    }

    #[test]
    fn generate_next_inserts_jingles_at_interval() {
        let lib = lib_with_jingle();
        let cfg = jingle_cfg();
        let mut history = RuleHistory::default();
        let mut kinds = Vec::new();
        let mut names = Vec::new();
        for _ in 0..6 {
            let t = generate_next(&lib, &cfg, &mut history)
                .unwrap()
                .expect("library is not empty");
            kinds.push(t.kind);
            names.push(t.file_name.clone());
        }
        // Jingle every 2 music picks, same cadence as the batch path below.
        assert_eq!(
            names,
            vec!["m1.mp3", "m2.mp3", "j1.mp3", "m3.mp3", "m1.mp3", "j1.mp3"]
        );
        assert!(kinds[2] == TrackKind::Jingle && kinds[5] == TrackKind::Jingle);
        // Batch rotation with the same config keeps the identical cadence:
        // one shared jingle logic, not two implementations drifting apart.
        let batch = generate(&lib, &cfg).unwrap();
        let batch_names: Vec<_> = batch.iter().map(|t| t.file_name.as_str()).collect();
        assert_eq!(
            batch_names,
            vec!["m1.mp3", "m2.mp3", "j1.mp3", "m3.mp3", "m1.mp3", "j1.mp3"]
        );
    }

    #[test]
    fn generate_next_without_jingles_stays_music() {
        let lib = mem_lib(&[("m1.mp3", "A", "Rock", 0)]);
        let cfg = jingle_cfg();
        let mut history = RuleHistory::default();
        for _ in 0..4 {
            let t = generate_next(&lib, &cfg, &mut history).unwrap().unwrap();
            assert_eq!(t.kind, TrackKind::Music);
        }
    }
}
