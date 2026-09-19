//! Icecast/Shoutcast streaming output (ROADMAP §1.5).
//!
//! The cpal mix bus (post-DSP, pre-monitor-volume: exactly what the
//! program feed plays at full level) is tapped into a [`StreamManager`], which encodes it to MP3 (LAME)
//! or Opus (in Ogg, constant bitrate) and pushes it to the server as a
//! source client — Icecast `PUT`/`SOURCE` (mount-based) or Shoutcast
//! DNAS v1/v2 (`password` + `icy-*` headers), paced at real time as a
//! live feed requires.

mod encoder;
mod encoder_mp3;
mod encoder_opus;
mod listeners;
mod manager;
mod shoutcast;
mod source;
mod stereotool;

pub use listeners::{fetch_listener_count, parse_listener_count, LISTENER_POLL_SECS};
pub use manager::{StreamManager, StreamTap};
pub use shoutcast::ShoutcastSource;
#[allow(unused_imports)]
pub use source::IcecastSource;
pub use stereotool::{StereoTool, StereoToolConfig};

use serde::{Deserialize, Serialize};

use crate::error::Result;

/// Encoder/container for the stream (MP3 via LAME, Opus in Ogg).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum StreamFormat {
    #[default]
    Mp3,
    Opus,
}

impl StreamFormat {
    /// MIME type sent to the server.
    pub fn content_type(self) -> &'static str {
        match self {
            StreamFormat::Mp3 => "audio/mpeg",
            StreamFormat::Opus => "audio/ogg; codecs=opus",
        }
    }

    /// Short UI label.
    pub fn label(self) -> &'static str {
        match self {
            StreamFormat::Mp3 => "MP3",
            StreamFormat::Opus => "Opus",
        }
    }
}

/// Stream server + mount configuration (persisted in settings.json).
///
/// `Debug` is hand-written to redact `password`: never let the secret
/// near logs, panic messages, or test output. Serialization still
/// carries the real value (plaintext settings file by explicit owner
/// decision — no credential-store stage planned).
#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct StreamConfig {
    /// Master switch: manager runs when true.
    pub enabled: bool,
    /// Which server protocol to speak (Icecast 2, Shoutcast v1/v2).
    pub protocol: StreamProtocol,
    pub host: String,
    pub port: u16,
    /// Mount path, normalized to start with `/` (e.g. `/stream`).
    /// Icecast only — ignored by Shoutcast (DNAS has no mounts).
    pub mount: String,
    /// Source username (Icecast default: `source`).
    pub username: String,
    /// Source password, stored **plaintext** in the local settings.json
    /// by explicit owner decision (2026-09-14: no credential-store
    /// stage). Anyone who can read the settings file or a backup of it
    /// can impersonate this source — protect the data dir + backups
    /// with OS file permissions and do not share them.
    pub password: String,
    /// Wrap the connection in TLS (for servers behind HTTPS, e.g. port 443).
    /// Uses the OS-native TLS stack with SNI set to `host`.
    pub tls: bool,
    /// Ice-Name shown in directories/players.
    pub name: String,
    pub genre: String,
    pub description: String,
    /// Advertise in YP directories.
    pub public: bool,
    /// Constant bitrate in kbps (encoder picks the nearest supported).
    pub bitrate_kbps: u32,
    pub format: StreamFormat,
    /// Shoutcast v2 stream ID (`:#sid` selection; 1 = server default).
    /// Ignored by Icecast and Shoutcast v1.
    #[serde(default = "default_stream_sid")]
    pub sid: u32,
    /// Optional Thimeo Stereo Tool on-air processing (post-tap,
    /// pre-encoder). Off unless fully configured (see
    /// [`StereoToolConfig`]).
    pub stereotool: StereoToolConfig,
}

/// Default Shoutcast v2 stream ID (the DNAS default stream).
fn default_stream_sid() -> u32 {
    1
}

/// Which server protocol the source client speaks (ROADMAP §1.5).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum StreamProtocol {
    /// Icecast 2 (`PUT` with `SOURCE` fallback, mount-based).
    /// The default; the only protocol with Opus support.
    #[default]
    Icecast,
    /// Shoutcast DNAS 1.x (`password` + `icy-*` headers on the
    /// *source* port — usually listener `portbase + 1`). MP3 only.
    #[serde(rename = "shoutcast-v1")]
    ShoutcastV1,
    /// Shoutcast DNAS 2.x (v1-compatible flow on `portbase` plus
    /// `:#sid` stream selection). MP3 only.
    #[serde(rename = "shoutcast-v2")]
    ShoutcastV2,
}

impl StreamProtocol {
    /// Short UI label.
    pub fn label(self) -> &'static str {
        match self {
            StreamProtocol::Icecast => "Icecast",
            StreamProtocol::ShoutcastV1 => "Shoutcast v1",
            StreamProtocol::ShoutcastV2 => "Shoutcast v2",
        }
    }

    /// True for either Shoutcast variant (MP3-only, no mounts).
    pub fn is_shoutcast(self) -> bool {
        !matches!(self, StreamProtocol::Icecast)
    }
}

/// A connected source, either protocol: audio in (paced by the
/// caller), titles via each protocol's metadata path.
#[derive(Debug)]
pub enum SourceConn {
    Icecast(IcecastSource),
    Shoutcast(ShoutcastSource),
}

impl SourceConn {
    /// Connect + handshake per the config's protocol.
    pub fn connect(config: &StreamConfig) -> Result<Self> {
        if config.clone().sanitized().protocol.is_shoutcast() {
            ShoutcastSource::connect(config).map(SourceConn::Shoutcast)
        } else {
            IcecastSource::connect(config).map(|(s, _)| SourceConn::Icecast(s))
        }
    }

    /// Send encoded audio bytes (passthrough; the caller paces).
    pub fn send(&mut self, audio: &[u8]) -> Result<()> {
        match self {
            SourceConn::Icecast(s) => s.send(audio),
            SourceConn::Shoutcast(s) => s.send(audio),
        }
    }

    /// Push a now-playing title (best effort; failures warn).
    pub fn set_metadata(&mut self, title: &str) -> Result<()> {
        match self {
            SourceConn::Icecast(s) => s.set_metadata(title),
            SourceConn::Shoutcast(s) => s.set_metadata(title),
        }
    }
}

impl std::fmt::Debug for StreamConfig {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("StreamConfig")
            .field("enabled", &self.enabled)
            .field("host", &self.host)
            .field("port", &self.port)
            .field("mount", &self.mount)
            .field("username", &self.username)
            .field("password", &"<redacted>")
            .field("tls", &self.tls)
            .field("protocol", &self.protocol)
            .field("sid", &self.sid)
            .field("stereotool", &self.stereotool)
            .field("name", &self.name)
            .field("genre", &self.genre)
            .field("description", &self.description)
            .field("public", &self.public)
            .field("bitrate_kbps", &self.bitrate_kbps)
            .field("format", &self.format)
            .finish()
    }
}

impl Default for StreamConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            protocol: StreamProtocol::Icecast,
            host: "127.0.0.1".into(),
            port: 8000,
            mount: "/stream".into(),
            username: "source".into(),
            password: String::new(),
            tls: false,
            name: "CrabBoss FM".into(),
            genre: String::new(),
            description: String::new(),
            public: false,
            bitrate_kbps: 128,
            format: StreamFormat::Mp3,
            sid: default_stream_sid(),
            stereotool: StereoToolConfig::default(),
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
        if self.sid == 0 {
            self.sid = default_stream_sid();
        }
        let st = std::mem::take(&mut self.stereotool);
        self.stereotool = st.sanitized();
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

    #[test]
    fn protocol_defaults_to_icecast_for_old_configs() {
        // Pre-Shoutcast settings.json files have no `protocol`/`sid`
        // keys: they keep working as Icecast on the default stream.
        let cfg: StreamConfig = serde_json::from_str(r#"{"host":"x"}"#).unwrap();
        assert_eq!(cfg.protocol, StreamProtocol::Icecast);
        assert_eq!(cfg.sid, 1);
        assert!(!cfg.protocol.is_shoutcast());
        assert!(StreamProtocol::ShoutcastV1.is_shoutcast());
        assert!(StreamProtocol::ShoutcastV2.is_shoutcast());
    }

    #[test]
    fn protocol_roundtrips_through_json() {
        let cfg = StreamConfig {
            protocol: StreamProtocol::ShoutcastV2,
            sid: 3,
            ..Default::default()
        };
        let json = serde_json::to_string(&cfg).unwrap();
        assert!(json.contains("shoutcast-v2"), "{json}");
        let back: StreamConfig = serde_json::from_str(&json).unwrap();
        assert_eq!(back, cfg);
        let v1: StreamProtocol = serde_json::from_str("\"shoutcast-v1\"").unwrap();
        assert_eq!(v1.label(), "Shoutcast v1");
    }

    #[test]
    fn sanitize_repairs_zero_sid() {
        let cfg = StreamConfig {
            sid: 0,
            ..Default::default()
        }
        .sanitized();
        assert_eq!(cfg.sid, 1);
    }

    #[test]
    fn opus_format_labels_serializes_and_streams() {
        assert_eq!(StreamFormat::Opus.content_type(), "audio/ogg; codecs=opus");
        assert_eq!(StreamFormat::Opus.label(), "Opus");
        let json = serde_json::to_string(&StreamFormat::Opus).unwrap();
        assert_eq!(json, "\"opus\"");
        // Old settings.json files have no `format` key: still MP3.
        let cfg: StreamConfig = serde_json::from_str(r#"{"bitrate_kbps":128}"#).unwrap();
        assert_eq!(cfg.format, StreamFormat::Mp3);
    }

    #[test]
    fn debug_redacts_password_but_keeps_other_fields() {
        let cfg = StreamConfig {
            password: "hunter2".into(),
            host: "cast.example.com".into(),
            ..Default::default()
        };
        let dbg = format!("{cfg:?}");
        assert!(!dbg.contains("hunter2"), "secret leaked: {dbg}");
        assert!(dbg.contains("<redacted>"));
        assert!(dbg.contains("cast.example.com"));
        // Serialization still carries the real value: Stage A keeps a
        // local plaintext settings file by explicit design.
        let json = serde_json::to_string(&cfg).unwrap();
        assert!(json.contains("hunter2"));
    }
}
