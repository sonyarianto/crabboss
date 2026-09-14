//! Station backup/restore: one versioned JSON file holding the settings
//! plus the SQLite-backed lists (scheduler, carts, ads).
//!
//! The core store types have no serde impls on purpose (SQLite is the
//! source of truth), so this module maps them to plain DTOs and applies
//! restores through the existing manager APIs (`create`/`delete`/…).
//!
//! Policy is **best-effort with validation first** (P0.3a): every row is
//! validated before anything is mutated, invalid rows are skipped with a
//! reason in the status line, and valid rows are applied. One bad row
//! never vetoes the whole restore. What this does NOT yet do is
//! transactional all-or-nothing across the three lists (P0.3b): each
//! list is still replaced wholesale, so an I/O failure mid-apply (after
//! validation passed) can leave that list partial. That needs the shared
//! transaction boundary from centralized SQLite bootstrap (P1.2).
//!
//! Restore applies settings live like `boot` does, except the audio
//! output device: switching outputs means reopening the engine, which is
//! a restart-class operation. The status line says so when it differs.

use std::path::Path;

use serde::{Deserialize, Serialize};

use crabcore::cart::WALL_SIZE;

use crate::app::App;

pub(crate) const BACKUP_VERSION: u32 = 1;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub(crate) struct Backup {
    pub(crate) version: u32,
    pub(crate) settings: crabcore::settings::AppSettings,
    pub(crate) scheduler: Vec<SchedBackup>,
    pub(crate) carts: Vec<CartBackup>,
    pub(crate) ads: Vec<AdBackup>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub(crate) struct SchedBackup {
    pub(crate) name: String,
    pub(crate) action_type: String,
    pub(crate) target: String,
    pub(crate) start_time: String,
    pub(crate) days: String,
    pub(crate) expires_on: Option<String>,
    pub(crate) enabled: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub(crate) struct CartBackup {
    pub(crate) label: String,
    pub(crate) file_path: String,
    pub(crate) position: i32,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub(crate) struct AdBackup {
    pub(crate) name: String,
    pub(crate) spot_path: String,
    pub(crate) intro_path: Option<String>,
    pub(crate) outro_path: Option<String>,
    pub(crate) start_date: String,
    pub(crate) end_date: String,
    pub(crate) play_time: String,
    pub(crate) days: String,
    pub(crate) enabled: bool,
}

pub(crate) fn write_backup(path: &Path, backup: &Backup) -> std::io::Result<()> {
    let text = serde_json::to_string_pretty(backup)
        .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))?;
    // Atomic like settings: a failed backup never truncates the previous
    // backup file.
    crabcore::settings::atomic_write(path, text.as_bytes())
}

pub(crate) fn read_backup(path: &Path) -> Result<Backup, String> {
    let text = std::fs::read_to_string(path)
        .map_err(|e| format!("cannot read {}: {e}", path.display()))?;
    let backup: Backup =
        serde_json::from_str(&text).map_err(|e| format!("not a CrabBoss backup: {e}"))?;
    if backup.version != BACKUP_VERSION {
        return Err(format!(
            "unsupported backup version {}, want {BACKUP_VERSION}",
            backup.version
        ));
    }
    Ok(backup)
}

/// One backup row that will not survive the manager on apply.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ValidationIssue {
    pub(crate) section: &'static str,
    pub(crate) index: usize,
    pub(crate) name: String,
    pub(crate) reason: String,
}

fn issue(
    section: &'static str,
    index: usize,
    name: &str,
    reason: impl Into<String>,
) -> ValidationIssue {
    ValidationIssue {
        section,
        index,
        name: name.to_string(),
        reason: reason.into(),
    }
}

fn parse_backup_date(s: &str) -> Result<chrono::NaiveDate, String> {
    chrono::NaiveDate::parse_from_str(s.trim(), "%Y-%m-%d")
        .map_err(|_| format!("bad date '{s}', want YYYY-MM-DD"))
}

/// Validate every row against the same rules the managers enforce, so a
/// restore plan is known before anything is mutated. (Manager I/O errors
/// can still occur at apply time; those are reported per row as well.)
pub(crate) fn validate_backup(b: &Backup) -> Vec<ValidationIssue> {
    let mut issues = Vec::new();
    for (i, e) in b.scheduler.iter().enumerate() {
        if e.name.trim().is_empty() {
            issues.push(issue("scheduler", i, &e.name, "name is empty"));
        }
        if !crabcore::scheduler::validate_hhmm(&e.start_time) {
            issues.push(issue(
                "scheduler",
                i,
                &e.name,
                format!("bad time '{}', want HH:MM", e.start_time),
            ));
        }
        if let Err(err) = crabcore::scheduler::parse_expires(e.expires_on.as_deref()) {
            issues.push(issue("scheduler", i, &e.name, err.to_string()));
        }
    }
    for (i, c) in b.carts.iter().enumerate() {
        if !(0..WALL_SIZE as i32).contains(&c.position) {
            issues.push(issue(
                "carts",
                i,
                &c.label,
                format!("bad position {}", c.position),
            ));
        }
    }
    for (i, a) in b.ads.iter().enumerate() {
        if a.name.trim().is_empty() {
            issues.push(issue("ads", i, &a.name, "name is empty"));
        }
        if a.spot_path.trim().is_empty() {
            issues.push(issue("ads", i, &a.name, "spot audio is required"));
        }
        if !crabcore::scheduler::validate_hhmm(&a.play_time) {
            issues.push(issue(
                "ads",
                i,
                &a.name,
                format!("bad time '{}', want HH:MM", a.play_time),
            ));
        }
        match (
            parse_backup_date(&a.start_date),
            parse_backup_date(&a.end_date),
        ) {
            (Ok(start), Ok(end)) if end < start => {
                issues.push(issue("ads", i, &a.name, "end date is before start date"));
            }
            (Err(e), _) => issues.push(issue("ads", i, &a.name, format!("bad start date: {e}"))),
            (_, Err(e)) => issues.push(issue("ads", i, &a.name, format!("bad end date: {e}"))),
            _ => {}
        }
    }
    issues
}

fn is_flagged(issues: &[ValidationIssue], section: &'static str, index: usize) -> bool {
    issues
        .iter()
        .any(|iss| iss.section == section && iss.index == index)
}

impl App {
    pub(crate) fn build_backup(&self) -> Backup {
        Backup {
            version: BACKUP_VERSION,
            settings: self.settings.clone(),
            scheduler: self
                .sched_events
                .iter()
                .map(|e| SchedBackup {
                    name: e.name.clone(),
                    action_type: e.action_type.clone(),
                    target: e.target.clone(),
                    start_time: e.start_time.clone(),
                    days: e.days.clone(),
                    expires_on: e.expires_on.clone(),
                    enabled: e.enabled,
                })
                .collect(),
            carts: self
                .cart_list
                .iter()
                .map(|c| CartBackup {
                    label: c.label.clone(),
                    file_path: c.file_path.clone(),
                    position: c.position,
                })
                .collect(),
            ads: self
                .ad_blocks
                .iter()
                .map(|b| AdBackup {
                    name: b.name.clone(),
                    spot_path: b.spot_path.clone(),
                    intro_path: b.intro_path.clone(),
                    outro_path: b.outro_path.clone(),
                    start_date: b.start_date.to_string(),
                    end_date: b.end_date.to_string(),
                    play_time: b.play_time.clone(),
                    days: b.days.clone(),
                    enabled: b.enabled,
                })
                .collect(),
        }
    }

    /// Replace settings + all three lists from a backup. Every row is
    /// validated before anything is mutated; invalid rows are skipped
    /// with a reason (best-effort policy, P0.3a). Manager I/O failures at
    /// apply time are reported per row the same way. Returns the
    /// user-facing status line.
    pub(crate) fn apply_backup(&mut self, backup: Backup) -> Result<String, String> {
        // -- Validate everything before touching live state ------------------
        let issues = validate_backup(&backup);
        for iss in &issues {
            tracing::warn!(
                "Restore will skip {} #{} '{}': {}",
                iss.section,
                iss.index,
                iss.name,
                iss.reason
            );
        }
        // -- Settings (live-apply mirrors boot) -------------------------------
        let device_changed = self.settings.output_device != backup.settings.output_device;
        let target_changed =
            self.settings.loudness_target_lufs != backup.settings.loudness_target_lufs;
        self.settings = backup.settings;
        self.save_settings();
        self.player.set_crossfade_secs(self.settings.crossfade_secs);
        self.player
            .set_silence_threshold_secs(self.settings.silence_threshold_secs);
        self.player.set_eq_enabled(self.settings.eq_enabled);
        for (band, gain) in self.settings.eq_gains_db.iter().enumerate() {
            self.player.set_eq_band(band, *gain);
        }
        self.player
            .set_limiter_ceiling(self.settings.limiter_ceiling);
        self.player
            .set_loudness_enabled(self.settings.loudness_norm);
        self.player.set_stream_config(self.settings.stream.clone());
        if self.settings.stream.enabled {
            if let Err(e) = self.player.stream_start() {
                tracing::warn!("Restore stream start failed: {e}");
            }
        }
        self.player.set_mic_config(self.settings.mic.clone());
        if self.settings.mic.enabled {
            if let Err(e) = self.player.mic_start() {
                tracing::warn!("Restore mic start failed: {e}");
            }
        }
        self.autodj = self.settings.autodj;
        self.station_name = self.settings.station_name.clone();
        self.sel_device = self.settings.output_device.clone().unwrap_or_default();
        if target_changed {
            match self
                .library
                .retarget_gains(self.settings.loudness_target_lufs)
            {
                Ok(n) => tracing::info!("Restore re-targeted {n} loudness gains"),
                Err(e) => tracing::warn!("Restore gain retarget failed: {e}"),
            }
        }

        // -- Scheduler (new ids; dedupe maps keyed by old ids go stale) -------
        let total_s = backup.scheduler.len();
        let ids: Vec<String> = self.sched_events.iter().map(|e| e.id.clone()).collect();
        for id in &ids {
            if let Err(e) = self.scheduler.delete(id) {
                tracing::warn!("Restore scheduler clear failed: {e}");
            }
        }
        let (mut ok_s, mut skip_s) = (0usize, 0usize);
        for (i, e) in backup.scheduler.iter().enumerate() {
            if is_flagged(&issues, "scheduler", i) {
                skip_s += 1;
                continue;
            }
            match self.scheduler.create(
                &e.name,
                &e.action_type,
                &e.target,
                &e.start_time,
                &e.days,
                e.expires_on.as_deref(),
            ) {
                Ok(ev) => {
                    if !e.enabled {
                        let _ = self.scheduler.set_enabled(&ev.id, false);
                    }
                    ok_s += 1;
                }
                Err(err) => {
                    tracing::warn!("Restore skipped scheduler '{}': {err}", e.name);
                    skip_s += 1;
                }
            }
        }

        // -- Carts (pads addressed by position) --------------------------------
        let total_c = backup.carts.len();
        let ids: Vec<String> = self.cart_list.iter().map(|c| c.id.clone()).collect();
        for id in &ids {
            if let Err(e) = self.carts.delete(id) {
                tracing::warn!("Restore carts clear failed: {e}");
            }
        }
        let (mut ok_c, mut skip_c) = (0usize, 0usize);
        for (i, c) in backup.carts.iter().enumerate() {
            if is_flagged(&issues, "carts", i) {
                skip_c += 1;
                continue;
            }
            match self.carts.assign_at(c.position, &c.label, &c.file_path) {
                Ok(()) => ok_c += 1,
                Err(err) => {
                    tracing::warn!("Restore skipped cart '{}': {err}", c.label);
                    skip_c += 1;
                }
            }
        }

        // -- Ads ---------------------------------------------------------------
        let total_a = backup.ads.len();
        let ids: Vec<String> = self.ad_blocks.iter().map(|b| b.id.clone()).collect();
        for id in &ids {
            if let Err(e) = self.ads.delete(id) {
                tracing::warn!("Restore ads clear failed: {e}");
            }
        }
        let (mut ok_a, mut skip_a) = (0usize, 0usize);
        for (i, a) in backup.ads.iter().enumerate() {
            if is_flagged(&issues, "ads", i) {
                skip_a += 1;
                continue;
            }
            match self.ads.create(
                &a.name,
                &a.spot_path,
                a.intro_path.as_deref().unwrap_or(""),
                a.outro_path.as_deref().unwrap_or(""),
                &a.start_date,
                &a.end_date,
                &a.play_time,
                &a.days,
            ) {
                Ok(block) => {
                    if !a.enabled {
                        let _ = self.ads.set_enabled(&block.id, false);
                    }
                    ok_a += 1;
                }
                Err(err) => {
                    tracing::warn!("Restore skipped ad block '{}': {err}", a.name);
                    skip_a += 1;
                }
            }
        }

        self.fired.clear();
        self.fired_ads.clear();
        self.refresh_scheduler();
        self.refresh_carts();
        self.refresh_ads();
        self.refresh_counts();

        let mut parts = vec![
            "settings".to_string(),
            format!("{ok_s}/{total_s} scheduler"),
            format!("{ok_c}/{total_c} carts"),
            format!("{ok_a}/{total_a} ads"),
        ];
        let skipped = skip_s + skip_c + skip_a;
        if skipped > 0 {
            // Status line stays readable: first three reasons inline, the
            // rest in the log (already warned above, one line per row).
            let shown: Vec<String> = issues
                .iter()
                .take(3)
                .map(|iss| format!("{} '{}': {}", iss.section, iss.name, iss.reason))
                .collect();
            parts.push(format!("{skipped} invalid skipped ({})", shown.join("; ")));
        }
        if device_changed {
            parts.push("restart to switch audio device".to_string());
        }
        Ok(format!("Restored: {}", parts.join(", ")))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn good_backup() -> Backup {
        Backup {
            version: BACKUP_VERSION,
            settings: crabcore::settings::AppSettings::default(),
            scheduler: vec![SchedBackup {
                name: "Morning".into(),
                action_type: "play".into(),
                target: "x.mp3".into(),
                start_time: "08:00".into(),
                days: "Daily".into(),
                expires_on: None,
                enabled: true,
            }],
            carts: vec![CartBackup {
                label: "Pad".into(),
                file_path: "C:/a.mp3".into(),
                position: 0,
            }],
            ads: vec![AdBackup {
                name: "Break".into(),
                spot_path: "C:/s.mp3".into(),
                intro_path: None,
                outro_path: None,
                start_date: "2026-01-01".into(),
                end_date: "2026-12-31".into(),
                play_time: "09:00".into(),
                days: "Daily".into(),
                enabled: true,
            }],
        }
    }

    #[test]
    fn validate_accepts_clean_backup() {
        assert!(validate_backup(&good_backup()).is_empty());
    }

    #[test]
    fn validate_flags_scheduler_rows_with_index() {
        let mut b = good_backup();
        b.scheduler.push(SchedBackup {
            name: "   ".into(),
            action_type: "play".into(),
            target: "x.mp3".into(),
            start_time: "25:99".into(),
            days: "Daily".into(),
            expires_on: Some("soon".into()),
            enabled: true,
        });
        let issues = validate_backup(&b);
        // Row 0 is clean; row 1 collects all three problems.
        assert!(issues.iter().all(|iss| iss.index == 1));
        assert_eq!(issues.len(), 3);
        let reasons: Vec<_> = issues.iter().map(|iss| iss.reason.as_str()).collect();
        assert!(reasons.iter().any(|r| r.contains("name is empty")));
        assert!(reasons.iter().any(|r| r.contains("want HH:MM")));
        assert!(reasons.iter().any(|r| r.contains("want YYYY-MM-DD")));
        assert!(issues.iter().all(|iss| iss.section == "scheduler"));
    }

    #[test]
    fn validate_flags_cart_positions() {
        let mut b = good_backup();
        b.carts.clear();
        for pos in [-1, 0, 7, 8, 99] {
            b.carts.push(CartBackup {
                label: format!("pad {pos}"),
                file_path: "C:/a.mp3".into(),
                position: pos,
            });
        }
        let bad: Vec<i32> = validate_backup(&b)
            .iter()
            .filter(|iss| iss.section == "carts")
            .map(|iss| b.carts[iss.index].position)
            .collect();
        assert_eq!(bad, vec![-1, 8, 99]);
    }

    #[test]
    fn validate_flags_ad_rows() {
        let mut b = good_backup();
        b.ads.push(AdBackup {
            name: "".into(),
            spot_path: "  ".into(),
            intro_path: None,
            outro_path: None,
            start_date: "2026-13-45".into(),
            end_date: "2026-01-01".into(),
            play_time: "9am".into(),
            days: "Daily".into(),
            enabled: true,
        });
        let issues: Vec<_> = validate_backup(&b)
            .into_iter()
            .filter(|iss| iss.section == "ads")
            .collect();
        assert_eq!(issues.len(), 4);
        let reasons: Vec<_> = issues.iter().map(|iss| iss.reason.as_str()).collect();
        assert!(reasons.iter().any(|r| r.contains("name is empty")));
        assert!(reasons.iter().any(|r| r.contains("spot audio is required")));
        assert!(reasons.iter().any(|r| r.contains("want HH:MM")));
        assert!(reasons.iter().any(|r| r.contains("bad start date")));
        // End-before-start is its own issue, not a parse failure.
        let mut b = good_backup();
        b.ads[0].start_date = "2026-12-31".into();
        b.ads[0].end_date = "2026-01-01".into();
        let issues = validate_backup(&b);
        assert_eq!(issues.len(), 1);
        assert!(issues[0].reason.contains("end date is before start date"));
    }

    #[test]
    fn backup_write_read_roundtrip() {
        let dir = std::env::temp_dir().join("crabboss-backup-test");
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("backup.json");
        let original = good_backup();
        write_backup(&path, &original).unwrap();
        let back = read_backup(&path).expect("written backup reads back");
        assert_eq!(
            serde_json::to_string(&back).unwrap(),
            serde_json::to_string(&original).unwrap()
        );
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn read_rejects_malformed_and_future_version() {
        let dir = std::env::temp_dir().join("crabboss-backup-bad");
        std::fs::create_dir_all(&dir).unwrap();
        let bad = dir.join("bad.json");
        std::fs::write(&bad, b"{not json").unwrap();
        let err = read_backup(&bad).expect_err("malformed rejects");
        assert!(err.contains("not a CrabBoss backup"), "{err}");
        let mut future = good_backup();
        future.version = BACKUP_VERSION + 1;
        let path = dir.join("future.json");
        write_backup(&path, &future).unwrap();
        let err = read_backup(&path).expect_err("future version rejects");
        assert!(err.contains("unsupported backup version"), "{err}");
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn write_failure_keeps_old_backup_file() {
        let dir = std::env::temp_dir().join("crabboss-backup-atomic");
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("backup.json");
        std::fs::write(&path, b"{\"version\":1}").unwrap();
        let bad_target = dir.join("no-such-dir").join("backup.json");
        assert!(write_backup(&bad_target, &good_backup()).is_err());
        assert_eq!(std::fs::read(&path).unwrap(), b"{\"version\":1}");
        // No temporary sibling left behind.
        let leftovers: Vec<_> = std::fs::read_dir(&dir)
            .unwrap()
            .map(|e| e.unwrap().file_name())
            .filter(|n| n.to_string_lossy() != "backup.json")
            .collect();
        assert!(leftovers.is_empty(), "temp file leaked: {leftovers:?}");
        std::fs::remove_dir_all(&dir).ok();
    }
}
