//! Playlist manager with SQLite persistence

use std::cell::RefCell;
use std::rc::Rc;

use chrono::{DateTime, Utc};
use rusqlite::{params, Connection};
use uuid::Uuid;

use crate::error::{CrabError, Result};
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

/// Parse an RFC-3339 timestamp column, or fail with row context instead
/// of silently substituting "now" (a wrong timestamp rewrites playlist
/// ordering/display with no trace).
fn parse_stamp(id: &str, field: &'static str, raw: String) -> Result<DateTime<Utc>> {
    DateTime::parse_from_rfc3339(&raw)
        .map(|dt| dt.with_timezone(&Utc))
        .map_err(|_| CrabError::Integrity {
            table: "playlists",
            id: id.to_string(),
            field,
            value: raw,
        })
}

/// Manages playlists backed by SQLite.
pub struct PlaylistManager {
    conn: Rc<RefCell<Connection>>,
}

impl PlaylistManager {
    /// Create a new playlist manager from an existing connection.
    pub fn new(conn: Connection) -> Result<Self> {
        // Keep FK enforcement explicit per connection (see Library::open).
        conn.execute_batch("PRAGMA foreign_keys = ON;")?;
        let mgr = Self {
            conn: Rc::new(RefCell::new(conn)),
        };
        mgr.init_tables()?;
        Ok(mgr)
    }

    /// Open (or create) the playlist store at the given SQLite file.
    pub fn open(path: &std::path::Path) -> Result<Self> {
        Self::new(crate::db::Database::open_connection(path)?)
    }

    fn init_tables(&self) -> Result<()> {
        self.conn.borrow().execute_batch(
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
        )?;
        Ok(())
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

        let mut rows = stmt.query([])?;
        let mut playlists = Vec::new();
        while let Some(row) = rows.next()? {
            let id: String = row.get(0)?;
            let created_raw: String = row.get(3)?;
            let updated_raw: String = row.get(4)?;
            playlists.push(Playlist {
                id: id.clone(),
                name: row.get(1)?,
                description: row.get(2)?,
                created_at: parse_stamp(&id, "created_at", created_raw)?,
                updated_at: parse_stamp(&id, "updated_at", updated_raw)?,
                items: Vec::new(),
            });
        }

        Ok(playlists)
    }

    /// Get a playlist with all its items loaded.
    pub fn get_with_items(&self, playlist_id: &str) -> Result<Option<Playlist>> {
        let conn = self.conn.borrow();

        let mut stmt = conn.prepare(
            "SELECT id, name, description, created_at, updated_at FROM playlists
             WHERE id = ?1",
        )?;

        let mut rows = stmt.query(params![playlist_id])?;
        let mut playlist = match rows.next()? {
            Some(row) => {
                let id: String = row.get(0)?;
                let created_raw: String = row.get(3)?;
                let updated_raw: String = row.get(4)?;
                Playlist {
                    id: id.clone(),
                    name: row.get(1)?,
                    description: row.get(2)?,
                    created_at: parse_stamp(&id, "created_at", created_raw)?,
                    updated_at: parse_stamp(&id, "updated_at", updated_raw)?,
                    items: Vec::new(),
                }
            }
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

    /// Rename a playlist (non-blank name, `updated_at` bumped).
    pub fn rename(&self, playlist_id: &str, name: &str) -> Result<()> {
        let name = name.trim();
        if name.is_empty() {
            return Err(CrabError::Playlist("name is empty".into()));
        }
        let n = self.conn.borrow().execute(
            "UPDATE playlists SET name = ?1, updated_at = ?2 WHERE id = ?3",
            params![name, Utc::now().to_rfc3339(), playlist_id],
        )?;
        if n == 0 {
            return Err(CrabError::Playlist("playlist not found".into()));
        }
        Ok(())
    }

    /// Remove a track from a playlist at the given position, then
    /// renumber the survivors dense (`0..n`). The old implementation
    /// left a gap, so `position` stopped matching list index and a
    /// later `add_track` (`MAX+1`) drifted further. Runs in one
    /// transaction: delete + rewrite commit together.
    pub fn remove_at(&self, playlist_id: &str, position: i32) -> Result<()> {
        let mut conn = self.conn.borrow_mut();
        let tx = conn.transaction()?;
        let mut items: Vec<(String, i32, i32)> = tx
            .prepare(
                "SELECT track_id, is_jingle, is_ad FROM playlist_items
                 WHERE playlist_id = ?1 ORDER BY position",
            )?
            .query_map(params![playlist_id], |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, i32>(1)?,
                    row.get::<_, i32>(2)?,
                ))
            })?
            .collect::<std::result::Result<Vec<_>, _>>()?;
        // Map the requested DB position to a list index (legacy rows may
        // have gaps from the pre-renumber implementation).
        let ordered_positions: Vec<i32> = tx
            .prepare(
                "SELECT position FROM playlist_items
                 WHERE playlist_id = ?1 ORDER BY position",
            )?
            .query_map(params![playlist_id], |row| row.get(0))?
            .collect::<std::result::Result<Vec<_>, _>>()?;
        let Some(idx) = ordered_positions.iter().position(|p| *p == position) else {
            return Err(CrabError::Playlist("item not found".into()));
        };
        items.remove(idx);
        tx.execute(
            "DELETE FROM playlist_items WHERE playlist_id = ?1",
            params![playlist_id],
        )?;
        for (i, (track_id, jingle, ad)) in items.iter().enumerate() {
            tx.execute(
                "INSERT INTO playlist_items (playlist_id, track_id, position, is_jingle, is_ad)
                 VALUES (?1, ?2, ?3, ?4, ?5)",
                params![playlist_id, track_id, i as i32, jingle, ad],
            )?;
        }
        tx.execute(
            "UPDATE playlists SET updated_at = ?1 WHERE id = ?2",
            params![Utc::now().to_rfc3339(), playlist_id],
        )?;
        tx.commit()?;
        Ok(())
    }

    /// Move a playlist entry from one list index to another (both ends
    /// inclusive, `0..len`). Indices are positions in the stored order
    /// (`ORDER BY position`), not raw DB values, so legacy gaps can't
    /// misaddress the move. No-op when `from == to`. Rewrites positions
    /// dense `0..n` in one transaction.
    pub fn move_item(&self, playlist_id: &str, from: usize, to: usize) -> Result<()> {
        if from == to {
            return Ok(());
        }
        let mut conn = self.conn.borrow_mut();
        let tx = conn.transaction()?;
        let mut items: Vec<(String, i32, i32)> = tx
            .prepare(
                "SELECT track_id, is_jingle, is_ad FROM playlist_items
                 WHERE playlist_id = ?1 ORDER BY position",
            )?
            .query_map(params![playlist_id], |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, i32>(1)?,
                    row.get::<_, i32>(2)?,
                ))
            })?
            .collect::<std::result::Result<Vec<_>, _>>()?;
        if from >= items.len() || to >= items.len() {
            return Err(CrabError::Playlist("item index out of range".into()));
        }
        let row = items.remove(from);
        items.insert(to, row);
        tx.execute(
            "DELETE FROM playlist_items WHERE playlist_id = ?1",
            params![playlist_id],
        )?;
        for (i, (track_id, jingle, ad)) in items.iter().enumerate() {
            tx.execute(
                "INSERT INTO playlist_items (playlist_id, track_id, position, is_jingle, is_ad)
                 VALUES (?1, ?2, ?3, ?4, ?5)",
                params![playlist_id, track_id, i as i32, jingle, ad],
            )?;
        }
        tx.execute(
            "UPDATE playlists SET updated_at = ?1 WHERE id = ?2",
            params![Utc::now().to_rfc3339(), playlist_id],
        )?;
        tx.commit()?;
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

#[cfg(test)]
mod tests {
    use super::*;

    fn mem_manager() -> PlaylistManager {
        // In-memory DB has no tracks table; create a minimal one for FK-less inserts.
        let conn = Connection::open_in_memory().unwrap();
        conn.execute_batch(
            "CREATE TABLE tracks (id TEXT PRIMARY KEY, file_path TEXT NOT NULL UNIQUE);",
        )
        .unwrap();
        PlaylistManager::new(conn).unwrap()
    }

    #[test]
    fn create_and_list() {
        let m = mem_manager();
        m.create("Morning", Some("AM show")).unwrap();
        m.create("Night", None).unwrap();
        let all = m.list_all().unwrap();
        assert_eq!(all.len(), 2);
        assert_eq!(all[0].name, "Morning");
    }

    #[test]
    fn add_remove_and_delete() {
        let m = mem_manager();
        let p = m.create("Test", None).unwrap();
        m.add_track(&p.id, "track-1", false, false).unwrap_err();
        // Seed referenced tracks (FK), then exercise items.
        m.conn.borrow().execute_batch(
            "INSERT INTO tracks (id, file_path) VALUES ('track-1', '/a.mp3'), ('track-2', '/b.mp3');",
        ).unwrap();
        m.add_track(&p.id, "track-1", false, false).unwrap();
        m.add_track(&p.id, "track-2", true, false).unwrap();
        let full = m.get_with_items(&p.id).unwrap().unwrap();
        assert_eq!(full.items.len(), 2);
        assert!(full.items[1].is_jingle);
        assert_eq!(full.items[1].position, 1);
        m.remove_at(&p.id, 0).unwrap();
        assert_eq!(m.get_with_items(&p.id).unwrap().unwrap().items.len(), 1);
        m.delete(&p.id).unwrap();
        assert!(m.list_all().unwrap().is_empty());
    }

    /// Deleting a library track must cascade across manager connections
    /// sharing one file (playlist items, tags, play log) — never orphan.
    #[test]
    fn remove_track_cascades_across_managers() {
        let dir = std::env::temp_dir().join(format!("crabboss-cascade-{}", Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        let db = dir.join("crabboss.db");
        let lib = crate::library::Library::open(&db).unwrap();
        let pm = PlaylistManager::open(&db).unwrap();
        lib.conn()
            .execute(
                "INSERT INTO tracks (id, file_path, file_name, added_at)
                 VALUES ('t1', '/m/a.mp3', 'a.mp3', '2024-01-01T00:00:00Z')",
                [],
            )
            .unwrap();
        lib.conn()
            .execute(
                "INSERT INTO tags (track_id, tag) VALUES ('t1', 'morning')",
                [],
            )
            .unwrap();
        lib.record_play("t1", Some(200.0)).unwrap();
        let pl = pm.create("Mix", None).unwrap();
        pm.add_track(&pl.id, "t1", false, false).unwrap();
        let count = |table: &str| -> i64 {
            lib.conn()
                .query_row(
                    &format!("SELECT COUNT(*) FROM {table} WHERE track_id = 't1'"),
                    [],
                    |r| r.get(0),
                )
                .unwrap()
        };
        assert_eq!(count("playlist_items"), 1);
        assert_eq!(count("tags"), 1);
        assert_eq!(count("play_log"), 1);
        lib.remove_track("t1").unwrap();
        assert_eq!(count("playlist_items"), 0);
        assert_eq!(count("tags"), 0);
        assert_eq!(count("play_log"), 0);
        // The playlist itself survives, just emptied.
        assert!(pm.get_with_items(&pl.id).unwrap().unwrap().items.is_empty());
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn malformed_timestamps_are_integrity_error() {
        let pm = mem_manager();
        pm.conn
            .borrow()
            .execute(
                "INSERT INTO playlists (id, name, description, created_at, updated_at)
                 VALUES ('b1','Bad',NULL,'not-a-time','also-bad')",
                [],
            )
            .unwrap();
        let err = pm.list_all().expect_err("bad timestamp must fail");
        match err {
            crate::error::CrabError::Integrity {
                table,
                id,
                field,
                value,
            } => {
                assert_eq!(table, "playlists");
                assert_eq!(id, "b1");
                assert_eq!(field, "created_at");
                assert_eq!(value, "not-a-time");
            }
            other => panic!("expected Integrity error, got {other:?}"),
        }
    }

    fn seed_three(m: &PlaylistManager) -> Playlist {
        m.conn
            .borrow()
            .execute_batch(
                "INSERT OR IGNORE INTO tracks (id, file_path) VALUES
                 ('t1', '/a.mp3'), ('t2', '/b.mp3'), ('t3', '/c.mp3');",
            )
            .unwrap();
        let p = m.create("Manual", None).unwrap();
        m.add_track(&p.id, "t1", false, false).unwrap();
        m.add_track(&p.id, "t2", false, false).unwrap();
        m.add_track(&p.id, "t3", false, false).unwrap();
        p
    }

    #[test]
    fn remove_renumbers_dense() {
        let m = mem_manager();
        let p = seed_three(&m);
        m.remove_at(&p.id, 1).unwrap();
        let full = m.get_with_items(&p.id).unwrap().unwrap();
        assert_eq!(full.items.len(), 2);
        assert_eq!(full.items[0].track_id, "t1");
        assert_eq!(full.items[1].track_id, "t3");
        assert_eq!(full.items[0].position, 0);
        assert_eq!(full.items[1].position, 1);
        // Appending after a remove continues the dense sequence.
        m.conn
            .borrow()
            .execute_batch("INSERT OR IGNORE INTO tracks (id, file_path) VALUES ('t4', '/d.mp3');")
            .unwrap();
        m.add_track(&p.id, "t4", false, false).unwrap();
        let full = m.get_with_items(&p.id).unwrap().unwrap();
        assert_eq!(full.items[2].position, 2);
    }

    #[test]
    fn remove_missing_position_is_error() {
        let m = mem_manager();
        let p = seed_three(&m);
        m.remove_at(&p.id, 9)
            .expect_err("unknown position must fail");
        // Failed remove leaves the list untouched.
        assert_eq!(m.get_with_items(&p.id).unwrap().unwrap().items.len(), 3);
    }

    #[test]
    fn move_item_reorders_and_renumbers() {
        let m = mem_manager();
        let p = seed_three(&m);
        // t1,t2,t3 -> move first to last.
        m.move_item(&p.id, 0, 2).unwrap();
        let ids: Vec<_> = m
            .get_with_items(&p.id)
            .unwrap()
            .unwrap()
            .items
            .into_iter()
            .map(|i| i.track_id)
            .collect();
        assert_eq!(ids, vec!["t2", "t3", "t1"]);
        let positions: Vec<_> = m
            .get_with_items(&p.id)
            .unwrap()
            .unwrap()
            .items
            .into_iter()
            .map(|i| i.position)
            .collect();
        assert_eq!(positions, vec![0, 1, 2]);
        // Move last back to first.
        m.move_item(&p.id, 2, 0).unwrap();
        let ids: Vec<_> = m
            .get_with_items(&p.id)
            .unwrap()
            .unwrap()
            .items
            .into_iter()
            .map(|i| i.track_id)
            .collect();
        assert_eq!(ids, vec!["t1", "t2", "t3"]);
        // Out-of-range moves fail without touching the list.
        m.move_item(&p.id, 0, 3)
            .expect_err("to out of range must fail");
        m.move_item(&p.id, 5, 0)
            .expect_err("from out of range must fail");
        assert_eq!(m.get_with_items(&p.id).unwrap().unwrap().items.len(), 3);
    }

    #[test]
    fn rename_validates_and_persists() {
        let m = mem_manager();
        let p = m.create("Old", None).unwrap();
        m.rename(&p.id, "  ").expect_err("blank name must fail");
        m.rename(&p.id, "New Name").unwrap();
        let all = m.list_all().unwrap();
        assert_eq!(all[0].name, "New Name");
        m.rename("missing-id", "X")
            .expect_err("unknown playlist must fail");
    }
}
