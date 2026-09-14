//! Station backup/restore: one versioned JSON file holding the settings
//! plus the SQLite-backed lists (scheduler, carts, ads).
//!
//! The core store types have no serde impls on purpose (SQLite is the
//! source of truth), so this module maps them to plain DTOs and applies
//! restores through the existing manager APIs (`create`/`delete`/…).
//! Validation rules therefore stay in exactly one place: the managers.
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
    std::fs::write(path, text)
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

    /// Replace settings + all three lists from a backup. Invalid rows are
    /// skipped with a warning (manager validation decides). Returns the
    /// user-facing status line.
    pub(crate) fn apply_backup(&mut self, backup: Backup) -> Result<String, String> {
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
        for e in &backup.scheduler {
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
        for c in &backup.carts {
            if !(0..WALL_SIZE as i32).contains(&c.position) {
                tracing::warn!("Restore skipped cart '{}': bad position", c.label);
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
        for a in &backup.ads {
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
            parts.push(format!("{skipped} invalid skipped (see log)"));
        }
        if device_changed {
            parts.push("restart to switch audio device".to_string());
        }
        Ok(format!("Restored: {}", parts.join(", ")))
    }
}
