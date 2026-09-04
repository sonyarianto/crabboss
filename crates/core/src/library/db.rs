//! SQLite-backed music library

use std::path::Path;

use chrono::{DateTime, Utc};
use rusqlite::{params, Connection};
use uuid::Uuid;

use crate::error::Result;

/// What a track *is* for station purposes. Jingles/bumpers/sweepers/IDs
/// and ads are plain audio files — `kind` only changes how the station
/// treats them (repeat protection, reports, generator slots, carts).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TrackKind {
    Music,
    Jingle,
    Ad,
}

impl TrackKind {
    pub fn as_str(self) -> &'static str {
        match self {
            TrackKind::Music => "music",
            TrackKind::Jingle => "jingle",
            TrackKind::Ad => "ad",
        }
    }

    pub fn parse(s: &str) -> Self {
        match s.to_lowercase().as_str() {
            "jingle" | "bumper" | "sweeper" | "id" | "stationid" | "toth" => TrackKind::Jingle,
            "ad" | "advert" | "commercial" | "promo" => TrackKind::Ad,
            _ => TrackKind::Music,
        }
    }

    /// Guess kind from a file path (folder names like `Jingles/`, `Ads/`).
    pub fn classify_path(path: &std::path::Path) -> Self {
        let lower = path.to_string_lossy().to_lowercase();
        // Check directory components first, then the file stem.
        let is_dir_hit = |keys: &[&str]| {
            path.parent()
                .map(|p| {
                    let d = p.to_string_lossy().to_lowercase();
                    keys.iter().any(|k| d.contains(k))
                })
                .unwrap_or(false)
        };
        if is_dir_hit(&[
            "jingle",
            "bumper",
            "sweeper",
            "stationid",
            "station id",
            "toth",
            "stinger",
            "beds",
            "liners",
            "drops",
        ]) {
            return TrackKind::Jingle;
        }
        if is_dir_hit(&["ads", "advert", "commercial", "promo", "spot"]) {
            return TrackKind::Ad;
        }
        let stem = path
            .file_stem()
            .map(|s| s.to_string_lossy().to_lowercase())
            .unwrap_or_default();
        for key in [
            "jingle",
            "bumper",
            "sweeper",
            "toth",
            "stationid",
            "stinger",
        ] {
            if stem.contains(key) {
                return TrackKind::Jingle;
            }
        }
        for key in ["advert", "commercial", "promo", "spot"] {
            if stem.contains(key) {
                return TrackKind::Ad;
            }
        }
        // Bare " ad "/"id" tokens only (avoid matching "radio", "madonna", ...).
        let padded = format!(" {} ", stem.replace(['_', '-', '.'], " "));
        if padded.contains(" ad ") || padded.contains(" ads ") {
            return TrackKind::Ad;
        }
        if padded.contains(" id ") || padded.contains(" ids ") {
            return TrackKind::Jingle;
        }
        let _ = lower;
        TrackKind::Music
    }
}

/// Unique identifier for a track in the library.
pub type TrackId = String;

/// Represents a single audio track in the library.
#[derive(Debug, Clone)]
pub struct Track {
    pub id: TrackId,
    pub file_path: String,
    pub file_name: String,
    pub title: Option<String>,
    pub artist: Option<String>,
    pub album: Option<String>,
    pub genre: Option<String>,
    pub year: Option<i32>,
    pub track_number: Option<i32>,
    pub duration_secs: Option<f64>,
    pub bpm: Option<f64>,
    pub file_size: Option<i64>,
    pub sample_rate: Option<i32>,
    pub channels: Option<i32>,
    /// Station role: music vs jingle/bumper vs ad. Same audio, different rules.
    pub kind: TrackKind,
    pub tags: Vec<String>,
    pub added_at: DateTime<Utc>,
    pub last_played_at: Option<DateTime<Utc>>,
    pub play_count: i32,
}

/// The music library backed by SQLite.
pub struct Library {
    conn: Connection,
}

impl Library {
    /// Open or create a library database at the given path.
    pub fn open(path: &Path) -> Result<Self> {
        let conn = Connection::open(path)?;
        let lib = Self { conn };
        lib.init_tables()?;
        Ok(lib)
    }

    /// Initialize the database schema.
    fn init_tables(&self) -> Result<()> {
        self.conn.execute_batch(
            "
            CREATE TABLE IF NOT EXISTS tracks (
                id              TEXT PRIMARY KEY,
                file_path       TEXT NOT NULL UNIQUE,
                file_name       TEXT NOT NULL,
                title           TEXT,
                artist          TEXT,
                album           TEXT,
                genre           TEXT,
                year            INTEGER,
                track_number    INTEGER,
                duration_secs   REAL,
                bpm             REAL,
                file_size       INTEGER,
                sample_rate     INTEGER,
                channels        INTEGER,
                added_at        TEXT NOT NULL,
                last_played_at  TEXT,
                play_count      INTEGER NOT NULL DEFAULT 0
            );

            CREATE INDEX IF NOT EXISTS idx_tracks_artist ON tracks(artist);
            CREATE INDEX IF NOT EXISTS idx_tracks_album  ON tracks(album);
            CREATE INDEX IF NOT EXISTS idx_tracks_genre  ON tracks(genre);

            CREATE TABLE IF NOT EXISTS tags (
                track_id TEXT NOT NULL,
                tag      TEXT NOT NULL,
                PRIMARY KEY (track_id, tag),
                FOREIGN KEY (track_id) REFERENCES tracks(id) ON DELETE CASCADE
            );

            CREATE TABLE IF NOT EXISTS play_log (
                id          INTEGER PRIMARY KEY AUTOINCREMENT,
                track_id    TEXT NOT NULL,
                played_at   TEXT NOT NULL,
                duration    REAL,
                FOREIGN KEY (track_id) REFERENCES tracks(id) ON DELETE CASCADE
            );
            ",
        )?;
        // Migrate pre-kind databases: add the column if missing.
        let has_kind: bool = self
            .conn
            .prepare("PRAGMA table_info(tracks)")?
            .query_map([], |row| row.get::<_, String>(1))?
            .collect::<std::result::Result<Vec<_>, _>>()?
            .iter()
            .any(|c| c == "kind");
        if !has_kind {
            self.conn.execute(
                "ALTER TABLE tracks ADD COLUMN kind TEXT NOT NULL DEFAULT 'music'",
                [],
            )?;
        }
        Ok(())
    }

    /// Add a track to the library by reading its metadata.
    /// Kind is auto-classified from the path (Jingles/…, *_bumper.mp3, …).
    pub fn add_track(&self, path: &Path) -> Result<Track> {
        let file_path = path.to_string_lossy().to_string();
        let file_name = path
            .file_name()
            .map(|n| n.to_string_lossy().to_string())
            .unwrap_or_default();

        // Read metadata using lofty
        let (title, artist, album, genre, year, track_number, duration, sample_rate, channels) =
            read_metadata(path)?;

        let id = Uuid::new_v4().to_string();
        let added_at = Utc::now();
        let file_size = std::fs::metadata(path).ok().map(|m| m.len() as i64);
        let kind = TrackKind::classify_path(path);

        self.conn.execute(
            "INSERT OR IGNORE INTO tracks
             (id, file_path, file_name, title, artist, album, genre, year,
              track_number, duration_secs, file_size, sample_rate, channels, added_at, play_count, kind)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, 0, ?15)",
            params![
                id,
                file_path,
                file_name,
                title,
                artist,
                album,
                genre,
                year,
                track_number,
                duration,
                file_size,
                sample_rate,
                channels,
                added_at.to_rfc3339(),
                kind.as_str(),
            ],
        )?;

        Ok(Track {
            id,
            file_path,
            file_name,
            title,
            artist,
            album,
            genre,
            year,
            track_number,
            duration_secs: duration,
            bpm: None,
            file_size,
            sample_rate,
            channels,
            kind,
            tags: Vec::new(),
            added_at,
            last_played_at: None,
            play_count: 0,
        })
    }

    /// Override a track's station role (e.g. mark a file as jingle).
    pub fn set_kind(&self, id: &str, kind: TrackKind) -> Result<()> {
        self.conn.execute(
            "UPDATE tracks SET kind = ?1 WHERE id = ?2",
            params![kind.as_str(), id],
        )?;
        Ok(())
    }

    /// All tracks of one kind (used by carts, generator jingle slots, filters).
    pub fn list_by_kind(&self, kind: TrackKind) -> Result<Vec<Track>> {
        let mut stmt = self.conn.prepare(
            "SELECT id, file_path, file_name, title, artist, album, genre, year,
                    track_number, duration_secs, file_size, sample_rate, channels,
                    kind, added_at, last_played_at, play_count
             FROM tracks WHERE kind = ?1 ORDER BY file_name",
        )?;
        let tracks = stmt
            .query_map(params![kind.as_str()], Self::map_row)?
            .collect::<std::result::Result<Vec<_>, _>>()?;
        Ok(tracks)
    }

    fn map_row(row: &rusqlite::Row) -> std::result::Result<Track, rusqlite::Error> {
        Ok(Track {
            id: row.get(0)?,
            file_path: row.get(1)?,
            file_name: row.get(2)?,
            title: row.get(3)?,
            artist: row.get(4)?,
            album: row.get(5)?,
            genre: row.get(6)?,
            year: row.get(7)?,
            track_number: row.get(8)?,
            duration_secs: row.get(9)?,
            bpm: None,
            file_size: row.get(10)?,
            sample_rate: row.get(11)?,
            channels: row.get(12)?,
            kind: TrackKind::parse(&row.get::<_, String>(13).unwrap_or_default()),
            tags: Vec::new(),
            added_at: DateTime::parse_from_rfc3339(&row.get::<_, String>(14)?)
                .map(|dt| dt.with_timezone(&Utc))
                .unwrap_or_else(|_| Utc::now()),
            last_played_at: row
                .get::<_, Option<String>>(15)?
                .and_then(|s| DateTime::parse_from_rfc3339(&s).ok())
                .map(|dt| dt.with_timezone(&Utc)),
            play_count: row.get(16)?,
        })
    }

    /// Get all tracks in the library.
    pub fn get_all_tracks(&self) -> Result<Vec<Track>> {
        let mut stmt = self.conn.prepare(
            "SELECT id, file_path, file_name, title, artist, album, genre, year,
                    track_number, duration_secs, file_size, sample_rate, channels,
                    kind, added_at, last_played_at, play_count
             FROM tracks ORDER BY artist, album, title",
        )?;

        let tracks = stmt
            .query_map([], Self::map_row)?
            .collect::<std::result::Result<Vec<_>, _>>()?;

        Ok(tracks)
    }

    /// Get a single track by ID.
    pub fn get_track(&self, id: &str) -> Result<Option<Track>> {
        let mut stmt = self.conn.prepare(
            "SELECT id, file_path, file_name, title, artist, album, genre, year,
                    track_number, duration_secs, file_size, sample_rate, channels,
                    kind, added_at, last_played_at, play_count
             FROM tracks WHERE id = ?1",
        )?;

        let mut rows = stmt.query_map(params![id], Self::map_row)?;

        Ok(rows.next().transpose()?)
    }

    /// Search tracks by query string (matches title, artist, album, filename).
    pub fn search(&self, query: &str) -> Result<Vec<Track>> {
        let like = format!("%{}%", query);
        let mut stmt = self.conn.prepare(
            "SELECT id, file_path, file_name, title, artist, album, genre, year,
                    track_number, duration_secs, file_size, sample_rate, channels,
                    kind, added_at, last_played_at, play_count
             FROM tracks
             WHERE title LIKE ?1 OR artist LIKE ?1 OR album LIKE ?1
                    OR file_name LIKE ?1 OR genre LIKE ?1
             ORDER BY title",
        )?;

        let tracks = stmt
            .query_map(params![like], Self::map_row)?
            .collect::<std::result::Result<Vec<_>, _>>()?;

        Ok(tracks)
    }

    /// Record that a track was played.
    pub fn record_play(&self, track_id: &str, duration: Option<f64>) -> Result<()> {
        let now = Utc::now();
        self.conn.execute(
            "UPDATE tracks SET play_count = play_count + 1, last_played_at = ?1
             WHERE id = ?2",
            params![now.to_rfc3339(), track_id],
        )?;
        self.conn.execute(
            "INSERT INTO play_log (track_id, played_at, duration) VALUES (?1, ?2, ?3)",
            params![track_id, now.to_rfc3339(), duration],
        )?;
        Ok(())
    }

    /// Remove a track from the library by ID.
    pub fn remove_track(&self, id: &str) -> Result<()> {
        self.conn
            .execute("DELETE FROM tracks WHERE id = ?1", params![id])?;
        Ok(())
    }

    /// Recursively scan a directory for audio files and add them to the library.
    /// Returns the count of successfully added tracks.
    pub fn scan_directory(&self, dir: &Path) -> Result<usize> {
        const AUDIO_EXTENSIONS: &[&str] = &[
            "mp3", "flac", "aac", "ogg", "wav", "aiff", "opus", "wv", "mpc", "m4a",
        ];
        let mut added = 0;
        for entry in walkdir::WalkDir::new(dir)
            .follow_links(true)
            .into_iter()
            .filter_map(|e| e.ok())
        {
            if !entry.file_type().is_file() {
                continue;
            }
            let path = entry.path();
            let ext = path
                .extension()
                .and_then(|e| e.to_str())
                .unwrap_or("")
                .to_lowercase();
            if !AUDIO_EXTENSIONS.contains(&ext.as_str()) {
                continue;
            }
            match self.add_track(path) {
                Ok(_) => added += 1,
                Err(e) => tracing::warn!("Skipping {}: {}", path.display(), e),
            }
        }
        tracing::info!("Scanned {}: {} tracks added", dir.display(), added);
        Ok(added)
    }
}

/// Read metadata from an audio file using lofty.
#[allow(clippy::type_complexity)]
fn read_metadata(
    path: &Path,
) -> Result<(
    Option<String>,
    Option<String>,
    Option<String>,
    Option<String>,
    Option<i32>,
    Option<i32>,
    Option<f64>,
    Option<i32>,
    Option<i32>,
)> {
    use lofty::file::{AudioFile, TaggedFileExt};
    use lofty::tag::Accessor;

    let tagged_file = lofty::read_from_path(path)
        .map_err(|e| crate::error::CrabError::Metadata(e.to_string()))?;

    let props = tagged_file.properties();
    let duration = props.duration().as_secs_f64();
    let sample_rate = props.sample_rate().map(|v| v as i32);
    let channels = props.channels().map(|v| v as i32);

    let (title, artist, album, genre, year, track_number) =
        if let Some(tag) = tagged_file.primary_tag() {
            (
                tag.title().map(|s| s.to_string()),
                tag.artist().map(|s| s.to_string()),
                tag.album().map(|s| s.to_string()),
                tag.genre().map(|s| s.to_string()),
                tag.year().map(|v| v as i32),
                tag.track().map(|v| v as i32),
            )
        } else if let Some(tag) = tagged_file.first_tag() {
            (
                tag.title().map(|s| s.to_string()),
                tag.artist().map(|s| s.to_string()),
                tag.album().map(|s| s.to_string()),
                tag.genre().map(|s| s.to_string()),
                tag.year().map(|v| v as i32),
                tag.track().map(|v| v as i32),
            )
        } else {
            (None, None, None, None, None, None)
        };

    Ok((
        title,
        artist,
        album,
        genre,
        year,
        track_number,
        Some(duration),
        sample_rate,
        channels,
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    #[test]
    fn classify_paths() {
        let j = |p: &str| TrackKind::classify_path(&PathBuf::from(p));
        assert_eq!(j("C:/radio/Jingles/toth.mp3"), TrackKind::Jingle);
        assert_eq!(j("/music/Station IDs/id1.wav"), TrackKind::Jingle);
        assert_eq!(j("/music/song_bumper.mp3"), TrackKind::Jingle);
        assert_eq!(j("/music/Ads/coke.mp3"), TrackKind::Ad);
        assert_eq!(j("/music/summer_ad.mp3"), TrackKind::Ad);
        assert_eq!(j("/music/Rock/madonna_hit.mp3"), TrackKind::Music);
        assert_eq!(j("/music/Rock/song.mp3"), TrackKind::Music);
    }

    #[test]
    fn set_kind_and_list_by_kind() {
        let dir = std::env::temp_dir().join(format!("crabboss-kind-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        let lib = Library::open(&dir.join("lib.db")).unwrap();
        // Seed rows directly (avoids needing real audio files).
        for (name, kind) in [("a.mp3", "music"), ("b.mp3", "jingle"), ("c.mp3", "ad")] {
            lib.conn
                .execute(
                    "INSERT INTO tracks (id, file_path, file_name, added_at, play_count, kind)
                 VALUES (?1, ?2, ?3, ?4, 0, ?5)",
                    rusqlite::params![
                        uuid::Uuid::new_v4().to_string(),
                        format!("/m/{}", name),
                        name,
                        chrono::Utc::now().to_rfc3339(),
                        kind,
                    ],
                )
                .unwrap();
        }
        assert_eq!(lib.list_by_kind(TrackKind::Music).unwrap().len(), 1);
        assert_eq!(lib.list_by_kind(TrackKind::Jingle).unwrap().len(), 1);
        let all = lib.get_all_tracks().unwrap();
        assert_eq!(all.len(), 3);
        let jingle = all.iter().find(|t| t.kind == TrackKind::Jingle).unwrap();
        lib.set_kind(&jingle.id, TrackKind::Music).unwrap();
        assert_eq!(lib.list_by_kind(TrackKind::Music).unwrap().len(), 2);
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn migrates_pre_kind_database() {
        let dir = std::env::temp_dir().join(format!("crabboss-mig-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("lib.db");
        // Create an old-schema db with all columns except kind.
        {
            let conn = rusqlite::Connection::open(&path).unwrap();
            conn.execute_batch(
                "CREATE TABLE tracks (
                    id TEXT PRIMARY KEY, file_path TEXT NOT NULL UNIQUE,
                    file_name TEXT NOT NULL, title TEXT, artist TEXT, album TEXT,
                    genre TEXT, year INTEGER, track_number INTEGER,
                    duration_secs REAL, bpm REAL, file_size INTEGER,
                    sample_rate INTEGER, channels INTEGER,
                    added_at TEXT NOT NULL, last_played_at TEXT,
                    play_count INTEGER NOT NULL DEFAULT 0
                );",
            )
            .unwrap();
            conn.execute(
                "INSERT INTO tracks (id, file_path, file_name, added_at)
                 VALUES ('1', '/m/old.mp3', 'old.mp3', '2024-01-01T00:00:00Z')",
                [],
            )
            .unwrap();
        }
        let lib = Library::open(&path).unwrap();
        let all = lib.get_all_tracks().unwrap();
        assert_eq!(all.len(), 1);
        assert_eq!(all[0].kind, TrackKind::Music);
        std::fs::remove_dir_all(&dir).ok();
    }
}
