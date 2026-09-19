//! Listener stats: Icecast's public `status-json.xsl`, or DNAS
//! `admin.cgi` (`mode=viewjson`) for Shoutcast.
//!
//! Optional telemetry, never an error state: every failure (disabled
//! admin pages, unreachable server, unparseable body) is `None` and the
//! UI shows "—". Transport mirrors `source.rs` (bounded connect, OS
//! TLS); the JSON shape is Icecast 2.4's, where `source` is one object
//! for a single mount and an array for several.

use std::io::{Read, Write};
use std::time::Duration;

use super::shoutcast::percent_encode;
use super::source::IcecastSource;
use super::StreamConfig;

/// Seconds between listener polls while live.
pub const LISTENER_POLL_SECS: u64 = 30;
/// One poll must stay far under the poll interval: a blackholed server
/// parks the worker, never the UI (the busy flag skips overlapping
/// rounds).
const POLL_TIMEOUT: Duration = Duration::from_secs(8);
/// Status bodies are kilobytes; cap the read so a rogue server can't
/// balloon the worker thread.
const MAX_BODY: u64 = 256 * 1024;

/// Listeners on `mount` inside a `status-json.xsl` body. `None` for
/// anything unparseable or a mount that isn't there — the caller
/// degrades, it never errors.
pub fn parse_listener_count(body: &str, mount: &str) -> Option<u64> {
    let v: serde_json::Value = serde_json::from_str(body).ok()?;
    let sources = v.get("icestats")?.get("source")?;
    let want = format!("/{}", mount.trim().trim_start_matches('/'));
    let list: Vec<&serde_json::Value> = match sources {
        serde_json::Value::Array(a) => a.iter().collect(),
        obj @ serde_json::Value::Object(_) => vec![obj],
        _ => return None,
    };
    list.iter().find_map(|s| {
        let url = s.get("listenurl")?.as_str()?;
        let after_scheme = url.split("://").nth(1)?;
        let path = &after_scheme[after_scheme.find('/')?..];
        (path == want).then(|| s.get("listeners")?.as_u64())?
    })
}

/// Blocking one-shot poll: this mount/stream's listener count.
/// `None` on any failure (optional stat).
pub fn fetch_listener_count(config: &StreamConfig) -> Option<u64> {
    let cfg = config.clone().sanitized();
    if cfg.protocol.is_shoutcast() {
        // DNAS honors the *source* password on admin.cgi (no admin
        // password needed); `sid` selects the stream on v2 servers.
        let path = format!(
            "/admin.cgi?sid={}&mode=viewjson&pass={}",
            cfg.sid,
            percent_encode(&cfg.password)
        );
        return match http_get(&cfg, &path) {
            Ok(body) => parse_shoutcast_listeners(&body),
            Err(e) => {
                tracing::debug!("Shoutcast listener poll failed: {e}");
                None
            }
        };
    }
    match http_get(&cfg, "/status-json.xsl") {
        Ok(body) => parse_listener_count(&body, &cfg.mount),
        Err(e) => {
            // Debug, not warn: a disabled admin page would spam every
            // poll otherwise, and "—" already tells the operator.
            tracing::debug!("Listener poll failed: {e}");
            None
        }
    }
}

/// Listeners inside a DNAS `viewjson` body: scan for
/// `"currentlisteners": N` without assuming the object shape (it
/// varies across DNAS builds).
pub fn parse_shoutcast_listeners(body: &str) -> Option<u64> {
    let key = "\"currentlisteners\"";
    let i = body.find(key)?;
    let after = body[i + key.len()..].split_once(':')?.1.trim_start();
    after
        .chars()
        .take_while(|c| c.is_ascii_digit())
        .collect::<String>()
        .parse()
        .ok()
}

fn http_get(cfg: &StreamConfig, path: &str) -> Result<String, String> {
    let addr = format!("{}:{}", cfg.host, cfg.port);
    let mut stream =
        IcecastSource::open(cfg, &addr, "Stream").map_err(|e| format!("connect {addr}: {e}"))?;
    // Tighten past the handshake-grade timeouts from `open`: one poll
    // must stay far under the poll interval even against a dripping
    // server (the busy flag skips overlapping rounds regardless).
    let socket = stream.socket();
    let _ = socket.set_read_timeout(Some(POLL_TIMEOUT));
    let _ = socket.set_write_timeout(Some(POLL_TIMEOUT));
    let req = format!(
        "GET {path} HTTP/1.0\r\nHost: {}\r\nUser-Agent: CrabBoss/0.1\r\nConnection: close\r\n\r\n",
        cfg.host
    );
    stream
        .write_all(req.as_bytes())
        .map_err(|e| format!("send: {e}"))?;
    let mut raw = Vec::new();
    std::io::Read::by_ref(&mut stream)
        .take(MAX_BODY)
        .read_to_end(&mut raw)
        .map_err(|e| format!("read: {e}"))?;
    let text = String::from_utf8(raw).map_err(|_| "non-UTF8 response".to_string())?;
    let (head, body) = text
        .split_once("\r\n\r\n")
        .ok_or_else(|| "no header/body split".to_string())?;
    let status = head.lines().next().unwrap_or_default();
    if !status.contains(" 200 ") {
        return Err(format!("server said: {status}"));
    }
    Ok(body.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    const SINGLE: &str = r#"{"icestats":{"source":{"listenurl":"http://cast:8000/stream","listeners":3,"listener_peak":9}}}"#;
    const MULTI: &str = r#"{"icestats":{"source":[{"listenurl":"http://cast:8000/stream","listeners":2},{"listenurl":"http://cast:8000/live","listeners":7}]}}"#;

    #[test]
    fn parse_reads_single_object_form() {
        assert_eq!(parse_listener_count(SINGLE, "/stream"), Some(3));
        assert_eq!(parse_listener_count(SINGLE, "stream"), Some(3));
    }

    #[test]
    fn parse_picks_the_right_mount_from_arrays() {
        assert_eq!(parse_listener_count(MULTI, "/stream"), Some(2));
        assert_eq!(parse_listener_count(MULTI, "/live"), Some(7));
    }

    #[test]
    fn parse_degrades_to_none() {
        assert_eq!(parse_listener_count(SINGLE, "/missing"), None);
        assert_eq!(parse_listener_count("not json", "/stream"), None);
        assert_eq!(parse_listener_count(r#"{"icestats":{}}"#, "/stream"), None);
        assert_eq!(
            parse_listener_count(r#"{"icestats":{"source":[]}}"#, "/stream"),
            None
        );
    }

    fn serve_once(body: &str, status: &str) -> u16 {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        let body = body.to_string();
        let status = status.to_string();
        std::thread::spawn(move || {
            let (mut sock, _) = listener.accept().unwrap();
            let mut buf = [0u8; 1024];
            let _ = std::io::Read::read(&mut sock, &mut buf);
            let resp = format!(
                "{status}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                body.len()
            );
            let _ = std::io::Write::write_all(&mut sock, resp.as_bytes());
        });
        port
    }

    fn cfg(port: u16) -> StreamConfig {
        StreamConfig {
            host: "127.0.0.1".into(),
            port,
            mount: "/stream".into(),
            ..Default::default()
        }
    }

    #[test]
    fn fetch_reads_canned_status_json() {
        let port = serve_once(SINGLE, "HTTP/1.0 200 OK");
        assert_eq!(fetch_listener_count(&cfg(port)), Some(3));
    }

    #[test]
    fn fetch_degrades_on_rejection_and_garbage() {
        let port = serve_once(SINGLE, "HTTP/1.0 403 Forbidden");
        assert_eq!(fetch_listener_count(&cfg(port)), None);
        let port = serve_once("nope", "HTTP/1.0 200 OK");
        assert_eq!(fetch_listener_count(&cfg(port)), None);
    }

    #[test]
    fn shoutcast_parse_reads_current_listeners() {
        let body = r#"{"shoutcast":{"source":{"sid":1,"currentlisteners":12,"peaklisteners":40}}}"#;
        assert_eq!(parse_shoutcast_listeners(body), Some(12));
        assert_eq!(parse_shoutcast_listeners(r#"{"a":1}"#), None);
        assert_eq!(parse_shoutcast_listeners("not json"), None);
    }

    #[test]
    fn shoutcast_fetch_hits_admin_cgi_with_sid() {
        use crate::stream::StreamProtocol;
        let body = r#"{"currentlisteners": 7}"#;
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        let body = body.to_string();
        let (tx, rx) = std::sync::mpsc::channel();
        std::thread::spawn(move || {
            let (mut sock, _) = listener.accept().unwrap();
            let mut buf = [0u8; 2048];
            let n = std::io::Read::read(&mut sock, &mut buf).unwrap_or(0);
            let _ = tx.send(String::from_utf8_lossy(&buf[..n]).to_string());
            let resp = format!(
                "HTTP/1.0 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                body.len()
            );
            let _ = std::io::Write::write_all(&mut sock, resp.as_bytes());
        });
        let cfg = StreamConfig {
            host: "127.0.0.1".into(),
            port,
            password: "secret".into(),
            protocol: StreamProtocol::ShoutcastV2,
            sid: 2,
            ..Default::default()
        };
        assert_eq!(fetch_listener_count(&cfg), Some(7));
        let req = rx.recv_timeout(std::time::Duration::from_secs(5)).unwrap();
        assert!(
            req.contains("GET /admin.cgi?sid=2&mode=viewjson&pass=secret HTTP/1.0"),
            "{req}"
        );
    }
}
