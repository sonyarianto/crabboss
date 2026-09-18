//! App settings persisted as JSON next to the database.
//!
//! Audio prefs (output device, crossfade, silence threshold) survive
//! restarts. Loads distinguish a first run (no file) from corrupt or
//! unreadable files; saves go through a temporary sibling file and an
//! atomic replace so a crash can never leave a truncated settings.json.

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::audio::CueConfig;
use crate::audio::MicConfig;
use crate::audio::EQ_BAND_COUNT;
use crate::audio::{TARGET_LUFS, TARGET_MAX_LUFS, TARGET_MIN_LUFS};
use crate::stream::StreamConfig;

/// Bounds for the folder auto-sync interval (minutes). Five minutes is
/// the floor: a library walk is cheap but not 200 ms-tick cheap, and
/// anything tighter is a busy loop with extra steps.
pub const AUTO_SYNC_MIN_MINUTES: u32 = 5;
/// A daily pass is the coarsest useful cadence for a station library.
pub const AUTO_SYNC_MAX_MINUTES: u32 = 1440;
/// Hourly passes catch drop-folder workflows without churning the disk.
pub const AUTO_SYNC_DEFAULT_MINUTES: u32 = 60;

/// Persisted preferences. Device applies on next launch (stream rebuild);
/// crossfade + silence threshold also apply live.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct AppSettings {
    /// Output device name (`None` = system default).
    pub output_device: Option<String>,
    /// Station display name (dashboard header + top bar).
    pub station_name: String,
    pub crossfade_secs: f32,
    pub silence_threshold_secs: f32,
    /// Auto-DJ continuity: keep the music going without a DJ.
    pub autodj: bool,
    /// Program EQ insert on/off (cpal engine only).
    pub eq_enabled: bool,
    /// Per-band EQ gains in dB (±12), index order = `EQ_CENTER_HZ`.
    pub eq_gains_db: [f32; EQ_BAND_COUNT],
    /// Limiter ceiling (linear amplitude, 0.1..1.0; ~0 dBFS default).
    pub limiter_ceiling: f32,
    /// ReplayGain-style loudness normalization (per-track gain toward an
    /// adjustable LUFS target, applied at decode time from library analysis).
    pub loudness_norm: bool,
    /// Normalization target in LUFS ([`TARGET_MIN_LUFS`]…[`TARGET_MAX_LUFS`],
    /// default [`TARGET_LUFS`], RadioBOSS-style). Changing it rewrites the
    /// stored per-track gains (no re-analysis needed); LUFS values are kept.
    pub loudness_target_lufs: f32,
    /// Icecast/Shoutcast streaming (§1.5): server, mount, encoder.
    pub stream: StreamConfig,
    /// Mic/line-in with ducking (§1.6): device, level, duck prefs.
    pub mic: MicConfig,
    /// Cue (PFL) audition output (B2 Phase 1: config only, no audio yet):
    /// private headphone bus that never feeds the stream. `None` device =
    /// cue unavailable. Old configs without this field load as unavailable
    /// via `#[serde(default)]`.
    pub cue: CueConfig,
    /// Folders re-scanned for new audio on a timer (§1.8 auto-sync).
    /// Empty = manual import only. Old configs without this field load
    /// as empty via `#[serde(default)]`.
    pub watch_folders: Vec<PathBuf>,
    /// Timer re-scan of `watch_folders`: new files queue through the
    /// normal import pump with progress; never deletes anything.
    pub auto_sync_enabled: bool,
    /// Minutes between auto-sync passes ([`AUTO_SYNC_MIN_MINUTES`]..
    /// [`AUTO_SYNC_MAX_MINUTES`]).
    pub auto_sync_interval_mins: u32,
}

impl Default for AppSettings {
    fn default() -> Self {
        Self {
            output_device: None,
            station_name: "CrabBoss FM".into(),
            crossfade_secs: 3.0,
            silence_threshold_secs: 10.0,
            autodj: true,
            eq_enabled: false,
            eq_gains_db: [0.0; EQ_BAND_COUNT],
            limiter_ceiling: 0.99,
            loudness_norm: true,
            loudness_target_lufs: TARGET_LUFS,
            stream: StreamConfig::default(),
            mic: MicConfig::default(),
            cue: CueConfig::default(),
            watch_folders: Vec::new(),
            auto_sync_enabled: false,
            auto_sync_interval_mins: AUTO_SYNC_DEFAULT_MINUTES,
        }
    }
}

impl AppSettings {
    /// Clamp/normalize deserialized values into their legal ranges.
    /// Behavior-preserving by design: every caller that used to get a
    /// `load`ed value must call this (or `load`) instead of hand-rolling
    /// its own clamps.
    pub fn sanitized(mut self) -> Self {
        if self.station_name.trim().is_empty() {
            self.station_name = "CrabBoss FM".into();
        }
        self.crossfade_secs = self.crossfade_secs.clamp(0.0, 30.0);
        self.silence_threshold_secs = self.silence_threshold_secs.clamp(1.0, 120.0);
        for g in &mut self.eq_gains_db {
            *g = g.clamp(-12.0, 12.0);
        }
        self.limiter_ceiling = self.limiter_ceiling.clamp(0.1, 1.0);
        self.loudness_target_lufs = self
            .loudness_target_lufs
            .clamp(TARGET_MIN_LUFS, TARGET_MAX_LUFS);
        self.auto_sync_interval_mins = self
            .auto_sync_interval_mins
            .clamp(AUTO_SYNC_MIN_MINUTES, AUTO_SYNC_MAX_MINUTES);
        // Drop empty entries and dedupe, keeping operator order. Pure
        // (no filesystem checks): a temporarily unplugged drive must not
        // silently lose its watch entry.
        let mut seen = std::collections::HashSet::new();
        self.watch_folders.retain(|p| {
            let s = p.to_string_lossy();
            !s.trim().is_empty() && seen.insert(s.into_owned())
        });
        self.mic = std::mem::take(&mut self.mic).sanitized();
        self.cue = std::mem::take(&mut self.cue).sanitized();
        self
    }

    /// Load settings, telling apart a first run (no file yet) from a
    /// corrupt or unreadable file. Never touches the file on disk: an
    /// invalid file is reported, not replaced.
    pub fn load(path: &Path) -> SettingsLoad {
        let text = match std::fs::read_to_string(path) {
            Ok(t) => t,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                return SettingsLoad::Missing(AppSettings::default());
            }
            Err(e) => {
                return SettingsLoad::IoError {
                    defaults: AppSettings::default(),
                    error: format!("cannot read {}: {e}", path.display()),
                };
            }
        };
        match serde_json::from_str::<AppSettings>(&text) {
            Ok(s) => SettingsLoad::Loaded(s.sanitized()),
            Err(e) => SettingsLoad::Invalid {
                defaults: AppSettings::default(),
                error: format!("invalid JSON in {}: {e}", path.display()),
            },
        }
    }

    /// Save atomically: serialize first (serialization errors are
    /// returned, never silently replaced with `{}`), write a temporary
    /// sibling file, `sync_all`, then replace the target. `tempfile`'s
    /// persist performs a platform-correct replace (including Windows,
    /// where plain rename refuses to overwrite); on any failure the
    /// previous target is left untouched and the temp file is removed.
    pub fn save(&self, path: &Path) -> std::io::Result<()> {
        let text = serde_json::to_string_pretty(self)
            .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))?;
        atomic_write(path, text.as_bytes())
    }
}

/// How an [`AppSettings`] load went. Every variant carries runnable
/// settings; the shape tells the caller whether the file was fine.
#[derive(Debug)]
pub enum SettingsLoad {
    /// No file on disk: first run. Defaults, no warning.
    Missing(AppSettings),
    /// Parsed and sanitized.
    Loaded(AppSettings),
    /// JSON malformed: defaults plus the parse error. The file is left
    /// untouched so it can be inspected or repaired by hand.
    Invalid {
        defaults: AppSettings,
        error: String,
    },
    /// Read failed (permissions, …): defaults plus the I/O error.
    IoError {
        defaults: AppSettings,
        error: String,
    },
}

impl SettingsLoad {
    /// Settings to run with, whatever happened on disk.
    pub fn settings(self) -> AppSettings {
        match self {
            SettingsLoad::Missing(s)
            | SettingsLoad::Loaded(s)
            | SettingsLoad::Invalid { defaults: s, .. }
            | SettingsLoad::IoError { defaults: s, .. } => s,
        }
    }

    /// Operator-facing warning for anything but a clean load.
    /// `None` for first-run `Missing` and clean `Loaded`.
    pub fn warning(&self) -> Option<String> {
        match self {
            SettingsLoad::Missing(_) | SettingsLoad::Loaded(_) => None,
            SettingsLoad::Invalid { error, .. } | SettingsLoad::IoError { error, .. } => {
                Some(error.clone())
            }
        }
    }

    /// True when the on-disk file could not be trusted (invalid or
    /// unreadable): the next save must quarantine it aside first instead
    /// of replacing it blindly.
    pub fn needs_quarantine(&self) -> bool {
        matches!(
            self,
            SettingsLoad::Invalid { .. } | SettingsLoad::IoError { .. }
        )
    }
}

/// Move an untrusted settings file aside (`settings.json` →
/// `settings.json.corrupt-20260914-153000`) so a later save cannot
/// silently destroy evidence. Returns the backup path, or `None` when
/// there was nothing to preserve. Fails without touching anything when
/// the rename itself fails.
pub fn quarantine_existing(path: &Path) -> std::io::Result<Option<PathBuf>> {
    if std::fs::metadata(path).is_err() {
        return Ok(None);
    }
    let stamp = chrono::Local::now().format("%Y%m%d-%H%M%S");
    let mut backup = path.as_os_str().to_owned();
    backup.push(format!(".corrupt-{stamp}"));
    let backup = PathBuf::from(backup);
    std::fs::rename(path, &backup)?;
    Ok(Some(backup))
}

/// Write `bytes` to `path` atomically via a temporary sibling file
/// (`sync_all`, then a platform-correct replace). Shared primitive for
/// everything that persists station data (settings, backups): on any
/// failure the previous target is left untouched and the temp file is
/// removed.
pub fn atomic_write(path: &Path, bytes: &[u8]) -> std::io::Result<()> {
    let parent = path
        .parent()
        .filter(|p| !p.as_os_str().is_empty())
        .ok_or_else(|| {
            std::io::Error::new(
                std::io::ErrorKind::InvalidInput,
                format!("no parent directory for {}", path.display()),
            )
        })?;
    let mut tmp = tempfile::NamedTempFile::new_in(parent)?;
    use std::io::Write;
    tmp.write_all(bytes)?;
    tmp.as_file().sync_all()?;
    tmp.persist(path).map_err(|e| e.error)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn roundtrip() {
        let dir = std::env::temp_dir();
        let path = dir.join("crabboss-settings-test.json");
        let s = AppSettings {
            output_device: Some("Speakers".into()),
            station_name: "Test FM".into(),
            crossfade_secs: 5.5,
            silence_threshold_secs: 8.0,
            autodj: false,
            eq_enabled: true,
            eq_gains_db: [0.0, 1.5, 3.0, 0.0, 0.0, 0.0, -2.0, 0.0, 0.0, 0.0, 4.0, 0.0],
            limiter_ceiling: 0.9,
            loudness_norm: false,
            loudness_target_lufs: -14.0,
            stream: StreamConfig {
                enabled: true,
                host: "cast.example.com".into(),
                port: 8443,
                mount: "/live".into(),
                password: "pw".into(),
                bitrate_kbps: 192,
                ..Default::default()
            },
            mic: MicConfig::default(),
            cue: CueConfig {
                device: Some("Headphones".into()),
                volume: 0.7,
            },
            watch_folders: vec![PathBuf::from("D:/mix")],
            auto_sync_enabled: true,
            auto_sync_interval_mins: 30,
        };
        s.save(&path).unwrap();
        let back = match AppSettings::load(&path) {
            SettingsLoad::Loaded(s) => s,
            other => panic!("expected Loaded, got {other:?}"),
        };
        assert_eq!(back.output_device.as_deref(), Some("Speakers"));
        assert_eq!(back.station_name, "Test FM");
        assert_eq!(back.watch_folders, vec![PathBuf::from("D:/mix")]);
        assert!(back.auto_sync_enabled);
        assert_eq!(back.auto_sync_interval_mins, 30);
        assert_eq!(
            (back.crossfade_secs, back.silence_threshold_secs),
            (5.5, 8.0)
        );
        assert!(!back.autodj);
        assert!(back.eq_enabled);
        assert_eq!(back.eq_gains_db[6], -2.0);
        assert_eq!(back.eq_gains_db[10], 4.0);
        assert!((back.limiter_ceiling - 0.9).abs() < 1e-6);
        assert!(!back.loudness_norm);
        assert!((back.loudness_target_lufs + 14.0).abs() < 1e-6);
        assert!(back.stream.enabled);
        assert_eq!(back.stream.host, "cast.example.com");
        assert_eq!(back.stream.port, 8443);
        assert_eq!(back.stream.mount, "/live");
        assert_eq!(back.stream.bitrate_kbps, 192);
        assert_eq!(back.cue.device.as_deref(), Some("Headphones"));
        assert!((back.cue.volume - 0.7).abs() < 1e-6);
        std::fs::remove_file(&path).ok();
    }

    #[test]
    fn cue_roundtrip_and_clamp() {
        let dir = std::env::temp_dir();
        let path = dir.join("crabboss-settings-cue.json");
        let s = AppSettings {
            cue: CueConfig {
                device: Some("Realtek".into()),
                volume: 0.7,
            },
            ..Default::default()
        };
        s.save(&path).unwrap();
        let back = AppSettings::load(&path).settings();
        assert_eq!(back.cue.device.as_deref(), Some("Realtek"));
        assert!((back.cue.volume - 0.7).abs() < 1e-6);
        std::fs::remove_file(&path).ok();
        // Old configs without "cue" load as unavailable; out-of-range
        // volume clamps on load.
        let old = dir.join("crabboss-settings-cue-old.json");
        std::fs::write(&old, br#"{"station_name":"Old"}"#).unwrap();
        let back = AppSettings::load(&old).settings();
        assert!(back.cue.device.is_none());
        assert!((back.cue.volume - 0.8).abs() < 1e-6);
        let bad = dir.join("crabboss-settings-cue-bad.json");
        std::fs::write(&bad, br#"{"cue":{"volume":9.0}}"#).unwrap();
        let back = AppSettings::load(&bad).settings();
        assert_eq!(back.cue.volume, 1.5);
        std::fs::remove_file(&old).ok();
        std::fs::remove_file(&bad).ok();
    }

    #[test]
    fn mic_roundtrip_and_clamp() {
        let dir = std::env::temp_dir();
        let path = dir.join("crabboss-settings-mic.json");
        let s = AppSettings {
            mic: MicConfig {
                enabled: true,
                device: Some("USB Mic".into()),
                level: 0.8,
                duck_enabled: true,
                duck_threshold_db: -24.0,
                duck_depth_db: 9.0,
                attack_ms: 8.0,
                release_ms: 300.0,
            },
            ..Default::default()
        };
        s.save(&path).unwrap();
        let back = AppSettings::load(&path).settings();
        assert!(back.mic.enabled);
        assert_eq!(back.mic.device.as_deref(), Some("USB Mic"));
        assert!((back.mic.level - 0.8).abs() < 1e-6);
        assert!((back.mic.duck_threshold_db + 24.0).abs() < 1e-6);
        assert!((back.mic.duck_depth_db - 9.0).abs() < 1e-6);
        std::fs::remove_file(&path).ok();
        // Out-of-range values clamp on load.
        let bad = dir.join("crabboss-settings-mic-bad.json");
        std::fs::write(
            &bad,
            br#"{"mic":{"level":9.0,"duck_threshold_db":5.0,"duck_depth_db":99.0,"attack_ms":0.0,"release_ms":99999.0}}"#,
        )
        .unwrap();
        let back = AppSettings::load(&bad).settings();
        assert_eq!(back.mic.level, 1.5);
        assert_eq!(back.mic.duck_threshold_db, 0.0);
        assert_eq!(back.mic.duck_depth_db, 24.0);
        assert_eq!(back.mic.attack_ms, 1.0);
        assert_eq!(back.mic.release_ms, 3000.0);
        std::fs::remove_file(&bad).ok();
    }

    #[test]
    fn corrupt_or_missing_falls_back() {
        let dir = std::env::temp_dir();
        let missing = dir.join("crabboss-settings-nope.json");
        std::fs::remove_file(&missing).ok();
        let load = AppSettings::load(&missing);
        assert!(matches!(load, SettingsLoad::Missing(_)));
        assert!(load.warning().is_none(), "first run stays quiet");
        assert!(!load.needs_quarantine());
        let d = AppSettings::load(&missing).settings();
        assert_eq!(d.crossfade_secs, 3.0);
        assert_eq!(d.station_name, "CrabBoss FM");
        let blank = dir.join("crabboss-settings-blank-name.json");
        std::fs::write(&blank, br#"{"station_name":"   "}"#).unwrap();
        let load = AppSettings::load(&blank);
        assert!(matches!(load, SettingsLoad::Loaded(_)));
        assert_eq!(load.settings().station_name, "CrabBoss FM");
        std::fs::remove_file(&blank).ok();
        let bad = dir.join("crabboss-settings-bad.json");
        std::fs::write(&bad, b"{not json").unwrap();
        let load = AppSettings::load(&bad);
        assert!(matches!(load, SettingsLoad::Invalid { .. }));
        let warn = load.warning().expect("invalid file warns");
        assert!(warn.contains("invalid JSON"), "actionable: {warn}");
        assert!(load.needs_quarantine());
        let d = AppSettings::load(&bad).settings();
        assert!(d.output_device.is_none());
        let clamped = dir.join("crabboss-settings-clamp.json");
        std::fs::write(
            &clamped,
            br#"{"output_device":null,"crossfade_secs":99.0,"silence_threshold_secs":0.1}"#,
        )
        .unwrap();
        let d = AppSettings::load(&clamped).settings();
        assert_eq!((d.crossfade_secs, d.silence_threshold_secs), (30.0, 1.0));
        let eq_clamp = dir.join("crabboss-settings-eq.json");
        std::fs::write(
            &eq_clamp,
            br#"{"eq_gains_db":[99,-99,0,0,0,0,0,0,0,0,0,0],"limiter_ceiling":5.0,"loudness_target_lufs":-99.0}"#,
        )
        .unwrap();
        let d = AppSettings::load(&eq_clamp).settings();
        assert_eq!((d.eq_gains_db[0], d.eq_gains_db[1]), (12.0, -12.0));
        assert!((d.limiter_ceiling - 1.0).abs() < 1e-6);
        assert!((d.loudness_target_lufs - TARGET_MIN_LUFS).abs() < 1e-6);
        std::fs::remove_file(&bad).ok();
        std::fs::remove_file(&clamped).ok();
        std::fs::remove_file(&eq_clamp).ok();
    }

    #[test]
    fn example_settings_loads_with_documented_defaults() {
        // Guards settings.example.json against drift: it must parse and
        // match the documented defaults.
        let path =
            std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../settings.example.json");
        let s = AppSettings::load(&path).settings();
        assert_eq!(s.station_name, "CrabBoss FM");
        assert!((s.crossfade_secs - 3.0).abs() < 1e-6);
        assert!(!s.stream.enabled);
        assert_eq!(s.stream.port, 8000);
        assert_eq!(s.stream.mount, "/stream");
        assert!(!s.stream.tls);
        assert!((s.loudness_target_lufs + 9.0).abs() < 1e-6);
        assert!(!s.mic.enabled);
    }

    #[test]
    fn autosync_defaults_clamps_and_dedupes() {
        let parse = |json: &str| {
            serde_json::from_str::<AppSettings>(json)
                .expect("test JSON parses")
                .sanitized()
        };
        // Old configs without the auto-sync fields load as manual-only.
        let d = parse(r#"{"station_name":"Old"}"#);
        assert!(d.watch_folders.is_empty());
        assert!(!d.auto_sync_enabled);
        assert_eq!(d.auto_sync_interval_mins, AUTO_SYNC_DEFAULT_MINUTES);
        // Interval clamps to its bounds.
        assert_eq!(
            parse(r#"{"auto_sync_interval_mins":1}"#).auto_sync_interval_mins,
            AUTO_SYNC_MIN_MINUTES
        );
        assert_eq!(
            parse(r#"{"auto_sync_interval_mins":99999}"#).auto_sync_interval_mins,
            AUTO_SYNC_MAX_MINUTES
        );
        // Empty entries drop, dupes collapse, order kept.
        let folders = parse(r#"{"watch_folders":["D:/mix","","D:/mix","E:/jingles"]}"#);
        let names: Vec<_> = folders
            .watch_folders
            .iter()
            .map(|p| p.to_string_lossy().into_owned())
            .collect();
        assert_eq!(names, vec!["D:/mix", "E:/jingles"]);
    }

    #[test]
    fn unreadable_path_is_io_error_not_missing() {
        // A directory reads as an I/O error on every platform, never as
        // "first run": the operator must hear about it.
        let dir = std::env::temp_dir().join("crabboss-settings-is-dir.json");
        std::fs::create_dir_all(&dir).ok();
        let load = AppSettings::load(&dir);
        assert!(matches!(load, SettingsLoad::IoError { .. }));
        let warn = load.warning().expect("I/O failure warns");
        assert!(warn.contains("cannot read"), "actionable: {warn}");
        assert!(load.needs_quarantine());
        std::fs::remove_dir(&dir).ok();
    }

    #[test]
    fn save_failure_keeps_old_file_and_leaves_no_temp() {
        let dir = std::env::temp_dir().join("crabboss-settings-atomic");
        std::fs::create_dir_all(&dir).unwrap();
        let before: Vec<_> = std::fs::read_dir(&dir)
            .unwrap()
            .map(|e| e.unwrap().file_name())
            .collect();
        let path = dir.join("settings.json");
        std::fs::write(&path, r#"{"station_name":"Keep Me"}"#).unwrap();
        // Saving into a nonexistent subdirectory must fail…
        let bad_target = dir.join("no-such-dir").join("settings.json");
        let s = AppSettings::default();
        assert!(s.save(&bad_target).is_err());
        // …while the previous file stays byte-identical…
        assert_eq!(
            std::fs::read_to_string(&path).unwrap(),
            r#"{"station_name":"Keep Me"}"#
        );
        // …and no temporary sibling is left behind.
        let after: Vec<_> = std::fs::read_dir(&dir)
            .unwrap()
            .map(|e| e.unwrap().file_name())
            .collect();
        let mut after_names: Vec<_> = after
            .iter()
            .map(|n| n.to_string_lossy().into_owned())
            .collect();
        after_names.retain(|n| n != "settings.json");
        let mut before_names: Vec<_> = before
            .iter()
            .map(|n| n.to_string_lossy().into_owned())
            .collect();
        before_names.retain(|n| n != "settings.json");
        assert_eq!(after_names, before_names, "temp file leaked");
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn save_over_existing_file_replaces_cleanly() {
        // The Windows-relevant path: temp sibling + replace over an
        // existing target. The result must parse and carry the new values.
        let dir = std::env::temp_dir().join("crabboss-settings-replace");
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("settings.json");
        std::fs::write(&path, r#"{"station_name":"Old"}"#).unwrap();
        let s = AppSettings {
            station_name: "New FM".into(),
            ..Default::default()
        };
        s.save(&path).unwrap();
        match AppSettings::load(&path) {
            SettingsLoad::Loaded(back) => assert_eq!(back.station_name, "New FM"),
            other => panic!("expected Loaded, got {other:?}"),
        }
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn quarantine_moves_untrusted_file_aside() {
        let dir = std::env::temp_dir().join("crabboss-settings-quar");
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("settings.json");
        std::fs::write(&path, b"{not json").unwrap();
        let backup = quarantine_existing(&path)
            .expect("quarantine works")
            .expect("something was preserved");
        assert!(!path.exists(), "original is out of the way");
        assert_eq!(std::fs::read(&backup).unwrap(), b"{not json");
        assert!(backup
            .file_name()
            .unwrap()
            .to_string_lossy()
            .starts_with("settings.json.corrupt-"));
        // Nothing on disk: nothing to preserve, still Ok.
        assert!(quarantine_existing(&path).unwrap().is_none());
        std::fs::remove_dir_all(&dir).ok();
    }
}
