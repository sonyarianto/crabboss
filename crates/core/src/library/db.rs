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
    ///
    /// Matching is token-based (split on non-alphanumerics): short keys
    /// like `ad`/`spot` must appear as whole words, so `Downloads/`,
    /// `Spotify/`, `Spotlight`, or `Bedroom Mix` never misfire. A few
    /// long keys additionally match as substrings (`StationIDs/`
    /// still hits) — long enough to never collide with ordinary words.
    pub fn classify_path(path: &std::path::Path) -> Self {
        /// Whole-word keys (short keys MUST stay here — never substrings).
        const JINGLE_TOKENS: &[&str] = &[
            "jingle",
            "jingles",
            "bumper",
            "bumpers",
            "sweeper",
            "sweepers",
            "stationid",
            "toth",
            "stinger",
            "stingers",
            "beds",
            "liners",
            "drops",
            "id",
            "ids",
        ];
        const AD_TOKENS: &[&str] = &[
            "ad",
            "ads",
            "advert",
            "adverts",
            "commercial",
            "commercials",
            "promo",
            "promos",
            "spot",
            "spots",
            "iklan",
        ];
        const JINGLE_SUBS: &[&str] = &[
            "jingle",
            "bumper",
            "sweeper",
            "stationid",
            "stinger",
            "liners",
        ];
        const AD_SUBS: &[&str] = &["advert", "commercial"];
        fn hit(text: &str, tokens: &[&str], subs: &[&str]) -> bool {
            if text
                .split(|c: char| !c.is_alphanumeric())
                .any(|t| tokens.contains(&t))
            {
                return true;
            }
            subs.iter().any(|k| text.contains(k))
        }
        // Check directory components first, then the file stem.
        // Jingle wins ties (a station ID inside an ad folder is still an ID).
        let parent = path
            .parent()
            .map(|p| p.to_string_lossy().to_lowercase())
            .unwrap_or_default();
        if hit(&parent, JINGLE_TOKENS, JINGLE_SUBS) {
            return TrackKind::Jingle;
        }
        if hit(&parent, AD_TOKENS, AD_SUBS) {
            return TrackKind::Ad;
        }
        let stem = path
            .file_stem()
            .map(|s| s.to_string_lossy().to_lowercase())
            .unwrap_or_default();
        if hit(&stem, JINGLE_TOKENS, JINGLE_SUBS) {
            return TrackKind::Jingle;
        }
        if hit(&stem, AD_TOKENS, AD_SUBS) {
            return TrackKind::Ad;
        }
        TrackKind::Music
    }
}

/// Unique identifier for a track in the library.
pub type TrackId = String;

impl Track {
    /// Daypart eligibility for `(hour 0-23, weekday Mon..Sun)`.
    pub fn eligible_at(&self, hour: u8, weekday: &str) -> bool {
        if !self.daypart_days.eq_ignore_ascii_case("daily")
            && !self
                .daypart_days
                .split(',')
                .any(|d| d.trim().eq_ignore_ascii_case(weekday))
        {
            return false;
        }
        match (self.daypart_start, self.daypart_end) {
            (Some(s), Some(e)) => {
                if s <= e {
                    hour >= s && hour < e
                } else {
                    hour >= s || hour < e
                }
            }
            _ => true,
        }
    }
}

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
    /// Dayparting: eligible start hour (0-23) or `None` = any time.
    pub daypart_start: Option<u8>,
    /// Dayparting: eligible end hour (exclusive, may wrap past midnight) or `None`.
    pub daypart_end: Option<u8>,
    /// Dayparting: `Daily` or comma list like `Mon,Tue`.
    pub daypart_days: String,
    /// Measured integrated loudness in LUFS (`None` = not analyzed yet).
    pub loudness_lufs: Option<f32>,
    /// ReplayGain-style correction (dB) toward the R128 target.
    pub loudness_gain_db: Option<f32>,
    pub tags: Vec<String>,
    pub added_at: DateTime<Utc>,
    pub last_played_at: Option<DateTime<Utc>>,
    pub play_count: i32,
}

/// The music library backed by SQLite.
///
/// Write discipline (deliberate, do not "fix" with a pool): every write
/// below runs on the UI thread. Background workers (loudness scan, import
/// pump) only read files and compute; results come back over channels and
/// the UI thread performs all writes. So a single `Connection` with no
/// extra locking is correct — SQLite's own locks are never contended.
pub struct Library {
    conn: Connection,
}

impl Library {
    /// Open or create a library database at the given path.
    pub fn open(path: &Path) -> Result<Self> {
        let conn = Connection::open(path)?;
        // The schema declares ON DELETE CASCADE (playlist items, tags, play
        // log). The bundled SQLite enforces FKs by default, but state it
        // explicitly so `remove_track` can never silently orphan rows no
        // matter which SQLite build this links against.
        conn.execute_batch("PRAGMA foreign_keys = ON;")?;
        let lib = Self { conn };
        lib.init_tables()?;
        Ok(lib)
    }

    pub(crate) fn conn(&self) -> &Connection {
        &self.conn
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
        // Migrate older databases: add any missing columns.
        let cols: Vec<String> = self
            .conn
            .prepare("PRAGMA table_info(tracks)")?
            .query_map([], |row| row.get::<_, String>(1))?
            .collect::<std::result::Result<Vec<_>, _>>()?;
        let add_col = |name: &str, ddl: &str| -> Result<()> {
            if !cols.iter().any(|c| c == name) {
                self.conn
                    .execute(&format!("ALTER TABLE tracks ADD COLUMN {}", ddl), [])?;
            }
            Ok(())
        };
        add_col("kind", "kind TEXT NOT NULL DEFAULT 'music'")?;
        add_col("daypart_start", "daypart_start INTEGER")?;
        add_col("daypart_end", "daypart_end INTEGER")?;
        add_col("daypart_days", "daypart_days TEXT NOT NULL DEFAULT 'Daily'")?;
        add_col("loudness_lufs", "loudness_lufs REAL")?;
        add_col("loudness_gain_db", "loudness_gain_db REAL")?;
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
            daypart_start: None,
            daypart_end: None,
            daypart_days: "Daily".to_string(),
            loudness_lufs: None,
            loudness_gain_db: None,
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

    /// Re-run path classification over every row, repairing labels stored
    /// by older over-eager rules (e.g. `Downloads/` matching "ads").
    /// Returns the number of rows changed.
    pub fn reclassify_all(&self) -> Result<usize> {
        let tracks = self.get_all_tracks()?;
        let mut fixed = 0;
        for t in &tracks {
            let kind = TrackKind::classify_path(std::path::Path::new(&t.file_path));
            if kind != t.kind {
                self.set_kind(&t.id, kind)?;
                fixed += 1;
            }
        }
        Ok(fixed)
    }

    /// Store a measured loudness analysis for a track.
    pub fn set_loudness(&self, id: &str, lufs: f32, gain_db: f32) -> Result<()> {
        self.conn.execute(
            "UPDATE tracks SET loudness_lufs = ?1, loudness_gain_db = ?2 WHERE id = ?3",
            params![lufs as f64, gain_db as f64, id],
        )?;
        Ok(())
    }

    /// Rewrite stored gains for a new normalization target (dB toward
    /// `target` from each measured LUFS, clamped). The raw LUFS values are
    /// kept, so this never needs re-analysis: changing the Settings target
    /// (or migrating rows baked under an older default) is one cheap UPDATE.
    /// Missing-file sentinels (`−70 LUFS / 0 dB`) and unanalyzed rows
    /// (`NULL`) are left untouched. Returns rows rewritten.
    pub fn retarget_gains(&self, target: f32) -> Result<usize> {
        use crate::audio::{MAX_GAIN_DB, TARGET_MAX_LUFS, TARGET_MIN_LUFS};
        let target = target.clamp(TARGET_MIN_LUFS, TARGET_MAX_LUFS);
        let n = self.conn.execute(
            "UPDATE tracks
             SET loudness_gain_db = max(-?1, min(?1, ?2 - loudness_lufs))
             WHERE loudness_lufs IS NOT NULL AND loudness_lufs > -69.0",
            params![MAX_GAIN_DB as f64, target as f64],
        )?;
        Ok(n)
    }

    /// Tracks still awaiting loudness analysis (bounded scan queue).
    pub fn tracks_missing_loudness(&self, limit: usize) -> Result<Vec<Track>> {
        let mut stmt = self.conn.prepare(
            "SELECT id, file_path, file_name, title, artist, album, genre, year,
                    track_number, duration_secs, file_size, sample_rate, channels,
                    kind, added_at, last_played_at, play_count,
                    daypart_start, daypart_end, daypart_days,
                    loudness_lufs, loudness_gain_db
             FROM tracks
             WHERE loudness_lufs IS NULL
             ORDER BY file_name LIMIT ?1",
        )?;
        let tracks = stmt
            .query_map(params![limit as i64], Self::map_row)?
            .collect::<std::result::Result<Vec<_>, _>>()?;
        Ok(tracks)
    }

    /// Count of tracks still awaiting loudness analysis — cheap status
    /// probe for the library confidence line (no row loading).
    pub fn count_missing_loudness(&self) -> Result<usize> {
        let n: i64 = self.conn.query_row(
            "SELECT COUNT(*) FROM tracks WHERE loudness_lufs IS NULL",
            [],
            |row| row.get(0),
        )?;
        Ok(n.max(0) as usize)
    }

    /// Tracks with a stored loudness measurement (id + gain in dB),
    /// queried by exact file path — used to look up per-deck playback gain.
    pub fn loudness_gain_by_path(&self, path: &str) -> Result<Option<f32>> {
        let gain: Option<f64> = self
            .conn
            .query_row(
                "SELECT loudness_gain_db FROM tracks WHERE file_path = ?1",
                params![path],
                |row| row.get(0),
            )
            .ok()
            .flatten();
        Ok(gain.map(|v| v as f32))
    }

    /// Override daypart eligibility (`start`/`end` hours, `days` like `Daily`
    /// or `Mon,Tue`). `None` hours mean any time.
    pub fn set_daypart(
        &self,
        id: &str,
        start: Option<u8>,
        end: Option<u8>,
        days: &str,
    ) -> Result<()> {
        self.conn.execute(
            "UPDATE tracks SET daypart_start = ?1, daypart_end = ?2, daypart_days = ?3
             WHERE id = ?4",
            params![start.map(|v| v as i64), end.map(|v| v as i64), days, id],
        )?;
        Ok(())
    }

    /// All tracks of one kind (used by carts, generator jingle slots, filters).
    pub fn list_by_kind(&self, kind: TrackKind) -> Result<Vec<Track>> {
        let mut stmt = self.conn.prepare(
            "SELECT id, file_path, file_name, title, artist, album, genre, year,
                    track_number, duration_secs, file_size, sample_rate, channels,
                    kind, added_at, last_played_at, play_count,
                    daypart_start, daypart_end, daypart_days,
                    loudness_lufs, loudness_gain_db
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
            daypart_start: row
                .get::<_, Option<i64>>(17)
                .ok()
                .flatten()
                .map(|v| v as u8),
            daypart_end: row
                .get::<_, Option<i64>>(18)
                .ok()
                .flatten()
                .map(|v| v as u8),
            daypart_days: row
                .get::<_, String>(19)
                .unwrap_or_else(|_| "Daily".to_string()),
            loudness_lufs: row
                .get::<_, Option<f64>>(20)
                .ok()
                .flatten()
                .map(|v| v as f32),
            loudness_gain_db: row
                .get::<_, Option<f64>>(21)
                .ok()
                .flatten()
                .map(|v| v as f32),
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
                    kind, added_at, last_played_at, play_count,
                    daypart_start, daypart_end, daypart_days,
                    loudness_lufs, loudness_gain_db
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
                    kind, added_at, last_played_at, play_count,
                    daypart_start, daypart_end, daypart_days,
                    loudness_lufs, loudness_gain_db
             FROM tracks WHERE id = ?1",
        )?;

        let mut rows = stmt.query_map(params![id], Self::map_row)?;

        Ok(rows.next().transpose()?)
    }

    /// Find a track by its exact file path (used to log cart/scheduler plays).
    pub fn find_by_path(&self, path: &str) -> Result<Option<Track>> {
        let mut stmt = self.conn.prepare(
            "SELECT id, file_path, file_name, title, artist, album, genre, year,
                    track_number, duration_secs, file_size, sample_rate, channels,
                    kind, added_at, last_played_at, play_count,
                    daypart_start, daypart_end, daypart_days,
                    loudness_lufs, loudness_gain_db
             FROM tracks WHERE file_path = ?1",
        )?;
        let mut rows = stmt.query_map(params![path], Self::map_row)?;
        Ok(rows.next().transpose()?)
    }

    /// Search tracks by query string (matches title, artist, album, filename).
    pub fn search(&self, query: &str) -> Result<Vec<Track>> {
        let like = format!("%{}%", query);
        let mut stmt = self.conn.prepare(
            "SELECT id, file_path, file_name, title, artist, album, genre, year,
                    track_number, duration_secs, file_size, sample_rate, channels,
                    kind, added_at, last_played_at, play_count,
                    daypart_start, daypart_end, daypart_days,
                    loudness_lufs, loudness_gain_db
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

    /// Proactive health pass: tracks whose files no longer resolve on disk.
    /// Call at startup / on demand instead of failing silently at play time.
    pub fn missing_files(&self) -> Result<Vec<Track>> {
        Ok(self
            .get_all_tracks()?
            .into_iter()
            .filter(|t| !Path::new(&t.file_path).is_file())
            .collect())
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
        // Regression: ordinary words containing short keys must NOT hit.
        // ("ads" ⊂ "downloads" labeled whole folders as ads.)
        assert_eq!(j("C:/Users/DJ/Downloads/track01.mp3"), TrackKind::Music);
        assert_eq!(j("C:/Users/DJ/Music/Spotify/song.mp3"), TrackKind::Music);
        assert_eq!(j("/music/Bedroom Mix/chill.mp3"), TrackKind::Music);
        assert_eq!(j("/music/Raindrops Best/song.mp3"), TrackKind::Music);
        assert_eq!(j("/music/Spotlight - hits.mp3"), TrackKind::Music);
        assert_eq!(j("/music/lead vocal take.mp3"), TrackKind::Music);
        assert_eq!(j("/music/promenade.mp3"), TrackKind::Music);
        // Genuine hits still classify (tokens, plurals, substrings, ID).
        assert_eq!(j("/music/My Ads/coke.mp3"), TrackKind::Ad);
        assert_eq!(j("/music/StationIDs/toth.mp3"), TrackKind::Jingle);
        assert_eq!(j("/music/Iklan/sirup.mp3"), TrackKind::Ad);
        assert_eq!(j("/music/coke_spot.mp3"), TrackKind::Ad);
        assert_eq!(j("/music/promo_mix.mp3"), TrackKind::Ad);
    }

    #[test]
    fn reclassify_repairs_over_eager_labels() {
        let dir = std::env::temp_dir().join(format!("crabboss-reclass-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        let lib = Library::open(&dir.join("lib.db")).unwrap();
        // Rows as the old substring rules stored them: a Downloads track
        // wrongly labeled Ad, plus one genuinely correct Ad row.
        for (id, path, kind) in [
            ("1", "C:/Users/DJ/Downloads/track01.mp3", "ad"),
            ("2", "/music/Ads/coke.mp3", "ad"),
            ("3", "/music/Rock/song.mp3", "music"),
        ] {
            lib.conn
                .execute(
                    "INSERT INTO tracks (id, file_path, file_name, added_at, play_count, kind)
                     VALUES (?1, ?2, ?3, ?4, 0, ?5)",
                    rusqlite::params![
                        id,
                        path,
                        path.rsplit('/').next().unwrap_or(path),
                        chrono::Utc::now().to_rfc3339(),
                        kind
                    ],
                )
                .unwrap();
        }
        assert_eq!(lib.reclassify_all().unwrap(), 1);
        let kinds: std::collections::HashMap<_, _> = lib
            .get_all_tracks()
            .unwrap()
            .into_iter()
            .map(|t| (t.id, t.kind))
            .collect();
        assert_eq!(kinds["1"], TrackKind::Music);
        assert_eq!(kinds["2"], TrackKind::Ad);
        assert_eq!(kinds["3"], TrackKind::Music);
        // Second run is a no-op.
        assert_eq!(lib.reclassify_all().unwrap(), 0);
        std::fs::remove_dir_all(&dir).ok();
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
    fn loudness_store_and_lookup() {
        let dir = std::env::temp_dir().join(format!("crabboss-loud-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        let lib = Library::open(&dir.join("lib.db")).unwrap();
        lib.conn
            .execute(
                "INSERT INTO tracks (id, file_path, file_name, added_at, play_count, kind)
                 VALUES ('1', '/m/a.mp3', 'a.mp3', ?1, 0, 'music')",
                rusqlite::params![chrono::Utc::now().to_rfc3339()],
            )
            .unwrap();
        // Unanalyzed: shows in the missing queue, no gain by path.
        assert_eq!(lib.tracks_missing_loudness(10).unwrap().len(), 1);
        assert_eq!(lib.loudness_gain_by_path("/m/a.mp3").unwrap(), None);
        // Store → gone from queue, gain + fields roundtrip.
        lib.set_loudness("1", -18.5, -4.5).unwrap();
        assert!(lib.tracks_missing_loudness(10).unwrap().is_empty());
        assert!((lib.loudness_gain_by_path("/m/a.mp3").unwrap().unwrap() + 4.5).abs() < 1e-6);
        let t = &lib.get_all_tracks().unwrap()[0];
        assert!((t.loudness_lufs.unwrap() + 18.5).abs() < 1e-6);
        assert!((t.loudness_gain_db.unwrap() + 4.5).abs() < 1e-6);
        // Unknown path → None, not an error.
        assert_eq!(lib.loudness_gain_by_path("/m/other.mp3").unwrap(), None);
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn retarget_rewrites_gains_without_rescan() {
        let dir = std::env::temp_dir().join(format!("crabboss-retarget-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        let lib = Library::open(&dir.join("lib.db")).unwrap();
        // Analyzed under the old −23 default, a missing-file sentinel,
        // and an unanalyzed row.
        for (id, lufs, gain) in [
            ("old", Some(-10.0), Some(-13.0)),
            ("sentinel", Some(-70.0), Some(0.0)),
            ("fresh", None, None),
        ] {
            lib.conn
                .execute(
                    "INSERT INTO tracks (id, file_path, file_name, added_at, play_count, kind,
                                         loudness_lufs, loudness_gain_db)
                     VALUES (?1, ?2, ?3, ?4, 0, 'music', ?5, ?6)",
                    rusqlite::params![
                        id,
                        format!("/m/{id}.mp3"),
                        format!("{id}.mp3"),
                        chrono::Utc::now().to_rfc3339(),
                        lufs,
                        gain,
                    ],
                )
                .unwrap();
        }
        // Retarget to the RadioBOSS-style −9 default: only the genuinely
        // analyzed row moves (−10 → +1 dB), no re-analysis needed.
        assert_eq!(lib.retarget_gains(-9.0).unwrap(), 1);
        let gains: std::collections::HashMap<_, _> = lib
            .get_all_tracks()
            .unwrap()
            .into_iter()
            .map(|t| (t.id, (t.loudness_lufs, t.loudness_gain_db)))
            .collect();
        assert!((gains["old"].1.unwrap() - 1.0).abs() < 1e-6);
        assert!((gains["old"].0.unwrap() + 10.0).abs() < 1e-6);
        assert_eq!(gains["sentinel"].1.unwrap(), 0.0);
        assert_eq!(gains["fresh"].1, None);
        // Out-of-range targets clamp instead of exploding the library.
        assert_eq!(lib.retarget_gains(-99.0).unwrap(), 1);
        let t = lib.find_by_path("/m/old.mp3").unwrap().unwrap();
        assert!((t.loudness_gain_db.unwrap() + 13.0).abs() < 1e-6);
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn count_missing_loudness_tracks_queue() {
        let dir = std::env::temp_dir().join(format!("crabboss-loudcount-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        let lib = Library::open(&dir.join("lib.db")).unwrap();
        assert_eq!(lib.count_missing_loudness().unwrap(), 0);
        for i in 0..3 {
            lib.conn
                .execute(
                    "INSERT INTO tracks (id, file_path, file_name, added_at, play_count, kind)
                     VALUES (?1, ?2, ?3, ?4, 0, 'music')",
                    rusqlite::params![
                        i.to_string(),
                        format!("/m/{i}.mp3"),
                        format!("{i}.mp3"),
                        chrono::Utc::now().to_rfc3339()
                    ],
                )
                .unwrap();
        }
        assert_eq!(lib.count_missing_loudness().unwrap(), 3);
        lib.set_loudness("1", -18.5, -4.5).unwrap();
        assert_eq!(lib.count_missing_loudness().unwrap(), 2);
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
