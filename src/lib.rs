//! AI Character relay server.
//!
//! A stock iroh relay (the same protocol the app's iroh endpoints speak), behind a small front
//! server that adds:
//! - access passwords that can be added and removed while running,
//! - blocking of addresses that keep guessing passwords or codes,
//! - 6-digit connection codes, online/offline state and direct-vs-relayed statistics for the app,
//! - an admin page (local port, optionally also `https://<domain>/admin/`),
//! - a phone page at `https://<domain>/app/` for chatting with a home computer's AI,
//! - Let's Encrypt certificates that renew themselves.

pub mod admin;
pub mod config;
pub mod front;
pub mod gate;
pub mod guard;
pub mod http;
pub mod hub;
pub mod web;

use admin::Admin;
use front::Front;
use iroh_relay::server::{QuicConfig, RelayConfig, Server, ServerConfig};
use std::net::SocketAddr;
use std::sync::Arc;
use tokio::net::TcpListener;
use tokio::task::JoinHandle;
use tokio_rustls_acme::caches::DirCache;
use tokio_rustls_acme::AcmeConfig;
use tokio_stream::StreamExt;

pub struct Running {
    relay: Server,
    tasks: Vec<JoinHandle<()>>,
    pub http_addr: SocketAddr,
    pub https_addr: Option<SocketAddr>,
    pub admin_addr: SocketAddr,
    pub gate: gate::Gate,
    pub guard: guard::Guard,
    pub hub: hub::Hub,
}

impl Running {
    /// Waits until the relay stops on its own (it shouldn't).
    pub fn quic_addr(&self) -> Option<SocketAddr> {
        self.relay.quic_addr()
    }

    pub async fn join(&mut self) -> String {
        format!("{:?}", self.relay.join().await)
    }

    pub async fn shutdown(self) -> Result<(), String> {
        for task in &self.tasks {
            task.abort();
        }
        self.relay.shutdown().await.map_err(|error| error.to_string())
    }
}

fn provider() -> Arc<rustls::crypto::CryptoProvider> {
    Arc::new(rustls::crypto::ring::default_provider())
}

async fn bind(addr: SocketAddr, what: &str) -> Result<TcpListener, String> {
    TcpListener::bind(addr).await.map_err(|error| format!("{what} 포트 {addr} 를 열지 못했습니다: {error} (다른 프로그램이 쓰고 있거나 권한이 없습니다)"))
}

fn tls_builder() -> Result<rustls::ConfigBuilder<rustls::ServerConfig, rustls::WantsVerifier>, String> {
    rustls::ServerConfig::builder_with_provider(provider()).with_safe_default_protocol_versions().map_err(|error| error.to_string())
}

fn certificate_files(cert: &std::path::Path, key: &std::path::Path) -> Result<(front::Tls, Arc<rustls::ServerConfig>), String> {
    use rustls::pki_types::pem::PemObject;
    use rustls::pki_types::{CertificateDer, PrivateKeyDer};
    let chain = CertificateDer::pem_file_iter(cert)
        .and_then(|certs| certs.collect::<Result<Vec<_>, _>>())
        .map_err(|error| format!("인증서 파일 {} 을(를) 읽지 못했습니다: {error}", cert.display()))?;
    let key = PrivateKeyDer::from_pem_file(key).map_err(|error| format!("키 파일 {} 을(를) 읽지 못했습니다: {error}", key.display()))?;
    let tls = Arc::new(tls_builder()?.with_no_client_auth().with_single_cert(chain, key).map_err(|error| format!("인증서와 키가 맞지 않습니다: {error}"))?);
    Ok((front::Tls::Files(tokio_rustls::TlsAcceptor::from(tls.clone())), tls))
}

/// Starts the certificate manager; returns the TLS acceptor and the server config both HTTPS and QUIC use.
fn certificates(config: &config::Config, tasks: &mut Vec<JoinHandle<()>>) -> Result<(front::Tls, Arc<rustls::ServerConfig>), String> {
    if let (Some(cert), Some(key)) = (&config.tls_cert, &config.tls_key) {
        return certificate_files(cert, key);
    }
    let mut state = AcmeConfig::new([config.domain.clone()])
        .contact_push(format!("mailto:{}", config.email))
        .cache(DirCache::new(config.cert_dir()))
        .directory_lets_encrypt(config.production_certs)
        .state();
    let tls = Arc::new(tls_builder()?.with_no_client_auth().with_cert_resolver(state.resolver()));
    let acceptor = state.acceptor();
    tasks.push(tokio::spawn(async move {
        while let Some(event) = state.next().await {
            match event {
                Ok(ok) => eprintln!("인증서: {ok:?}"),
                Err(error) => eprintln!("인증서 오류 (DNS 와 80/443 포트를 확인하세요): {error:?}"),
            }
        }
    }));
    Ok((front::Tls::Acme(acceptor, tls.clone()), tls))
}

pub async fn spawn(config: &config::Config, gate: gate::Gate) -> Result<Running, String> {
    let guard = guard::Guard::new(Some(config.blocks_path()), config.dev);
    let hub = hub::Hub::default();
    let mut tasks = Vec::new();

    let tls = if config.dev { None } else { Some(certificates(config, &mut tasks)?) };

    let mut relay = RelayConfig::new(SocketAddr::from(([127, 0, 0, 1], 0)));
    relay.access = Arc::new(gate.clone());
    let mut server = ServerConfig::default();
    server.relay = Some(relay);
    server.metrics_addr = None;
    if let (Some((_, tls)), true) = (&tls, config.enable_quic) {
        let mut quic = QuicConfig::new(config.quic_bind);
        quic.server_config = Some((**tls).clone());
        server.quic = Some(quic);
    }
    let relay = Server::spawn(server).await.map_err(|error| format!("중계 서버를 시작하지 못했습니다: {error}"))?;
    let relay_addr = relay.http_addr().ok_or("중계 서버 주소를 알 수 없습니다.")?;

    let admin = Admin { gate: gate.clone(), guard: guard.clone(), hub: hub.clone(), password: config.admin_password.clone(), domain: config.domain.clone() };
    let front = Arc::new(Front {
        gate: gate.clone(),
        guard: guard.clone(),
        hub: hub.clone(),
        relay: relay_addr,
        admin: config.https_admin.then(|| admin.clone()),
        phone_web: config.phone_web,
        dev: config.dev,
    });

    let http = bind(config.http_bind, "HTTP").await?;
    let http_addr = http.local_addr().map_err(|error| error.to_string())?;
    tasks.push(tokio::spawn(front::serve_plain(http, front.clone())));

    let mut https_addr = None;
    if let Some((acceptor, _)) = tls {
        let https = bind(config.https_bind, "HTTPS").await?;
        https_addr = Some(https.local_addr().map_err(|error| error.to_string())?);
        tasks.push(tokio::spawn(front::serve_tls(https, front.clone(), acceptor)));
    }

    let admin_listener = bind(config.admin_bind, "관리 화면").await?;
    let admin_addr = admin_listener.local_addr().map_err(|error| error.to_string())?;
    tasks.push(tokio::spawn(admin::serve(admin_listener, admin)));

    Ok(Running { relay, tasks, http_addr, https_addr, admin_addr, gate, guard, hub })
}
