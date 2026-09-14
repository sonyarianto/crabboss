//! Possible-duplicate detection (non-destructive, MVP).
//!
//! Grouping key: normalized title + artist, with a duration gate when
//! both sides know their length. Deliberately metadata-based, not
//! content-hash: two different encodings/rips of the same song differ
//! in bytes (and file size), so hashing would *miss* exactly the dupes
//! this view is for. File size and path ride along in each group for
//! the human decision.
//!
//! Rules (documented so the UI never has to guess):
//! - Title falls back to the file-name stem when the tag is missing; a
//!   track with neither is never grouped.
//! - Artist must match exactly after normalization — an untagged track
//!   only matches other untagged tracks (fewer false flags).
//! - Duration splits a group only when both sides are known and differ
//!   by more than the tolerance; unknown lengths join the first
//!   cluster (weak match, human decides).
//! - Nothing here deletes, merges, or rewrites anything. Content-hash
//!   confirmation is an explicit follow-up, not this MVP.

use std::collections::{BTreeMap, HashSet};

use super::db::{Track, TrackId};

/// Default duration gate: same song, different rip/encode, rarely drifts
/// further than this.
pub const DURATION_TOLERANCE_SECS: f64 = 2.0;

/// Normalize one metadata field for comparison: lowercase, drop
/// apostrophes (so "don't" meets "dont"), turn other punctuation into
/// separators (so "hip-hop" meets "hip hop"), collapse whitespace.
pub fn normalize_text(s: &str) -> String {
    s.to_lowercase()
        .chars()
        .filter_map(|c| {
            if c.is_alphanumeric() {
                Some(c)
            } else if matches!(c, '\'' | '\u{2018}' | '\u{2019}' | '`') {
                None
            } else {
                Some(' ')
            }
        })
        .collect::<String>()
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
}

/// File-name stem without extension (`song` from `C:/mix/song (1).mp3`),
/// normalized for the untagged-title fallback.
fn normalize_stem(file_name: &str) -> String {
    let stem = file_name.rsplit(['/', '\\']).next().unwrap_or(file_name);
    let stem = stem.rsplit_once('.').map(|(s, _)| s).unwrap_or(stem);
    normalize_text(stem)
}

/// Grouping key for one track, or `None` when there is nothing usable
/// to match on.
fn group_key(t: &Track) -> Option<(String, String)> {
    let title = match t.title.as_deref() {
        Some(s) if !normalize_text(s).is_empty() => normalize_text(s),
        _ => normalize_stem(&t.file_name),
    };
    if title.is_empty() {
        return None;
    }
    let artist = normalize_text(t.artist.as_deref().unwrap_or(""));
    Some((title, artist))
}

/// One set of tracks that look like the same song. Display strings keep
/// the first track's original casing; `tracks` are sorted by path so
/// the view is stable run to run.
#[derive(Debug, Clone)]
pub struct DuplicateGroup {
    pub title: String,
    pub artist: String,
    pub tracks: Vec<Track>,
}

/// Group `tracks` into possible duplicates. Pure and deterministic:
/// same input, same groups in the same order. Only clusters of 2+
/// are returned.
pub fn find_duplicate_groups(tracks: &[Track], tolerance_secs: f64) -> Vec<DuplicateGroup> {
    let mut by_key: BTreeMap<(String, String), Vec<usize>> = BTreeMap::new();
    for (i, t) in tracks.iter().enumerate() {
        if let Some(key) = group_key(t) {
            by_key.entry(key).or_default().push(i);
        }
    }
    let mut out = Vec::new();
    for ((_, _), idxs) in by_key {
        if idxs.len() < 2 {
            continue;
        }
        // Split one key into duration clusters: known lengths chain in
        // ascending order (a gap past tolerance starts a new cluster);
        // unknown lengths attach to the first cluster as weak matches.
        let mut known: Vec<usize> = idxs
            .iter()
            .copied()
            .filter(|&i| tracks[i].duration_secs.is_some())
            .collect();
        known.sort_by(|&a, &b| {
            tracks[a]
                .duration_secs
                .partial_cmp(&tracks[b].duration_secs)
                .unwrap_or(std::cmp::Ordering::Equal)
        });
        let unknown: Vec<usize> = idxs
            .iter()
            .copied()
            .filter(|&i| tracks[i].duration_secs.is_none())
            .collect();
        let mut clusters: Vec<Vec<usize>> = Vec::new();
        for i in known {
            let dur = tracks[i].duration_secs.unwrap_or(0.0);
            let extend = clusters.last().and_then(|c: &Vec<usize>| {
                c.iter()
                    .filter_map(|&j| tracks[j].duration_secs)
                    .next_back()
            });
            match extend {
                Some(last) if (dur - last).abs() <= tolerance_secs => {
                    clusters.last_mut().expect("checked above").push(i);
                }
                _ => clusters.push(vec![i]),
            }
        }
        if clusters.is_empty() {
            // Nobody knows its length: one weak cluster, human decides.
            clusters.push(Vec::new());
        }
        clusters[0].extend(unknown);
        for c in clusters {
            if c.len() < 2 {
                continue;
            }
            let mut members: Vec<Track> = c.into_iter().map(|i| tracks[i].clone()).collect();
            members.sort_by(|a, b| a.file_path.cmp(&b.file_path));
            let title = members[0]
                .title
                .clone()
                .unwrap_or_else(|| members[0].file_name.clone());
            let artist = members[0].artist.clone().unwrap_or_default();
            out.push(DuplicateGroup {
                title,
                artist,
                tracks: members,
            });
        }
    }
    out
}

/// Ids of every track sitting in any group — the UI's "duplicates only"
/// filter without recomputing groups per row.
pub fn duplicate_ids(groups: &[DuplicateGroup]) -> HashSet<TrackId> {
    groups
        .iter()
        .flat_map(|g| g.tracks.iter().map(|t| t.id.clone()))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::library::TrackKind;
    use chrono::Utc;

    fn track(id: &str, title: Option<&str>, artist: Option<&str>, dur: Option<f64>) -> Track {
        track_file(id, title, artist, dur, &format!("C:/mix/{id}.mp3"))
    }

    fn track_file(
        id: &str,
        title: Option<&str>,
        artist: Option<&str>,
        dur: Option<f64>,
        path: &str,
    ) -> Track {
        Track {
            id: id.to_string(),
            file_path: path.to_string(),
            file_name: path.rsplit(['/', '\\']).next().unwrap_or(path).to_string(),
            title: title.map(str::to_string),
            artist: artist.map(str::to_string),
            album: None,
            genre: None,
            year: None,
            track_number: None,
            duration_secs: dur,
            bpm: None,
            file_size: None,
            sample_rate: None,
            channels: None,
            kind: TrackKind::Music,
            daypart_start: None,
            daypart_end: None,
            daypart_days: "Daily".to_string(),
            loudness_lufs: None,
            loudness_gain_db: None,
            tags: Vec::new(),
            added_at: Utc::now(),
            last_played_at: None,
            play_count: 0,
        }
    }

    fn ids(groups: &[DuplicateGroup]) -> Vec<Vec<String>> {
        groups
            .iter()
            .map(|g| g.tracks.iter().map(|t| t.id.clone()).collect())
            .collect()
    }

    #[test]
    fn normalize_folds_case_space_and_punctuation() {
        assert_eq!(normalize_text("  Hip-Hop  Anthem! "), "hip hop anthem");
        assert_eq!(normalize_text("DON'T Stop"), "dont stop");
        assert_eq!(normalize_text(""), "");
    }

    #[test]
    fn same_song_groups_despite_case_and_spacing() {
        let tracks = vec![
            track("a", Some("Morning Show"), Some("The Band"), Some(180.0)),
            track("b", Some("  morning  show "), Some("the band"), Some(180.5)),
        ];
        assert_eq!(
            ids(&find_duplicate_groups(&tracks, 2.0)),
            vec![vec!["a", "b"]]
        );
    }

    #[test]
    fn different_artist_or_title_stays_apart() {
        let tracks = vec![
            track("a", Some("Song"), Some("Band A"), Some(180.0)),
            track("b", Some("Song"), Some("Band B"), Some(180.0)),
            track("c", Some("Other"), Some("Band A"), Some(180.0)),
        ];
        assert!(find_duplicate_groups(&tracks, 2.0).is_empty());
    }

    #[test]
    fn untagged_matches_only_untagged() {
        let tracks = vec![
            track("a", Some("Song"), None, Some(180.0)),
            track("b", Some("Song"), Some("Band"), Some(180.0)),
        ];
        assert!(find_duplicate_groups(&tracks, 2.0).is_empty());
    }

    #[test]
    fn untagged_title_falls_back_to_file_stem() {
        let tracks = vec![
            track_file("a", None, None, Some(180.0), "D:/rips/song.mp3"),
            track_file("b", None, None, Some(180.0), "E:/backup/song.mp3"),
            track_file("c", None, None, Some(180.0), "E:/backup/other.mp3"),
        ];
        assert_eq!(
            ids(&find_duplicate_groups(&tracks, 2.0)),
            vec![vec!["a", "b"]]
        );
    }

    #[test]
    fn duration_gate_splits_versions() {
        let tracks = vec![
            track("a", Some("Song"), Some("Band"), Some(180.0)),
            track("b", Some("Song"), Some("Band"), Some(181.0)),
            track("c", Some("Song"), Some("Band"), Some(240.0)),
        ];
        assert_eq!(
            ids(&find_duplicate_groups(&tracks, 2.0)),
            vec![vec!["a", "b"]]
        );
    }

    #[test]
    fn unknown_duration_joins_as_weak_match() {
        let tracks = vec![
            track("a", Some("Song"), Some("Band"), Some(180.0)),
            track("b", Some("Song"), Some("Band"), None),
        ];
        assert_eq!(
            ids(&find_duplicate_groups(&tracks, 2.0)),
            vec![vec!["a", "b"]]
        );
    }

    #[test]
    fn all_unknown_lengths_still_flagged() {
        let tracks = vec![
            track("a", Some("Song"), Some("Band"), None),
            track("b", Some("Song"), Some("Band"), None),
        ];
        assert_eq!(
            ids(&find_duplicate_groups(&tracks, 2.0)),
            vec![vec!["a", "b"]]
        );
    }

    #[test]
    fn output_is_deterministic() {
        let tracks = vec![
            track("b", Some("Zed"), Some("Band"), Some(180.0)),
            track("a", Some("Zed"), Some("Band"), Some(180.0)),
            track("d", Some("Alpha"), Some("Band"), Some(180.0)),
            track("c", Some("Alpha"), Some("Band"), Some(180.0)),
        ];
        let groups = find_duplicate_groups(&tracks, 2.0);
        assert_eq!(groups.len(), 2);
        assert_eq!(groups[0].title, "Alpha");
        assert_eq!(groups[1].title, "Zed");
        assert_eq!(ids(&groups), vec![vec!["c", "d"], vec!["a", "b"]]);
        // Display keeps original casing, members sort by path.
        assert_eq!(groups[0].tracks[0].file_path, "C:/mix/c.mp3");
    }

    #[test]
    fn duplicate_ids_covers_group_members_only() {
        let tracks = vec![
            track("a", Some("Song"), Some("Band"), Some(180.0)),
            track("b", Some("Song"), Some("Band"), Some(180.0)),
            track("c", Some("Solo"), Some("Band"), Some(180.0)),
        ];
        let set = duplicate_ids(&find_duplicate_groups(&tracks, 2.0));
        assert!(set.contains("a") && set.contains("b") && !set.contains("c"));
    }
}
