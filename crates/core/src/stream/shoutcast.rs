//! Shoutcast (DNAS 1.x / 2.x) source client.
//!
//! Wire flow (v1 — works on DNAS 1.x and, per the DNAS docs and
//! v1-only clients like BUTT, on 2.x as well):
//!
//! 1. TCP connect. v1 expects the *source* port (usually listener
//!    `portbase + 1`, e.g. 8001); v2 expects `portbase` itself
//!    (e.g. 8000). The port is taken as configured — no magic.
//! 2. Send `{password}\n`; the server replies a line containing `OK`
//!    (`OK2`) or drops the connection (wrong password/port).
//! 3. Send `icy-*` headers + blank line; the server replies `OK`
//!    (usually `OK2` plus `icy-caps`).
//! 4. Raw MP3 bytes follow. Titles go out-of-band via `admin.cgi` on a
//!    separate connection ([`ShoutcastSource::set_metadata`]).
//!
//! v2 stream selection is the documented DNAS ≥ 2.4.7 mechanism: the
//! password carries `:#<sid>` (`secret:#2`); sid 1 is the default and
//! needs no suffix. The native Ultravox POST handshake is deliberately
//! NOT spoken — its bytes are unpublished and untestable here, and the
//! v1-compatible flow is what DNAS 2.x itself recommends to
//! third-party sources. Shoutcast is MP3-only (no Opus/HE-AAC encode
//! path); a non-MP3 config fails at connect with a plain message.

use std::io::{Read, Write};
use std::time::Duration;

use crate::error::{CrabError, Result};
use crate::stream::source::{IcecastSource, SourceStream};
use crate::stream::{StreamConfig, StreamFormat, StreamProtocol};

/// One metadata poke must stay quick; it runs off the audio thread but
/// a wedged admin page shouldn't park the worker long.
/// (Handshake timeouts come from the shared `open` transport.)
const META_TIMEOUT: Duration = Duration::from_secs(8);
/// A single handshake line / header block cap (DNAS replies are tiny).
const MAX_HANDSHAKE: usize = 64 * 1024;

/// An established source connection to a Shoutcast DNAS server.
#[derive(Debug)]
pub struct ShoutcastSource {
    stream: SourceStream,
    /// Send timeout so a stalled server can't wedge the audio thread.
    send_timeout: Duration,
    admin: ShoutcastAdmin,
    v2: bool,
}

/// What `set_metadata` needs on its own connection (the streaming
/// socket carries audio only).
#[derive(Debug, Clone)]
struct ShoutcastAdmin {
    host: String,
    port: u16,
    tls: bool,
    /// Password as sent on the wire (sid already mangled for v2).
    pass: String,
    sid: u32,
}

impl ShoutcastSource {
    /// Connect + handshake on the configured port.
    pub fn connect(config: &StreamConfig) -> Result<Self> {
        let cfg = config.clone().sanitized();
        if cfg.format != StreamFormat::Mp3 {
            return Err(CrabError::Audio(
                "Shoutcast output supports MP3 only — switch Format to MP3 (or Protocol to Icecast for Opus/HE-AAC)"
                    .into(),
            ));
        }
        let v2 = cfg.protocol == StreamProtocol::ShoutcastV2;
        let password = login_password(&cfg.password, cfg.sid, v2);
        let addr = format!("{}:{}", cfg.host, cfg.port);
        let mut stream = IcecastSource::open(&cfg, &addr, "Shoutcast")?;

        // --- Password line ---
        stream
            .write_all(format!("{password}\n").as_bytes())
            .map_err(|e| CrabError::Audio(format!("Shoutcast send: {e}")))?;
        stream
            .flush()
            .map_err(|e| CrabError::Audio(format!("Shoutcast flush: {e}")))?;
        let reply = read_line(&mut stream).map_err(|e| {
            // DNAS drops the connection on a bad password with no reply.
            // A clean FIN reads as EOF, but depending on timing the server
            // RSTs instead (10053/10054) — real servers do both, so both
            // map to the same operator guidance.
            let dropped = matches!(
                e.kind(),
                std::io::ErrorKind::UnexpectedEof
                    | std::io::ErrorKind::ConnectionAborted
                    | std::io::ErrorKind::ConnectionReset
            ) || e.to_string().contains("closed");
            if dropped {
                CrabError::Audio(
                    "Shoutcast closed the connection during login — wrong password, \
                     or wrong port (v1 needs the source port, usually listener port + 1; v2 needs portbase)"
                        .into(),
                )
            } else {
                CrabError::Audio(format!("Shoutcast login: {e}"))
            }
        })?;
        if !reply.contains("OK") {
            return Err(CrabError::Audio(format!("Shoutcast rejected: {reply}")));
        }

        // --- Station headers (Winamp-DSP shape) + blank line ---
        let name = sanitize_field(&cfg.name, "CrabBoss");
        let headers = format!(
            "icy-name:{name}\n\
             icy-genre:{}\n\
             icy-pub:{}\n\
             icy-br:{}\n\
             icy-irc:\n\
             icy-icq:\n\
             icy-aim:\n\
             Content-Type: audio/mpeg\n\
             \n",
            sanitize_field(&cfg.genre, ""),
            u8::from(cfg.public),
            cfg.bitrate_kbps,
        );
        stream
            .write_all(headers.as_bytes())
            .map_err(|e| CrabError::Audio(format!("Shoutcast send: {e}")))?;
        stream
            .flush()
            .map_err(|e| CrabError::Audio(format!("Shoutcast flush: {e}")))?;
        let reply = read_block(&mut stream)?;
        if !reply.contains("OK") {
            return Err(CrabError::Audio(format!("Shoutcast rejected: {reply}")));
        }
        tracing::info!(
            "Shoutcast handshake OK ({}, sid {})",
            if v2 { "v2" } else { "v1" },
            cfg.sid
        );

        // Steady-state send timeout: slower than handshake, still bounded.
        let send_timeout = Duration::from_secs(15);
        let _ = stream.socket().set_write_timeout(Some(send_timeout));
        let _ = stream.socket().set_read_timeout(Some(send_timeout));
        Ok(Self {
            stream,
            send_timeout,
            admin: ShoutcastAdmin {
                host: cfg.host,
                port: cfg.port,
                tls: cfg.tls,
                pass: password,
                sid: cfg.sid,
            },
            v2,
        })
    }

    /// Send encoded audio bytes (passthrough: the caller paces the rate).
    pub fn send(&mut self, audio: &[u8]) -> Result<()> {
        let mut bytes = audio;
        while !bytes.is_empty() {
            match self.stream.write(bytes) {
                Ok(0) => {
                    return Err(CrabError::Audio("Shoutcast write made no progress".into()));
                }
                Ok(n) => bytes = &bytes[n..],
                Err(e)
                    if e.kind() == std::io::ErrorKind::WouldBlock
                        || e.kind() == std::io::ErrorKind::TimedOut =>
                {
                    return Err(CrabError::Audio(format!(
                        "Shoutcast send stalled (>{}s)",
                        self.send_timeout.as_secs()
                    )));
                }
                Err(e) => return Err(CrabError::Audio(format!("Shoutcast send: {e}"))),
            }
        }
        self.stream
            .flush()
            .map_err(|e| CrabError::Audio(format!("Shoutcast flush: {e}")))
    }

    /// Push a now-playing title via `admin.cgi` (`mode=updinfo`).
    ///
    /// Fire-and-forget on a detached thread: the streaming socket
    /// carries audio only, and the paced sender loop must never block
    /// on admin HTTP. Best effort — failures warn, the audio continues.
    pub fn set_metadata(&mut self, title: &str) -> Result<()> {
        let admin = self.admin.clone();
        let v2 = self.v2;
        let title = title.replace(['\r', '\n'], " ");
        std::thread::Builder::new()
            .name("shoutcast-meta".into())
            .spawn(move || {
                if let Err(e) = admin_update(&admin, v2, &title) {
                    tracing::warn!("Shoutcast metadata update failed: {e}");
                }
            })
            .map_err(|e| CrabError::Audio(format!("Shoutcast metadata thread: {e}")))?;
        Ok(())
    }
}

/// v2 stream selection (DNAS ≥ 2.4.7): the password carries `:#<sid>`.
/// Sid 1 is the server default and needs no suffix; v1 sends the
/// password untouched (the operator may still hand-craft
/// `dj:pass:#sid` — it passes through verbatim).
fn login_password(password: &str, sid: u32, v2: bool) -> String {
    if v2 && sid > 1 {
        format!("{password}:#{sid}")
    } else {
        password.to_string()
    }
}

/// One-line header field: no newlines (header injection), trimmed, with
/// a fallback when the config left it blank.
fn sanitize_field(value: &str, fallback: &str) -> String {
    let clean = value.replace(['\r', '\n'], " ").trim().to_string();
    if clean.is_empty() {
        fallback.to_string()
    } else {
        clean
    }
}

/// Minimal percent-encoding for `admin.cgi` query values (RFC 3986
/// unreserved set passes through; everything else is `%XX`).
pub(crate) fn percent_encode(input: &str) -> String {
    const UNRESERVED: &[u8] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789-_.~";
    let mut out = String::with_capacity(input.len());
    for b in input.bytes() {
        if UNRESERVED.contains(&b) {
            out.push(b as char);
        } else {
            out.push_str(&format!("%{b:02X}"));
        }
    }
    out
}

/// Read one `\n`-terminated line (bounded).
fn read_line(stream: &mut SourceStream) -> std::io::Result<String> {
    let mut line = Vec::new();
    let mut byte = [0u8; 1];
    loop {
        if line.len() > MAX_HANDSHAKE {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                "handshake line too long",
            ));
        }
        match stream.read(&mut byte) {
            Ok(0) => {
                return Err(std::io::Error::new(
                    std::io::ErrorKind::UnexpectedEof,
                    "server closed the connection",
                ));
            }
            Ok(_) => {
                if byte[0] == b'\n' {
                    break;
                }
                line.push(byte[0]);
            }
            Err(e) => return Err(e),
        }
    }
    let mut s = String::from_utf8_lossy(&line).to_string();
    while s.ends_with('\r') {
        s.pop();
    }
    Ok(s)
}

/// Read through the first blank line (multi-line `OK2` + `icy-caps`
/// replies), bounded.
fn read_block(stream: &mut SourceStream) -> Result<String> {
    let mut buf = Vec::new();
    let mut chunk = [0u8; 1024];
    loop {
        if buf.len() > MAX_HANDSHAKE {
            return Err(CrabError::Audio("Shoutcast handshake header flood".into()));
        }
        let n = stream
            .read(&mut chunk)
            .map_err(|e| CrabError::Audio(format!("Shoutcast read: {e}")))?;
        if n == 0 {
            return Err(CrabError::Audio(
                "Shoutcast closed the connection during handshake".into(),
            ));
        }
        buf.extend_from_slice(&chunk[..n]);
        let text = String::from_utf8_lossy(&buf).to_string();
        if text.contains("\n\n") || text.contains("\r\n\r\n") {
            return Ok(text);
        }
        // A bare single-line `OK2` with no blank line yet: keep reading
        // only briefly — some servers send just `OK2\r\n`.
        if text.trim_end().ends_with("OK2") && buf.len() >= 3 {
            // Give the (optional) caps line a short grace window.
            let _ = stream
                .socket()
                .set_read_timeout(Some(Duration::from_millis(500)));
            match stream.read(&mut chunk) {
                Ok(0) | Err(_) => return Ok(text),
                Ok(m) => {
                    buf.extend_from_slice(&chunk[..m]);
                    return Ok(String::from_utf8_lossy(&buf).to_string());
                }
            }
        }
    }
}

/// One `admin.cgi` title poke on a fresh short-lived connection.
fn admin_update(admin: &ShoutcastAdmin, v2: bool, title: &str) -> Result<()> {
    let path = if v2 {
        format!(
            "/admin.cgi?sid={}&pass={}&mode=updinfo&song={}",
            admin.sid,
            percent_encode(&admin.pass),
            percent_encode(title)
        )
    } else {
        format!(
            "/admin.cgi?pass={}&mode=updinfo&song={}",
            percent_encode(&admin.pass),
            percent_encode(title)
        )
    };
    let addr = format!("{}:{}", admin.host, admin.port);
    let probe = StreamConfig {
        host: admin.host.clone(),
        tls: admin.tls,
        ..Default::default()
    };
    let mut stream = IcecastSource::open(&probe, &addr, "Shoutcast")?;
    let _ = stream.socket().set_read_timeout(Some(META_TIMEOUT));
    let _ = stream.socket().set_write_timeout(Some(META_TIMEOUT));
    let req = format!(
        "GET {path} HTTP/1.0\r\nHost: {}\r\nUser-Agent: CrabBoss/0.1\r\nConnection: close\r\n\r\n",
        admin.host
    );
    stream
        .write_all(req.as_bytes())
        .map_err(|e| CrabError::Audio(format!("Shoutcast metadata send: {e}")))?;
    let mut raw = Vec::new();
    std::io::Read::by_ref(&mut stream)
        .take(16 * 1024)
        .read_to_end(&mut raw)
        .map_err(|e| CrabError::Audio(format!("Shoutcast metadata read: {e}")))?;
    let head = String::from_utf8_lossy(&raw).to_string();
    let status = head.lines().next().unwrap_or_default();
    if !status.contains(" 200 ") {
        return Err(CrabError::Audio(format!(
            "Shoutcast metadata rejected: {status}"
        )));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::net::TcpListener;
    use std::sync::mpsc;

    /// Fake DNAS: Greets `OK2`, captures the header block, replies
    /// `OK2` + caps, then captures everything else (audio bytes).
    /// Returns the server port plus receivers for (login_line, headers, audio).
    fn fake_dnas() -> (
        u16,
        mpsc::Receiver<String>,
        mpsc::Receiver<String>,
        mpsc::Receiver<Vec<u8>>,
    ) {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        let (login_tx, login_rx) = mpsc::channel();
        let (head_tx, head_rx) = mpsc::channel();
        let (audio_tx, audio_rx) = mpsc::channel();
        std::thread::spawn(move || {
            let Ok((mut sock, _)) = listener.accept() else {
                return;
            };
            let _ = sock.set_read_timeout(Some(Duration::from_secs(10)));
            // 1. password line.
            let mut line = Vec::new();
            let mut b = [0u8; 1];
            loop {
                match sock.read(&mut b) {
                    Ok(1) => {
                        if b[0] == b'\n' {
                            break;
                        }
                        line.push(b[0]);
                    }
                    _ => return,
                }
            }
            let _ = login_tx.send(String::from_utf8_lossy(&line).to_string());
            let _ = sock.write_all(b"OK2\r\n");
            // 2. header block through the blank line.
            let mut head = Vec::new();
            let mut chunk = [0u8; 1024];
            loop {
                match sock.read(&mut chunk) {
                    Ok(0) => return,
                    Ok(n) => {
                        head.extend_from_slice(&chunk[..n]);
                        let t = String::from_utf8_lossy(&head).to_string();
                        if t.contains("\n\n") {
                            break;
                        }
                    }
                    Err(_) => return,
                }
            }
            let _ = head_tx.send(String::from_utf8_lossy(&head).to_string());
            let _ = sock.write_all(b"OK2\r\nicy-caps:11\r\n\r\n");
            // 3. everything after is audio.
            let mut audio = Vec::new();
            loop {
                match sock.read(&mut chunk) {
                    Ok(0) => break,
                    Ok(n) => audio.extend_from_slice(&chunk[..n]),
                    Err(_) => break,
                }
            }
            let _ = audio_tx.send(audio);
        });
        (port, login_rx, head_rx, audio_rx)
    }

    fn cfg(port: u16) -> StreamConfig {
        StreamConfig {
            host: "127.0.0.1".into(),
            port,
            password: "secret".into(),
            protocol: StreamProtocol::ShoutcastV1,
            ..Default::default()
        }
    }

    #[test]
    fn v1_handshake_sends_password_then_icy_headers() {
        let (port, login_rx, head_rx, _audio) = fake_dnas();
        let mut src = ShoutcastSource::connect(&cfg(port)).expect("v1 must connect");
        assert_eq!(
            login_rx.recv_timeout(Duration::from_secs(5)).unwrap(),
            "secret"
        );
        let head = head_rx.recv_timeout(Duration::from_secs(5)).unwrap();
        assert!(head.contains("icy-name:"), "{head}");
        assert!(head.contains("icy-pub:"), "{head}");
        assert!(head.contains("icy-br:128"), "{head}");
        assert!(head.contains("Content-Type: audio/mpeg"), "{head}");
        src.send(b"MP3BYTES").unwrap();
    }

    #[test]
    fn v2_mangles_password_with_sid() {
        let (port, login_rx, _head, _audio) = fake_dnas();
        let mut c = cfg(port);
        c.protocol = StreamProtocol::ShoutcastV2;
        c.sid = 2;
        let _src = ShoutcastSource::connect(&c).expect("v2 must connect");
        assert_eq!(
            login_rx.recv_timeout(Duration::from_secs(5)).unwrap(),
            "secret:#2"
        );
    }

    #[test]
    fn v2_sid1_sends_password_verbatim() {
        let (port, login_rx, _head, _audio) = fake_dnas();
        let mut c = cfg(port);
        c.protocol = StreamProtocol::ShoutcastV2;
        c.sid = 1;
        let _src = ShoutcastSource::connect(&c).expect("v2 sid 1 must connect");
        assert_eq!(
            login_rx.recv_timeout(Duration::from_secs(5)).unwrap(),
            "secret"
        );
    }

    #[test]
    fn rejects_wrong_password_reply() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        std::thread::spawn(move || {
            if let Ok((mut sock, _)) = listener.accept() {
                let mut buf = [0u8; 256];
                let _ = sock.read(&mut buf);
                let _ = sock.write_all(b"invalid password\r\n");
            }
        });
        let err = ShoutcastSource::connect(&cfg(port)).unwrap_err();
        assert!(err.to_string().contains("rejected"), "{err}");
    }

    #[test]
    fn closed_login_hints_at_password_or_port() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        std::thread::spawn(move || {
            // Drop immediately: the DNAS wrong-password behavior. The
            // client may observe this as a clean FIN or as an RST
            // (10053/10054) depending on timing — both must hint.
            let _ = listener.accept();
        });
        let err = ShoutcastSource::connect(&cfg(port)).unwrap_err();
        assert!(err.to_string().contains("wrong password"), "{err}");
    }

    #[test]
    fn opus_is_rejected_for_shoutcast() {
        let (port, _l, _h, _a) = fake_dnas();
        let mut c = cfg(port);
        c.format = StreamFormat::Opus;
        let err = ShoutcastSource::connect(&c).unwrap_err();
        assert!(err.to_string().contains("MP3 only"), "{err}");
    }

    #[test]
    fn login_password_rules() {
        assert_eq!(login_password("pw", 1, false), "pw");
        assert_eq!(login_password("pw", 3, false), "pw");
        assert_eq!(login_password("pw", 1, true), "pw");
        assert_eq!(login_password("pw", 2, true), "pw:#2");
        // Operators may hand-craft DJ/sid suffixes for v1: passthrough.
        assert_eq!(login_password("dj:pw:#2", 1, false), "dj:pw:#2");
    }

    #[test]
    fn percent_encode_covers_query_values() {
        assert_eq!(percent_encode("abc-_.~09"), "abc-_.~09");
        assert_eq!(
            percent_encode("Artist - Title & More?"),
            "Artist%20-%20Title%20%26%20More%3F"
        );
        assert_eq!(percent_encode("100%"), "100%25");
    }

    #[test]
    fn metadata_poke_hits_admin_cgi() {
        // Fake admin endpoint: capture the request path, answer 200.
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        let (tx, rx) = mpsc::channel();
        std::thread::spawn(move || {
            if let Ok((mut sock, _)) = listener.accept() {
                let mut buf = vec![0u8; 4096];
                let n = sock.read(&mut buf).unwrap_or(0);
                let _ = tx.send(String::from_utf8_lossy(&buf[..n]).to_string());
                let _ = sock.write_all(b"HTTP/1.0 200 OK\r\nContent-Length: 0\r\n\r\n");
            }
        });
        let admin = ShoutcastAdmin {
            host: "127.0.0.1".into(),
            port,
            tls: false,
            pass: "secret".into(),
            sid: 1,
        };
        admin_update(&admin, false, "Artist - Title").expect("admin poke works");
        let req = rx.recv_timeout(Duration::from_secs(5)).unwrap();
        assert!(
            req.contains(
                "GET /admin.cgi?pass=secret&mode=updinfo&song=Artist%20-%20Title HTTP/1.0"
            ),
            "{req}"
        );
    }

    #[test]
    fn metadata_poke_v2_carries_sid() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        let (tx, rx) = mpsc::channel();
        std::thread::spawn(move || {
            if let Ok((mut sock, _)) = listener.accept() {
                let mut buf = vec![0u8; 4096];
                let n = sock.read(&mut buf).unwrap_or(0);
                let _ = tx.send(String::from_utf8_lossy(&buf[..n]).to_string());
                let _ = sock.write_all(b"HTTP/1.0 200 OK\r\nContent-Length: 0\r\n\r\n");
            }
        });
        let admin = ShoutcastAdmin {
            host: "127.0.0.1".into(),
            port,
            tls: false,
            pass: "secret:#3".into(),
            sid: 3,
        };
        admin_update(&admin, true, "X").expect("v2 admin poke works");
        let req = rx.recv_timeout(Duration::from_secs(5)).unwrap();
        assert!(
            req.contains("GET /admin.cgi?sid=3&pass=secret%3A%233&mode=updinfo&song=X HTTP/1.0"),
            "{req}"
        );
    }
}
