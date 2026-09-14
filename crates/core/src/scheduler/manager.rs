//! SQLite-backed scheduler event store.

use std::cell::RefCell;
use std::rc::Rc;

use chrono::{DateTime, NaiveDate, Utc};
use rusqlite::{params, Connection};
use uuid::Uuid;

use crate::error::{CrabError, Result};

/// A scheduled automation event (mirrors RadioBOSS Scheduler tab row).
#[derive(Debug, Clone)]
pub struct ScheduledEvent {
    pub id: String,
    pub name: String,
    /// `play` | `load` | `generate` | `queue` | `command`
    pub action_type: String,
    /// Playlist name / file path / preset name / raw command.
    pub target: String,
    /// Daily start time as `HH:MM` (24h).
    pub start_time: String,
    /// Repeat days: `Daily` or comma list like `Mon,Tue,Wed`.
    pub days: String,
    /// "Valid until" date `YYYY-MM-DD` (inclusive) or `None` = runs forever.
    pub expires_on: Option<String>,
    pub enabled: bool,
    pub created_at: DateTime<Utc>,
}

/// Expiration state of an event relative to a date.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ExpiryStatus {
    /// No expiration set.
    Forever,
    /// Still valid, with this many days left (≥ 1).
    Active(u32),
    /// Valid until date == today: last firing day.
    ExpiresToday,
    /// Valid-until date has passed; won't fire anymore.
    Expired,
}

impl ScheduledEvent {
    /// Does this event fire on `today` (`YYYY-MM-DD`) at `HH:MM` +
    /// 3-letter weekday (`Mon`..`Sun`)? Expiration is inclusive: an event
    /// valid until today still fires today.
    pub fn is_due(&self, today: &str, now_hhmm: &str, weekday: &str) -> bool {
        if !self.enabled || self.start_time != now_hhmm {
            return false;
        }
        match self.expiry_status(today) {
            ExpiryStatus::Expired => return false,
            ExpiryStatus::Forever | ExpiryStatus::Active(_) | ExpiryStatus::ExpiresToday => {}
        }
        if self.days.eq_ignore_ascii_case("daily") {
            return true;
        }
        self.days
            .split(',')
            .any(|d| d.trim().eq_ignore_ascii_case(weekday))
    }

    /// Expiration state relative to `today` (`YYYY-MM-DD`).
    pub fn expiry_status(&self, today: &str) -> ExpiryStatus {
        let Some(until) = self.expires_on.as_deref() else {
            return ExpiryStatus::Forever;
        };
        let (Ok(until), Ok(now)) = (
            NaiveDate::parse_from_str(until, "%Y-%m-%d"),
            NaiveDate::parse_from_str(today, "%Y-%m-%d"),
        ) else {
            return ExpiryStatus::Forever; // unparsable stored date: don't disable
        };
        match until.cmp(&now) {
            std::cmp::Ordering::Less => ExpiryStatus::Expired,
            std::cmp::Ordering::Equal => ExpiryStatus::ExpiresToday,
            std::cmp::Ordering::Greater => {
                ExpiryStatus::Active((until - now).num_days().max(0) as u32)
            }
        }
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

/// Validate an optional `YYYY-MM-DD` date ("" or None = no expiration).
pub fn parse_expires(v: Option<&str>) -> Result<Option<String>> {
    match v.map(str::trim).filter(|s| !s.is_empty()) {
        None => Ok(None),
        Some(s) => NaiveDate::parse_from_str(s, "%Y-%m-%d")
            .map(|_| Some(s.to_string()))
            .map_err(|_| {
                crate::error::CrabError::Scheduler(format!("bad date '{s}', want YYYY-MM-DD"))
            }),
    }
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

    /// Open (or create) the scheduler store at the given SQLite file.
    /// Shares the same `crabboss.db` file as the library — separate connection.
    pub fn open(path: &std::path::Path) -> Result<Self> {
        Self::new(crate::db::Database::open_connection(path)?)
    }

    fn init_tables(&self) -> Result<()> {
        self.conn.borrow().execute_batch(
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
        )?;
        // Migrate older stores: add the expiration column if missing.
        // (Central bootstrap v2 covers this too; this stays so the
        // manager is self-sufficient when opened directly, e.g. tests.)
        let cols: Vec<String> = self
            .conn
            .borrow()
            .prepare("PRAGMA table_info(scheduled_events)")?
            .query_map([], |row| row.get::<_, String>(1))?
            .collect::<std::result::Result<Vec<_>, _>>()?;
        if !cols.iter().any(|c| c == "expires_on") {
            self.conn.borrow().execute(
                "ALTER TABLE scheduled_events ADD COLUMN expires_on TEXT",
                [],
            )?;
        }
        Ok(())
    }

    /// Create a new event. `start_time` must be `HH:MM`; `expires_on` an
    /// optional `YYYY-MM-DD` (inclusive) or empty for "runs forever".
    #[allow(clippy::too_many_arguments)]
    pub fn create(
        &self,
        name: &str,
        action_type: &str,
        target: &str,
        start_time: &str,
        days: &str,
        expires_on: Option<&str>,
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
        let expires_on = parse_expires(expires_on)?;
        let id = Uuid::new_v4().to_string();
        let now = Utc::now();
        self.conn.borrow().execute(
            "INSERT INTO scheduled_events
             (id, name, action_type, target, start_time, days, enabled, created_at, expires_on)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, 1, ?7, ?8)",
            params![
                id,
                name,
                action_type,
                target,
                start_time,
                days,
                now.to_rfc3339(),
                expires_on,
            ],
        )?;
        Ok(ScheduledEvent {
            id,
            name: name.to_string(),
            action_type: action_type.to_string(),
            target: target.to_string(),
            start_time: start_time.to_string(),
            days: days.to_string(),
            expires_on,
            enabled: true,
            created_at: now,
        })
    }

    pub fn list_all(&self) -> Result<Vec<ScheduledEvent>> {
        let conn = self.conn.borrow();
        let mut stmt = conn.prepare(
            "SELECT id, name, action_type, target, start_time, days, enabled, created_at, expires_on
             FROM scheduled_events ORDER BY start_time, name",
        )?;
        let mut rows = stmt.query([])?;
        let mut events = Vec::new();
        while let Some(row) = rows.next()? {
            let id: String = row.get(0)?;
            let created_raw: String = row.get(7)?;
            let created_at = DateTime::parse_from_rfc3339(&created_raw)
                .map(|dt| dt.with_timezone(&Utc))
                .map_err(|_| CrabError::Integrity {
                    table: "scheduled_events",
                    id: id.clone(),
                    field: "created_at",
                    value: created_raw,
                })?;
            events.push(ScheduledEvent {
                id,
                name: row.get(1)?,
                action_type: row.get(2)?,
                target: row.get(3)?,
                start_time: row.get(4)?,
                days: row.get(5)?,
                enabled: row.get::<_, i32>(6)? != 0,
                created_at,
                expires_on: row.get::<_, Option<String>>(8)?,
            });
        }
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
    #[allow(clippy::too_many_arguments)]
    pub fn update(
        &self,
        id: &str,
        name: &str,
        action_type: &str,
        target: &str,
        start_time: &str,
        days: &str,
        expires_on: Option<&str>,
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
        let expires_on = parse_expires(expires_on)?;
        self.conn.borrow().execute(
            "UPDATE scheduled_events
             SET name = ?1, action_type = ?2, target = ?3, start_time = ?4, days = ?5, expires_on = ?6
             WHERE id = ?7",
            params![name, action_type, target, start_time, days, expires_on, id],
        )?;
        Ok(())
    }

    pub fn delete(&self, id: &str) -> Result<()> {
        self.conn
            .borrow()
            .execute("DELETE FROM scheduled_events WHERE id = ?1", params![id])?;
        Ok(())
    }

    /// Events that should fire now (`today` = `YYYY-MM-DD`, `now_hhmm` =
    /// `HH:MM`, `weekday` = `Mon`..). Expiration-aware and inclusive.
    pub fn due_events(
        &self,
        today: &str,
        now_hhmm: &str,
        weekday: &str,
    ) -> Result<Vec<ScheduledEvent>> {
        Ok(self
            .list_all()?
            .into_iter()
            .filter(|e| e.is_due(today, now_hhmm, weekday))
            .collect())
    }

    /// Warnings for the Scheduler screen: events whose validity ends soon
    /// (within `warn_days`) or that have already expired silently.
    pub fn expiry_warnings(&self, today: &str, warn_days: u32) -> Result<Vec<String>> {
        let mut out = Vec::new();
        for e in self.list_all()? {
            match e.expiry_status(today) {
                ExpiryStatus::Expired => out.push(format!(
                    "⚠ '{}' expired {} (set to run {}) — edit or delete it",
                    e.name,
                    e.expires_on.clone().unwrap_or_default(),
                    e.start_time
                )),
                ExpiryStatus::ExpiresToday => out.push(format!(
                    "⚠ '{}' runs for the last time today at {}",
                    e.name, e.start_time
                )),
                ExpiryStatus::Active(days) if days <= warn_days => out.push(format!(
                    "⏳ '{}' valid {} more day{} (until {})",
                    e.name,
                    days,
                    if days == 1 { "" } else { "s" },
                    e.expires_on.clone().unwrap_or_default()
                )),
                _ => {}
            }
        }
        Ok(out)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn mem_manager() -> SchedulerManager {
        SchedulerManager::new(Connection::open_in_memory().unwrap()).unwrap()
    }

    #[test]
    fn create_and_list() {
        let m = mem_manager();
        m.create(
            "Morning show",
            "load",
            "Morning.m3u",
            "08:00",
            "Daily",
            None,
        )
        .unwrap();
        let all = m.list_all().unwrap();
        assert_eq!(all.len(), 1);
        assert!(all[0].enabled);
        assert_eq!(all[0].expires_on, None);
    }

    #[test]
    fn due_matching() {
        let m = mem_manager();
        m.create("TOTH jingle", "play", "toth.mp3", "09:00", "Mon,Tue", None)
            .unwrap();
        assert_eq!(m.due_events("2026-09-07", "09:00", "Mon").unwrap().len(), 1);
        assert_eq!(m.due_events("2026-09-07", "09:00", "Wed").unwrap().len(), 0);
        assert_eq!(m.due_events("2026-09-07", "10:00", "Mon").unwrap().len(), 0);
    }

    #[test]
    fn toggle_and_delete() {
        let m = mem_manager();
        let e = m
            .create("Night", "generate", "Day", "00:00", "Daily", None)
            .unwrap();
        m.set_enabled(&e.id, false).unwrap();
        assert_eq!(m.due_events("2026-09-07", "00:00", "Fri").unwrap().len(), 0);
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
        let e = m
            .create("X", "play", "a.mp3", "08:00", "Daily", None)
            .unwrap();
        assert!(m
            .update(&e.id, "X", "play", "a.mp3", "99:99", "Daily", None)
            .is_err());
        m.update(&e.id, "Y", "load", "b.m3u", "09:30", "Mon,Fri", None)
            .unwrap();
        let all = m.list_all().unwrap();
        assert_eq!(all[0].name, "Y");
        assert_eq!(all[0].start_time, "09:30");
    }

    #[test]
    fn expiration_gates_due() {
        let m = mem_manager();
        // Valid until 2026-09-07: fires ON that date, not after.
        m.create(
            "Campaign spot",
            "play",
            "spot.mp3",
            "12:00",
            "Daily",
            Some("2026-09-07"),
        )
        .unwrap();
        assert_eq!(m.due_events("2026-09-07", "12:00", "Mon").unwrap().len(), 1);
        assert_eq!(m.due_events("2026-09-08", "12:00", "Tue").unwrap().len(), 0);
        // Expiration is honored even when the weekday still matches.
        assert_eq!(m.due_events("2026-09-14", "12:00", "Mon").unwrap().len(), 0);
        // Forever events are unaffected.
        m.create("Forever", "play", "x.mp3", "12:00", "Daily", None)
            .unwrap();
        assert_eq!(m.due_events("2026-09-14", "12:00", "Mon").unwrap().len(), 1);
        // Disabled events never fire, even on their last day.
        let e = m.list_all().unwrap().remove(0);
        m.set_enabled(&e.id, false).unwrap();
        assert_eq!(m.due_events("2026-09-07", "12:00", "Mon").unwrap().len(), 1);
    }

    #[test]
    fn expiry_status_and_warnings() {
        let m = mem_manager();
        m.create("Old", "play", "a.mp3", "08:00", "Daily", Some("2026-09-01"))
            .unwrap();
        m.create(
            "Today",
            "play",
            "b.mp3",
            "09:00",
            "Daily",
            Some("2026-09-07"),
        )
        .unwrap();
        m.create(
            "Soon",
            "play",
            "c.mp3",
            "10:00",
            "Daily",
            Some("2026-09-09"),
        )
        .unwrap();
        m.create(
            "Later",
            "play",
            "d.mp3",
            "11:00",
            "Daily",
            Some("2027-01-01"),
        )
        .unwrap();
        m.create("Always", "play", "e.mp3", "12:00", "Daily", None)
            .unwrap();

        let all = m.list_all().unwrap();
        let status = |name: &str| {
            all.iter()
                .find(|e| e.name == name)
                .unwrap()
                .expiry_status("2026-09-07")
        };
        assert_eq!(status("Old"), ExpiryStatus::Expired);
        assert_eq!(status("Today"), ExpiryStatus::ExpiresToday);
        assert_eq!(status("Soon"), ExpiryStatus::Active(2));
        assert_eq!(status("Later"), ExpiryStatus::Active(116));
        assert_eq!(status("Always"), ExpiryStatus::Forever);

        let warns = m.expiry_warnings("2026-09-07", 3).unwrap();
        assert!(
            warns.iter().any(|w| w.contains("'Old' expired")),
            "{warns:?}"
        );
        assert!(
            warns.iter().any(|w| w.contains("last time today")),
            "{warns:?}"
        );
        assert!(
            warns.iter().any(|w| w.contains("'Soon' valid 2 more days")),
            "{warns:?}"
        );
        assert!(
            !warns.iter().any(|w| w.contains("Later")),
            "beyond window: {warns:?}"
        );
        assert!(!warns.iter().any(|w| w.contains("Always")), "{warns:?}");
    }

    #[test]
    fn parse_expires_validates() {
        assert_eq!(parse_expires(None).unwrap(), None);
        assert_eq!(parse_expires(Some("")).unwrap(), None);
        assert_eq!(parse_expires(Some("  ")).unwrap(), None);
        assert_eq!(
            parse_expires(Some("2026-12-31")).unwrap(),
            Some("2026-12-31".to_string())
        );
        assert!(parse_expires(Some("31-12-2026")).is_err());
        assert!(parse_expires(Some("soon")).is_err());
    }

    #[test]
    fn malformed_created_at_is_integrity_error() {
        let m = mem_manager();
        m.conn
            .borrow()
            .execute(
                "INSERT INTO scheduled_events
                 (id, name, action_type, target, start_time, days, enabled, created_at)
                 VALUES ('b1','Bad','play','x','08:00','Daily',1,'not-a-time')",
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
                assert_eq!(table, "scheduled_events");
                assert_eq!(id, "b1");
                assert_eq!(field, "created_at");
                assert_eq!(value, "not-a-time");
            }
            other => panic!("expected Integrity error, got {other:?}"),
        }
    }
}
