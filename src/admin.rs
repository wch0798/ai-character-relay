//! Admin page and API, protected by HTTP Basic auth.
//!
//! Served on `admin_bind` (127.0.0.1 by default, reached over SSH) and, when `https_admin` is on,
//! at `https://<domain>/admin/` through the front server.

use crate::gate::{constant_time_eq, Gate};
use crate::guard::Guard;
use crate::http::{self, Io, Request};
use crate::hub::Hub;
use base64::Engine;
use serde_json::{json, Value};
use std::net::IpAddr;
use std::time::Duration;
use tokio::net::TcpListener;

const PAGE: &str = include_str!("admin.html");
/// Browsers cannot add this header to cross-site form posts, which blocks CSRF.
pub const ADMIN_HEADER: &str = "x-aic-admin";

#[derive(Clone)]
pub struct Admin {
    pub gate: Gate,
    pub guard: Guard,
    pub hub: Hub,
    pub password: String,
    pub domain: String,
}

/// The local admin port. Loopback visitors already passed SSH, so they are never blocked.
pub async fn serve(listener: TcpListener, admin: Admin) {
    while let Ok((mut stream, peer)) = listener.accept().await {
        let admin = admin.clone();
        tokio::spawn(async move {
            let _ = tokio::time::timeout(Duration::from_secs(20), async {
                let Some(mut head) = http::read_head(&mut stream).await else { return };
                if http::read_body(&mut stream, &mut head).await.is_none() {
                    return;
                }
                let ip = peer.ip();
                respond(&mut stream, &admin, &head.request, &head.request.path, ip, !ip.is_loopback()).await;
            })
            .await;
        });
    }
}

/// Accepts any user name; only the password matters.
pub fn authorized(request: &Request, password: &str) -> bool {
    let Some(encoded) = request.header("authorization").and_then(|value| value.strip_prefix("Basic ")) else { return false };
    let Ok(decoded) = base64::engine::general_purpose::STANDARD.decode(encoded.trim()) else { return false };
    let Ok(text) = String::from_utf8(decoded) else { return false };
    text.split_once(':').is_some_and(|(_, given)| constant_time_eq(given, password))
}

pub fn route(admin: &Admin, method: &str, path: &str, body: &Value) -> (&'static str, Value) {
    let name = body.get("name").and_then(Value::as_str).unwrap_or_default();
    match (method, path) {
        ("GET", "/api/status") => {
            let mut status = admin.gate.status();
            status["domain"] = json!(admin.domain);
            status["guard"] = admin.guard.status();
            status["hub"] = admin.hub.status();
            ("200 OK", status)
        }
        ("GET", "/api/tokens") => ("200 OK", json!({ "tokens": admin.gate.tokens() })),
        ("POST", "/api/tokens") => match admin.gate.add_token(name) {
            Ok(token) => ("200 OK", json!({ "token": token })),
            Err(error) => ("400 Bad Request", json!({ "error": error })),
        },
        ("POST", "/api/tokens/remove") => match admin.gate.remove_token(name) {
            Ok(true) => ("200 OK", json!({ "removed": true })),
            Ok(false) => ("404 Not Found", json!({ "error": "그 이름의 암호가 없습니다." })),
            Err(error) => ("500 Internal Server Error", json!({ "error": error })),
        },
        ("POST", "/api/unblock") => match body.get("ip").and_then(Value::as_str).and_then(|ip| ip.trim().parse::<IpAddr>().ok()) {
            Some(ip) if admin.guard.unblock(ip) => ("200 OK", json!({ "unblocked": true })),
            Some(_) => ("404 Not Found", json!({ "error": "막혀 있는 주소가 아닙니다." })),
            None => ("400 Bad Request", json!({ "error": "IP 주소 형식이 아닙니다." })),
        },
        _ => ("404 Not Found", json!({ "error": "not found" })),
    }
}

/// Answers one admin request. `path` is relative to the admin root (starts with `/`).
pub async fn respond<S: Io>(stream: &mut S, admin: &Admin, request: &Request, path: &str, ip: IpAddr, count_failures: bool) {
    if count_failures && admin.guard.is_blocked(ip) {
        return http::reply(stream, "429 Too Many Requests", "text/plain; charset=utf-8", "", "잠시 막혔습니다. 30분 뒤에 다시 시도하세요.".as_bytes()).await;
    }
    if !authorized(request, &admin.password) {
        if count_failures {
            admin.guard.fail(ip, "admin");
        }
        tokio::time::sleep(Duration::from_millis(500)).await;
        return http::reply(stream, "401 Unauthorized", "text/plain; charset=utf-8", "WWW-Authenticate: Basic realm=\"AI Character Relay\", charset=\"UTF-8\"\r\n", "관리자 암호가 필요합니다.".as_bytes()).await;
    }
    if request.method == "GET" && path == "/" {
        return http::reply(
            stream,
            "200 OK",
            "text/html; charset=utf-8",
            "Content-Security-Policy: default-src 'self'; script-src 'unsafe-inline'; style-src 'unsafe-inline'\r\n",
            PAGE.as_bytes(),
        )
        .await;
    }
    if request.method == "POST" && request.header(ADMIN_HEADER) != Some("1") {
        return http::reply_json(stream, "403 Forbidden", &json!({ "error": "missing admin header" })).await;
    }
    let (status, value) = route(admin, &request.method, path, &request.json());
    http::reply_json(stream, status, &value).await;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn basic_auth_checks_only_the_password() {
        let (request, _, _) = http::parse_head(b"POST /api/tokens HTTP/1.1\r\nAuthorization: Basic YWRtaW46c2VjcmV0LXBhc3N3b3Jk\r\n\r\n").unwrap();
        assert!(authorized(&request, "secret-password"));
        assert!(!authorized(&request, "secret-passwore"));
        let (anonymous, _, _) = http::parse_head(b"GET / HTTP/1.1\r\n\r\n").unwrap();
        assert!(!authorized(&anonymous, "secret-password"));
    }
}
