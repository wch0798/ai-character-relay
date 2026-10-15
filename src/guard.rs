//! Blocks addresses that keep presenting wrong passwords or guessing connection codes.
//!
//! `LIMIT` failures within `WINDOW` blocks the address for `BLOCK`. Active blocks are saved to a
//! file so a restart doesn't lift them; failure counts are in memory only.
//!
//! A public server normally only sees public visitor addresses. If it sees private ones, it is
//! behind something (a reverse proxy, Cloudflare's proxy, Docker's userland proxy) that makes every
//! visitor share one address; blocking that address would lock everyone out, so such addresses are
//! not counted and the admin page shows a warning instead.

use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::collections::{HashMap, VecDeque};
use std::net::IpAddr;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

pub const LIMIT: usize = 20;
pub const WINDOW: Duration = Duration::from_secs(10 * 60);
pub const BLOCK: Duration = Duration::from_secs(30 * 60);
const MAX_TRACKED: usize = 50_000;

#[derive(Default)]
struct Entry {
    failures: VecDeque<Instant>,
    blocked_until: Option<Instant>,
    blocked_at: u64,
    reason: String,
}

#[derive(Default)]
struct State {
    entries: HashMap<IpAddr, Entry>,
    blocks: u64,
}

#[derive(Serialize, Deserialize)]
struct SavedBlock {
    ip: IpAddr,
    since: u64,
    until: u64,
    reason: String,
}

#[derive(Clone, Default)]
pub struct Guard {
    state: Arc<Mutex<State>>,
    file: Option<Arc<PathBuf>>,
    /// Count failures from private addresses too (local tests, where every visitor is 127.0.0.1).
    count_private: bool,
    shared_address: Arc<AtomicBool>,
    proxy_headers: Arc<AtomicBool>,
}

/// IPv6 users usually own a whole /64, so they are tracked per /64.
pub fn key(ip: IpAddr) -> IpAddr {
    match ip {
        IpAddr::V6(v6) => match v6.to_ipv4_mapped() {
            Some(v4) => IpAddr::V4(v4),
            None => {
                let mut segments = v6.segments();
                segments[4..].fill(0);
                IpAddr::V6(segments.into())
            }
        },
        v4 => v4,
    }
}

/// Addresses that never reach a public server directly, so a proxy or Docker must be in between.
pub fn is_private(ip: IpAddr) -> bool {
    match key(ip) {
        IpAddr::V4(v4) => {
            let [a, b, ..] = v4.octets();
            v4.is_loopback() || v4.is_private() || v4.is_link_local() || v4.is_unspecified() || (a == 100 && (64..128).contains(&b))
        }
        IpAddr::V6(v6) => v6.is_loopback() || v6.is_unspecified() || (v6.segments()[0] & 0xfe00) == 0xfc00 || (v6.segments()[0] & 0xffc0) == 0xfe80,
    }
}

impl Guard {
    /// `file` keeps active blocks across restarts; `count_private` is for local tests only.
    pub fn new(file: Option<PathBuf>, count_private: bool) -> Self {
        let guard = Self { file: file.map(Arc::new), count_private, ..Self::default() };
        guard.load();
        guard
    }

    fn state(&self) -> std::sync::MutexGuard<'_, State> {
        self.state.lock().unwrap_or_else(|poisoned| poisoned.into_inner())
    }

    fn load(&self) {
        let Some(file) = &self.file else { return };
        let Ok(text) = std::fs::read_to_string(file.as_ref()) else { return };
        let saved: Vec<SavedBlock> = serde_json::from_str(&text).unwrap_or_default();
        let (now, now_secs) = (Instant::now(), crate::gate::now_secs());
        let mut state = self.state();
        for block in saved.into_iter().filter(|block| block.until > now_secs) {
            let left = Duration::from_secs((block.until - now_secs).min(BLOCK.as_secs()));
            let entry = state.entries.entry(key(block.ip)).or_default();
            entry.blocked_until = Some(now + left);
            entry.blocked_at = block.since;
            entry.reason = block.reason;
        }
    }

    fn save(&self, state: &State) {
        let Some(file) = &self.file else { return };
        let (now, now_secs) = (Instant::now(), crate::gate::now_secs());
        let saved: Vec<SavedBlock> = state
            .entries
            .iter()
            .filter_map(|(ip, entry)| {
                let until = entry.blocked_until.filter(|until| *until > now)?;
                Some(SavedBlock { ip: *ip, since: entry.blocked_at, until: now_secs + (until - now).as_secs(), reason: entry.reason.clone() })
            })
            .collect();
        let temp = file.with_extension("json.tmp");
        if let Ok(text) = serde_json::to_string(&saved) {
            if std::fs::write(&temp, text).is_ok() {
                let _ = std::fs::rename(&temp, file.as_ref());
            }
        }
    }

    /// Notes that a visitor sent proxy headers (only for the admin warning; they are never trusted).
    pub fn note_proxy_headers(&self) {
        self.proxy_headers.store(true, Ordering::Relaxed);
    }

    pub fn is_blocked(&self, ip: IpAddr) -> bool {
        let now = Instant::now();
        let mut state = self.state();
        match state.entries.get_mut(&key(ip)) {
            Some(entry) => match entry.blocked_until {
                Some(until) if until > now => true,
                Some(_) => {
                    entry.blocked_until = None;
                    entry.failures.clear();
                    false
                }
                None => false,
            },
            None => false,
        }
    }

    /// Records a failure; returns true when this failure blocked the address.
    pub fn fail(&self, ip: IpAddr, reason: &str) -> bool {
        if !self.count_private && is_private(ip) {
            self.shared_address.store(true, Ordering::Relaxed);
            return false;
        }
        let now = Instant::now();
        let mut state = self.state();
        if state.entries.len() >= MAX_TRACKED {
            state.entries.retain(|_, entry| entry.blocked_until.is_some_and(|until| until > now) || entry.failures.back().is_some_and(|last| now - *last < WINDOW));
        }
        let entry = state.entries.entry(key(ip)).or_default();
        while entry.failures.front().is_some_and(|first| now - *first > WINDOW) {
            entry.failures.pop_front();
        }
        entry.failures.push_back(now);
        if entry.failures.len() >= LIMIT && entry.blocked_until.is_none_or(|until| until <= now) {
            entry.blocked_until = Some(now + BLOCK);
            entry.blocked_at = crate::gate::now_secs();
            entry.reason = reason.to_string();
            state.blocks += 1;
            self.save(&state);
            return true;
        }
        false
    }

    pub fn unblock(&self, ip: IpAddr) -> bool {
        let mut state = self.state();
        let was_blocked = state.entries.remove(&key(ip)).is_some_and(|entry| entry.blocked_until.is_some());
        if was_blocked {
            self.save(&state);
        }
        was_blocked
    }

    pub fn status(&self) -> Value {
        let now = Instant::now();
        let state = self.state();
        let blocked: Vec<Value> = state
            .entries
            .iter()
            .filter_map(|(ip, entry)| {
                let until = entry.blocked_until.filter(|until| *until > now)?;
                Some(json!({ "ip": ip.to_string(), "since": entry.blocked_at, "remainingSecs": (until - now).as_secs(), "reason": entry.reason }))
            })
            .collect();
        json!({
            "blocked": blocked,
            "totalBlocks": state.blocks,
            "limit": LIMIT,
            "windowMins": WINDOW.as_secs() / 60,
            "blockMins": BLOCK.as_secs() / 60,
            "sharedAddress": self.shared_address.load(Ordering::Relaxed),
            "proxyHeaders": self.proxy_headers.load(Ordering::Relaxed),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn repeated_failures_block_and_unblock() {
        let guard = Guard::default();
        let ip: IpAddr = "203.0.113.7".parse().unwrap();
        for _ in 0..LIMIT - 1 {
            assert!(!guard.fail(ip, "token"));
        }
        assert!(!guard.is_blocked(ip));
        assert!(guard.fail(ip, "token"));
        assert!(guard.is_blocked(ip));
        assert!(!guard.is_blocked("203.0.113.8".parse().unwrap()));
        assert_eq!(guard.status()["blocked"][0]["ip"], "203.0.113.7");
        assert!(guard.unblock(ip));
        assert!(!guard.is_blocked(ip));
    }

    #[test]
    fn ipv6_is_tracked_per_64() {
        assert_eq!(key("2001:db8:1:2:aaaa::1".parse().unwrap()), key("2001:db8:1:2:bbbb::9".parse().unwrap()));
        assert_ne!(key("2001:db8:1:2::1".parse().unwrap()), key("2001:db8:1:3::1".parse().unwrap()));
        assert_eq!(key("::ffff:203.0.113.7".parse().unwrap()), "203.0.113.7".parse::<IpAddr>().unwrap());
    }

    #[test]
    fn private_addresses_are_never_blocked_in_production() {
        let guard = Guard::new(None, false);
        for ip in ["172.17.0.1", "10.0.0.2", "127.0.0.1", "100.64.0.1", "::ffff:192.168.1.1", "fd00::1"] {
            let ip: IpAddr = ip.parse().unwrap();
            assert!(is_private(ip), "{ip}");
            for _ in 0..LIMIT * 2 {
                assert!(!guard.fail(ip, "token"));
            }
            assert!(!guard.is_blocked(ip));
        }
        assert_eq!(guard.status()["sharedAddress"], true);
        assert!(!is_private("203.0.113.7".parse().unwrap()));
        assert!(!is_private("2001:db8::1".parse().unwrap()));
    }

    #[test]
    fn blocks_survive_a_restart() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("blocks.json");
        let ip: IpAddr = "198.51.100.4".parse().unwrap();
        let guard = Guard::new(Some(file.clone()), false);
        for _ in 0..LIMIT {
            guard.fail(ip, "code");
        }
        assert!(guard.is_blocked(ip));
        let restarted = Guard::new(Some(file.clone()), false);
        assert!(restarted.is_blocked(ip));
        assert_eq!(restarted.status()["blocked"][0]["reason"], "code");
        assert!(restarted.unblock(ip));
        assert!(!Guard::new(Some(file), false).is_blocked(ip));
    }
}
