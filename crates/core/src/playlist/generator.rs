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
    let mut artists: VecDeque<String> = VecDeque::new();
    let mut titles: VecDeque<String> = VecDeque::new();
    let mut albums: VecDeque<String> = VecDeque::new();
    let mut genres: VecDeque<String> = VecDeque::new();
    let mut music_since_jingle = 0usize;
    let mut jingle_cursor = 0usize;

    let push = |t: &Track,
                artists: &mut VecDeque<String>,
                titles: &mut VecDeque<String>,
                albums: &mut VecDeque<String>,
                genres: &mut VecDeque<String>,
                out: &mut Vec<Track>| {
        push_key(
            artists,
            t.artist.clone().unwrap_or_default(),
            cfg.artist_window,
        );
        push_key(titles, title_key(t), cfg.title_window);
        push_key(
            albums,
            t.album.clone().unwrap_or_default(),
            cfg.album_window,
        );
        push_key(genres, t.genre.clone().unwrap_or_default(), cfg.genre_gap);
        out.push(t.clone());
    };

    while out.len() < cfg.target_tracks {
        // Jingle slot (round-robin, skip if it just played).
        if cfg.jingles_every > 0 && music_since_jingle >= cfg.jingles_every && !jingles.is_empty() {
            let mut picked = None;
            for i in 0..jingles.len() {
                let idx = (jingle_cursor + i) % jingles.len();
                let last_title = titles.back().cloned().unwrap_or_default();
                if jingles.len() == 1 || title_key(&jingles[idx]) != last_title {
                    picked = Some(idx);
                    break;
                }
            }
            if let Some(idx) = picked {
                jingle_cursor = idx + 1;
                let j = jingles[idx].clone();
                push(
                    &j,
                    &mut artists,
                    &mut titles,
                    &mut albums,
                    &mut genres,
                    &mut out,
                );
                music_since_jingle = 0;
                continue;
            }
        }
        // Music slot: first ranked candidate satisfying every rule, else relax.
        let choice = music
            .iter()
            .find(|t| {
                !within(&artists, &t.artist.clone().unwrap_or_default())
                    && !within(&titles, &title_key(t))
                    && !within(&albums, &t.album.clone().unwrap_or_default())
                    && !within(&genres, &t.genre.clone().unwrap_or_default())
            })
            .or_else(|| music.first())
            .cloned();
        match choice {
            Some(t) => {
                push(
                    &t,
                    &mut artists,
                    &mut titles,
                    &mut albums,
                    &mut genres,
                    &mut out,
                );
                music_since_jingle += 1;
            }
            None => break,
        }
    }
    Ok(out)
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
}
