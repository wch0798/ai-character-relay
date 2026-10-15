//! The public HTTP(S) server.
//!
//! The iroh relay itself runs on a loopback port. This server sits in front of it so that it can
//! see real visitor addresses (to block password guessing), check the relay password before a
//! connection reaches the relay, and answer the app's own requests under `/aic/v1/`.
//!
//! Relay traffic is passed through byte for byte after the request line, so the relay protocol
//! (and its end-to-end encryption) is untouched.

use crate::admin::{self, Admin};
use crate::gate::Gate;
use crate::guard::Guard;
use crate::http::{self, Head, Io};
use crate::hub::{Hub, MAX_CODE_MINUTES};
use crate::web;
use iroh_base::EndpointId;
use serde_json::{json, Value};
use std::net::{IpAddr, SocketAddr};
use std::str::FromStr;
use std::sync::Arc;
use tokio::net::{TcpListener, TcpStream};

/// What the relay answers itself without a password: health checks and the captive-portal probe.
const RELAY_PUBLIC: &[&str] = &["/", "/index.html", "/ping", "/healthz", "/generate_204", "/robots.txt"];
const RELAY_PATH: &str = "/relay";
const MAX_PRESENCE_IDS: usize = 32;
/// Signs of a proxy in front of this server (never trusted for the visitor address).
const PROXY_HEADERS: &[&str] = &["x-forwarded-for", "x-real-ip", "forwarded", "cf-connecting-ip"];

pub struct Front {
    pub gate: Gate,
    pub guard: Guard,
    pub hub: Hub,
    /// The loopback address of the iroh relay.
    pub relay: SocketAddr,
    /// `https://<domain>/admin/`, when enabled in the config.
    pub admin: Option<Admin>,
    /// Serve the phone page at `/app/`.
    pub phone_web: bool,
    /// Plain HTTP carries everything (local tests only).
    pub dev: bool,
}

/// Let's Encrypt (answers its TLS-ALPN-01 checks itself) or a certificate from files.
#[derive(Clone)]
pub enum Tls {
    Acme(tokio_rustls_acme::AcmeAcceptor, Arc<rustls::ServerConfig>),
    Files(tokio_rustls::TlsAcceptor),
}

impl Tls {
    async fn accept(&self, stream: TcpStream) -> Option<tokio_rustls::server::TlsStream<TcpStream>> {
        match self {
            Tls::Acme(acceptor, config) => {
                // None: a Let's Encrypt TLS-ALPN-01 check, already answered by the acceptor.
                let start = acceptor.accept(stream).await.ok()??;
                start.into_stream(config.clone()).await.ok()
            }
            Tls::Files(acceptor) => acceptor.accept(stream).await.ok(),
        }
    }
}

pub async fn serve_plain(listener: TcpListener, front: Arc<Front>) {
    while let Ok((stream, peer)) = listener.accept().await {
        let front = front.clone();
        tokio::spawn(async move { front.handle(stream, peer, false).await });
    }
}

pub async fn serve_tls(listener: TcpListener, front: Arc<Front>, tls: Tls) {
    while let Ok((stream, peer)) = listener.accept().await {
        let (front, tls) = (front.clone(), tls.clone());
        tokio::spawn(async move {
            if let Ok(Some(tls)) = tokio::time::timeout(http::HEAD_TIMEOUT, tls.accept(stream)).await {
                front.handle(tls, peer, true).await;
            }
        });
    }
}

impl Front {
    pub async fn handle<S: Io>(&self, mut stream: S, peer: SocketAddr, secure: bool) {
        let ip = peer.ip();
        let Ok(Some(mut head)) = tokio::time::timeout(http::HEAD_TIMEOUT, http::read_head(&mut stream)).await else { return };
        if PROXY_HEADERS.iter().any(|name| head.request.header(name).is_some()) {
            self.guard.note_proxy_headers();
        }
        if self.guard.is_blocked(ip) {
            return http::reply_json(&mut stream, "429 Too Many Requests", &json!({ "error": "blocked", "message": "틀린 암호를 너무 많이 보내 잠시 막혔습니다. 30분 뒤에 다시 시도하세요." })).await;
        }
        let path = head.request.path.clone();
        let carries_secrets = secure || self.dev;
        if path.starts_with("/aic/v1/") && carries_secrets {
            if tokio::time::timeout(http::HEAD_TIMEOUT, http::read_body(&mut stream, &mut head)).await.ok().flatten().is_none() {
                return http::reply_json(&mut stream, "413 Payload Too Large", &json!({ "error": "bad request" })).await;
            }
            let (status, value) = self.api(&head, ip);
            return http::reply_json(&mut stream, status, &value).await;
        }
        if let Some(admin) = self.admin.as_ref().filter(|_| carries_secrets) {
            if path == "/admin" {
                return http::reply(&mut stream, "308 Permanent Redirect", "text/plain; charset=utf-8", "Location: /admin/\r\n", b"").await;
            }
            if let Some(rest) = path.strip_prefix("/admin/") {
                if tokio::time::timeout(http::HEAD_TIMEOUT, http::read_body(&mut stream, &mut head)).await.ok().flatten().is_none() {
                    return;
                }
                let rest = format!("/{rest}");
                return admin::respond(&mut stream, admin, &head.request, &rest, ip, true).await;
            }
        }
        if self.phone_web && carries_secrets && head.request.method == "GET" {
            if path == "/" {
                return http::reply(&mut stream, "302 Found", "text/plain; charset=utf-8", "Location: /app/\r\n", b"").await;
            }
            if web::handles(&path) {
                return web::respond(&mut stream, &head.request).await;
            }
        }
        if path == RELAY_PATH && carries_secrets {
            if self.gate.peek(head.request.bearer()).is_none() {
                self.guard.fail(ip, "token");
                return http::reply_json(&mut stream, "401 Unauthorized", &json!({ "error": "access token required" })).await;
            }
            return self.pass_to_relay(stream, head.buffer).await;
        }
        if RELAY_PUBLIC.contains(&path.as_str()) && head.request.method == "GET" {
            return self.pass_to_relay(stream, http::closing(&head)).await;
        }
        http::reply_json(&mut stream, "404 Not Found", &json!({ "error": "not found" })).await;
    }

    async fn pass_to_relay<S: Io>(&self, mut stream: S, request: Vec<u8>) {
        let Ok(mut relay) = TcpStream::connect(self.relay).await else {
            return http::reply_json(&mut stream, "502 Bad Gateway", &json!({ "error": "relay unavailable" })).await;
        };
        let _ = relay.set_nodelay(true);
        if tokio::io::AsyncWriteExt::write_all(&mut relay, &request).await.is_err() {
            return;
        }
        let _ = tokio::io::copy_bidirectional(&mut stream, &mut relay).await;
    }

    /// The app's own requests. All need a valid relay password.
    fn api(&self, head: &Head, ip: IpAddr) -> (&'static str, Value) {
        let request = &head.request;
        let Some(token) = self.gate.peek(request.bearer()) else {
            self.guard.fail(ip, "token");
            return ("401 Unauthorized", json!({ "error": "access token required" }));
        };
        let body = request.json();
        let id = body.get("id").and_then(Value::as_str).unwrap_or_default();
        match (request.method.as_str(), request.path.as_str()) {
            ("GET", "/aic/v1/info") => ("200 OK", json!({ "version": env!("CARGO_PKG_VERSION"), "features": ["codes", "presence", "paths"], "codeMinutes": MAX_CODE_MINUTES })),
            ("POST", "/aic/v1/codes") => {
                let sealed = body.get("sealed").and_then(Value::as_str).unwrap_or_default();
                let minutes = body.get("minutes").and_then(Value::as_u64).unwrap_or(MAX_CODE_MINUTES);
                match self.hub.put_code(&token, id, sealed, minutes) {
                    Ok(()) => ("200 OK", json!({ "ok": true })),
                    Err((status, error)) => (status, json!({ "error": error })),
                }
            }
            ("POST", "/aic/v1/codes/take") => match self.hub.take_code(id) {
                Some(sealed) => ("200 OK", json!({ "sealed": sealed })),
                None => {
                    self.guard.fail(ip, "code");
                    ("404 Not Found", json!({ "error": "no such code" }))
                }
            },
            ("POST", "/aic/v1/codes/cancel") => ("200 OK", json!({ "cancelled": self.hub.cancel_code(&token, id) })),
            ("POST", "/aic/v1/presence") => {
                let ids: Vec<String> = body.get("ids").and_then(Value::as_array).into_iter().flatten().filter_map(Value::as_str).take(MAX_PRESENCE_IDS).map(str::to_string).collect();
                let parsed: Vec<(String, EndpointId)> = ids.into_iter().filter_map(|text| EndpointId::from_str(&text).ok().map(|id| (text, id))).collect();
                let states = self.gate.presence(&parsed.iter().map(|(_, id)| *id).collect::<Vec<_>>());
                let devices: serde_json::Map<String, Value> = parsed.into_iter().zip(states).map(|((text, _), (online, last_seen))| (text, json!({ "online": online, "lastSeen": last_seen }))).collect();
                ("200 OK", json!({ "devices": devices }))
            }
            ("POST", "/aic/v1/paths") => match body.get("path").and_then(Value::as_str) {
                Some("direct") => {
                    self.hub.report_path(true);
                    ("200 OK", json!({ "ok": true }))
                }
                Some("relay") => {
                    self.hub.report_path(false);
                    ("200 OK", json!({ "ok": true }))
                }
                _ => ("400 Bad Request", json!({ "error": "path must be direct or relay" })),
            },
            _ => ("404 Not Found", json!({ "error": "not found" })),
        }
    }
}
