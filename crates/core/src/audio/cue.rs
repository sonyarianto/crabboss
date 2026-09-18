//! Cue (PFL) output: private audition bus for the operator's headphones.
//!
//! Phase 1 only: persisted [`CueConfig`] + live [`CueState`]. No audio
//! callback is touched yet — `CpalEngine` keeps the trait defaults
//! (cue unavailable) until Phase 2 opens the second output stream.
//!
//! Design (agreed B2): program (LG monitor / stream) and cue (laptop
//! Realtek / headphones) are independent `cpal::Stream`s. The cue bus
//! never feeds the stream tap, the mic ducker, the silence monitor, or
//! `Library::record_play` — previewing must not pollute reports.

use serde::{Deserialize, Serialize};

/// Persisted cue preferences (lives in `AppSettings::cue`).
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct CueConfig {
    /// Cue output device name (`None` = no cue device selected →
    /// cue unavailable, UI shows a hint instead of failing).
    pub device: Option<String>,
    /// Cue monitor gain 0.0..1.5, independent from the program volume.
    /// The stream tap is pre-volume so this never affects broadcast.
    pub volume: f32,
}

impl Default for CueConfig {
    fn default() -> Self {
        Self {
            device: None,
            volume: 0.8,
        }
    }
}

impl CueConfig {
    /// Clamp user input to sane ranges.
    pub fn sanitized(mut self) -> Self {
        self.volume = self.volume.clamp(0.0, 1.5);
        self
    }
}

/// Live state of the cue pipeline (surfaced in Library/Settings).
#[derive(Debug, Clone, PartialEq)]
pub enum CueState {
    /// No cue device selected or cue engine not running (Phase 1 stub
    /// always reports this).
    Unavailable,
    /// Cue device ready, nothing previewing.
    Stopped,
    /// Previewing on the cue bus (program untouched).
    Playing,
    /// Last cue open/start failed; `String` carries the message.
    Error(String),
}

impl CueState {
    /// Short one-line status for the UI.
    pub fn label(&self) -> String {
        match self {
            CueState::Unavailable => "Cue: no device".into(),
            CueState::Stopped => "Cue: ready".into(),
            CueState::Playing => "Cue: playing".into(),
            CueState::Error(e) => format!("Cue error: {e}"),
        }
    }

    pub fn is_playing(&self) -> bool {
        matches!(self, CueState::Playing)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn defaults_are_quiet_and_unavailable() {
        let c = CueConfig::default();
        assert!(c.device.is_none());
        assert!((c.volume - 0.8).abs() < 1e-6);
        assert_eq!(CueState::Unavailable.label(), "Cue: no device");
        assert!(!CueState::Unavailable.is_playing());
        assert!(CueState::Playing.is_playing());
    }

    #[test]
    fn volume_clamps() {
        assert_eq!(CueConfig { device: None, volume: 9.0 }.sanitized().volume, 1.5);
        assert_eq!(CueConfig { device: None, volume: -2.0 }.sanitized().volume, 0.0);
    }

    #[test]
    fn old_json_without_cue_loads_as_default() {
        // Backward compat: settings.json from before B2 has no "cue" key.
        let c: CueConfig = serde_json::from_str("{}").expect("empty parses");
        assert!(c.device.is_none());
        // Missing volume also defaults (serde default on the struct).
        let v: CueConfig = serde_json::from_str(r#"{"device":"Realtek"}"#).expect("parses");
        assert_eq!(v.device.as_deref(), Some("Realtek"));
        assert!((v.volume - 0.8).abs() < 1e-6);
    }
}
