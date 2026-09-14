//! Play-log reporting (royalty/BMI-style exports).
//!
//! Reads the `play_log` table written by [`Library::record_play`] and
//! produces ranged, kind-filtered reports plus CSV text for export.

use chrono::{DateTime, Utc};
use rusqlite::params;

use crate::error::{CrabError, Result};
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
    let mut rows = stmt.query(params![from.to_rfc3339(), to.to_rfc3339()])?;
    let mut entries = Vec::new();
    while let Some(row) = rows.next()? {
        let title: Option<String> = row.get(0)?;
        let file_name: String = row.get(1)?;
        let artist: Option<String> = row.get(2)?;
        let kind: String = row.get(3)?;
        let played_at: String = row.get(4)?;
        let duration: Option<f64> = row.get(5)?;
        let played_at = DateTime::parse_from_rfc3339(&played_at)
            .map(|dt| dt.with_timezone(&Utc))
            .map_err(|_| CrabError::Integrity {
                // No numeric play_log id on the entry: the file
                // name identifies the row for the operator.
                table: "play_log",
                id: file_name.clone(),
                field: "played_at",
                value: played_at,
            })?;
        entries.push(PlayLogEntry {
            title: title.unwrap_or(file_name),
            artist: artist.unwrap_or_default(),
            kind: TrackKind::parse(&kind),
            played_at,
            duration_secs: duration,
        });
    }
    Ok(entries
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

/// Shared column layout for every tabular export (CSV, XLSX).
pub const REPORT_COLUMNS: [&str; 5] = ["played_at", "title", "artist", "kind", "duration_secs"];

/// One entry as plain display strings. No quoting here: each format
/// encodes on its own terms (CSV quotes, XLSX stores typed cells).
pub fn report_row(e: &PlayLogEntry) -> [String; 5] {
    [
        e.played_at.to_rfc3339(),
        e.title.clone(),
        e.artist.clone(),
        e.kind.as_str().to_string(),
        e.duration_secs.map(|d| d.to_string()).unwrap_or_default(),
    ]
}

/// RFC-4180-ish CSV for spreadsheets / royalty bodies.
pub fn to_csv(entries: &[PlayLogEntry]) -> String {
    // Header from the shared layout: CSV and XLSX cannot drift apart.
    let mut out = REPORT_COLUMNS.join(",") + "\n";
    for e in entries {
        let r = report_row(e);
        out.push_str(&format!(
            "{},{},{},{},{}\n",
            csv_cell(&r[0]),
            csv_cell(&r[1]),
            csv_cell(&r[2]),
            csv_cell(&r[3]),
            csv_cell(&r[4]),
        ));
    }
    out
}

/// XLSX workbook bytes for royalty bodies that want spreadsheets: same
/// rows and columns as [`to_csv`], values as plain strings so Excel
/// never mangles timestamps. `String` error (not [`Result`]): a
/// formatting failure surfaces verbatim in the export status line,
/// while database errors stay typed upstream.
pub fn to_xlsx(entries: &[PlayLogEntry]) -> std::result::Result<Vec<u8>, String> {
    let mut book = rust_xlsxwriter::Workbook::new();
    let sheet = book.add_worksheet();
    sheet
        .set_name("Plays")
        .map_err(|e| format!("xlsx sheet: {e}"))?;
    for (c, h) in REPORT_COLUMNS.iter().enumerate() {
        sheet
            .write_string(0, c as u16, *h)
            .map_err(|e| format!("xlsx header: {e}"))?;
    }
    for (r, e) in entries.iter().enumerate() {
        for (c, v) in report_row(e).iter().enumerate() {
            sheet
                .write_string((r + 1) as u32, c as u16, v)
                .map_err(|e| format!("xlsx row {}: {e}", r + 1))?;
        }
    }
    book.save_to_buffer().map_err(|e| format!("xlsx pack: {e}"))
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

    fn xlsx_entry() -> Vec<PlayLogEntry> {
        vec![PlayLogEntry {
            title: "Night Song".to_string(),
            artist: "Owl".to_string(),
            kind: TrackKind::Music,
            played_at: DateTime::parse_from_rfc3339("2024-05-01T10:00:00Z")
                .unwrap()
                .with_timezone(&Utc),
            duration_secs: Some(200.0),
        }]
    }

    #[test]
    fn report_row_is_raw_csv_escapes() {
        // Same entry, two encodings: the row carries raw values, CSV
        // quotes them, XLSX stores them as-is.
        let row = report_row(&xlsx_entry()[0]);
        assert_eq!(
            row,
            [
                "2024-05-01T10:00:00+00:00",
                "Night Song",
                "Owl",
                "music",
                "200",
            ]
        );
        let tricky = PlayLogEntry {
            title: "Say \"Hi\", Now".to_string(),
            artist: "A, B".to_string(),
            ..xlsx_entry()[0].clone()
        };
        let row = report_row(&tricky);
        assert_eq!(row[1], "Say \"Hi\", Now");
        assert!(to_csv(&[tricky]).contains("\"Say \"\"Hi\"\", Now\""));
    }

    #[test]
    fn xlsx_packs_a_workbook() {
        let bytes = to_xlsx(&xlsx_entry()).expect("xlsx packs");
        // ZIP container magic; content itself is zipped XML (no reader
        // in-tree, so the row mapping above carries the exactness).
        assert!(bytes.starts_with(b"PK"), "not a zip container");
        assert!(bytes.len() > 1_000, "suspiciously small: {}", bytes.len());
        let empty = to_xlsx(&[]).expect("header-only packs");
        assert!(empty.starts_with(b"PK"));
        assert!(bytes.len() > empty.len(), "rows must grow the workbook");
    }

    #[test]
    fn malformed_played_at_is_integrity_error() {
        let lib = seed();
        // Malformed but lexicographically inside the query window (the
        // range predicate compares strings, so an out-of-window value
        // would simply be filtered, never mapped).
        lib.conn()
            .execute(
                "INSERT INTO play_log (track_id, played_at, duration)
                 VALUES ('t1', '2026-13-45T99:99:99Z', 180.0)",
                [],
            )
            .unwrap();
        let from = DateTime::parse_from_rfc3339("2020-01-01T00:00:00Z")
            .unwrap()
            .with_timezone(&Utc);
        let to = DateTime::parse_from_rfc3339("2030-01-01T00:00:00Z")
            .unwrap()
            .with_timezone(&Utc);
        let err = play_report(&lib, from, to, &[]).expect_err("bad timestamp must fail");
        match err {
            crate::error::CrabError::Integrity {
                table,
                field,
                value,
                ..
            } => {
                assert_eq!(table, "play_log");
                assert_eq!(field, "played_at");
                assert_eq!(value, "2026-13-45T99:99:99Z");
            }
            other => panic!("expected Integrity error, got {other:?}"),
        }
    }
}
