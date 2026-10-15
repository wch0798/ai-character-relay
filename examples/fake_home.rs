//! A stand-in for a home computer running AI Character, for trying the phone page locally:
//!
//!     cargo run --example fake_home -- http://localhost:3340 <access password>
//!
//! Prints a long connection code. It answers pairing, the model list and streamed chat replies
//! (chunked, like Ollama) without needing Ollama.

use base64::Engine;
use iroh::endpoint::presets;
use iroh::{Endpoint, RelayConfig, RelayMap, RelayMode, RelayUrl};
use std::time::Duration;

const ALPN: &[u8] = b"ai-character/share/1";

fn chunk(text: &str) -> Vec<u8> {
    format!("{:x}\r\n{text}\r\n", text.len()).into_bytes()
}

#[tokio::main]
async fn main() {
    let _ = rustls::crypto::ring::default_provider().install_default();
    let mut args = std::env::args().skip(1);
    let relay = args.next().expect("relay url");
    let token = args.next().expect("access password");
    let url: RelayUrl = relay.parse().unwrap();
    let endpoint = Endpoint::builder(presets::Minimal)
        .alpns(vec![ALPN.to_vec()])
        .relay_mode(RelayMode::Custom(RelayMap::from_iter([RelayConfig::from(url).with_auth_token(token.clone())])))
        .bind()
        .await
        .unwrap();
    tokio::time::timeout(Duration::from_secs(10), endpoint.online()).await.expect("relay reachable");
    let ticket = serde_json::json!({ "v": 1, "n": "가짜 집 컴퓨터", "id": endpoint.id().to_string(), "r": relay, "t": token, "a": [], "s": "fake-secret-0123456789" });
    println!("AIC1.{}", base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(ticket.to_string()));
    while let Some(incoming) = endpoint.accept().await {
        tokio::spawn(async move {
            let Ok(conn) = incoming.accept().unwrap().await else { return };
            eprintln!("connected: {}", conn.remote_id());
            while let Ok((mut send, mut recv)) = conn.accept_bi().await {
                tokio::spawn(async move {
                    let raw = recv.read_to_end(64 * 1024).await.unwrap_or_default();
                    let text = String::from_utf8_lossy(&raw).to_string();
                    let line = text.lines().next().unwrap_or_default().to_string();
                    eprintln!("request: {line}");
                    let reply: Vec<u8> = if line.starts_with("POST /ai-character/pair") {
                        b"HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: 30\r\n\r\n{\"name\":\"fake-home-computer\"}\n".to_vec()
                    } else if line.starts_with("GET /api/tags") {
                        let body = r#"{"models":[{"name":"qwen3:8b"},{"name":"nomic-embed-text"},{"name":"gemma3:4b"}]}"#;
                        format!("HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\n\r\n{body}", body.len()).into_bytes()
                    } else if line.starts_with("POST /v1/chat/completions") {
                        send.write_all(b"HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nTransfer-Encoding: chunked\r\n\r\n").await.unwrap();
                        for word in ["안녕하세요! ", "집 컴퓨터의 ", "AI가 ", "휴대폰으로 ", "답하고 있습니다. ", "한글도 ", "잘 나옵니다 ✅"] {
                            let event = format!("data: {}\n\n", serde_json::json!({ "choices": [{ "delta": { "content": word } }] }));
                            send.write_all(&chunk(&event)).await.unwrap();
                            tokio::time::sleep(Duration::from_millis(150)).await;
                        }
                        send.write_all(&chunk("data: [DONE]\n\n")).await.unwrap();
                        b"0\r\n\r\n".to_vec()
                    } else {
                        b"HTTP/1.1 404 Not Found\r\nContent-Length: 0\r\n\r\n".to_vec()
                    };
                    let _ = send.write_all(&reply).await;
                    let _ = send.finish();
                });
            }
        });
    }
}
