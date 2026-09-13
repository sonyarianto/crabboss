//! App settings persisted as JSON next to the database.
//!
//! Audio prefs (output device, crossfade, silence threshold) survive
//! restarts. Missing or corrupt files fall back to defaults.

use std::path::Path;

use serde::{Deserialize, Serialize};

use crate::audio::MicConfig;
use crate::audio::EQ_BAND_COUNT;
use crate::audio::{TARGET_LUFS, TARGET_MAX_LUFS, TARGET_MIN_LUFS};
use crate::stream::StreamConfig;

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
        }
    }
}

impl AppSettings {
    pub fn load(path: &Path) -> Self {
        let mut s: AppSettings = std::fs::read_to_string(path)
            .ok()
            .and_then(|t| serde_json::from_str(&t).ok())
            .unwrap_or_default();
        if s.station_name.trim().is_empty() {
            s.station_name = "CrabBoss FM".into();
        }
        s.crossfade_secs = s.crossfade_secs.clamp(0.0, 30.0);
        s.silence_threshold_secs = s.silence_threshold_secs.clamp(1.0, 120.0);
        for g in &mut s.eq_gains_db {
            *g = g.clamp(-12.0, 12.0);
        }
        s.limiter_ceiling = s.limiter_ceiling.clamp(0.1, 1.0);
        s.loudness_target_lufs = s
            .loudness_target_lufs
            .clamp(TARGET_MIN_LUFS, TARGET_MAX_LUFS);
        s.mic = std::mem::take(&mut s.mic).sanitized();
        s
    }

    pub fn save(&self, path: &Path) -> std::io::Result<()> {
        std::fs::write(path, serde_json::to_string_pretty(self).unwrap_or_default())
    }
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
        };
        s.save(&path).unwrap();
        let back = AppSettings::load(&path);
        assert_eq!(back.output_device.as_deref(), Some("Speakers"));
        assert_eq!(back.station_name, "Test FM");
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
        std::fs::remove_file(&path).ok();
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
        let back = AppSettings::load(&path);
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
        let back = AppSettings::load(&bad);
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
        let d = AppSettings::load(&missing);
        assert_eq!(d.crossfade_secs, 3.0);
        assert_eq!(d.station_name, "CrabBoss FM");
        let blank = dir.join("crabboss-settings-blank-name.json");
        std::fs::write(&blank, br#"{"station_name":"   "}"#).unwrap();
        assert_eq!(AppSettings::load(&blank).station_name, "CrabBoss FM");
        std::fs::remove_file(&blank).ok();
        let bad = dir.join("crabboss-settings-bad.json");
        std::fs::write(&bad, b"{not json").unwrap();
        let d = AppSettings::load(&bad);
        assert!(d.output_device.is_none());
        let clamped = dir.join("crabboss-settings-clamp.json");
        std::fs::write(
            &clamped,
            br#"{"output_device":null,"crossfade_secs":99.0,"silence_threshold_secs":0.1}"#,
        )
        .unwrap();
        let d = AppSettings::load(&clamped);
        assert_eq!((d.crossfade_secs, d.silence_threshold_secs), (30.0, 1.0));
        let eq_clamp = dir.join("crabboss-settings-eq.json");
        std::fs::write(
            &eq_clamp,
            br#"{"eq_gains_db":[99,-99,0,0,0,0,0,0,0,0,0,0],"limiter_ceiling":5.0,"loudness_target_lufs":-99.0}"#,
        )
        .unwrap();
        let d = AppSettings::load(&eq_clamp);
        assert_eq!((d.eq_gains_db[0], d.eq_gains_db[1]), (12.0, -12.0));
        assert!((d.limiter_ceiling - 1.0).abs() < 1e-6);
        assert!((d.loudness_target_lufs - TARGET_MIN_LUFS).abs() < 1e-6);
        std::fs::remove_file(&bad).ok();
        std::fs::remove_file(&clamped).ok();
        std::fs::remove_file(&eq_clamp).ok();
    }
}
