//! Icecast/Shoutcast streaming output (ROADMAP §1.5).
//!
//! The cpal mix bus (post-volume, exactly what the program feed plays)
//! is tapped into a [`StreamManager`], which encodes it to MP3 (LAME,
//! constant bitrate) and pushes it to an Icecast server as a source
//! client — `PUT` protocol with legacy `SOURCE` fallback, paced at
//! real time as the Icecast spec requires.

mod encoder;
mod manager;
mod source;

pub use manager::{StreamManager, StreamTap};
#[allow(unused_imports)]
pub use source::IcecastSource;

use serde::{Deserialize, Serialize};

/// Encoder/container for the stream (MP3 via LAME for now).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum StreamFormat {
    #[default]
    Mp3,
}

impl StreamFormat {
    /// MIME type sent to the server.
    pub fn content_type(self) -> &'static str {
        match self {
            StreamFormat::Mp3 => "audio/mpeg",
        }
    }
}

/// Stream server + mount configuration (persisted in settings.json).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct StreamConfig {
    /// Master switch: manager runs when true.
    pub enabled: bool,
    pub host: String,
    pub port: u16,
    /// Mount path, normalized to start with `/` (e.g. `/stream`).
    pub mount: String,
    /// Source username (Icecast default: `source`).
    pub username: String,
    pub password: String,
    /// Ice-Name shown in directories/players.
    pub name: String,
    pub genre: String,
    pub description: String,
    /// Advertise in YP directories.
    pub public: bool,
    /// Constant bitrate in kbps (encoder picks the nearest supported).
    pub bitrate_kbps: u32,
    pub format: StreamFormat,
}

impl Default for StreamConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            host: "127.0.0.1".into(),
            port: 8000,
            mount: "/stream".into(),
            username: "source".into(),
            password: String::new(),
            name: "CrabBoss FM".into(),
            genre: String::new(),
            description: String::new(),
            public: false,
            bitrate_kbps: 128,
            format: StreamFormat::Mp3,
        }
    }
}

impl StreamConfig {
    /// Normalize user input: trim, default mount, validate ranges.
    pub fn sanitized(mut self) -> Self {
        self.host = self.host.trim().to_string();
        if self.host.is_empty() {
            self.host = "127.0.0.1".into();
        }
        self.mount = {
            let m = self.mount.trim().to_string();
            if m.is_empty() {
                "/stream".to_string()
            } else if m.starts_with('/') {
                m
            } else {
                format!("/{m}")
            }
        };
        if self.username.trim().is_empty() {
            self.username = "source".into();
        }
        if !(1..=320).contains(&self.bitrate_kbps) {
            self.bitrate_kbps = 128;
        }
        self
    }
}

/// Live state of the streaming pipeline (surfaced in Settings).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum StreamState {
    /// Master switch off / no manager installed.
    Off,
    /// Trying to connect (or reconnect) to the server.
    Connecting,
    /// Connected and sending audio.
    Live,
    /// Last connection attempt failed; retrying.
    Error(String),
}

impl StreamState {
    /// Short one-line status for the Settings UI.
    pub fn label(&self) -> String {
        match self {
            StreamState::Off => "⏸ Off".into(),
            StreamState::Connecting => "⏳ Connecting…".into(),
            StreamState::Live => "🔴 Live".into(),
            StreamState::Error(e) => format!("⚠ {e}"),
        }
    }

    /// True when audio is actually flowing.
    pub fn is_live(&self) -> bool {
        matches!(self, StreamState::Live)
    }
}

/// Counters since the manager started running.
#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct StreamStats {
    /// Encoded bytes handed to the server.
    pub bytes_sent: u64,
    /// Seconds of audio encoded+sent since this connection started.
    pub stream_secs: u64,
    /// Successful reconnects after drops.
    pub reconnects: u32,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sanitize_normalizes_mount_and_fields() {
        let cfg = StreamConfig {
            host: " radio.example.com ".into(),
            mount: "live.mp3".into(),
            username: "  ".into(),
            bitrate_kbps: 9999,
            ..Default::default()
        }
        .sanitized();
        assert_eq!(cfg.host, "radio.example.com");
        assert_eq!(cfg.mount, "/live.mp3");
        assert_eq!(cfg.username, "source");
        assert_eq!(cfg.bitrate_kbps, 128);

        let cfg = StreamConfig {
            mount: "".into(),
            bitrate_kbps: 0,
            ..Default::default()
        }
        .sanitized();
        assert_eq!(cfg.mount, "/stream");
        assert_eq!(cfg.bitrate_kbps, 128);
    }

    #[test]
    fn config_roundtrips_through_json() {
        let cfg = StreamConfig {
            enabled: true,
            host: "cast.example.com".into(),
            port: 8443,
            mount: "/live".into(),
            password: "pw".into(),
            public: true,
            bitrate_kbps: 192,
            ..Default::default()
        };
        let json = serde_json::to_string(&cfg).unwrap();
        let back: StreamConfig = serde_json::from_str(&json).unwrap();
        assert_eq!(back, cfg);
        // Missing fields fall back to defaults (serde(default)).
        let partial: StreamConfig = serde_json::from_str(r#"{"host":"x"}"#).unwrap();
        assert_eq!(partial.port, 8000);
        assert!(!partial.enabled);
    }

    #[test]
    fn content_type_for_mp3() {
        assert_eq!(StreamFormat::Mp3.content_type(), "audio/mpeg");
    }
}
