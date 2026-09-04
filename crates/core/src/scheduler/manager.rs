//! SQLite-backed scheduler event store.

use std::cell::RefCell;
use std::rc::Rc;

use chrono::{DateTime, Utc};
use rusqlite::{params, Connection};
use uuid::Uuid;

use crate::error::Result;

/// A scheduled automation event (mirrors RadioBOSS Scheduler tab row).
#[derive(Debug, Clone)]
pub struct ScheduledEvent {
    pub id: String,
    pub name: String,
    /// `play` | `load` | `generate` | `command`
    pub action_type: String,
    /// Playlist name / file path / preset name / raw command.
    pub target: String,
    /// Daily start time as `HH:MM` (24h).
    pub start_time: String,
    /// Repeat days: `Daily` or comma list like `Mon,Tue,Wed`.
    pub days: String,
    pub enabled: bool,
    pub created_at: DateTime<Utc>,
}

impl ScheduledEvent {
    /// Does this event fire at the given `HH:MM` + 3-letter weekday (`Mon`..`Sun`)?
    pub fn is_due(&self, now_hhmm: &str, weekday: &str) -> bool {
        if !self.enabled || self.start_time != now_hhmm {
            return false;
        }
        if self.days.eq_ignore_ascii_case("daily") {
            return true;
        }
        self.days.split(',').any(|d| d.trim().eq_ignore_ascii_case(weekday))
    }
}

/// Validate `HH:MM` 24h format.
pub fn validate_hhmm(v: &str) -> bool {
    let b = v.as_bytes();
    if b.len() != 5 || b[2] != b':' {
        return false;
    }
    let hh: Option<u32> = v[0..2].parse().ok();
    let mm: Option<u32> = v[3..5].parse().ok();
    matches!((hh, mm), (Some(h), Some(m)) if h < 24 && m < 60)
}

/// Day bitmask: Mon=1, Tue=2, Wed=4, Thu=8, Fri=16, Sat=32, Sun=64.
/// 0 or 127 means `Daily`.
pub fn days_from_mask(mask: u8) -> String {
    if mask == 0 || mask == 127 {
        return "Daily".to_string();
    }
    let names = ["Mon", "Tue", "Wed", "Thu", "Fri", "Sat", "Sun"];
    let mut out = Vec::new();
    for (i, n) in names.iter().enumerate() {
        if mask & (1 << i) != 0 {
            out.push(*n);
        }
    }
    out.join(",")
}

/// Bitmask for a stored days string (`Daily` -> 127).
pub fn mask_from_days(days: &str) -> u8 {
    if days.eq_ignore_ascii_case("daily") {
        return 127;
    }
    let mut mask = 0u8;
    for d in days.split(',') {
        match d.trim().to_lowercase().as_str() {
            "mon" => mask |= 1,
            "tue" => mask |= 2,
            "wed" => mask |= 4,
            "thu" => mask |= 8,
            "fri" => mask |= 16,
            "sat" => mask |= 32,
            "sun" => mask |= 64,
            _ => {}
        }
    }
    if mask == 0 {
        127
    } else {
        mask
    }
}

/// Manages scheduled events backed by SQLite.
pub struct SchedulerManager {
    conn: Rc<RefCell<Connection>>,
}

impl SchedulerManager {
    pub fn new(conn: Connection) -> Self {
        let mgr = Self {
            conn: Rc::new(RefCell::new(conn)),
        };
        mgr.init_tables();
        mgr
    }

    /// Open (or create) the scheduler store at the given SQLite file.
    /// Shares the same `crabboss.db` file as the library — separate connection.
    pub fn open(path: &std::path::Path) -> Result<Self> {
        Ok(Self::new(Connection::open(path)?))
    }

    fn init_tables(&self) {
        self.conn
            .borrow()
            .execute_batch(
                "
                CREATE TABLE IF NOT EXISTS scheduled_events (
                    id          TEXT PRIMARY KEY,
                    name        TEXT NOT NULL,
                    action_type TEXT NOT NULL,
                    target      TEXT NOT NULL DEFAULT '',
                    start_time  TEXT NOT NULL,
                    days        TEXT NOT NULL DEFAULT 'Daily',
                    enabled     INTEGER NOT NULL DEFAULT 1,
                    created_at  TEXT NOT NULL
                );
                ",
            )
            .expect("Failed to initialize scheduler tables");
    }

    /// Create a new event. `start_time` must be `HH:MM`.
    pub fn create(
        &self,
        name: &str,
        action_type: &str,
        target: &str,
        start_time: &str,
        days: &str,
    ) -> Result<ScheduledEvent> {
        if name.trim().is_empty() {
            return Err(crate::error::CrabError::Scheduler("name is empty".into()));
        }
        if !validate_hhmm(start_time) {
            return Err(crate::error::CrabError::Scheduler(format!(
                "bad time '{}', want HH:MM",
                start_time
            )));
        }
        let id = Uuid::new_v4().to_string();
        let now = Utc::now();
        self.conn.borrow().execute(
            "INSERT INTO scheduled_events
             (id, name, action_type, target, start_time, days, enabled, created_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, 1, ?7)",
            params![id, name, action_type, target, start_time, days, now.to_rfc3339()],
        )?;
        Ok(ScheduledEvent {
            id,
            name: name.to_string(),
            action_type: action_type.to_string(),
            target: target.to_string(),
            start_time: start_time.to_string(),
            days: days.to_string(),
            enabled: true,
            created_at: now,
        })
    }

    pub fn list_all(&self) -> Result<Vec<ScheduledEvent>> {
        let conn = self.conn.borrow();
        let mut stmt = conn.prepare(
            "SELECT id, name, action_type, target, start_time, days, enabled, created_at
             FROM scheduled_events ORDER BY start_time, name",
        )?;
        let events = stmt
            .query_map([], |row| {
                Ok(ScheduledEvent {
                    id: row.get(0)?,
                    name: row.get(1)?,
                    action_type: row.get(2)?,
                    target: row.get(3)?,
                    start_time: row.get(4)?,
                    days: row.get(5)?,
                    enabled: row.get::<_, i32>(6)? != 0,
                    created_at: row
                        .get::<_, String>(7)
                        .ok()
                        .and_then(|s| DateTime::parse_from_rfc3339(&s).ok())
                        .map(|dt| dt.with_timezone(&Utc))
                        .unwrap_or_else(Utc::now),
                })
            })?
            .collect::<std::result::Result<Vec<_>, _>>()?;
        Ok(events)
    }

    pub fn set_enabled(&self, id: &str, enabled: bool) -> Result<()> {
        self.conn.borrow().execute(
            "UPDATE scheduled_events SET enabled = ?1 WHERE id = ?2",
            params![enabled as i32, id],
        )?;
        Ok(())
    }

    /// Full update from the Add/Edit dialog.
    pub fn update(
        &self,
        id: &str,
        name: &str,
        action_type: &str,
        target: &str,
        start_time: &str,
        days: &str,
    ) -> Result<()> {
        if name.trim().is_empty() {
            return Err(crate::error::CrabError::Scheduler("name is empty".into()));
        }
        if !validate_hhmm(start_time) {
            return Err(crate::error::CrabError::Scheduler(format!(
                "bad time '{}', want HH:MM",
                start_time
            )));
        }
        self.conn.borrow().execute(
            "UPDATE scheduled_events
             SET name = ?1, action_type = ?2, target = ?3, start_time = ?4, days = ?5
             WHERE id = ?6",
            params![name, action_type, target, start_time, days, id],
        )?;
        Ok(())
    }

    pub fn delete(&self, id: &str) -> Result<()> {
        self.conn
            .borrow()
            .execute("DELETE FROM scheduled_events WHERE id = ?1", params![id])?;
        Ok(())
    }

    /// Events that should fire now (`now_hhmm` = `HH:MM`, `weekday` = `Mon`..).
    pub fn due_events(&self, now_hhmm: &str, weekday: &str) -> Result<Vec<ScheduledEvent>> {
        Ok(self
            .list_all()?
            .into_iter()
            .filter(|e| e.is_due(now_hhmm, weekday))
            .collect())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn mem_manager() -> SchedulerManager {
        SchedulerManager::new(Connection::open_in_memory().unwrap())
    }

    #[test]
    fn create_and_list() {
        let m = mem_manager();
        m.create("Morning show", "load", "Morning.m3u", "08:00", "Daily")
            .unwrap();
        let all = m.list_all().unwrap();
        assert_eq!(all.len(), 1);
        assert!(all[0].enabled);
    }

    #[test]
    fn due_matching() {
        let m = mem_manager();
        m.create("TOTH jingle", "play", "toth.mp3", "09:00", "Mon,Tue")
            .unwrap();
        assert_eq!(m.due_events("09:00", "Mon").unwrap().len(), 1);
        assert_eq!(m.due_events("09:00", "Wed").unwrap().len(), 0);
        assert_eq!(m.due_events("10:00", "Mon").unwrap().len(), 0);
    }

    #[test]
    fn toggle_and_delete() {
        let m = mem_manager();
        let e = m.create("Night", "generate", "Day", "00:00", "Daily").unwrap();
        m.set_enabled(&e.id, false).unwrap();
        assert_eq!(m.due_events("00:00", "Fri").unwrap().len(), 0);
        m.delete(&e.id).unwrap();
        assert!(m.list_all().unwrap().is_empty());
    }

    #[test]
    fn validate_and_days_mask() {
        assert!(validate_hhmm("00:00"));
        assert!(validate_hhmm("23:59"));
        assert!(!validate_hhmm("24:00"));
        assert!(!validate_hhmm("9:00"));
        assert!(!validate_hhmm("ab:cd"));
        assert_eq!(days_from_mask(127), "Daily");
        assert_eq!(days_from_mask(0), "Daily");
        assert_eq!(days_from_mask(1 | 8 | 64), "Mon,Thu,Sun");
        assert_eq!(mask_from_days("Daily"), 127);
        assert_eq!(mask_from_days("Mon,Wed"), 1 | 4);
    }

    #[test]
    fn update_rejects_bad_time() {
        let m = mem_manager();
        let e = m.create("X", "play", "a.mp3", "08:00", "Daily").unwrap();
        assert!(m.update(&e.id, "X", "play", "a.mp3", "99:99", "Daily").is_err());
        m.update(&e.id, "Y", "load", "b.m3u", "09:30", "Mon,Fri").unwrap();
        let all = m.list_all().unwrap();
        assert_eq!(all[0].name, "Y");
        assert_eq!(all[0].start_time, "09:30");
    }
}
