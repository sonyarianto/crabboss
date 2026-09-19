//! SQLite-backed voice-track store: operator name + recorded WAV file.
//!
//! Voice tracks are deliberately NOT library tracks: they never enter
//! music reports/royalty play logs, Auto-DJ rotations, or loudness
//! scans. The manager owns its table (cart-pattern `init_tables`, no
//! central migration needed) and removes the audio file on delete.

use std::cell::RefCell;
use std::rc::Rc;

use chrono::{DateTime, Utc};
use rusqlite::{params, Connection};
use uuid::Uuid;

use crate::error::{CrabError, Result};

/// One recorded voice track.
#[derive(Debug, Clone)]
pub struct VoiceTrack {
    pub id: String,
    pub name: String,
    pub file_path: String,
    pub duration_secs: f64,
    pub sample_rate: u32,
    pub created_at: DateTime<Utc>,
}

/// Manages voice tracks backed by SQLite.
pub struct VoiceManager {
    conn: Rc<RefCell<Connection>>,
}

impl VoiceManager {
    pub fn new(conn: Connection) -> Result<Self> {
        conn.execute_batch("PRAGMA foreign_keys = ON;")?;
        let mgr = Self {
            conn: Rc::new(RefCell::new(conn)),
        };
        mgr.init_tables()?;
        Ok(mgr)
    }

    /// Open (or create) the voice store at the given SQLite file.
    pub fn open(path: &std::path::Path) -> Result<Self> {
        Self::new(crate::db::Database::open_connection(path)?)
    }

    fn init_tables(&self) -> Result<()> {
        self.conn.borrow().execute_batch(
            "
                CREATE TABLE IF NOT EXISTS voice_tracks (
                    id            TEXT PRIMARY KEY,
                    name          TEXT NOT NULL,
                    file_path     TEXT NOT NULL,
                    duration_secs REAL NOT NULL DEFAULT 0,
                    sample_rate   INTEGER NOT NULL DEFAULT 48000,
                    created_at    TEXT NOT NULL
                );
                ",
        )?;
        Ok(())
    }

    pub fn create(
        &self,
        name: &str,
        file_path: &str,
        duration_secs: f64,
        sample_rate: u32,
    ) -> Result<VoiceTrack> {
        let id = Uuid::new_v4().to_string();
        let now = Utc::now();
        self.conn.borrow().execute(
            "INSERT INTO voice_tracks (id, name, file_path, duration_secs, sample_rate, created_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
            params![
                id,
                name,
                file_path,
                duration_secs,
                sample_rate as i64,
                now.to_rfc3339()
            ],
        )?;
        Ok(VoiceTrack {
            id,
            name: name.to_string(),
            file_path: file_path.to_string(),
            duration_secs,
            sample_rate,
            created_at: now,
        })
    }

    /// Newest first (the take just recorded is what the operator wants).
    pub fn list_all(&self) -> Result<Vec<VoiceTrack>> {
        let conn = self.conn.borrow();
        let mut stmt = conn.prepare(
            "SELECT id, name, file_path, duration_secs, sample_rate, created_at
             FROM voice_tracks ORDER BY created_at DESC, rowid DESC",
        )?;
        let mut rows = stmt.query([])?;
        let mut out = Vec::new();
        while let Some(row) = rows.next()? {
            let id: String = row.get(0)?;
            let created_raw: String = row.get(5)?;
            let created_at = DateTime::parse_from_rfc3339(&created_raw)
                .map(|dt| dt.with_timezone(&Utc))
                .map_err(|_| CrabError::Integrity {
                    table: "voice_tracks",
                    id: id.clone(),
                    field: "created_at",
                    value: created_raw,
                })?;
            out.push(VoiceTrack {
                id,
                name: row.get(1)?,
                file_path: row.get(2)?,
                duration_secs: row.get(3)?,
                sample_rate: row.get(4).map(|v: i64| v as u32)?,
                created_at,
            });
        }
        Ok(out)
    }

    /// Lookup by audio path (the tick uses this to label a promoted
    /// voice deck — the one place voice meets the program display).
    pub fn find_by_path(&self, file_path: &str) -> Result<Option<VoiceTrack>> {
        let conn = self.conn.borrow();
        let mut stmt = conn.prepare(
            "SELECT id, name, file_path, duration_secs, sample_rate, created_at
             FROM voice_tracks WHERE file_path = ?1 LIMIT 1",
        )?;
        let mut rows = stmt.query([file_path])?;
        let Some(row) = rows.next()? else {
            return Ok(None);
        };
        let id: String = row.get(0)?;
        let created_raw: String = row.get(5)?;
        let created_at = DateTime::parse_from_rfc3339(&created_raw)
            .map(|dt| dt.with_timezone(&Utc))
            .map_err(|_| CrabError::Integrity {
                table: "voice_tracks",
                id: id.clone(),
                field: "created_at",
                value: created_raw,
            })?;
        Ok(Some(VoiceTrack {
            id,
            name: row.get(1)?,
            file_path: row.get(2)?,
            duration_secs: row.get(3)?,
            sample_rate: row.get(4).map(|v: i64| v as u32)?,
            created_at,
        }))
    }

    /// Delete the row and its audio file (a missing file is fine —
    /// the take may have been removed by hand).
    pub fn delete(&self, id: &str) -> Result<()> {
        let path: Option<String> = self
            .conn
            .borrow()
            .query_row(
                "SELECT file_path FROM voice_tracks WHERE id = ?1",
                [id],
                |r| r.get(0),
            )
            .ok();
        self.conn
            .borrow()
            .execute("DELETE FROM voice_tracks WHERE id = ?1", [id])?;
        if let Some(p) = path {
            let _ = std::fs::remove_file(p);
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn mem() -> VoiceManager {
        VoiceManager::new(Connection::open_in_memory().unwrap()).unwrap()
    }

    #[test]
    fn crud_roundtrips_newest_first() {
        let mgr = mem();
        let a = mgr.create("Voice A", "/v/a.wav", 3.5, 48_000).unwrap();
        let b = mgr.create("Voice B", "/v/b.wav", 7.25, 44_100).unwrap();
        assert_ne!(a.id, b.id);
        let list = mgr.list_all().unwrap();
        assert_eq!(list.len(), 2);
        // Newest first: B before A.
        assert_eq!(list[0].id, b.id);
        assert_eq!(list[1].id, a.id);
        assert_eq!(list[0].duration_secs, 7.25);
        assert_eq!(list[0].sample_rate, 44_100);
    }

    #[test]
    fn find_by_path_hits_and_misses() {
        let mgr = mem();
        let v = mgr.create("Take", "/v/take.wav", 2.0, 48_000).unwrap();
        let hit = mgr.find_by_path("/v/take.wav").unwrap().unwrap();
        assert_eq!(hit.id, v.id);
        assert_eq!(hit.name, "Take");
        assert!(mgr.find_by_path("/v/nope.wav").unwrap().is_none());
    }

    #[test]
    fn delete_removes_row_and_file() {
        let mgr = mem();
        let dir = std::env::temp_dir();
        let path = dir.join("crabboss-voice-test-del.wav");
        std::fs::write(&path, b"fake-wav").unwrap();
        let v = mgr
            .create("Gone", &path.to_string_lossy(), 1.0, 48_000)
            .unwrap();
        mgr.delete(&v.id).unwrap();
        assert!(mgr.list_all().unwrap().is_empty());
        assert!(!path.exists(), "audio file must go with the row");
        // Deleting again (or a row whose file is already gone) is fine.
        mgr.delete(&v.id).unwrap();
    }
}
