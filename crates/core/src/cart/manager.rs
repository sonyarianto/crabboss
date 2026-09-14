//! SQLite-backed cart store: label + audio file, fired instantly.

use std::cell::RefCell;
use std::rc::Rc;

use chrono::{DateTime, Utc};
use rusqlite::{params, Connection};
use uuid::Uuid;

use crate::error::Result;

/// Cart wall size (RadioBOSS-style fixed pad grid).
pub const WALL_SIZE: usize = 8;

/// One cart pad.
#[derive(Debug, Clone)]
pub struct Cart {
    pub id: String,
    pub label: String,
    pub file_path: String,
    pub position: i32,
    pub created_at: DateTime<Utc>,
}

/// Manages carts backed by SQLite.
pub struct CartManager {
    conn: Rc<RefCell<Connection>>,
}

impl CartManager {
    pub fn new(conn: Connection) -> Self {
        // Same FK pragma as the other managers (harmless here — this
        // table has no FKs today — but keeps every connection uniform).
        conn.execute_batch("PRAGMA foreign_keys = ON;")
            .expect("Failed to enable foreign keys");
        let mgr = Self {
            conn: Rc::new(RefCell::new(conn)),
        };
        mgr.init_tables();
        mgr
    }

    /// Open (or create) the cart store at the given SQLite file.
    pub fn open(path: &std::path::Path) -> Result<Self> {
        Ok(Self::new(Connection::open(path)?))
    }

    fn init_tables(&self) {
        self.conn
            .borrow()
            .execute_batch(
                "
                CREATE TABLE IF NOT EXISTS carts (
                    id          TEXT PRIMARY KEY,
                    label       TEXT NOT NULL,
                    file_path   TEXT NOT NULL DEFAULT '',
                    position    INTEGER NOT NULL DEFAULT 0,
                    created_at  TEXT NOT NULL
                );
                ",
            )
            .expect("Failed to initialize cart tables");
    }

    pub fn create(&self, label: &str, file_path: &str) -> Result<Cart> {
        let id = Uuid::new_v4().to_string();
        let now = Utc::now();
        let pos: i32 = self
            .conn
            .borrow()
            .query_row(
                "SELECT COALESCE(MAX(position), -1) + 1 FROM carts",
                [],
                |r| r.get(0),
            )
            .unwrap_or(0);
        self.conn.borrow().execute(
            "INSERT INTO carts (id, label, file_path, position, created_at)
             VALUES (?1, ?2, ?3, ?4, ?5)",
            params![id, label, file_path, pos, now.to_rfc3339()],
        )?;
        Ok(Cart {
            id,
            label: label.to_string(),
            file_path: file_path.to_string(),
            position: pos,
            created_at: now,
        })
    }

    pub fn list_all(&self) -> Result<Vec<Cart>> {
        let conn = self.conn.borrow();
        let mut stmt = conn.prepare(
            "SELECT id, label, file_path, position, created_at
             FROM carts ORDER BY position",
        )?;
        let carts = stmt
            .query_map([], |row| {
                Ok(Cart {
                    id: row.get(0)?,
                    label: row.get(1)?,
                    file_path: row.get(2)?,
                    position: row.get(3)?,
                    created_at: row
                        .get::<_, String>(4)
                        .ok()
                        .and_then(|s| DateTime::parse_from_rfc3339(&s).ok())
                        .map(|dt| dt.with_timezone(&Utc))
                        .unwrap_or_else(Utc::now),
                })
            })?
            .collect::<std::result::Result<Vec<_>, _>>()?;
        Ok(carts)
    }

    pub fn delete(&self, id: &str) -> Result<()> {
        self.conn
            .borrow()
            .execute("DELETE FROM carts WHERE id = ?1", params![id])?;
        Ok(())
    }

    /// Assign a track to a pad slot (0-based). Replaces whatever was there
    /// (carts are pads, not an ordered list). No-op when `position` is
    /// beyond the configured wall size.
    pub fn assign_at(&self, position: i32, label: &str, file_path: &str) -> Result<()> {
        if !(0..WALL_SIZE as i32).contains(&position) {
            return Ok(());
        }
        let conn = self.conn.borrow();
        if let Ok(existing) = conn.query_row(
            "SELECT id FROM carts WHERE position = ?1",
            params![position],
            |r| r.get::<_, String>(0),
        ) {
            conn.execute("DELETE FROM carts WHERE id = ?1", params![existing])?;
        }
        let id = Uuid::new_v4().to_string();
        conn.execute(
            "INSERT INTO carts (id, label, file_path, position, created_at)
             VALUES (?1, ?2, ?3, ?4, ?5)",
            params![id, label, file_path, position, Utc::now().to_rfc3339()],
        )?;
        Ok(())
    }

    /// Overwrite label/path of the pad at `position` (no-op when empty).
    pub fn update(&self, position: i32, label: &str, file_path: &str) -> Result<()> {
        self.conn.borrow().execute(
            "UPDATE carts SET label = ?1, file_path = ?2 WHERE position = ?3",
            params![label, file_path, position],
        )?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn create_list_delete() {
        let m = CartManager::new(Connection::open_in_memory().unwrap());
        m.create("Jingle 1", "/tmp/a.mp3").unwrap();
        m.create("Jingle 2", "/tmp/b.mp3").unwrap();
        let all = m.list_all().unwrap();
        assert_eq!(all.len(), 2);
        assert_eq!(all[0].label, "Jingle 1");
        m.delete(&all[0].id).unwrap();
        assert_eq!(m.list_all().unwrap().len(), 1);
    }

    #[test]
    fn assign_at_replaces_and_respects_bounds() {
        let m = CartManager::new(Connection::open_in_memory().unwrap());
        m.create("Old", "/tmp/old.mp3").unwrap(); // takes position 0
        m.assign_at(0, "New", "/tmp/new.mp3").unwrap();
        let all = m.list_all().unwrap();
        assert_eq!(all.len(), 1, "replace, not append");
        assert_eq!(all[0].position, 0);
        assert_eq!(all[0].label, "New");
        // Out-of-range slots are ignored (fixed 8-pad wall).
        m.assign_at(8, "Ghost", "/tmp/x.mp3").unwrap();
        m.assign_at(-1, "Ghost", "/tmp/x.mp3").unwrap();
        assert_eq!(m.list_all().unwrap().len(), 1);
        // Any free slot can be targeted directly.
        m.assign_at(5, "Slot5", "/tmp/s5.mp3").unwrap();
        let all = m.list_all().unwrap();
        assert_eq!(all.len(), 2);
        assert!(all.iter().any(|c| c.label == "Slot5" && c.position == 5));
        // update() overwrites in place; empty slot is a no-op.
        m.update(5, "Slot5b", "/tmp/s5b.mp3").unwrap();
        m.update(7, "Noop", "/tmp/nope.mp3").unwrap();
        let all = m.list_all().unwrap();
        let s5 = all.iter().find(|c| c.position == 5).unwrap();
        assert_eq!(
            (s5.label.as_str(), s5.file_path.as_str()),
            ("Slot5b", "/tmp/s5b.mp3")
        );
        assert!(!all.iter().any(|c| c.position == 7));
    }
}
