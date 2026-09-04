//! App settings persisted as JSON next to the database.
//!
//! Audio prefs (output device, crossfade, silence threshold) survive
//! restarts. Missing or corrupt files fall back to defaults.

use std::path::Path;

use serde::{Deserialize, Serialize};

/// Persisted preferences. Device applies on next launch (stream rebuild);
/// crossfade + silence threshold also apply live.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct AppSettings {
    /// Output device name (`None` = system default).
    pub output_device: Option<String>,
    pub crossfade_secs: f32,
    pub silence_threshold_secs: f32,
    /// Auto-DJ continuity: keep the music going without a DJ.
    pub autodj: bool,
}

impl Default for AppSettings {
    fn default() -> Self {
        Self {
            output_device: None,
            crossfade_secs: 3.0,
            silence_threshold_secs: 10.0,
            autodj: true,
        }
    }
}

impl AppSettings {
    pub fn load(path: &Path) -> Self {
        let mut s: AppSettings = std::fs::read_to_string(path)
            .ok()
            .and_then(|t| serde_json::from_str(&t).ok())
            .unwrap_or_default();
        s.crossfade_secs = s.crossfade_secs.clamp(0.0, 30.0);
        s.silence_threshold_secs = s.silence_threshold_secs.clamp(1.0, 120.0);
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
            crossfade_secs: 5.5,
            silence_threshold_secs: 8.0,
            autodj: false,
        };
        s.save(&path).unwrap();
        let back = AppSettings::load(&path);
        assert_eq!(back.output_device.as_deref(), Some("Speakers"));
        assert_eq!(
            (back.crossfade_secs, back.silence_threshold_secs),
            (5.5, 8.0)
        );
        assert!(!back.autodj);
        std::fs::remove_file(&path).ok();
    }

    #[test]
    fn corrupt_or_missing_falls_back() {
        let dir = std::env::temp_dir();
        let missing = dir.join("crabboss-settings-nope.json");
        std::fs::remove_file(&missing).ok();
        let d = AppSettings::load(&missing);
        assert_eq!(d.crossfade_secs, 3.0);
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
        std::fs::remove_file(&bad).ok();
        std::fs::remove_file(&clamped).ok();
    }
}
