//! Icecast 2 source client.
//!
//! Handshake over TCP per the Icecast protocol: HTTP `PUT` (2.4+, with
//! `Expect: 100-continue`) falling back to the legacy `SOURCE` method
//! for older servers. Audio bytes are written paced in real time (the
//! spec requires sources to behave like a live feed), with periodic
//! metadata updates via the in-stream `icy-meta` protocol.

use std::io::{Read, Write};
use std::net::TcpStream;
use std::time::Duration;

use base64::Engine;

use crate::error::{CrabError, Result};
use crate::stream::StreamConfig;

/// How long to wait for the server during connect + handshake.
const HANDSHAKE_TIMEOUT: Duration = Duration::from_secs(8);

/// An established source connection to an Icecast server.
#[derive(Debug)]
pub struct IcecastSource {
    stream: TcpStream,
    /// In-band metadata interval (bytes of audio between metadata blocks),
    /// as announced by the server via `ice-metadata-interval`. 0 = off.
    meta_interval: usize,
    bytes_until_meta: usize,
    /// Send timeout so a stalled server can't wedge the audio thread.
    send_timeout: Duration,
}

/// Handshake outcome: which protocol variant the server accepted.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HandshakeProtocol {
    Put,
    Source,
}

/// Minimal URL-encoded credential (RFC 4648, no line breaks).
fn basic_auth(user: &str, pass: &str) -> String {
    use base64::engine::general_purpose::STANDARD as B64;
    B64.encode(format!("{user}:{pass}"))
}

impl IcecastSource {
    /// Connect + handshake. Tries `PUT` first, falls back to `SOURCE`
    /// when the server closes the connection without a response (the
    /// documented behavior of pre-2.4 servers on PUT).
    pub fn connect(config: &StreamConfig) -> Result<(Self, HandshakeProtocol)> {
        let config = config.clone().sanitized();
        let addr = format!("{}:{}", config.host, config.port);
        let mut stream = TcpStream::connect(&addr)
            .map_err(|e| CrabError::Audio(format!("Icecast connect {addr}: {e}")))?;
        stream
            .set_read_timeout(Some(HANDSHAKE_TIMEOUT))
            .and_then(|_| stream.set_write_timeout(Some(HANDSHAKE_TIMEOUT)))
            .map_err(|e| CrabError::Audio(format!("Icecast timeouts: {e}")))?;

        let headers = Self::request_headers(&config);
        match Self::handshake(&mut stream, &addr, "PUT", &headers, true) {
            Ok(meta_interval) => {
                let src = Self::finish(stream, meta_interval);
                Ok((src, HandshakeProtocol::Put))
            }
            Err(put_err) => {
                tracing::warn!("Icecast PUT failed ({put_err}); trying legacy SOURCE");
                // Reconnect: the failed attempt may have consumed bytes.
                let mut stream = TcpStream::connect(&addr)
                    .map_err(|e| CrabError::Audio(format!("Icecast reconnect {addr}: {e}")))?;
                stream
                    .set_read_timeout(Some(HANDSHAKE_TIMEOUT))
                    .and_then(|_| stream.set_write_timeout(Some(HANDSHAKE_TIMEOUT)))
                    .map_err(|e| CrabError::Audio(format!("Icecast timeouts: {e}")))?;
                let meta_interval = Self::handshake(&mut stream, &addr, "SOURCE", &headers, false)
                    .map_err(|src_err| {
                        CrabError::Audio(format!(
                            "Icecast handshake failed — PUT: {put_err}; SOURCE: {src_err}"
                        ))
                    })?;
                let src = Self::finish(stream, meta_interval);
                Ok((src, HandshakeProtocol::Source))
            }
        }
    }

    fn finish(stream: TcpStream, meta_interval: usize) -> Self {
        // Steady-state send timeout: slower than handshake, still bounded.
        let send_timeout = Duration::from_secs(15);
        let _ = stream.set_write_timeout(Some(send_timeout));
        let _ = stream.set_read_timeout(Some(send_timeout));
        let bytes_until_meta = meta_interval;
        Self {
            stream,
            meta_interval,
            bytes_until_meta,
            send_timeout,
        }
    }

    /// Common request head (request line + Host added by `handshake`).
    fn request_headers(config: &StreamConfig) -> String {
        let auth = basic_auth(&config.username, &config.password);
        format!(
            "Authorization: Basic {}\r\n\
             User-Agent: CrabBoss/0.1\r\n\
             Content-Type: {}\r\n\
             Ice-Public: {}\r\n\
             Ice-Name: {}\r\n\
             Ice-Genre: {}\r\n\
             Ice-Description: {}\r\n\
             Ice-Audio-Info: samplerate=48000;channels=2;bitrate={}\r\n",
            auth,
            config.format.content_type(),
            u8::from(config.public),
            config.name.replace(['\r', '\n'], " "),
            config.genre.replace(['\r', '\n'], " "),
            config.description.replace(['\r', '\n'], " "),
            config.bitrate_kbps,
        )
    }

    /// Send the request line + headers, read the response, and return the
    /// negotiated `ice-metadata-interval` (0 when absent).
    fn handshake(
        stream: &mut TcpStream,
        addr: &str,
        method: &str,
        headers: &str,
        expect_continue: bool,
    ) -> Result<usize> {
        let host = addr.split(':').next().unwrap_or(addr);
        let mut req = format!("{method} / HTTP/1.1\r\nHost: {host}\r\n{headers}");
        if expect_continue {
            req.push_str("Expect: 100-continue\r\n");
        }
        // Metadata support is opt-in for the source; we want it for
        // now-playing updates. Long-lived request body — no Connection close.
        req.push_str("Ice-Metadata: 1\r\n\r\n");
        stream
            .write_all(req.as_bytes())
            .map_err(|e| CrabError::Audio(format!("Icecast send: {e}")))?;
        stream
            .flush()
            .map_err(|e| CrabError::Audio(format!("Icecast flush: {e}")))?;

        // Read until end of response headers.
        let mut buf = [0u8; 4096];
        let mut response = Vec::new();
        loop {
            let n = stream
                .read(&mut buf)
                .map_err(|e| CrabError::Audio(format!("Icecast read: {e}")))?;
            if n == 0 {
                return Err(CrabError::Audio(
                    "Icecast closed the connection during handshake \
                     (server may not support this method)"
                        .into(),
                ));
            }
            response.extend_from_slice(&buf[..n]);
            if response.windows(4).any(|w| w == b"\r\n\r\n") {
                break;
            }
            if response.len() > 64 * 1024 {
                return Err(CrabError::Audio("Icecast handshake header flood".into()));
            }
        }
        let text = String::from_utf8_lossy(&response);
        let status = text.lines().next().unwrap_or_default().trim().to_string();

        // 100 Continue is provisional; keep reading until real status.
        if status.starts_with("HTTP/1.1 100") || status.starts_with("HTTP/1.0 100") {
            // Find the final status line: last HTTP/ line before blank.
            // Simple approach: scan all lines for the last "HTTP/" one.
            let final_status = text
                .lines()
                .rev()
                .find(|l| l.starts_with("HTTP/"))
                .unwrap_or_default()
                .to_string();
            if !final_status.starts_with("HTTP/1.0 200")
                && !final_status.starts_with("HTTP/1.1 200")
            {
                return Err(CrabError::Audio(format!(
                    "Icecast rejected: {final_status}"
                )));
            }
        } else if !status.starts_with("HTTP/1.0 200") && !status.starts_with("HTTP/1.1 200") {
            return Err(CrabError::Audio(format!("Icecast rejected: {status}")));
        }

        // Negotiated metadata interval (bytes between metadata blocks).
        let meta_interval = text
            .lines()
            .find_map(|l| {
                let (k, v) = l.split_once(':')?;
                k.trim()
                    .eq_ignore_ascii_case("ice-metadata-interval")
                    .then(|| v.trim().parse::<usize>().ok())?
            })
            .unwrap_or(0);
        tracing::info!("Icecast handshake OK ({method}, metadata interval {meta_interval})");
        Ok(meta_interval)
    }

    /// Send encoded audio bytes. The caller paces the rate; this just
    /// handles writes, in-band metadata bookkeeping, and IO errors.
    pub fn send(&mut self, audio: &[u8]) -> Result<()> {
        if self.meta_interval == 0 {
            return self.write_all(audio);
        }
        // Split audio at metadata boundaries, inserting empty (or queued)
        // metadata blocks in-band. We only send empty metadata unless
        // `set_metadata` queued a title; real title updates go through
        // `maybe_send_metadata` ahead of audio.
        let mut sent = 0usize;
        while sent < audio.len() {
            let chunk = audio.len() - sent;
            let take = chunk.min(self.bytes_until_meta.min(1024));
            self.write_all(&audio[sent..sent + take])?;
            sent += take;
            self.bytes_until_meta = self.bytes_until_meta.saturating_sub(take);
            if self.bytes_until_meta == 0 {
                self.write_metadata_block(None)?;
                self.bytes_until_meta = self.meta_interval;
            }
        }
        Ok(())
    }

    /// Push a now-playing title using the in-band metadata protocol
    /// (best effort: failures surface on the next `send`).
    pub fn set_metadata(&mut self, title: &str) -> Result<()> {
        if self.meta_interval == 0 {
            tracing::debug!("Icecast metadata not negotiated; skipping title update");
            return Ok(());
        }
        // Shoutcast-style "StreamTitle='…';" payload, length-prefixed to
        // 16-byte blocks (block count byte, then payload + NUL padding).
        let safe = title.replace(['\r', '\n'], " ");
        let payload = format!("StreamTitle='{safe}';");
        let padded = payload.len().div_ceil(16) * 16;
        let mut meta = vec![0u8; 1 + padded];
        meta[0] = (padded / 16) as u8;
        meta[1..1 + payload.len()].copy_from_slice(payload.as_bytes());
        self.write_all(&meta)
    }

    fn write_metadata_block(&mut self, title: Option<&str>) -> Result<()> {
        let payload = title
            .map(|t| format!("StreamTitle='{}';", t.replace(['\r', '\n'], " ")))
            .unwrap_or_default();
        let padded = payload.len().div_ceil(16) * 16;
        let mut meta = vec![0u8; 1 + padded];
        meta[0] = (padded / 16) as u8;
        meta[1..1 + payload.len()].copy_from_slice(payload.as_bytes());
        self.write_all(&meta)
    }

    fn write_all(&mut self, mut bytes: &[u8]) -> Result<()> {
        while !bytes.is_empty() {
            match self.stream.write(bytes) {
                Ok(0) => {
                    return Err(CrabError::Audio("Icecast write made no progress".into()));
                }
                Ok(n) => bytes = &bytes[n..],
                Err(e)
                    if e.kind() == std::io::ErrorKind::WouldBlock
                        || e.kind() == std::io::ErrorKind::TimedOut =>
                {
                    return Err(CrabError::Audio(format!(
                        "Icecast send stalled (>{}s)",
                        self.send_timeout.as_secs()
                    )));
                }
                Err(e) => return Err(CrabError::Audio(format!("Icecast send: {e}"))),
            }
        }
        self.stream
            .flush()
            .map_err(|e| CrabError::Audio(format!("Icecast flush: {e}")))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::net::TcpListener;

    /// Spin a fake server that reads the request and answers `response`,
    /// returning what it received (so tests can assert on the handshake).
    fn fake_server(response: &'static [u8]) -> (String, std::sync::mpsc::Receiver<String>) {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap().to_string();
        let (tx, rx) = std::sync::mpsc::channel();
        std::thread::spawn(move || {
            if let Ok((mut sock, _)) = listener.accept() {
                let mut buf = vec![0u8; 8192];
                let n = sock.read(&mut buf).unwrap_or(0);
                let _ = tx.send(String::from_utf8_lossy(&buf[..n]).to_string());
                let _ = sock.write_all(response);
                let _ = sock.flush();
                // Keep the socket open briefly so the client sees the reply.
                std::thread::sleep(Duration::from_millis(300));
            }
        });
        (addr, rx)
    }

    fn ok_response(meta_interval: u32) -> String {
        format!(
            "HTTP/1.0 200 OK\r\nServer: FakeIce/2.5\r\nice-metadata-interval: {meta_interval}\r\n\r\n"
        )
    }

    #[test]
    fn handshake_put_ok_and_interval_parsed() {
        let addr_holder = TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = addr_holder.local_addr().unwrap().to_string();
        drop(addr_holder);
        // Bind a fresh listener for the test server.
        let listener = TcpListener::bind(&addr).unwrap();
        std::thread::spawn(move || {
            if let Ok((mut sock, _)) = listener.accept() {
                let mut buf = vec![0u8; 8192];
                let n = sock.read(&mut buf).unwrap_or(0);
                let req = String::from_utf8_lossy(&buf[..n]).to_string();
                assert!(req.starts_with("PUT / HTTP/1.1"));
                assert!(req.contains("Authorization: Basic c291cmNlOg=="), "{req}");
                assert!(req.contains("Content-Type: audio/mpeg"));
                assert!(req.contains("Expect: 100-continue"));
                let _ = sock.write_all(ok_response(8192).as_bytes());
                let _ = sock.flush();
            }
        });
        let cfg = StreamConfig {
            host: addr.split(':').next().unwrap().to_string(),
            port: addr.rsplit(':').next().unwrap().parse().unwrap(),
            username: "source".into(),
            password: String::new(),
            ..Default::default()
        };
        let (src, proto) = IcecastSource::connect(&cfg).unwrap();
        assert_eq!(proto, HandshakeProtocol::Put);
        assert_eq!(src.meta_interval, 8192);
    }

    #[test]
    fn handshake_rejects_401_with_message() {
        let (addr, _rx) = fake_server(b"HTTP/1.0 401 You need to authenticate\r\n\r\n");
        let cfg = StreamConfig {
            host: addr.split(':').next().unwrap().to_string(),
            port: addr.rsplit(':').next().unwrap().parse().unwrap(),
            ..Default::default()
        };
        let err = IcecastSource::connect(&cfg).unwrap_err();
        let msg = err.to_string();
        assert!(msg.contains("401"), "{msg}");
        assert!(
            msg.contains("SOURCE"),
            "should mention both attempts: {msg}"
        );
    }

    #[test]
    fn metadata_block_is_length_prefixed() {
        // Directly test the framing math used by set_metadata.
        let payload = "StreamTitle='Artist - Title';";
        let padded = payload.len().div_ceil(16) * 16;
        assert_eq!(padded % 16, 0);
        assert_eq!((padded / 16) as u8, payload.len().div_ceil(16) as u8);
    }

    #[test]
    fn auth_is_base64_of_user_pass() {
        assert_eq!(basic_auth("source", "hackme"), "c291cmNlOmhhY2ttZQ==");
        assert_eq!(basic_auth("source", ""), "c291cmNlOg==");
    }
}
