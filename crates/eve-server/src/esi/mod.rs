//! `esi` — EVE Swagger Interface (ESI) HTTP/1.1 server, OpenAPI 3.0 router, OAuth2 Bearer
//! scope guard, conditional `ETag` caching, `X-Pages` pagination, and schema validator (`ESI-2`).

pub mod char_social_routes;
pub mod character_routes;
pub mod corp_alliance_routes;
pub mod economy_industry_routes;
pub mod economy_routes;
pub mod fw_universe_routes;
pub mod openapi;
pub mod router;
pub mod runtime_state;
pub mod sample_routes;
pub mod sov_fw_routes;
pub mod universe_routes;

pub use openapi::OpenApiSpec;
pub use router::EsiRouter;

use crate::esi::sample_routes::civil_from_unix;
use crate::router::ServerContext;
use serde_json::Value;
use std::collections::HashMap;
use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Duration;
use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader};
use tokio::net::{TcpListener, TcpStream};
use tokio::task::JoinHandle;
use tokio::time::timeout;

const HTTP_IO_TIMEOUT: Duration = Duration::from_secs(15);
const MAX_HTTP_BODY_BYTES: usize = 2 * 1024 * 1024;
const MAX_HTTP_HEADER_LINE_BYTES: u64 = 8 * 1024;
const MAX_HTTP_HEADERS: usize = 64;

/// Parsed HTTP/1.1 request targeting an ESI endpoint.
#[derive(Debug, Clone)]
pub struct EsiHttpRequest {
    pub method: String,
    pub raw_target: String,
    pub headers: HashMap<String, String>,
    pub body: Vec<u8>,
    pub peer_ip: String,
}

impl EsiHttpRequest {
    pub fn new(method: impl Into<String>, raw_target: impl Into<String>) -> Self {
        Self {
            method: method.into(),
            raw_target: raw_target.into(),
            headers: HashMap::new(),
            body: Vec::new(),
            peer_ip: "127.0.0.1".to_string(),
        }
    }

    pub fn with_peer_ip(mut self, peer_ip: impl Into<String>) -> Self {
        self.peer_ip = peer_ip.into();
        self
    }

    pub fn with_header(mut self, name: impl Into<String>, value: impl Into<String>) -> Self {
        self.headers.insert(name.into(), value.into());
        self
    }

    pub fn with_bearer_token(self, token: &str) -> Self {
        self.with_header("Authorization", format!("Bearer {token}"))
    }

    pub fn with_body(mut self, body: Vec<u8>) -> Self {
        self.body = body;
        self
    }

    /// Case-insensitive header lookup.
    pub fn header(&self, name: &str) -> Option<&str> {
        self.headers
            .iter()
            .find(|(k, _)| k.eq_ignore_ascii_case(name))
            .map(|(_, v)| v.as_str())
    }
}

/// Serialized HTTP/1.1 response returned by the ESI router.
#[derive(Debug, Clone)]
pub struct EsiHttpResponse {
    pub status_code: u16,
    pub headers: HashMap<String, String>,
    pub body: Vec<u8>,
}

impl EsiHttpResponse {
    /// Case-insensitive header lookup.
    pub fn header(&self, name: &str) -> Option<&str> {
        self.headers
            .iter()
            .find(|(k, _)| k.eq_ignore_ascii_case(name))
            .map(|(_, v)| v.as_str())
    }

    /// Parses the response body as JSON.
    pub fn json(&self) -> Result<Value, String> {
        serde_json::from_slice(&self.body).map_err(|e| format!("Invalid JSON body: {e}"))
    }

    pub fn reason_phrase(&self) -> &'static str {
        http_reason_phrase(self.status_code)
    }

    /// Serializes the response into a wire-level HTTP/1.1 byte buffer.
    pub fn to_http11_bytes(&self) -> Vec<u8> {
        let reason = self.reason_phrase();
        let mut head = format!("HTTP/1.1 {} {}\r\n", self.status_code, reason);
        let mut sorted_hdrs: Vec<(&String, &String)> = self.headers.iter().collect();
        sorted_hdrs.sort_by(|(a, _), (b, _)| a.cmp(b));
        for (k, v) in sorted_hdrs {
            if !k.eq_ignore_ascii_case("content-length") && !k.eq_ignore_ascii_case("connection") {
                head.push_str(&format!("{k}: {v}\r\n"));
            }
        }
        let content_len = if self.status_code == 204 || self.status_code == 304 {
            0
        } else {
            self.body.len()
        };
        head.push_str(&format!("Content-Length: {content_len}\r\n"));
        head.push_str("Connection: close\r\n\r\n");

        let mut out = head.into_bytes();
        if content_len > 0 {
            out.extend_from_slice(&self.body);
        }
        out
    }
}

pub fn http_reason_phrase(status_code: u16) -> &'static str {
    match status_code {
        200 => "OK",
        201 => "Created",
        204 => "No Content",
        304 => "Not Modified",
        400 => "Bad Request",
        401 => "Unauthorized",
        403 => "Forbidden",
        404 => "Not Found",
        405 => "Method Not Allowed",
        413 => "Payload Too Large",
        420 => "Error Limited",
        422 => "Unprocessable Entity",
        500 => "Internal Server Error",
        501 => "Not Implemented",
        503 => "Service Unavailable",
        _ => "Response",
    }
}

/// Computes a deterministic 64-bit FNV-1a strong ETag (`"\"<16-hex-digits>\""`) over `body`.
pub fn compute_etag(body: &[u8]) -> String {
    let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
    for &b in body {
        hash ^= b as u64;
        hash = hash.wrapping_mul(0x0000_0100_0000_01b3);
    }
    format!("\"{hash:016x}\"")
}

/// Evaluates whether an `If-None-Match` header value matches `current_etag` (supporting `*`,
/// comma-separated lists, and `W/` weak prefixes per RFC 7232).
pub fn if_none_match_matches(header_val: &str, current_etag: &str) -> bool {
    let trimmed = header_val.trim();
    if trimmed == "*" {
        return true;
    }
    let norm_current = current_etag.trim().strip_prefix("W/").unwrap_or(current_etag.trim());
    for candidate in trimmed.split(',') {
        let cand = candidate.trim();
        let norm_cand = cand.strip_prefix("W/").unwrap_or(cand);
        if norm_cand == norm_current {
            return true;
        }
    }
    false
}

/// Formats a Unix epoch timestamp (seconds) as an RFC 7231 HTTP-date (`Day, DD Mon YYYY HH:MM:SS GMT`).
pub fn format_http_date(unix_secs: u64) -> String {
    const DOW: [&str; 7] = ["Thu", "Fri", "Sat", "Sun", "Mon", "Tue", "Wed"];
    const MON: [&str; 12] = [
        "Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec",
    ];
    let (year, month, day, hour, min, sec) = civil_from_unix(unix_secs);
    let dow_idx = ((unix_secs / 86_400) % 7) as usize;
    let mon_str = MON.get((month.saturating_sub(1)) as usize).copied().unwrap_or("Jan");
    format!(
        "{}, {:02} {} {:04} {:02}:{:02}:{:02} GMT",
        DOW[dow_idx], day, mon_str, year, hour, min, sec
    )
}

/// Spawned ESI HTTP/1.1 TCP server handle.
pub struct EsiServer;

impl EsiServer {
    /// Binds an ESI HTTP/1.1 TCP listener on `bind_addr` (e.g. `"127.0.0.1:0"`) and spawns the
    /// accept loop in the background.
    pub async fn spawn(
        ctx: Arc<ServerContext>,
        bind_addr: &str,
    ) -> std::io::Result<(SocketAddr, Arc<EsiRouter>, JoinHandle<()>)> {
        let router = Arc::new(EsiRouter::new(ctx));
        let (addr, handle) = Self::spawn_with_router(Arc::clone(&router), bind_addr).await?;
        Ok((addr, router, handle))
    }

    /// Binds an ESI HTTP/1.1 TCP listener using an existing `Arc<EsiRouter>`.
    pub async fn spawn_with_router(
        router: Arc<EsiRouter>,
        bind_addr: &str,
    ) -> std::io::Result<(SocketAddr, JoinHandle<()>)> {
        let listener = TcpListener::bind(bind_addr).await?;
        let local_addr = listener.local_addr()?;
        let handle = tokio::spawn(async move {
            while let Ok((stream, _peer)) = listener.accept().await {
                let r = Arc::clone(&router);
                tokio::spawn(async move {
                    let _ = handle_esi_http_connection(stream, r).await;
                });
            }
        });
        Ok((local_addr, handle))
    }
}

/// Handles a single incoming HTTP/1.1 TCP connection on the ESI listener.
pub async fn handle_esi_http_connection(
    mut stream: TcpStream,
    router: Arc<EsiRouter>,
) -> std::io::Result<()> {
    let read_res = timeout(HTTP_IO_TIMEOUT, read_http11_request(&mut stream))
        .await
        .map_err(|_| std::io::Error::new(std::io::ErrorKind::TimedOut, "HTTP read timeout"))?;
    let resp = match read_res {
        Ok(Some(req)) => router.handle_request(&req),
        Ok(None) => return Ok(()),
        Err(e) if e.kind() == std::io::ErrorKind::InvalidData => {
            let msg = e.to_string();
            let status_code = if msg.contains("Content-Length exceeds") {
                413
            } else {
                400
            };
            let mut headers = HashMap::new();
            headers.insert(
                "Content-Type".to_string(),
                "application/json; charset=UTF-8".to_string(),
            );
            let body = serde_json::to_vec(&serde_json::json!({
                "error": msg,
                "status": status_code,
            }))
            .unwrap_or_default();
            EsiHttpResponse {
                status_code,
                headers,
                body,
            }
        }
        Err(e) => return Err(e),
    };
    let wire_bytes = resp.to_http11_bytes();
    timeout(HTTP_IO_TIMEOUT, stream.write_all(&wire_bytes))
        .await
        .map_err(|_| std::io::Error::new(std::io::ErrorKind::TimedOut, "HTTP write timeout"))??;
    let _ = stream.shutdown().await;
    Ok(())
}

async fn read_bounded_line(reader: &mut BufReader<&mut TcpStream>) -> std::io::Result<String> {
    let mut line = String::new();
    let mut limited = reader.take(MAX_HTTP_HEADER_LINE_BYTES);
    let n = limited.read_line(&mut line).await?;
    if n as u64 >= MAX_HTTP_HEADER_LINE_BYTES && !line.ends_with('\n') {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            "HTTP header line exceeds maximum allowed length",
        ));
    }
    Ok(line)
}

async fn read_http11_request(stream: &mut TcpStream) -> std::io::Result<Option<EsiHttpRequest>> {
    let peer_ip = stream
        .peer_addr()
        .map(|a| a.ip().to_string())
        .unwrap_or_else(|_| "127.0.0.1".to_string());
    let mut reader = BufReader::new(stream);
    let request_line = read_bounded_line(&mut reader).await?;
    if request_line.is_empty() {
        return Ok(None);
    }
    let trimmed_line = request_line.trim_end_matches(&['\r', '\n'][..]);
    let mut parts = trimmed_line.split_whitespace();
    let method = parts.next().unwrap_or("GET").to_uppercase();
    let raw_target = parts.next().unwrap_or("/").to_string();

    let mut headers = HashMap::new();
    let mut content_len = 0usize;
    let mut saw_end_of_headers = false;
    for _ in 0..=MAX_HTTP_HEADERS {
        let line = read_bounded_line(&mut reader).await?;
        if line.is_empty() {
            saw_end_of_headers = true;
            break;
        }
        let clean = line.trim_end_matches(&['\r', '\n'][..]);
        if clean.is_empty() {
            saw_end_of_headers = true;
            break;
        }
        if headers.len() >= MAX_HTTP_HEADERS {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                "HTTP header count exceeds maximum allowed",
            ));
        }
        if let Some((k, v)) = clean.split_once(':') {
            let key = k.trim().to_string();
            let val = v.trim().to_string();
            if key.eq_ignore_ascii_case("content-length") {
                let parsed = val.parse::<usize>().map_err(|_| {
                    std::io::Error::new(
                        std::io::ErrorKind::InvalidData,
                        "Invalid HTTP Content-Length header",
                    )
                })?;
                if parsed > MAX_HTTP_BODY_BYTES {
                    return Err(std::io::Error::new(
                        std::io::ErrorKind::InvalidData,
                        "HTTP Content-Length exceeds maximum allowed body size",
                    ));
                }
                content_len = parsed;
            }
            headers.insert(key, val);
        }
    }
    if !saw_end_of_headers {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            "HTTP header count exceeds maximum allowed",
        ));
    }

    let mut body = vec![0u8; content_len];
    if content_len > 0 {
        reader.read_exact(&mut body).await?;
    }

    Ok(Some(EsiHttpRequest {
        method,
        raw_target,
        headers,
        body,
        peer_ip,
    }))
}

/// Sends an HTTP/1.1 request over TCP to `addr` and parses the `EsiHttpResponse`.
pub async fn send_raw_esi_request(
    addr: SocketAddr,
    req: &EsiHttpRequest,
) -> std::io::Result<EsiHttpResponse> {
    let mut stream = TcpStream::connect(addr).await?;
    let mut wire = format!("{} {} HTTP/1.1\r\nHost: {}\r\n", req.method, req.raw_target, addr);
    for (k, v) in &req.headers {
        if !k.eq_ignore_ascii_case("host")
            && !k.eq_ignore_ascii_case("content-length")
            && !k.eq_ignore_ascii_case("connection")
        {
            wire.push_str(&format!("{k}: {v}\r\n"));
        }
    }
    let cl_val = req
        .header("Content-Length")
        .map(str::to_string)
        .unwrap_or_else(|| req.body.len().to_string());
    wire.push_str(&format!("Content-Length: {cl_val}\r\n"));
    wire.push_str("Connection: close\r\n\r\n");

    let mut out = wire.into_bytes();
    out.extend_from_slice(&req.body);
    stream.write_all(&out).await?;

    let mut reader = BufReader::new(stream);
    let mut status_line = String::new();
    reader.read_line(&mut status_line).await?;
    let status_code = status_line
        .split_whitespace()
        .nth(1)
        .and_then(|s| s.parse::<u16>().ok())
        .unwrap_or(500);

    let mut headers = HashMap::new();
    loop {
        let mut line = String::new();
        let n = reader.read_line(&mut line).await?;
        if n == 0 {
            break;
        }
        let clean = line.trim_end_matches(&['\r', '\n'][..]);
        if clean.is_empty() {
            break;
        }
        if let Some((k, v)) = clean.split_once(':') {
            headers.insert(k.trim().to_string(), v.trim().to_string());
        }
    }

    let mut body = Vec::new();
    reader.read_to_end(&mut body).await?;
    Ok(EsiHttpResponse {
        status_code,
        headers,
        body,
    })
}
