//! Playlist manager with SQLite persistence

use std::cell::RefCell;
use std::rc::Rc;

use chrono::{DateTime, Utc};
use rusqlite::{params, Connection};
use uuid::Uuid;

use crate::error::Result;
use crate::library::TrackId;

/// A playlist item (a track reference with ordering).
#[derive(Debug, Clone)]
pub struct PlaylistItem {
    pub track_id: TrackId,
    pub position: i32,
    pub is_jingle: bool,
    pub is_ad: bool,
}

/// A named playlist.
#[derive(Debug, Clone)]
pub struct Playlist {
    pub id: String,
    pub name: String,
    pub description: Option<String>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
    pub items: Vec<PlaylistItem>,
}

/// Manages playlists backed by SQLite.
pub struct PlaylistManager {
    conn: Rc<RefCell<Connection>>,
}

impl PlaylistManager {
    /// Create a new playlist manager from an existing connection.
    pub fn new(conn: Connection) -> Self {
        let mgr = Self {
            conn: Rc::new(RefCell::new(conn)),
        };
        mgr.init_tables();
        mgr
    }

    fn init_tables(&self) {
        self.conn
            .borrow()
            .execute_batch(
                "
                CREATE TABLE IF NOT EXISTS playlists (
                    id          TEXT PRIMARY KEY,
                    name        TEXT NOT NULL,
                    description TEXT,
                    created_at  TEXT NOT NULL,
                    updated_at  TEXT NOT NULL
                );

                CREATE TABLE IF NOT EXISTS playlist_items (
                    playlist_id TEXT NOT NULL,
                    track_id    TEXT NOT NULL,
                    position    INTEGER NOT NULL,
                    is_jingle   INTEGER NOT NULL DEFAULT 0,
                    is_ad       INTEGER NOT NULL DEFAULT 0,
                    PRIMARY KEY (playlist_id, position),
                    FOREIGN KEY (playlist_id) REFERENCES playlists(id) ON DELETE CASCADE,
                    FOREIGN KEY (track_id)    REFERENCES tracks(id) ON DELETE CASCADE
                );
                ",
            )
            .expect("Failed to initialize playlist tables");
    }

    /// Create a new empty playlist.
    pub fn create(&self, name: &str, description: Option<&str>) -> Result<Playlist> {
        let id = Uuid::new_v4().to_string();
        let now = Utc::now();

        self.conn.borrow().execute(
            "INSERT INTO playlists (id, name, description, created_at, updated_at)
             VALUES (?1, ?2, ?3, ?4, ?5)",
            params![id, name, description, now.to_rfc3339(), now.to_rfc3339()],
        )?;

        Ok(Playlist {
            id,
            name: name.to_string(),
            description: description.map(|s| s.to_string()),
            created_at: now,
            updated_at: now,
            items: Vec::new(),
        })
    }

    /// Get all playlists (without items — load items separately).
    pub fn list_all(&self) -> Result<Vec<Playlist>> {
        let conn = self.conn.borrow();
        let mut stmt = conn.prepare(
            "SELECT id, name, description, created_at, updated_at FROM playlists
             ORDER BY name",
        )?;

        let playlists = stmt
            .query_map([], |row| {
                Ok(Playlist {
                    id: row.get(0)?,
                    name: row.get(1)?,
                    description: row.get(2)?,
                    created_at: DateTime::parse_from_rfc3339(&row.get::<_, String>(3)?)
                        .map(|dt| dt.with_timezone(&Utc))
                        .unwrap_or_else(|_| Utc::now()),
                    updated_at: DateTime::parse_from_rfc3339(&row.get::<_, String>(4)?)
                        .map(|dt| dt.with_timezone(&Utc))
                        .unwrap_or_else(|_| Utc::now()),
                    items: Vec::new(),
                })
            })?
            .collect::<std::result::Result<Vec<_>, _>>()?;

        Ok(playlists)
    }

    /// Get a playlist with all its items loaded.
    pub fn get_with_items(&self, playlist_id: &str) -> Result<Option<Playlist>> {
        let conn = self.conn.borrow();

        let mut stmt = conn.prepare(
            "SELECT id, name, description, created_at, updated_at FROM playlists
             WHERE id = ?1",
        )?;

        let mut rows = stmt.query_map(params![playlist_id], |row| {
            Ok(Playlist {
                id: row.get(0)?,
                name: row.get(1)?,
                description: row.get(2)?,
                created_at: DateTime::parse_from_rfc3339(&row.get::<_, String>(3)?)
                    .map(|dt| dt.with_timezone(&Utc))
                    .unwrap_or_else(|_| Utc::now()),
                updated_at: DateTime::parse_from_rfc3339(&row.get::<_, String>(4)?)
                    .map(|dt| dt.with_timezone(&Utc))
                    .unwrap_or_else(|_| Utc::now()),
                items: Vec::new(),
            })
        })?;

        let mut playlist = match rows.next().transpose()? {
            Some(p) => p,
            None => return Ok(None),
        };

        // Load items
        let mut item_stmt = conn.prepare(
            "SELECT track_id, position, is_jingle, is_ad
             FROM playlist_items
             WHERE playlist_id = ?1
             ORDER BY position",
        )?;

        playlist.items = item_stmt
            .query_map(params![playlist_id], |row| {
                Ok(PlaylistItem {
                    track_id: row.get(0)?,
                    position: row.get(1)?,
                    is_jingle: row.get::<_, i32>(2)? != 0,
                    is_ad: row.get::<_, i32>(3)? != 0,
                })
            })?
            .collect::<std::result::Result<Vec<_>, _>>()?;

        Ok(Some(playlist))
    }

    /// Add a track to a playlist at the end.
    pub fn add_track(
        &self,
        playlist_id: &str,
        track_id: &str,
        is_jingle: bool,
        is_ad: bool,
    ) -> Result<()> {
        let conn = self.conn.borrow();

        // Find the next position
        let max_pos: i32 = conn
            .query_row(
                "SELECT COALESCE(MAX(position), -1) FROM playlist_items WHERE playlist_id = ?1",
                params![playlist_id],
                |row| row.get(0),
            )
            .unwrap_or(-1);

        let new_pos = max_pos + 1;

        conn.execute(
            "INSERT INTO playlist_items (playlist_id, track_id, position, is_jingle, is_ad)
             VALUES (?1, ?2, ?3, ?4, ?5)",
            params![
                playlist_id,
                track_id,
                new_pos,
                is_jingle as i32,
                is_ad as i32,
            ],
        )?;

        // Update the playlist's updated_at
        conn.execute(
            "UPDATE playlists SET updated_at = ?1 WHERE id = ?2",
            params![Utc::now().to_rfc3339(), playlist_id],
        )?;

        Ok(())
    }

    /// Remove a track from a playlist at the given position.
    pub fn remove_at(&self, playlist_id: &str, position: i32) -> Result<()> {
        self.conn.borrow().execute(
            "DELETE FROM playlist_items WHERE playlist_id = ?1 AND position = ?2",
            params![playlist_id, position],
        )?;
        Ok(())
    }

    /// Delete an entire playlist.
    pub fn delete(&self, playlist_id: &str) -> Result<()> {
        self.conn
            .borrow()
            .execute("DELETE FROM playlists WHERE id = ?1", params![playlist_id])?;
        Ok(())
    }
}
