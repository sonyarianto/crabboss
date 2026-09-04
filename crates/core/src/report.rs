//! Play-log reporting (royalty/BMI-style exports).
//!
//! Reads the `play_log` table written by [`Library::record_play`] and
//! produces ranged, kind-filtered reports plus CSV text for export.

use chrono::{DateTime, Utc};
use rusqlite::params;

use crate::error::Result;
use crate::library::{Library, TrackKind};

/// One played item in a report (newest first).
#[derive(Debug, Clone)]
pub struct PlayLogEntry {
    pub title: String,
    pub artist: String,
    pub kind: TrackKind,
    pub played_at: DateTime<Utc>,
    pub duration_secs: Option<f64>,
}

/// Plays between `from` and `to` (inclusive), skipping `exclude` kinds
/// (jingles/IDs are not music airplay and usually stay out of reports).
pub fn play_report(
    library: &Library,
    from: DateTime<Utc>,
    to: DateTime<Utc>,
    exclude: &[TrackKind],
) -> Result<Vec<PlayLogEntry>> {
    let mut stmt = library.conn().prepare(
        "SELECT t.title, t.file_name, t.artist, t.kind, p.played_at, p.duration
         FROM play_log p JOIN tracks t ON t.id = p.track_id
         WHERE p.played_at >= ?1 AND p.played_at <= ?2
         ORDER BY p.played_at DESC",
    )?;
    let rows = stmt
        .query_map(params![from.to_rfc3339(), to.to_rfc3339()], |row| {
            let title: Option<String> = row.get(0)?;
            let file_name: String = row.get(1)?;
            let artist: Option<String> = row.get(2)?;
            let kind: String = row.get(3)?;
            let played_at: String = row.get(4)?;
            let duration: Option<f64> = row.get(5)?;
            Ok(PlayLogEntry {
                title: title.unwrap_or(file_name),
                artist: artist.unwrap_or_default(),
                kind: TrackKind::parse(&kind),
                played_at: DateTime::parse_from_rfc3339(&played_at)
                    .map(|dt| dt.with_timezone(&Utc))
                    .unwrap_or_else(|_| Utc::now()),
                duration_secs: duration,
            })
        })?
        .collect::<std::result::Result<Vec<_>, _>>()?;
    Ok(rows
        .into_iter()
        .filter(|e| !exclude.contains(&e.kind))
        .collect())
}

fn csv_cell(s: &str) -> String {
    if s.contains([',', '"', '\n']) {
        format!("\"{}\"", s.replace('"', "\"\""))
    } else {
        s.to_string()
    }
}

/// RFC-4180-ish CSV for spreadsheets / royalty bodies.
pub fn to_csv(entries: &[PlayLogEntry]) -> String {
    let mut out = String::from("played_at,title,artist,kind,duration_secs\n");
    for e in entries {
        out.push_str(&format!(
            "{},{},{},{},{}\n",
            e.played_at.to_rfc3339(),
            csv_cell(&e.title),
            csv_cell(&e.artist),
            e.kind.as_str(),
            e.duration_secs.map(|d| d.to_string()).unwrap_or_default(),
        ));
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn seed() -> Library {
        let lib = Library::open(std::path::Path::new(":memory:")).unwrap();
        for (id, file, title, kind) in [
            ("t1", "/m/song.mp3", "Song", "music"),
            ("t2", "/m/id.mp3", "Stn ID", "jingle"),
            ("t3", "/m/ad.mp3", "Ad", "ad"),
        ] {
            lib.conn()
                .execute(
                    "INSERT INTO tracks
                     (id, file_path, file_name, title, added_at, play_count, kind)
                     VALUES (?1, ?2, ?3, ?4, '2024-01-01T00:00:00Z', 0, ?5)",
                    rusqlite::params![id, file, file, title, kind],
                )
                .unwrap();
        }
        lib
    }

    #[test]
    fn records_and_ranges() {
        let lib = seed();
        lib.record_play("t1", Some(180.0)).unwrap();
        let all = play_report(
            &lib,
            Utc::now() - chrono::Duration::days(1),
            Utc::now() + chrono::Duration::days(1),
            &[],
        )
        .unwrap();
        assert_eq!(all.len(), 1);
        assert_eq!(all[0].title, "Song");
        // Out-of-range window finds nothing.
        let old = play_report(
            &lib,
            Utc::now() - chrono::Duration::days(30),
            Utc::now() - chrono::Duration::days(20),
            &[],
        )
        .unwrap();
        assert!(old.is_empty());
    }

    #[test]
    fn excludes_kinds() {
        let lib = seed();
        for id in ["t1", "t2", "t3"] {
            lib.record_play(id, None).unwrap();
        }
        let music_only = play_report(
            &lib,
            Utc::now() - chrono::Duration::days(1),
            Utc::now() + chrono::Duration::days(1),
            &[TrackKind::Jingle, TrackKind::Ad],
        )
        .unwrap();
        assert_eq!(music_only.len(), 1);
        assert_eq!(music_only[0].kind, TrackKind::Music);
    }

    #[test]
    fn csv_escapes() {
        let entries = vec![PlayLogEntry {
            title: "Say \"Hi\", Now".to_string(),
            artist: "A, B".to_string(),
            kind: TrackKind::Music,
            played_at: DateTime::parse_from_rfc3339("2024-05-01T10:00:00Z")
                .unwrap()
                .with_timezone(&Utc),
            duration_secs: Some(200.0),
        }];
        let csv = to_csv(&entries);
        assert!(csv.starts_with("played_at,title,artist,kind,duration_secs\n"));
        assert!(csv.contains("\"Say \"\"Hi\"\", Now\",\"A, B\",music,200"));
    }
}
