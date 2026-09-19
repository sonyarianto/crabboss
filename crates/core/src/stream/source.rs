//! Icecast 2 source client.
//!
//! Handshake over TCP per the Icecast protocol: HTTP `PUT` (2.4+, with
//! `Expect: 100-continue`) falling back to the legacy `SOURCE` method
//! for older servers. Audio bytes are written paced in real time (the
//! spec requires sources to behave like a live feed), with periodic
//! metadata updates via the in-stream `icy-meta` protocol.

use std::io::{Read, Write};
use std::net::{TcpStream, ToSocketAddrs};
use std::time::{Duration, Instant};

use base64::Engine;

use crate::error::{CrabError, Result};
use crate::stream::StreamConfig;

/// How long to wait for the server during connect + handshake.
/// Generous on purpose: post-auth source setup can stall behind slow
/// server-side auth hooks or event handlers, and giving up too early
/// leaves a half-open connection holding the mount (the next attempt
/// then eats a 409 "in use" for our own zombie).
const HANDSHAKE_TIMEOUT: Duration = Duration::from_secs(20);
/// How long to wait for a handshake REPLY once connected. Rejections
/// (401/403/404/409) always arrive instantly; a `100 Continue` means
/// "send the body now". Past this wait with total silence we proceed
/// optimistically — some stacks only finalize after body bytes start
/// flowing, and stalling here is worse than streaming into the void
/// (a dead socket surfaces on the first send anyway).
const HANDSHAKE_GRACE: Duration = Duration::from_secs(5);

/// An established source connection to an Icecast server.
#[derive(Debug)]
pub struct IcecastSource {
    stream: SourceStream,
    /// In-band metadata interval (bytes of audio between metadata blocks),
    /// as announced by the server via `ice-metadata-interval`. 0 = off.
    meta_interval: usize,
    bytes_until_meta: usize,
    /// Send timeout so a stalled server can't wedge the audio thread.
    send_timeout: Duration,
}

/// Transport under the source connection: plain TCP, or TLS over TCP for
/// servers behind HTTPS. Handshake and send logic are identical on both.
/// The TLS session is boxed: it dwarfs the socket, and an unbalanced enum
/// would bloat every value.
#[derive(Debug)]
pub(crate) enum SourceStream {
    Plain(TcpStream),
    Tls(Box<native_tls::TlsStream<TcpStream>>),
}

impl Read for SourceStream {
    fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        match self {
            SourceStream::Plain(s) => s.read(buf),
            SourceStream::Tls(s) => s.read(buf),
        }
    }
}

impl Write for SourceStream {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        match self {
            SourceStream::Plain(s) => s.write(buf),
            SourceStream::Tls(s) => s.write(buf),
        }
    }

    fn flush(&mut self) -> std::io::Result<()> {
        match self {
            SourceStream::Plain(s) => s.flush(),
            SourceStream::Tls(s) => s.flush(),
        }
    }
}

impl SourceStream {
    /// Reach the raw socket through either variant (socket timeouts live
    /// there; the TLS session passes them through to the same socket).
    /// Shared with the listener-stats poll, which tightens them past
    /// the handshake grade.
    pub(crate) fn socket(&mut self) -> &TcpStream {
        match self {
            SourceStream::Plain(s) => s,
            SourceStream::Tls(s) => s.get_ref(),
        }
    }
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
        let mut stream = Self::open(&config, &addr, "Icecast")?;
        let headers = Self::request_headers(&config);
        let mount = config.mount.clone();
        match Self::handshake(&mut stream, &addr, "PUT", &mount, &headers, true) {
            Ok(meta_interval) => {
                let src = Self::finish(stream, meta_interval);
                Ok((src, HandshakeProtocol::Put))
            }
            Err(put_err) => {
                tracing::warn!("Icecast PUT failed ({put_err}); trying legacy SOURCE");
                // Reconnect: the failed attempt may have consumed bytes.
                let mut stream = Self::open(&config, &addr, "Icecast")?;
                let meta_interval =
                    Self::handshake(&mut stream, &addr, "SOURCE", &mount, &headers, false)
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

    /// Open the transport: plain TCP, or TLS-wrapped when the config asks
    /// (servers behind HTTPS, e.g. port 443). Timeouts go on the raw
    /// socket first so the TLS handshake itself stays bounded; SNI uses
    /// the configured host. Shared with the listener-stats poll and the
    /// Shoutcast source client (`service` names the protocol in errors).
    pub(crate) fn open(config: &StreamConfig, addr: &str, service: &str) -> Result<SourceStream> {
        // Bounded resolve + connect: a filtered port must fail here in
        // seconds, not after the OS minute-long TCP timeout.
        let sock_addr = addr
            .to_socket_addrs()
            .map_err(|e| CrabError::Audio(format!("{service} resolve {addr}: {e}")))?
            .next()
            .ok_or_else(|| CrabError::Audio(format!("{service} resolve {addr}: no address")))?;
        let stream = TcpStream::connect_timeout(&sock_addr, HANDSHAKE_TIMEOUT)
            .map_err(|e| CrabError::Audio(format!("{service} connect {addr}: {e}")))?;
        stream
            .set_read_timeout(Some(HANDSHAKE_TIMEOUT))
            .and_then(|_| stream.set_write_timeout(Some(HANDSHAKE_TIMEOUT)))
            .map_err(|e| CrabError::Audio(format!("{service} timeouts: {e}")))?;
        if !config.tls {
            return Ok(SourceStream::Plain(stream));
        }
        let connector = native_tls::TlsConnector::new()
            .map_err(|e| CrabError::Audio(format!("TLS setup: {e}")))?;
        connector
            .connect(config.host.as_str(), stream)
            .map(|tls| SourceStream::Tls(Box::new(tls)))
            .map_err(|e| CrabError::Audio(format!("TLS handshake {addr}: {e}")))
    }

    fn finish(mut stream: SourceStream, meta_interval: usize) -> Self {
        // Steady-state send timeout: slower than handshake, still bounded.
        let send_timeout = Duration::from_secs(15);
        let socket = stream.socket();
        let _ = socket.set_write_timeout(Some(send_timeout));
        let _ = socket.set_read_timeout(Some(send_timeout));
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
    ///
    /// The mountpoint travels in the request path per the Icecast source
    /// protocol (`PUT /live HTTP/1.1`) — a bare `/` leaves the server with
    /// no mount to attach, so real servers reject it.
    fn handshake(
        stream: &mut SourceStream,
        addr: &str,
        method: &str,
        mount: &str,
        headers: &str,
        expect_continue: bool,
    ) -> Result<usize> {
        let host = addr.split(':').next().unwrap_or(addr);
        let mut req = format!("{method} {mount} HTTP/1.1\r\nHost: {host}\r\n{headers}");
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

        // Bound the reply wait. Rejections (401/403/404/409) always arrive
        // instantly and are honored below. A `100 Continue` IS the go-ahead
        // ("send the body now") — per Icecast's own source code the final
        // 200 for a PUT source may only arrive at teardown, so waiting for
        // one here can stall a healthy connection forever. Total silence
        // gets an optimistic proceed: some stacks only finalize once body
        // bytes start flowing, and a dead socket surfaces on the first
        // send anyway.
        stream
            .socket()
            .set_read_timeout(Some(HANDSHAKE_GRACE))
            .map_err(|e| CrabError::Audio(format!("Icecast timeouts: {e}")))?;
        let t0 = Instant::now();
        let mut pending = Vec::new();
        let response = match Self::read_response_headers(stream, &mut pending) {
            Ok(r) => r,
            Err(_) if t0.elapsed() >= HANDSHAKE_GRACE => {
                tracing::warn!(
                    "No handshake reply in {:?}; streaming optimistically — \
                     the server may finalize once audio starts flowing",
                    HANDSHAKE_GRACE
                );
                return Ok(0);
            }
            Err(e) => return Err(e),
        };
        let status = response
            .lines()
            .next()
            .unwrap_or_default()
            .trim()
            .to_string();
        if status.starts_with("HTTP/1.1 100") || status.starts_with("HTTP/1.0 100") {
            tracing::info!("Icecast handshake OK ({method}, go-ahead 100)");
            return Ok(Self::meta_interval(&response));
        }
        if !status.starts_with("HTTP/1.0 200") && !status.starts_with("HTTP/1.1 200") {
            if status.contains("404") {
                return Err(CrabError::Audio(format!(
                    "Icecast 404 for mount '{mount}': mountpoint not found — \
                     check the mount name and the server's mount configuration"
                )));
            }
            return Err(CrabError::Audio(format!("Icecast rejected: {status}")));
        }

        // Negotiated metadata interval (bytes between metadata blocks).
        let meta_interval = Self::meta_interval(&response);
        tracing::info!("Icecast handshake OK ({method}, metadata interval {meta_interval})");
        Ok(meta_interval)
    }

    /// `ice-metadata-interval` harvested from a handshake block (0 absent).
    fn meta_interval(response: &str) -> usize {
        response
            .lines()
            .find_map(|l| {
                let (k, v) = l.split_once(':')?;
                k.trim()
                    .eq_ignore_ascii_case("ice-metadata-interval")
                    .then(|| v.trim().parse::<usize>().ok())?
            })
            .unwrap_or(0)
    }

    /// Read one response header block (through the blank line), preserving
    /// any pipelined bytes after it in `pending` for the next call. This is
    /// what lets a split `100 Continue` + final status (or both coalesced
    /// in one segment) parse correctly either way.
    fn read_response_headers(stream: &mut SourceStream, pending: &mut Vec<u8>) -> Result<String> {
        let mut buf = [0u8; 4096];
        loop {
            if let Some(pos) = pending.windows(4).position(|w| w == b"\r\n\r\n") {
                let head: Vec<u8> = pending.drain(..pos + 4).collect();
                return Ok(String::from_utf8_lossy(&head).to_string());
            }
            if pending.len() > 64 * 1024 {
                return Err(CrabError::Audio("Icecast handshake header flood".into()));
            }
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
            pending.extend_from_slice(&buf[..n]);
        }
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
                assert!(req.starts_with("PUT /stream HTTP/1.1"), "{req}");
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

    /// A strict server sends `100 Continue` first and the final status in a
    /// LATER segment (separate writes + delay so they cannot coalesce).
    /// The client must wait for the real status instead of rejecting the 100.
    #[test]
    fn handshake_split_100_continue_then_200() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap().to_string();
        std::thread::spawn(move || {
            if let Ok((mut sock, _)) = listener.accept() {
                let mut head = Vec::new();
                let mut one = [0u8; 1];
                while !head.windows(4).any(|w| w == b"\r\n\r\n") && head.len() < 65536 {
                    match sock.read(&mut one) {
                        Ok(1) => head.push(one[0]),
                        _ => break,
                    }
                }
                let req = String::from_utf8_lossy(&head).to_string();
                assert!(req.starts_with("PUT /stream HTTP/1.1"), "{req}");
                let _ = sock.write_all(b"HTTP/1.1 100 Continue\r\n\r\n");
                let _ = sock.flush();
                // Force separate segments: the 200 must arrive later.
                std::thread::sleep(Duration::from_millis(400));
                let _ = sock.write_all(ok_response(0).as_bytes());
                let _ = sock.flush();
                std::thread::sleep(Duration::from_millis(300));
            }
        });
        let cfg = StreamConfig {
            host: addr.split(':').next().unwrap().to_string(),
            port: addr.rsplit(':').next().unwrap().parse().unwrap(),
            ..Default::default()
        };
        let (_src, proto) = IcecastSource::connect(&cfg).unwrap();
        assert_eq!(proto, HandshakeProtocol::Put);
    }

    /// Pre-2.4 servers close the PUT connection without a reply; the client
    /// must reconnect with legacy SOURCE — carrying the mount in the path.
    /// A lone `100 Continue` with nothing after it is already the go-ahead:
    /// Icecast only sends the final 200 at teardown, so waiting for one
    /// would stall a healthy connection until timeout.
    #[test]
    fn handshake_lone_100_is_enough() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap().to_string();
        std::thread::spawn(move || {
            if let Ok((mut sock, _)) = listener.accept() {
                let mut buf = vec![0u8; 8192];
                let _ = sock.read(&mut buf);
                let _ = sock.write_all(b"HTTP/1.1 100 Continue\r\n\r\n");
                let _ = sock.flush();
                // Then silence: no 200 ever follows on a live source.
                std::thread::sleep(Duration::from_secs(3));
            }
        });
        let cfg = StreamConfig {
            host: addr.split(':').next().unwrap().to_string(),
            port: addr.rsplit(':').next().unwrap().parse().unwrap(),
            ..Default::default()
        };
        let (_src, proto) = IcecastSource::connect(&cfg).unwrap();
        assert_eq!(proto, HandshakeProtocol::Put);
    }

    /// Total silence gets an optimistic proceed after the grace wait (some
    /// stacks only finalize once body bytes flow); a dead socket surfaces
    /// on the first send instead of hanging the connect forever.
    #[test]
    fn handshake_silence_proceeds_optimistically() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap().to_string();
        std::thread::spawn(move || {
            if let Ok((mut sock, _)) = listener.accept() {
                let mut buf = vec![0u8; 8192];
                let _ = sock.read(&mut buf);
                // Say nothing at all, but keep the socket open.
                std::thread::sleep(Duration::from_secs(15));
            }
        });
        let cfg = StreamConfig {
            host: addr.split(':').next().unwrap().to_string(),
            port: addr.rsplit(':').next().unwrap().parse().unwrap(),
            ..Default::default()
        };
        let t0 = std::time::Instant::now();
        let (_src, proto) = IcecastSource::connect(&cfg).unwrap();
        assert_eq!(proto, HandshakeProtocol::Put);
        assert!(
            t0.elapsed() >= Duration::from_secs(4),
            "should wait out the grace wait, took {:?}",
            t0.elapsed()
        );
    }

    /// A hangup with no reply at all is a definite rejection (legacy
    /// servers do this to PUT): must Err so the SOURCE fallback still runs.
    #[test]
    fn handshake_eof_is_rejection_not_optimism() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap().to_string();
        std::thread::spawn(move || {
            // Both attempts (PUT then SOURCE): read, then hang up silently.
            for _ in 0..2 {
                if let Ok((mut sock, _)) = listener.accept() {
                    let mut buf = vec![0u8; 8192];
                    let _ = sock.read(&mut buf);
                }
            }
        });
        let cfg = StreamConfig {
            host: addr.split(':').next().unwrap().to_string(),
            port: addr.rsplit(':').next().unwrap().parse().unwrap(),
            ..Default::default()
        };
        let err = IcecastSource::connect(&cfg).unwrap_err();
        assert!(err.to_string().contains("SOURCE"), "{err}");
    }

    #[test]
    fn handshake_source_fallback_carries_mount() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap().to_string();
        let (tx, rx) = std::sync::mpsc::channel();
        std::thread::spawn(move || {
            // Attempt 1 (PUT): hang up with no reply, like old servers.
            if let Ok((mut sock, _)) = listener.accept() {
                let mut buf = vec![0u8; 8192];
                let _ = sock.read(&mut buf);
            }
            // Attempt 2 (SOURCE): answer 200.
            if let Ok((mut sock, _)) = listener.accept() {
                let mut buf = vec![0u8; 8192];
                let n = sock.read(&mut buf).unwrap_or(0);
                let _ = tx.send(String::from_utf8_lossy(&buf[..n]).to_string());
                let _ = sock.write_all(ok_response(0).as_bytes());
                let _ = sock.flush();
                std::thread::sleep(Duration::from_millis(300));
            }
        });
        let cfg = StreamConfig {
            host: addr.split(':').next().unwrap().to_string(),
            port: addr.rsplit(':').next().unwrap().parse().unwrap(),
            ..Default::default()
        };
        let (_src, proto) = IcecastSource::connect(&cfg).unwrap();
        assert_eq!(proto, HandshakeProtocol::Source);
        let req = rx.recv_timeout(Duration::from_secs(15)).unwrap();
        assert!(req.starts_with("SOURCE /stream "), "{req}");
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
