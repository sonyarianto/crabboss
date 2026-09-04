//! SQLite-backed cart store: label + audio file, fired instantly.

use std::cell::RefCell;
use std::rc::Rc;

use chrono::{DateTime, Utc};
use rusqlite::{params, Connection};
use uuid::Uuid;

use crate::error::Result;

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
}
