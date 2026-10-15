//! Just enough HTTP/1.1 for the front server, the admin page and the app API.

use serde_json::Value;
use std::time::Duration;
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};

pub const MAX_HEAD: usize = 16 * 1024;
pub const MAX_BODY: usize = 64 * 1024;
pub const HEAD_TIMEOUT: Duration = Duration::from_secs(15);

pub trait Io: AsyncRead + AsyncWrite + Unpin + Send {}
impl<T: AsyncRead + AsyncWrite + Unpin + Send> Io for T {}

#[derive(Debug, PartialEq)]
pub struct Request {
    pub method: String,
    /// Path and query as sent.
    pub target: String,
    pub path: String,
    pub headers: Vec<(String, String)>,
    pub body: Vec<u8>,
}

impl Request {
    pub fn header(&self, name: &str) -> Option<&str> {
        self.headers.iter().find(|(key, _)| key.eq_ignore_ascii_case(name)).map(|(_, value)| value.as_str())
    }

    pub fn query(&self, name: &str) -> Option<&str> {
        let (_, query) = self.target.split_once('?')?;
        query.split('&').filter_map(|pair| pair.split_once('=')).find(|(key, _)| *key == name).map(|(_, value)| value)
    }

    /// The relay password, the way iroh clients send it: `Authorization: Bearer` or `?token=`.
    pub fn bearer(&self) -> Option<&str> {
        let header = self.headers.iter().filter(|(key, _)| key.eq_ignore_ascii_case("authorization")).find_map(|(_, value)| {
            let (scheme, token) = value.split_once(' ')?;
            scheme.eq_ignore_ascii_case("bearer").then_some(token.trim())
        });
        header.or_else(|| self.query("token"))
    }

    pub fn json(&self) -> Value {
        serde_json::from_slice(&self.body).unwrap_or(Value::Null)
    }
}

/// Returns the request (without body), the head length and the declared body length.
pub fn parse_head(buffer: &[u8]) -> Option<(Request, usize, usize)> {
    let end = buffer.windows(4).position(|window| window == b"\r\n\r\n")?;
    let text = std::str::from_utf8(&buffer[..end]).ok()?;
    let mut lines = text.split("\r\n");
    let mut first = lines.next()?.split(' ');
    let method = first.next()?.to_string();
    let target = first.next()?.to_string();
    let path = target.split('?').next()?.to_string();
    let headers: Vec<(String, String)> = lines.filter_map(|line| line.split_once(':')).map(|(key, value)| (key.trim().to_string(), value.trim().to_string())).collect();
    let length = headers.iter().find(|(key, _)| key.eq_ignore_ascii_case("content-length")).and_then(|(_, value)| value.parse().ok()).unwrap_or(0);
    Some((Request { method, target, path, headers, body: Vec::new() }, end + 4, length))
}

pub struct Head {
    pub request: Request,
    pub head_len: usize,
    pub body_len: usize,
    /// Everything read so far, which may include the start of the body (or a WebSocket's first frames).
    pub buffer: Vec<u8>,
}

pub async fn read_head<S: Io>(stream: &mut S) -> Option<Head> {
    let mut buffer = Vec::new();
    let mut chunk = [0u8; 4096];
    loop {
        if let Some((request, head_len, body_len)) = parse_head(&buffer) {
            return Some(Head { request, head_len, body_len, buffer });
        }
        if buffer.len() > MAX_HEAD {
            return None;
        }
        let read = stream.read(&mut chunk).await.ok()?;
        if read == 0 {
            return None;
        }
        buffer.extend_from_slice(&chunk[..read]);
    }
}

/// Reads the rest of a small body into `head.request.body`.
pub async fn read_body<S: Io>(stream: &mut S, head: &mut Head) -> Option<()> {
    if head.body_len > MAX_BODY {
        return None;
    }
    let mut chunk = [0u8; 4096];
    while head.buffer.len() < head.head_len + head.body_len {
        let read = stream.read(&mut chunk).await.ok()?;
        if read == 0 {
            return None;
        }
        head.buffer.extend_from_slice(&chunk[..read]);
    }
    head.request.body = head.buffer[head.head_len..head.head_len + head.body_len].to_vec();
    Some(())
}

/// The buffered request with `Connection: close`, so the server it is passed to answers once and
/// hangs up instead of taking later requests on the same connection past the front's checks.
pub fn closing(head: &Head) -> Vec<u8> {
    let text = String::from_utf8_lossy(&head.buffer[..head.head_len - 4]);
    let mut out = String::new();
    for (index, line) in text.split("\r\n").enumerate() {
        let name = line.split(':').next().unwrap_or_default().trim();
        if index > 0 && (name.eq_ignore_ascii_case("connection") || name.eq_ignore_ascii_case("keep-alive")) {
            continue;
        }
        out.push_str(line);
        out.push_str("\r\n");
    }
    out.push_str("Connection: close\r\n\r\n");
    let mut bytes = out.into_bytes();
    bytes.extend_from_slice(&head.buffer[head.head_len..]);
    bytes
}

pub async fn reply<S: Io>(stream: &mut S, status: &str, content_type: &str, extra: &str, body: &[u8]) {
    let head = format!(
        "HTTP/1.1 {status}\r\nContent-Type: {content_type}\r\nContent-Length: {}\r\nCache-Control: no-store\r\nX-Frame-Options: DENY\r\nX-Content-Type-Options: nosniff\r\nReferrer-Policy: no-referrer\r\n{extra}Connection: close\r\n\r\n",
        body.len()
    );
    let _ = stream.write_all(head.as_bytes()).await;
    let _ = stream.write_all(body).await;
    let _ = stream.flush().await;
    let _ = stream.shutdown().await;
}

pub async fn reply_json<S: Io>(stream: &mut S, status: &str, value: &Value) {
    reply(stream, status, "application/json; charset=utf-8", "", value.to_string().as_bytes()).await;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn heads_queries_and_bearer_tokens() {
        let raw = b"POST /aic/v1/codes?x=1&token=abc HTTP/1.1\r\nAuthorization: Bearer secret\r\nContent-Length: 2\r\n\r\n{}";
        let (request, head, length) = parse_head(raw).unwrap();
        assert_eq!((request.method.as_str(), request.path.as_str(), length), ("POST", "/aic/v1/codes", 2));
        assert_eq!(&raw[head..], b"{}");
        assert_eq!(request.bearer(), Some("secret"));
        assert_eq!(request.query("x"), Some("1"));
        let (query_only, _, _) = parse_head(b"GET /relay?token=abc HTTP/1.1\r\n\r\n").unwrap();
        assert_eq!(query_only.bearer(), Some("abc"));
        let (none, _, _) = parse_head(b"GET /relay HTTP/1.1\r\nAuthorization: Basic eDp5\r\n\r\n").unwrap();
        assert_eq!(none.bearer(), None);
    }

    #[test]
    fn passed_on_requests_close_the_connection() {
        let raw = b"GET /ping HTTP/1.1\r\nHost: x\r\nConnection: keep-alive\r\nKeep-Alive: timeout=5\r\n\r\nextra";
        let (request, head_len, body_len) = parse_head(raw).unwrap();
        let head = Head { request, head_len, body_len, buffer: raw.to_vec() };
        assert_eq!(closing(&head), b"GET /ping HTTP/1.1\r\nHost: x\r\nConnection: close\r\n\r\nextra");
    }
}
