//! SQLite-backed cart store: label + audio file, fired instantly.

use std::cell::RefCell;
use std::rc::Rc;

use chrono::{DateTime, Utc};
use rusqlite::{params, Connection};
use uuid::Uuid;

use crate::error::{CrabError, Result};

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
    pub fn new(conn: Connection) -> Result<Self> {
        // Same FK pragma as the other managers (harmless here — this
        // table has no FKs today — but keeps every connection uniform).
        conn.execute_batch("PRAGMA foreign_keys = ON;")?;
        let mgr = Self {
            conn: Rc::new(RefCell::new(conn)),
        };
        mgr.init_tables()?;
        Ok(mgr)
    }

    /// Open (or create) the cart store at the given SQLite file.
    pub fn open(path: &std::path::Path) -> Result<Self> {
        Self::new(crate::db::Database::open_connection(path)?)
    }

    fn init_tables(&self) -> Result<()> {
        self.conn.borrow().execute_batch(
            "
                CREATE TABLE IF NOT EXISTS carts (
                    id          TEXT PRIMARY KEY,
                    label       TEXT NOT NULL,
                    file_path   TEXT NOT NULL DEFAULT '',
                    position    INTEGER NOT NULL DEFAULT 0,
                    created_at  TEXT NOT NULL,
                    UNIQUE(position)
                );
                ",
        )?;
        Ok(())
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
        let mut rows = stmt.query([])?;
        let mut carts = Vec::new();
        while let Some(row) = rows.next()? {
            let id: String = row.get(0)?;
            let created_raw: String = row.get(4)?;
            let created_at = DateTime::parse_from_rfc3339(&created_raw)
                .map(|dt| dt.with_timezone(&Utc))
                .map_err(|_| CrabError::Integrity {
                    table: "carts",
                    id: id.clone(),
                    field: "created_at",
                    value: created_raw,
                })?;
            carts.push(Cart {
                id,
                label: row.get(1)?,
                file_path: row.get(2)?,
                position: row.get(3)?,
                created_at,
            });
        }
        Ok(carts)
    }

    pub fn delete(&self, id: &str) -> Result<()> {
        self.conn
            .borrow()
            .execute("DELETE FROM carts WHERE id = ?1", params![id])?;
        Ok(())
    }

    /// Assign a track to a pad slot (0-based), replacing whatever was
    /// there (carts are pads, not an ordered list). Runs in one
    /// transaction: delete + insert commit together, so a failure can
    /// never leave the pad empty. Out-of-range positions are a loud
    /// error — every UI caller addresses real pads.
    pub fn assign_at(&self, position: i32, label: &str, file_path: &str) -> Result<()> {
        if !(0..WALL_SIZE as i32).contains(&position) {
            return Err(CrabError::InvalidCartPosition(position));
        }
        let mut conn = self.conn.borrow_mut();
        let tx = conn.transaction()?;
        tx.execute("DELETE FROM carts WHERE position = ?1", params![position])?;
        let id = Uuid::new_v4().to_string();
        tx.execute(
            "INSERT INTO carts (id, label, file_path, position, created_at)
             VALUES (?1, ?2, ?3, ?4, ?5)",
            params![id, label, file_path, position, Utc::now().to_rfc3339()],
        )?;
        tx.commit()?;
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
        let m = CartManager::new(Connection::open_in_memory().unwrap()).unwrap();
        m.create("Jingle 1", "/tmp/a.mp3").unwrap();
        m.create("Jingle 2", "/tmp/b.mp3").unwrap();
        let all = m.list_all().unwrap();
        assert_eq!(all.len(), 2);
        assert_eq!(all[0].label, "Jingle 1");
        m.delete(&all[0].id).unwrap();
        assert_eq!(m.list_all().unwrap().len(), 1);
    }

    #[test]
    fn duplicate_position_violates_unique_constraint() {
        // The DB itself is the backstop: even hand-written SQL cannot
        // create two pads on one position.
        let m = CartManager::new(Connection::open_in_memory().unwrap()).unwrap();
        m.assign_at(2, "A", "/tmp/a.mp3").unwrap();
        let dup = m.conn.borrow().execute(
            "INSERT INTO carts (id, label, file_path, position, created_at)
             VALUES ('x', 'B', '/tmp/b.mp3', 2, '2026-01-01T00:00:00Z')",
            [],
        );
        assert!(dup.is_err(), "UNIQUE(position) must hold");
        assert_eq!(m.list_all().unwrap().len(), 1);
    }

    #[test]
    fn assign_at_replaces_and_respects_bounds() {
        let m = CartManager::new(Connection::open_in_memory().unwrap()).unwrap();
        m.create("Old", "/tmp/old.mp3").unwrap(); // takes position 0
        m.assign_at(0, "New", "/tmp/new.mp3").unwrap();
        let all = m.list_all().unwrap();
        assert_eq!(all.len(), 1, "replace, not append");
        assert_eq!(all[0].position, 0);
        assert_eq!(all[0].label, "New");
        // Out-of-range slots are a loud error (fixed 8-pad wall); the old
        // cart stays exactly where it was.
        assert!(m.assign_at(8, "Ghost", "/tmp/x.mp3").is_err());
        assert!(m.assign_at(-1, "Ghost", "/tmp/x.mp3").is_err());
        let all = m.list_all().unwrap();
        assert_eq!(all.len(), 1);
        assert_eq!(all[0].label, "New");
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

    #[test]
    fn malformed_created_at_is_integrity_error() {
        let m = CartManager::new(Connection::open_in_memory().unwrap()).unwrap();
        m.conn
            .borrow()
            .execute(
                "INSERT INTO carts (id, label, file_path, position, created_at)
                 VALUES ('b1','Bad','/tmp/b.mp3',0,'not-a-time')",
                [],
            )
            .unwrap();
        let err = m.list_all().expect_err("bad timestamp must fail");
        match err {
            crate::error::CrabError::Integrity {
                table,
                id,
                field,
                value,
            } => {
                assert_eq!(table, "carts");
                assert_eq!(id, "b1");
                assert_eq!(field, "created_at");
                assert_eq!(value, "not-a-time");
            }
            other => panic!("expected Integrity error, got {other:?}"),
        }
    }
}
