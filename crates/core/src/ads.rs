//! Advertisement blocks (RadioBOSS-style Ads Scheduler MVP).
//!
//! A block is one commercial break: an optional intro sting, the spot
//! itself, and an optional outro — fired at `HH:MM` on given days, but only
//! inside its validity window (`start_date`..=`end_date`). The UI plays the
//! chain through the engine queue: intro now, spot + outro appended.

use std::cell::RefCell;
use std::rc::Rc;

use chrono::NaiveDate;
use rusqlite::{params, Connection};
use uuid::Uuid;

use crate::error::Result;
use crate::scheduler::validate_hhmm;

/// One commercial break.
#[derive(Debug, Clone)]
pub struct AdBlock {
    pub id: String,
    pub name: String,
    /// The commercial audio (required).
    pub spot_path: String,
    /// Optional sting played immediately before the spot.
    pub intro_path: Option<String>,
    /// Optional sting queued right after the spot.
    pub outro_path: Option<String>,
    pub start_date: NaiveDate,
    pub end_date: NaiveDate,
    /// Daily start time as `HH:MM` (24h).
    pub play_time: String,
    /// `Daily` or comma list like `Mon,Tue`.
    pub days: String,
    pub enabled: bool,
}

impl AdBlock {
    /// Should this block fire now? (`date` bounds the validity window.)
    pub fn is_due(&self, date: NaiveDate, hhmm: &str, weekday: &str) -> bool {
        if !self.enabled || self.play_time != hhmm {
            return false;
        }
        if date < self.start_date || date > self.end_date {
            return false;
        }
        if self.days.eq_ignore_ascii_case("daily") {
            return true;
        }
        self.days
            .split(',')
            .any(|d| d.trim().eq_ignore_ascii_case(weekday))
    }

    /// Ordered clip chain for the break (existing files only).
    pub fn chain(&self) -> Vec<String> {
        [
            &self.intro_path,
            &Some(self.spot_path.clone()),
            &self.outro_path,
        ]
        .into_iter()
        .flatten()
        .filter(|p| !p.trim().is_empty() && std::path::Path::new(p).is_file())
        .cloned()
        .collect()
    }
}

fn parse_date(s: &str, field: &str) -> Result<NaiveDate> {
    NaiveDate::parse_from_str(s.trim(), "%Y-%m-%d").map_err(|_| {
        crate::error::CrabError::Scheduler(format!("bad {} '{}', want YYYY-MM-DD", field, s))
    })
}

/// Manages ad blocks backed by SQLite.
pub struct AdsManager {
    conn: Rc<RefCell<Connection>>,
}

impl AdsManager {
    pub fn new(conn: Connection) -> Self {
        let mgr = Self {
            conn: Rc::new(RefCell::new(conn)),
        };
        mgr.init_tables();
        mgr
    }

    pub fn open(path: &std::path::Path) -> Result<Self> {
        Ok(Self::new(Connection::open(path)?))
    }

    fn init_tables(&self) {
        self.conn
            .borrow()
            .execute_batch(
                "
                CREATE TABLE IF NOT EXISTS ad_blocks (
                    id          TEXT PRIMARY KEY,
                    name        TEXT NOT NULL,
                    spot_path   TEXT NOT NULL,
                    intro_path  TEXT NOT NULL DEFAULT '',
                    outro_path  TEXT NOT NULL DEFAULT '',
                    start_date  TEXT NOT NULL,
                    end_date    TEXT NOT NULL,
                    play_time   TEXT NOT NULL,
                    days        TEXT NOT NULL DEFAULT 'Daily',
                    enabled     INTEGER NOT NULL DEFAULT 1
                );
                ",
            )
            .expect("Failed to initialize ads tables");
    }

    #[allow(clippy::too_many_arguments)]
    pub fn create(
        &self,
        name: &str,
        spot: &str,
        intro: &str,
        outro: &str,
        start: &str,
        end: &str,
        play_time: &str,
        days: &str,
    ) -> Result<AdBlock> {
        if name.trim().is_empty() {
            return Err(crate::error::CrabError::Scheduler("name is empty".into()));
        }
        if spot.trim().is_empty() {
            return Err(crate::error::CrabError::Scheduler(
                "spot audio is required".into(),
            ));
        }
        if !validate_hhmm(play_time) {
            return Err(crate::error::CrabError::Scheduler(format!(
                "bad time '{}', want HH:MM",
                play_time
            )));
        }
        let start_date = parse_date(start, "start date")?;
        let end_date = parse_date(end, "end date")?;
        if end_date < start_date {
            return Err(crate::error::CrabError::Scheduler(
                "end date is before start date".into(),
            ));
        }
        let id = Uuid::new_v4().to_string();
        self.conn.borrow().execute(
            "INSERT INTO ad_blocks
             (id, name, spot_path, intro_path, outro_path,
              start_date, end_date, play_time, days, enabled)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, 1)",
            params![id, name, spot, intro, outro, start, end, play_time, days],
        )?;
        Ok(AdBlock {
            id,
            name: name.to_string(),
            spot_path: spot.to_string(),
            intro_path: Self::opt(intro),
            outro_path: Self::opt(outro),
            start_date,
            end_date,
            play_time: play_time.to_string(),
            days: days.to_string(),
            enabled: true,
        })
    }

    fn opt(s: &str) -> Option<String> {
        if s.trim().is_empty() {
            None
        } else {
            Some(s.to_string())
        }
    }

    pub fn list_all(&self) -> Result<Vec<AdBlock>> {
        let conn = self.conn.borrow();
        let mut stmt = conn.prepare(
            "SELECT id, name, spot_path, intro_path, outro_path,
                    start_date, end_date, play_time, days, enabled
             FROM ad_blocks ORDER BY play_time, name",
        )?;
        let blocks = stmt
            .query_map([], |row| {
                let start: String = row.get(5)?;
                let end: String = row.get(6)?;
                Ok(AdBlock {
                    id: row.get(0)?,
                    name: row.get(1)?,
                    spot_path: row.get(2)?,
                    intro_path: Self::opt(&row.get::<_, String>(3)?),
                    outro_path: Self::opt(&row.get::<_, String>(4)?),
                    start_date: NaiveDate::parse_from_str(&start, "%Y-%m-%d")
                        .unwrap_or(NaiveDate::from_ymd_opt(2000, 1, 1).unwrap()),
                    end_date: NaiveDate::parse_from_str(&end, "%Y-%m-%d")
                        .unwrap_or(NaiveDate::from_ymd_opt(2099, 12, 31).unwrap()),
                    play_time: row.get(7)?,
                    days: row.get(8)?,
                    enabled: row.get::<_, i32>(9)? != 0,
                })
            })?
            .collect::<std::result::Result<Vec<_>, _>>()?;
        Ok(blocks)
    }

    #[allow(clippy::too_many_arguments)]
    pub fn update(
        &self,
        id: &str,
        name: &str,
        spot: &str,
        intro: &str,
        outro: &str,
        start: &str,
        end: &str,
        play_time: &str,
        days: &str,
    ) -> Result<()> {
        if name.trim().is_empty() {
            return Err(crate::error::CrabError::Scheduler("name is empty".into()));
        }
        if spot.trim().is_empty() {
            return Err(crate::error::CrabError::Scheduler(
                "spot audio is required".into(),
            ));
        }
        if !validate_hhmm(play_time) {
            return Err(crate::error::CrabError::Scheduler(format!(
                "bad time '{}', want HH:MM",
                play_time
            )));
        }
        let start_date = parse_date(start, "start date")?;
        let end_date = parse_date(end, "end date")?;
        if end_date < start_date {
            return Err(crate::error::CrabError::Scheduler(
                "end date is before start date".into(),
            ));
        }
        let _ = (start_date, end_date);
        self.conn.borrow().execute(
            "UPDATE ad_blocks SET name = ?1, spot_path = ?2, intro_path = ?3,
             outro_path = ?4, start_date = ?5, end_date = ?6, play_time = ?7, days = ?8
             WHERE id = ?9",
            params![name, spot, intro, outro, start, end, play_time, days, id],
        )?;
        Ok(())
    }

    pub fn set_enabled(&self, id: &str, enabled: bool) -> Result<()> {
        self.conn.borrow().execute(
            "UPDATE ad_blocks SET enabled = ?1 WHERE id = ?2",
            params![enabled as i32, id],
        )?;
        Ok(())
    }

    pub fn delete(&self, id: &str) -> Result<()> {
        self.conn
            .borrow()
            .execute("DELETE FROM ad_blocks WHERE id = ?1", params![id])?;
        Ok(())
    }

    /// Blocks that should fire now.
    pub fn due_blocks(&self, date: NaiveDate, hhmm: &str, weekday: &str) -> Result<Vec<AdBlock>> {
        Ok(self
            .list_all()?
            .into_iter()
            .filter(|b| b.is_due(date, hhmm, weekday))
            .collect())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::Datelike;

    fn mem_manager() -> AdsManager {
        AdsManager::new(Connection::open_in_memory().unwrap())
    }

    fn block(m: &AdsManager) -> AdBlock {
        m.create(
            "Morning ads",
            "/ads/spot.mp3",
            "/ads/intro.mp3",
            "",
            "2024-01-01",
            "2024-12-31",
            "08:00",
            "Daily",
        )
        .unwrap()
    }

    #[test]
    fn validity_window_gates() {
        let m = mem_manager();
        let b = block(&m);
        let inside = NaiveDate::from_ymd_opt(2024, 6, 1).unwrap();
        assert!(b.is_due(inside, "08:00", "Mon"));
        assert!(!b.is_due(inside, "09:00", "Mon"));
        let before = NaiveDate::from_ymd_opt(2023, 6, 1).unwrap();
        assert!(!b.is_due(before, "08:00", "Mon"));
        let after = NaiveDate::from_ymd_opt(2025, 1, 1).unwrap();
        assert!(!b.is_due(after, "08:00", "Mon"));
        assert_eq!(m.due_blocks(inside, "08:00", "Mon").unwrap().len(), 1);
        assert!(m.due_blocks(after, "08:00", "Mon").unwrap().is_empty());
    }

    #[test]
    fn rejects_bad_input() {
        let m = mem_manager();
        assert!(m
            .create(
                "x",
                "",
                "",
                "",
                "2024-01-01",
                "2024-02-01",
                "08:00",
                "Daily"
            )
            .is_err());
        assert!(m
            .create(
                "x",
                "s.mp3",
                "",
                "",
                "2024-13-01",
                "2024-02-01",
                "08:00",
                "Daily"
            )
            .is_err());
        assert!(m
            .create(
                "x",
                "s.mp3",
                "",
                "",
                "2024-02-01",
                "2024-01-01",
                "08:00",
                "Daily"
            )
            .is_err());
        assert!(m
            .create(
                "x",
                "s.mp3",
                "",
                "",
                "2024-01-01",
                "2024-02-01",
                "8am",
                "Daily"
            )
            .is_err());
    }

    #[test]
    fn weekday_filter_and_toggle() {
        let m = mem_manager();
        let b = m
            .create(
                "WF",
                "/a.mp3",
                "",
                "",
                "2024-01-01",
                "2024-12-31",
                "08:00",
                "Mon,Wed",
            )
            .unwrap();
        let mon = NaiveDate::from_ymd_opt(2024, 6, 3).unwrap(); // a Monday
        assert_eq!(mon.weekday(), chrono::Weekday::Mon);
        assert!(b.is_due(mon, "08:00", "Mon"));
        assert!(!b.is_due(mon, "08:00", "Tue"));
        m.set_enabled(&b.id, false).unwrap();
        assert!(m.due_blocks(mon, "08:00", "Mon").unwrap().is_empty());
        m.delete(&b.id).unwrap();
        assert!(m.list_all().unwrap().is_empty());
    }

    #[test]
    fn chain_orders_existing_files_only() {
        let dir = std::env::temp_dir();
        let intro = dir.join("crabboss-ad-intro.tmp");
        let spot = dir.join("crabboss-ad-spot.tmp");
        std::fs::write(&intro, b"x").unwrap();
        std::fs::write(&spot, b"y").unwrap();
        let b = AdBlock {
            id: "1".into(),
            name: "t".into(),
            spot_path: spot.to_string_lossy().to_string(),
            intro_path: Some(intro.to_string_lossy().to_string()),
            outro_path: Some("/nope/missing.mp3".into()),
            start_date: NaiveDate::from_ymd_opt(2024, 1, 1).unwrap(),
            end_date: NaiveDate::from_ymd_opt(2024, 12, 31).unwrap(),
            play_time: "08:00".into(),
            days: "Daily".into(),
            enabled: true,
        };
        let chain = b.chain();
        assert_eq!(chain.len(), 2);
        assert!(chain[0].ends_with("crabboss-ad-intro.tmp"));
        assert!(chain[1].ends_with("crabboss-ad-spot.tmp"));
        std::fs::remove_file(&intro).ok();
        std::fs::remove_file(&spot).ok();
    }
}
