//! Small helpers the app uses besides relaying: the short-code mailbox and path statistics.
//!
//! Short codes: the sharing computer encrypts its long connection code with a key derived from a
//! 6-digit number and leaves it here under a lookup id derived from the same number. The other
//! computer types the number, fetches the box once and decrypts it. The server never receives the
//! number itself; boxes vanish after one fetch or when they expire.

use serde_json::{json, Value};
use std::collections::{HashMap, VecDeque};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

pub const MAX_CODE_MINUTES: u64 = 10;
pub const MAX_SEALED_LEN: usize = 8 * 1024;
const MAX_CODES: usize = 5_000;
const MAX_CODES_PER_TOKEN: usize = 20;
const RECENT: Duration = Duration::from_secs(24 * 60 * 60);
const MAX_RECENT: usize = 10_000;

struct Code {
    sealed: String,
    expires: Instant,
    token: String,
}

#[derive(Default)]
struct State {
    codes: HashMap<String, Code>,
    codes_made: u64,
    codes_used: u64,
    direct: u64,
    relayed: u64,
    recent: VecDeque<(Instant, bool)>,
}

#[derive(Clone, Default)]
pub struct Hub {
    state: Arc<Mutex<State>>,
}

pub fn valid_code_id(id: &str) -> bool {
    id.len() == 64 && id.bytes().all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
}

fn valid_sealed(sealed: &str) -> bool {
    !sealed.is_empty() && sealed.len() <= MAX_SEALED_LEN && sealed.bytes().all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'=' | b'+' | b'/' | b'.'))
}

impl Hub {
    fn state(&self) -> std::sync::MutexGuard<'_, State> {
        self.state.lock().unwrap_or_else(|poisoned| poisoned.into_inner())
    }

    pub fn put_code(&self, token: &str, id: &str, sealed: &str, minutes: u64) -> Result<(), (&'static str, &'static str)> {
        if !valid_code_id(id) || !valid_sealed(sealed) {
            return Err(("400 Bad Request", "bad code"));
        }
        let now = Instant::now();
        let mut state = self.state();
        state.codes.retain(|_, code| code.expires > now);
        if state.codes.contains_key(id) {
            return Err(("409 Conflict", "code in use"));
        }
        if state.codes.len() >= MAX_CODES || state.codes.values().filter(|code| code.token == token).count() >= MAX_CODES_PER_TOKEN {
            return Err(("429 Too Many Requests", "too many codes"));
        }
        let minutes = minutes.clamp(1, MAX_CODE_MINUTES);
        state.codes.insert(id.to_string(), Code { sealed: sealed.to_string(), expires: now + Duration::from_secs(minutes * 60), token: token.to_string() });
        state.codes_made += 1;
        Ok(())
    }

    /// Hands out a box once.
    pub fn take_code(&self, id: &str) -> Option<String> {
        let now = Instant::now();
        let mut state = self.state();
        let code = state.codes.remove(id)?;
        if code.expires <= now {
            return None;
        }
        state.codes_used += 1;
        Some(code.sealed)
    }

    pub fn cancel_code(&self, token: &str, id: &str) -> bool {
        let mut state = self.state();
        match state.codes.get(id) {
            Some(code) if code.token == token => state.codes.remove(id).is_some(),
            _ => false,
        }
    }

    pub fn report_path(&self, direct: bool) {
        let now = Instant::now();
        let mut state = self.state();
        if direct {
            state.direct += 1;
        } else {
            state.relayed += 1;
        }
        while state.recent.front().is_some_and(|(at, _)| now - *at > RECENT) || state.recent.len() >= MAX_RECENT {
            state.recent.pop_front();
        }
        state.recent.push_back((now, direct));
    }

    pub fn status(&self) -> Value {
        let now = Instant::now();
        let state = self.state();
        let recent: Vec<bool> = state.recent.iter().filter(|(at, _)| now - *at <= RECENT).map(|(_, direct)| *direct).collect();
        let recent_direct = recent.iter().filter(|direct| **direct).count();
        json!({
            "codesWaiting": state.codes.values().filter(|code| code.expires > now).count(),
            "codesMade": state.codes_made,
            "codesUsed": state.codes_used,
            "paths": {
                "direct": state.direct,
                "relayed": state.relayed,
                "day": { "direct": recent_direct, "relayed": recent.len() - recent_direct },
            },
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const ID: &str = "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef";

    #[test]
    fn codes_are_handed_out_once() {
        let hub = Hub::default();
        assert!(hub.put_code("우리집", "short", "box", 10).is_err());
        assert!(hub.put_code("우리집", ID, "not a box!", 10).is_err());
        hub.put_code("우리집", ID, "c2VhbGVk", 10).unwrap();
        assert_eq!(hub.put_code("우리집", ID, "b3RoZXI", 10).unwrap_err().0, "409 Conflict");
        assert_eq!(hub.status()["codesWaiting"], 1);
        assert!(!hub.cancel_code("회사", ID));
        assert_eq!(hub.take_code(ID).as_deref(), Some("c2VhbGVk"));
        assert_eq!(hub.take_code(ID), None);
        assert_eq!(hub.status()["codesUsed"], 1);
    }

    #[test]
    fn paths_are_counted() {
        let hub = Hub::default();
        hub.report_path(true);
        hub.report_path(true);
        hub.report_path(false);
        let status = hub.status();
        assert_eq!(status["paths"]["direct"], 2);
        assert_eq!(status["paths"]["day"]["relayed"], 1);
    }
}
