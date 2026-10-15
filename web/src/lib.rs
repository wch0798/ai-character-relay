//! The browser side of the phone client: an iroh endpoint that reaches a home computer through
//! the relay. Everything above raw HTTP bytes (pairing, chat, parsing) lives in `app.js`.
//!
//! Browsers can't open UDP sockets, so connections always go through the relay; they are still
//! end-to-end encrypted between the phone and the home computer.

use iroh::endpoint::{presets, Connection};
use iroh::{Endpoint, EndpointAddr, EndpointId, RelayConfig, RelayMap, RelayMode, RelayUrl, SecretKey, TransportAddr};
use std::cell::RefCell;
use std::collections::HashMap;
use std::sync::Arc;
use wasm_bindgen::prelude::*;

const ALPN: &[u8] = b"ai-character/share/1";
const MAX_CHUNK: usize = 16 * 1024;

fn error(message: impl std::fmt::Display) -> JsError {
    JsError::new(&message.to_string())
}

fn relay_config(url: &str, token: &str) -> Result<RelayConfig, JsError> {
    let url: RelayUrl = url.parse().map_err(|_| error("중계 서버 주소가 올바르지 않습니다."))?;
    let config = RelayConfig::from(url);
    Ok(if token.is_empty() { config } else { config.with_auth_token(token.to_string()) })
}

async fn sleep(ms: i32) {
    let promise = js_sys::Promise::new(&mut |resolve, _| {
        let global = js_sys::global();
        let set_timeout = js_sys::Reflect::get(&global, &"setTimeout".into()).ok().and_then(|value| value.dyn_into::<js_sys::Function>().ok());
        if let Some(set_timeout) = set_timeout {
            let _ = set_timeout.call2(&JsValue::NULL, &resolve, &ms.into());
        }
    });
    let _ = wasm_bindgen_futures::JsFuture::from(promise).await;
}

#[wasm_bindgen]
pub struct Phone {
    endpoint: Endpoint,
    secret: String,
    conns: RefCell<HashMap<String, Connection>>,
}

#[wasm_bindgen]
impl Phone {
    /// `secret_hex` is the phone's saved key (empty on first use); `relay`/`token` is the relay this page came from.
    pub async fn start(secret_hex: String, relay: String, token: String) -> Result<Phone, JsError> {
        console_error_panic_hook::set_once();
        let key = match hex::decode(secret_hex.trim()).ok().and_then(|bytes| <[u8; 32]>::try_from(bytes).ok()) {
            Some(bytes) => SecretKey::from_bytes(&bytes),
            None => SecretKey::generate(),
        };
        let secret = hex::encode(key.to_bytes());
        let endpoint = Endpoint::builder(presets::Minimal)
            .secret_key(key)
            .alpns(vec![ALPN.to_vec()])
            .relay_mode(RelayMode::Custom(RelayMap::from_iter([relay_config(&relay, &token)?])))
            .bind()
            .await
            .map_err(|e| error(format!("연결 준비에 실패했습니다: {e}")))?;
        Ok(Phone { endpoint, secret, conns: RefCell::new(HashMap::new()) })
    }

    /// Save this to use the same identity next time (paired computers remember it).
    pub fn secret(&self) -> String {
        self.secret.clone()
    }

    pub fn id(&self) -> String {
        self.endpoint.id().to_string()
    }

    /// Waits up to `timeout_ms` for the relay to accept this phone.
    pub async fn online(&self, timeout_ms: i32) -> bool {
        let online = self.endpoint.online();
        futures_select(online, sleep(timeout_ms)).await
    }

    async fn connection(&self, id: &str, relay: &str, token: &str) -> Result<Connection, JsError> {
        let cached = self.conns.borrow().get(id).filter(|conn| conn.close_reason().is_none()).cloned();
        if let Some(conn) = cached {
            return Ok(conn);
        }
        let endpoint_id: EndpointId = id.parse().map_err(|_| error("저장된 컴퓨터 정보가 손상됐습니다. 다시 연결해 주세요."))?;
        let config = relay_config(relay, token)?;
        let url = config.url.clone();
        let _ = self.endpoint.insert_relay(url.clone(), Arc::new(config)).await;
        let addr = EndpointAddr::from_parts(endpoint_id, [TransportAddr::Relay(url)]);
        let connect = self.endpoint.connect(addr, ALPN);
        let conn = match futures_timeout(connect, 15_000).await {
            Some(Ok(conn)) => conn,
            _ => return Err(error("그 컴퓨터에 연결하지 못했습니다. 컴퓨터가 켜져 있고 AI Character의 공유가 켜져 있는지 확인해 주세요.")),
        };
        self.conns.borrow_mut().insert(id.to_string(), conn.clone());
        Ok(conn)
    }

    /// Sends one raw HTTP/1.1 request to computer `id` and passes the response bytes to `on_data` as they arrive.
    pub async fn request(&self, id: String, relay: String, token: String, raw: Vec<u8>, on_data: js_sys::Function) -> Result<(), JsError> {
        let broken = || error("연결이 끊겼습니다. 다시 시도해 주세요.");
        let conn = self.connection(&id, &relay, &token).await?;
        let (mut send, mut recv) = match conn.open_bi().await {
            Ok(streams) => streams,
            Err(_) => {
                // A saved connection can die while the phone sleeps; try once with a fresh one.
                self.conns.borrow_mut().remove(&id);
                let conn = self.connection(&id, &relay, &token).await?;
                conn.open_bi().await.map_err(|_| {
                    self.conns.borrow_mut().remove(&id);
                    broken()
                })?
            }
        };
        send.write_all(&raw).await.map_err(|_| broken())?;
        let _ = send.finish();
        let mut buffer = vec![0u8; MAX_CHUNK];
        loop {
            match recv.read(&mut buffer).await {
                Ok(Some(read)) => {
                    let chunk = js_sys::Uint8Array::from(&buffer[..read]);
                    on_data.call1(&JsValue::NULL, &chunk).map_err(|_| broken())?;
                }
                Ok(None) => return Ok(()),
                Err(_) => return Err(broken()),
            }
        }
    }

    /// Stops a running answer by closing the connection to that computer.
    pub fn disconnect(&self, id: String) {
        if let Some(conn) = self.conns.borrow_mut().remove(&id) {
            conn.close(0u32.into(), b"stop");
        }
    }

    /// Drops every saved connection (after the page slept, they may be silently dead).
    #[wasm_bindgen(js_name = forgetAll)]
    pub fn forget_all(&self) {
        for (_, conn) in self.conns.borrow_mut().drain() {
            conn.close(0u32.into(), b"sleep");
        }
    }
}

async fn futures_select<A: std::future::Future<Output = ()>, B: std::future::Future<Output = ()>>(a: A, b: B) -> bool {
    let a = std::pin::pin!(a);
    let b = std::pin::pin!(b);
    match futures_lite_select(a, b).await {
        Either::Left(()) => true,
        Either::Right(()) => false,
    }
}

async fn futures_timeout<F: std::future::Future>(future: F, ms: i32) -> Option<F::Output> {
    let future = std::pin::pin!(future);
    let timer = std::pin::pin!(sleep(ms));
    match futures_lite_select(future, timer).await {
        Either::Left(value) => Some(value),
        Either::Right(()) => None,
    }
}

enum Either<L, R> {
    Left(L),
    Right(R),
}

async fn futures_lite_select<A: std::future::Future, B: std::future::Future>(mut a: std::pin::Pin<&mut A>, mut b: std::pin::Pin<&mut B>) -> Either<A::Output, B::Output> {
    std::future::poll_fn(move |cx| {
        if let std::task::Poll::Ready(value) = a.as_mut().poll(cx) {
            return std::task::Poll::Ready(Either::Left(value));
        }
        if let std::task::Poll::Ready(value) = b.as_mut().poll(cx) {
            return std::task::Poll::Ready(Either::Right(value));
        }
        std::task::Poll::Pending
    })
    .await
}
