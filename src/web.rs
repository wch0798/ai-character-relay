//! The phone page at `/app/`. Static files only, built by `web/build.sh` and embedded here; the
//! page talks to the home computer end-to-end encrypted, so the server never sees the chat.

use crate::http::{self, Io, Request};
use tokio::io::AsyncWriteExt;

struct File {
    path: &'static str,
    content_type: &'static str,
    body: &'static [u8],
}

const FILES: &[File] = &[
    File { path: "/app/", content_type: "text/html; charset=utf-8", body: include_bytes!("../web/dist/index.html") },
    File { path: "/app/app.js", content_type: "text/javascript; charset=utf-8", body: include_bytes!("../web/dist/app.js") },
    File { path: "/app/aic_web.js", content_type: "text/javascript; charset=utf-8", body: include_bytes!("../web/dist/aic_web.js") },
    File { path: "/app/aic_web_bg.wasm", content_type: "application/wasm", body: include_bytes!("../web/dist/aic_web_bg.wasm") },
];

const CSP: &str = "default-src 'none'; script-src 'self' 'wasm-unsafe-eval'; style-src 'unsafe-inline'; connect-src 'self'; img-src 'self' data:; base-uri 'none'; form-action 'none'; frame-ancestors 'none'";

pub fn handles(path: &str) -> bool {
    path == "/app" || FILES.iter().any(|file| file.path == path)
}

fn etag(file: &File) -> String {
    let hash = file.body.iter().fold(0xcbf2_9ce4_8422_2325u64, |hash, byte| (hash ^ u64::from(*byte)).wrapping_mul(0x100_0000_01b3));
    format!("\"{hash:016x}\"")
}

pub async fn respond<S: Io>(stream: &mut S, request: &Request) {
    let Some(file) = FILES.iter().find(|file| file.path == request.path) else {
        return http::reply(stream, "308 Permanent Redirect", "text/plain; charset=utf-8", "Location: /app/\r\n", b"").await;
    };
    let tag = etag(file);
    let fresh = request.header("if-none-match").is_some_and(|given| given == tag);
    let (status, body): (&str, &[u8]) = if fresh { ("304 Not Modified", b"") } else { ("200 OK", file.body) };
    let head = format!(
        "HTTP/1.1 {status}\r\nContent-Type: {}\r\nContent-Length: {}\r\nCache-Control: no-cache\r\nETag: {tag}\r\nContent-Security-Policy: {CSP}\r\nX-Content-Type-Options: nosniff\r\nReferrer-Policy: no-referrer\r\nConnection: close\r\n\r\n",
        file.content_type,
        body.len()
    );
    let _ = stream.write_all(head.as_bytes()).await;
    let _ = stream.write_all(body).await;
    let _ = stream.flush().await;
    let _ = stream.shutdown().await;
}
