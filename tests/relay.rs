//! Runs the real relay (dev mode, plain HTTP) and connects iroh endpoints through it,
//! the same way the AI Character app does.

use aic_relay::config::Config;
use aic_relay::gate::Gate;
use iroh::endpoint::presets;
use iroh::{Endpoint, EndpointAddr, RelayConfig, RelayMap, RelayMode, RelayUrl, TransportAddr};
use std::time::Duration;

const ALPN: &[u8] = b"ai-character/share/1";

async fn endpoint(url: &RelayUrl, token: &str) -> Endpoint {
    let config = RelayConfig::from(url.clone()).with_auth_token(token.to_string());
    Endpoint::builder(presets::Minimal)
        .alpns(vec![ALPN.to_vec()])
        .relay_mode(RelayMode::Custom(RelayMap::from_iter([config])))
        .bind()
        .await
        .unwrap()
}

#[tokio::test]
async fn app_endpoints_meet_through_the_relay_and_tokens_change_live() {
    let _ = rustls::crypto::ring::default_provider().install_default();
    let dir = tempfile::tempdir().unwrap();
    let mut config = Config::new("localhost", "me@example.com", "admin-password-123".into());
    config.dev = true;
    config.data_dir = dir.path().to_path_buf();
    config.http_bind = "127.0.0.1:0".parse().unwrap();
    config.admin_bind = "127.0.0.1:0".parse().unwrap();
    config.https_admin = true;
    let gate = Gate::open(&config.tokens_path()).unwrap();
    let home = gate.add_token("우리집").unwrap();

    let server = aic_relay::spawn(&config, gate.clone()).await.unwrap();
    let url: RelayUrl = format!("http://{}", server.http_addr).parse().unwrap();
    let admin_addr = server.admin_addr;

    let host = endpoint(&url, &home.token).await;
    let host_id = host.id().to_string();
    let client = endpoint(&url, &home.token).await;
    tokio::time::timeout(Duration::from_secs(10), host.online()).await.expect("host reached the relay");
    let relay_only = EndpointAddr::from_parts(host.id(), host.addr().relay_urls().cloned().map(TransportAddr::Relay));
    tokio::spawn(async move {
        let conn = host.accept().await.unwrap().accept().unwrap().await.unwrap();
        let (mut send, mut recv) = conn.accept_bi().await.unwrap();
        let data = recv.read_to_end(1024).await.unwrap();
        send.write_all(&data).await.unwrap();
        send.finish().unwrap();
        conn.closed().await;
    });
    let conn = tokio::time::timeout(Duration::from_secs(15), client.connect(relay_only, ALPN)).await.unwrap().unwrap();
    let (mut send, mut recv) = conn.open_bi().await.unwrap();
    send.write_all(b"hello through our relay").await.unwrap();
    send.finish().unwrap();
    assert_eq!(recv.read_to_end(1024).await.unwrap(), b"hello through our relay");

    let status = gate.status();
    assert!(status["liveDevices"].as_u64().unwrap() >= 1, "{status}");
    assert_eq!(status["tokens"][0]["name"], "우리집");

    // App API on the front server: presence, path reports, 6-digit code mailbox.
    let http = reqwest::Client::new();
    let api = format!("http://{}/aic/v1", server.http_addr);
    let presence: serde_json::Value = http.post(format!("{api}/presence")).bearer_auth(&home.token).json(&serde_json::json!({ "ids": [host_id, "junk"] })).send().await.unwrap().json().await.unwrap();
    assert_eq!(presence["devices"][&host_id]["online"], true, "{presence}");
    assert!(presence["devices"].get("junk").is_none());
    assert_eq!(http.post(format!("{api}/paths")).bearer_auth(&home.token).json(&serde_json::json!({ "path": "relay" })).send().await.unwrap().status(), 200);
    let id = "ab".repeat(32);
    assert_eq!(http.post(format!("{api}/codes")).bearer_auth(&home.token).json(&serde_json::json!({ "id": id, "sealed": "c2VhbGVk", "minutes": 10 })).send().await.unwrap().status(), 200);
    let taken: serde_json::Value = http.post(format!("{api}/codes/take")).bearer_auth(&home.token).json(&serde_json::json!({ "id": id })).send().await.unwrap().json().await.unwrap();
    assert_eq!(taken["sealed"], "c2VhbGVk");
    assert_eq!(http.post(format!("{api}/codes/take")).bearer_auth(&home.token).json(&serde_json::json!({ "id": id })).send().await.unwrap().status(), 404, "a code works once");
    assert_eq!(http.get(format!("{api}/info")).send().await.unwrap().status(), 401);
    assert_eq!(server.hub.status()["paths"]["relayed"], 1);
    server.guard.unblock("127.0.0.1".parse().unwrap());

    let stranger = endpoint(&url, "not-a-token").await;
    assert!(tokio::time::timeout(Duration::from_secs(4), stranger.online()).await.is_err(), "unknown tokens are refused");
    stranger.close().await;
    server.guard.unblock("127.0.0.1".parse().unwrap());

    // Admin API: password required, CSRF header required, new token works without restart.
    let base = format!("http://{admin_addr}");
    assert_eq!(http.get(format!("{base}/api/status")).send().await.unwrap().status(), 401);
    assert_eq!(http.get(format!("{base}/api/status")).basic_auth("admin", Some("wrong-password")).send().await.unwrap().status(), 401);
    let status: serde_json::Value = http.get(format!("{base}/api/status")).basic_auth("admin", Some(&config.admin_password)).send().await.unwrap().json().await.unwrap();
    assert_eq!(status["domain"], "localhost");
    assert!(status["hub"]["codesUsed"].as_u64().unwrap() == 1);
    let via_front = http.get(format!("http://{}/admin/api/status", server.http_addr)).basic_auth("admin", Some(&config.admin_password)).send().await.unwrap();
    assert_eq!(via_front.status(), 200, "https_admin serves the same page under /admin/");
    let page = http.get(format!("{base}/")).basic_auth("admin", Some(&config.admin_password)).send().await.unwrap().text().await.unwrap();
    assert!(page.contains("AI Character 중계 서버"));
    let forged = http.post(format!("{base}/api/tokens")).basic_auth("admin", Some(&config.admin_password)).json(&serde_json::json!({ "name": "회사" })).send().await.unwrap();
    assert_eq!(forged.status(), 403);
    let created: serde_json::Value = http
        .post(format!("{base}/api/tokens"))
        .basic_auth("admin", Some(&config.admin_password))
        .header("X-AIC-Admin", "1")
        .json(&serde_json::json!({ "name": "회사" }))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let office = created["token"]["token"].as_str().unwrap().to_string();
    let laptop = endpoint(&url, &office).await;
    tokio::time::timeout(Duration::from_secs(10), laptop.online()).await.expect("a token added while running works at once");

    assert!(gate.remove_token("회사").unwrap());
    let late = endpoint(&url, &office).await;
    assert!(tokio::time::timeout(Duration::from_secs(4), late.online()).await.is_err(), "a removed token is refused for new connections");
    late.close().await;
    server.guard.unblock("127.0.0.1".parse().unwrap());

    // Guessing gets the address blocked, even with the right password afterwards.
    for _ in 0..aic_relay::guard::LIMIT {
        http.post(format!("{api}/codes/take")).bearer_auth(&home.token).json(&serde_json::json!({ "id": "cd".repeat(32) })).send().await.unwrap();
    }
    assert_eq!(http.get(format!("{api}/info")).bearer_auth(&home.token).send().await.unwrap().status(), 429);
    assert_eq!(server.guard.status()["blocked"][0]["reason"], "code");
    let blocked: serde_json::Value = http.get(format!("{base}/api/status")).basic_auth("admin", Some(&config.admin_password)).send().await.unwrap().json().await.unwrap();
    assert_eq!(blocked["guard"]["blocked"][0]["ip"], "127.0.0.1", "loopback admin still works and shows the block");
    let unblocked = http.post(format!("{base}/api/unblock")).basic_auth("admin", Some(&config.admin_password)).header("X-AIC-Admin", "1").json(&serde_json::json!({ "ip": "127.0.0.1" })).send().await.unwrap();
    assert_eq!(unblocked.status(), 200);
    assert_eq!(http.get(format!("{api}/info")).bearer_auth(&home.token).send().await.unwrap().status(), 200);

    // Phone page: static files, the wasm with its proper type, and the bare domain leads there.
    let front = format!("http://{}", server.http_addr);
    let page = http.get(format!("{front}/app/")).send().await.unwrap();
    assert_eq!(page.status(), 200);
    assert!(page.headers()["content-security-policy"].to_str().unwrap().contains("wasm-unsafe-eval"));
    let etag = page.headers()["etag"].to_str().unwrap().to_string();
    assert!(page.text().await.unwrap().contains("우리집 AI"));
    assert_eq!(http.get(format!("{front}/app/")).header("If-None-Match", &etag).send().await.unwrap().status(), 304);
    let wasm = http.get(format!("{front}/app/aic_web_bg.wasm")).send().await.unwrap();
    assert_eq!(wasm.headers()["content-type"], "application/wasm");
    let no_redirects = reqwest::Client::builder().redirect(reqwest::redirect::Policy::none()).build().unwrap();
    assert_eq!(no_redirects.get(format!("{front}/")).send().await.unwrap().headers()["location"], "/app/");
    assert_eq!(http.get(format!("{front}/ping")).send().await.unwrap().status(), 200, "relay probes still pass through");

    conn.close(0u32.into(), b"done");
    server.shutdown().await.unwrap();
}

/// Production mode with a certificate from files: the same HTTPS front, QUIC address discovery
/// and TLS relay connections a real server runs (Let's Encrypt only changes where the certificate
/// comes from).
#[tokio::test]
async fn production_mode_serves_https_quic_and_tls_relay() {
    use iroh::Watcher;
    use iroh_relay::tls::CaTlsConfig;
    use iroh_relay::RelayQuicConfig;

    let _ = rustls::crypto::ring::default_provider().install_default();
    let dir = tempfile::tempdir().unwrap();
    let certified = rcgen::generate_simple_self_signed(vec!["localhost".to_string()]).unwrap();
    std::fs::write(dir.path().join("cert.pem"), certified.cert.pem()).unwrap();
    std::fs::write(dir.path().join("key.pem"), certified.signing_key.serialize_pem()).unwrap();

    let mut config = Config::new("localhost", "me@example.com", "admin-password-123".into());
    config.data_dir = dir.path().to_path_buf();
    config.http_bind = "127.0.0.1:0".parse().unwrap();
    config.https_bind = "127.0.0.1:0".parse().unwrap();
    config.quic_bind = "127.0.0.1:0".parse().unwrap();
    config.admin_bind = "127.0.0.1:0".parse().unwrap();
    config.tls_cert = Some(dir.path().join("cert.pem"));
    config.tls_key = Some(dir.path().join("key.pem"));
    let gate = Gate::open(&config.tokens_path()).unwrap();
    let home = gate.add_token("우리집").unwrap();
    let server = aic_relay::spawn(&config, gate.clone()).await.unwrap();
    let https = server.https_addr.expect("HTTPS listener");
    let quic = server.quic_addr().expect("QUIC address discovery listener");

    let http = reqwest::Client::builder()
        .add_root_certificate(reqwest::Certificate::from_pem(certified.cert.pem().as_bytes()).unwrap())
        .resolve("localhost", https)
        .build()
        .unwrap();
    let site = format!("https://localhost:{}", https.port());
    assert_eq!(http.get(format!("{site}/ping")).send().await.unwrap().status(), 200);
    assert_eq!(http.get(format!("{site}/aic/v1/info")).bearer_auth(&home.token).send().await.unwrap().status(), 200, "a reused connection after a relay probe still reaches the front");
    assert_eq!(http.get(format!("{site}/app/")).send().await.unwrap().status(), 200);
    let plain = format!("http://{}", server.http_addr);
    assert_eq!(http.get(format!("{plain}/aic/v1/info")).bearer_auth(&home.token).send().await.unwrap().status(), 404, "passwords never travel over plain HTTP");

    // Visitors that all look like 127.0.0.1 share one address: never blocked, but flagged.
    for _ in 0..aic_relay::guard::LIMIT + 5 {
        http.get(format!("{site}/aic/v1/info")).bearer_auth("wrong").send().await.unwrap();
    }
    assert_eq!(http.get(format!("{site}/aic/v1/info")).bearer_auth(&home.token).send().await.unwrap().status(), 200);
    http.get(format!("{site}/ping")).header("X-Forwarded-For", "203.0.113.9").send().await.unwrap();
    let guard = server.guard.status();
    assert_eq!(guard["sharedAddress"], true, "{guard}");
    assert_eq!(guard["proxyHeaders"], true, "{guard}");

    let url: RelayUrl = format!("https://localhost:{}", https.port()).parse().unwrap();
    let tls_endpoint = || async {
        let relay = RelayConfig::new(url.clone(), Some(RelayQuicConfig::new(quic.port()))).with_auth_token(home.token.clone());
        Endpoint::builder(presets::Minimal)
            .alpns(vec![ALPN.to_vec()])
            .relay_mode(RelayMode::Custom(RelayMap::from_iter([relay])))
            .ca_tls_config(CaTlsConfig::custom_roots([certified.cert.der().clone()]))
            .bind()
            .await
            .unwrap()
    };
    let host = tls_endpoint().await;
    let client = tls_endpoint().await;
    tokio::time::timeout(Duration::from_secs(10), host.online()).await.expect("host reached the relay over TLS");
    let mut reports = host.net_report();
    let report = tokio::time::timeout(Duration::from_secs(15), async {
        loop {
            if let Some(report) = reports.get().filter(|report| report.udp_v4) {
                return report;
            }
            reports.updated().await.unwrap();
        }
    })
    .await
    .expect("QUIC address discovery answered");
    assert!(report.global_v4.is_some(), "{report:?}");

    let relay_only = EndpointAddr::from_parts(host.id(), host.addr().relay_urls().cloned().map(TransportAddr::Relay));
    tokio::spawn(async move {
        let conn = host.accept().await.unwrap().accept().unwrap().await.unwrap();
        let (mut send, mut recv) = conn.accept_bi().await.unwrap();
        let data = recv.read_to_end(1024).await.unwrap();
        send.write_all(&data).await.unwrap();
        send.finish().unwrap();
        conn.closed().await;
    });
    let conn = tokio::time::timeout(Duration::from_secs(15), client.connect(relay_only, ALPN)).await.unwrap().unwrap();
    let (mut send, mut recv) = conn.open_bi().await.unwrap();
    send.write_all(b"hello over https").await.unwrap();
    send.finish().unwrap();
    assert_eq!(recv.read_to_end(1024).await.unwrap(), b"hello over https");

    conn.close(0u32.into(), b"done");
    server.shutdown().await.unwrap();
}
