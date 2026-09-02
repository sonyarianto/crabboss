//! SQLite-backed music library

use std::path::Path;

use chrono::{DateTime, Utc};
use rusqlite::{params, Connection};
use uuid::Uuid;

use crate::error::Result;

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
        Ok(())
    }

    /// Add a track to the library by reading its metadata.
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

        self.conn.execute(
            "INSERT OR IGNORE INTO tracks
             (id, file_path, file_name, title, artist, album, genre, year,
              track_number, duration_secs, file_size, sample_rate, channels, added_at, play_count)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, 0)",
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
            tags: Vec::new(),
            added_at,
            last_played_at: None,
            play_count: 0,
        })
    }

    /// Get all tracks in the library.
    pub fn get_all_tracks(&self) -> Result<Vec<Track>> {
        let mut stmt = self.conn.prepare(
            "SELECT id, file_path, file_name, title, artist, album, genre, year,
                    track_number, duration_secs, file_size, sample_rate, channels,
                    added_at, last_played_at, play_count
             FROM tracks ORDER BY artist, album, title",
        )?;

        let tracks = stmt
            .query_map([], |row| {
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
                    tags: Vec::new(),
                    added_at: DateTime::parse_from_rfc3339(
                        &row.get::<_, String>(13)?,
                    )
                    .map(|dt| dt.with_timezone(&Utc))
                    .unwrap_or_else(|_| Utc::now()),
                    last_played_at: row
                        .get::<_, Option<String>>(14)?
                        .and_then(|s| DateTime::parse_from_rfc3339(&s).ok())
                        .map(|dt| dt.with_timezone(&Utc)),
                    play_count: row.get(15)?,
                })
            })?
            .collect::<std::result::Result<Vec<_>, _>>()?;

        Ok(tracks)
    }

    /// Get a single track by ID.
    pub fn get_track(&self, id: &str) -> Result<Option<Track>> {
        let mut stmt = self.conn.prepare(
            "SELECT id, file_path, file_name, title, artist, album, genre, year,
                    track_number, duration_secs, file_size, sample_rate, channels,
                    added_at, last_played_at, play_count
             FROM tracks WHERE id = ?1",
        )?;

        let mut rows = stmt.query_map(params![id], |row| {
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
                tags: Vec::new(),
                added_at: DateTime::parse_from_rfc3339(
                    &row.get::<_, String>(13)?,
                )
                .map(|dt| dt.with_timezone(&Utc))
                .unwrap_or_else(|_| Utc::now()),
                last_played_at: row
                    .get::<_, Option<String>>(14)?
                    .and_then(|s| DateTime::parse_from_rfc3339(&s).ok())
                    .map(|dt| dt.with_timezone(&Utc)),
                play_count: row.get(15)?,
            })
        })?;

        Ok(rows.next().transpose()?)
    }

    /// Search tracks by query string (matches title, artist, album, filename).
    pub fn search(&self, query: &str) -> Result<Vec<Track>> {
        let like = format!("%{}%", query);
        let mut stmt = self.conn.prepare(
            "SELECT id, file_path, file_name, title, artist, album, genre, year,
                    track_number, duration_secs, file_size, sample_rate, channels,
                    added_at, last_played_at, play_count
             FROM tracks
             WHERE title LIKE ?1 OR artist LIKE ?1 OR album LIKE ?1
                    OR file_name LIKE ?1 OR genre LIKE ?1
             ORDER BY title",
        )?;

        let tracks = stmt
            .query_map(params![like], |row| {
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
                    tags: Vec::new(),
                    added_at: DateTime::parse_from_rfc3339(
                        &row.get::<_, String>(13)?,
                    )
                    .map(|dt| dt.with_timezone(&Utc))
                    .unwrap_or_else(|_| Utc::now()),
                    last_played_at: row
                        .get::<_, Option<String>>(14)?
                        .and_then(|s| DateTime::parse_from_rfc3339(&s).ok())
                        .map(|dt| dt.with_timezone(&Utc)),
                    play_count: row.get(15)?,
                })
            })?
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
}

/// Read metadata from an audio file using lofty.
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
