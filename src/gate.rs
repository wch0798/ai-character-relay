//! Access passwords ("접속 암호") and live connection counts.
//!
//! Tokens live in `tokens.json`. The file is re-read whenever it changes, so `aic-relay token add`
//! and the admin page take effect without restarting the server. Removing a token blocks new
//! connections; devices already connected stay until they reconnect (or the server restarts).

use iroh_relay::server::{Access, AccessControl, ClientRequest, ConnectionId};
use iroh_base::EndpointId;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::{Instant, SystemTime, UNIX_EPOCH};

const TOKEN_ALPHABET: &[u8] = b"ABCDEFGHJKLMNPQRSTUVWXYZabcdefghijkmnopqrstuvwxyz23456789";
pub const TOKEN_LEN: usize = 32;
pub const MAX_TOKENS: usize = 100;
const MAX_SEEN: usize = 100_000;

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct Token {
    pub name: String,
    pub token: String,
    #[serde(default)]
    pub created: u64,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq)]
pub struct TokenFile {
    #[serde(default)]
    pub tokens: Vec<Token>,
}

pub fn now_secs() -> u64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map(|time| time.as_secs()).unwrap_or(0)
}

pub fn random_string(len: usize) -> Result<String, String> {
    let mut bytes = vec![0u8; len];
    getrandom::getrandom(&mut bytes).map_err(|error| error.to_string())?;
    Ok(bytes.iter().map(|byte| TOKEN_ALPHABET[*byte as usize % TOKEN_ALPHABET.len()] as char).collect())
}

pub fn constant_time_eq(left: &str, right: &str) -> bool {
    left.len() == right.len() && left.bytes().zip(right.bytes()).fold(0u8, |acc, (a, b)| acc | (a ^ b)) == 0
}

pub fn clean_name(name: &str) -> Result<String, String> {
    let name: String = name.trim().chars().filter(|c| !c.is_control()).collect();
    if name.is_empty() || name.chars().count() > 40 {
        return Err("이름은 1~40자로 넣어 주세요 (예: 우리집, 회사).".into());
    }
    Ok(name)
}

impl TokenFile {
    pub fn load(path: &Path) -> Result<Self, String> {
        match std::fs::read_to_string(path) {
            Ok(text) => serde_json::from_str(&text).map_err(|error| format!("{} 이(가) 손상됐습니다: {error}", path.display())),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(Self::default()),
            Err(error) => Err(format!("{} 을(를) 읽지 못했습니다: {error}", path.display())),
        }
    }

    /// Writes in place so the file keeps the owner and mode the installer gave it.
    pub fn save(&self, path: &Path) -> Result<(), String> {
        let text = serde_json::to_string_pretty(self).map_err(|error| error.to_string())?;
        let existed = path.exists();
        std::fs::write(path, text).map_err(|error| format!("{} 에 저장하지 못했습니다: {error}", path.display()))?;
        #[cfg(unix)]
        if !existed {
            use std::os::unix::fs::PermissionsExt;
            let _ = std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600));
        }
        #[cfg(not(unix))]
        let _ = existed;
        Ok(())
    }

    pub fn add(&mut self, name: &str) -> Result<Token, String> {
        let name = clean_name(name)?;
        if self.tokens.iter().any(|token| token.name == name) {
            return Err(format!("'{name}' 이름의 암호가 이미 있습니다. 다른 이름을 쓰거나 먼저 지우세요."));
        }
        if self.tokens.len() >= MAX_TOKENS {
            return Err(format!("접속 암호는 {MAX_TOKENS}개까지 만들 수 있습니다."));
        }
        let token = Token { name, token: random_string(TOKEN_LEN)?, created: now_secs() };
        self.tokens.push(token.clone());
        Ok(token)
    }

    pub fn remove(&mut self, name: &str) -> bool {
        let before = self.tokens.len();
        self.tokens.retain(|token| token.name != name.trim());
        before != self.tokens.len()
    }

    pub fn find(&self, presented: &str) -> Option<&Token> {
        let mut found = None;
        for token in &self.tokens {
            if constant_time_eq(&token.token, presented) {
                found = Some(token);
            }
        }
        found
    }
}

struct Live {
    endpoint: EndpointId,
    token: String,
}

struct State {
    file: TokenFile,
    modified: Option<SystemTime>,
    live: HashMap<ConnectionId, Live>,
    /// When each device was last connected, for "last seen" in the app. Memory only.
    seen: HashMap<EndpointId, u64>,
    accepted: u64,
    denied: u64,
}

struct Inner {
    path: PathBuf,
    started: Instant,
    state: Mutex<State>,
}

/// The relay's access control. Cheap to clone; all clones share state.
#[derive(Clone)]
pub struct Gate {
    inner: Arc<Inner>,
}

impl std::fmt::Debug for Gate {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Gate").field("path", &self.inner.path).finish_non_exhaustive()
    }
}

fn modified(path: &Path) -> Option<SystemTime> {
    std::fs::metadata(path).and_then(|meta| meta.modified()).ok()
}

impl Gate {
    pub fn open(path: &Path) -> Result<Self, String> {
        let file = TokenFile::load(path)?;
        Ok(Self {
            inner: Arc::new(Inner {
                path: path.to_path_buf(),
                started: Instant::now(),
                state: Mutex::new(State { file, modified: modified(path), live: HashMap::new(), seen: HashMap::new(), accepted: 0, denied: 0 }),
            }),
        })
    }

    fn state(&self) -> std::sync::MutexGuard<'_, State> {
        self.inner.state.lock().unwrap_or_else(|poisoned| poisoned.into_inner())
    }

    /// Picks up edits made by the CLI. A broken file keeps the last good token list.
    fn reload_if_changed(&self, state: &mut State) {
        let current = modified(&self.inner.path);
        if current == state.modified {
            return;
        }
        if let Ok(file) = TokenFile::load(&self.inner.path) {
            state.file = file;
            state.modified = current;
        }
    }

    pub fn tokens(&self) -> Vec<Token> {
        let mut state = self.state();
        self.reload_if_changed(&mut state);
        state.file.tokens.clone()
    }

    pub fn add_token(&self, name: &str) -> Result<Token, String> {
        let mut state = self.state();
        self.reload_if_changed(&mut state);
        let mut file = state.file.clone();
        let token = file.add(name)?;
        file.save(&self.inner.path)?;
        state.file = file;
        state.modified = modified(&self.inner.path);
        Ok(token)
    }

    pub fn remove_token(&self, name: &str) -> Result<bool, String> {
        let mut state = self.state();
        self.reload_if_changed(&mut state);
        let mut file = state.file.clone();
        if !file.remove(name) {
            return Ok(false);
        }
        file.save(&self.inner.path)?;
        state.file = file;
        state.modified = modified(&self.inner.path);
        Ok(true)
    }

    pub fn check(&self, presented: Option<&str>) -> Option<String> {
        let mut state = self.state();
        self.reload_if_changed(&mut state);
        let name = presented.and_then(|token| state.file.find(token)).map(|token| token.name.clone());
        if name.is_none() {
            state.denied += 1;
        }
        name
    }

    /// Like `check`, for requests the front server answers itself; doesn't count denials twice.
    pub fn peek(&self, presented: Option<&str>) -> Option<String> {
        let mut state = self.state();
        self.reload_if_changed(&mut state);
        presented.and_then(|token| state.file.find(token)).map(|token| token.name.clone())
    }

    fn admit(&self, connection: ConnectionId, endpoint: EndpointId, token: String) {
        let mut state = self.state();
        state.accepted += 1;
        state.live.insert(connection, Live { endpoint, token });
        Self::touch(&mut state, endpoint);
    }

    fn touch(state: &mut State, endpoint: EndpointId) {
        if state.seen.len() >= MAX_SEEN && !state.seen.contains_key(&endpoint) {
            let live: HashSet<EndpointId> = state.live.values().map(|live| live.endpoint).collect();
            state.seen.retain(|id, _| live.contains(id));
        }
        state.seen.insert(endpoint, now_secs());
    }

    /// Online state and last-seen time (unix seconds, 0 = not seen since the server started).
    /// Device ids are long public keys only paired computers know, so the id itself is the permission.
    pub fn presence(&self, ids: &[EndpointId]) -> Vec<(bool, u64)> {
        let state = self.state();
        let live: HashSet<EndpointId> = state.live.values().map(|live| live.endpoint).collect();
        let now = now_secs();
        ids.iter().map(|id| if live.contains(id) { (true, now) } else { (false, state.seen.get(id).copied().unwrap_or(0)) }).collect()
    }

    pub fn status(&self) -> Value {
        let mut state = self.state();
        self.reload_if_changed(&mut state);
        let endpoints: HashSet<EndpointId> = state.live.values().map(|live| live.endpoint).collect();
        let per_token: Vec<Value> = state
            .file
            .tokens
            .iter()
            .map(|token| json!({ "name": token.name, "created": token.created, "live": state.live.values().filter(|live| live.token == token.name).count() }))
            .collect();
        json!({
            "version": env!("CARGO_PKG_VERSION"),
            "uptimeSecs": self.inner.started.elapsed().as_secs(),
            "liveConnections": state.live.len(),
            "liveDevices": endpoints.len(),
            "accepted": state.accepted,
            "denied": state.denied,
            "tokens": per_token,
        })
    }
}

impl AccessControl for Gate {
    async fn on_connect(&self, request: &ClientRequest) -> Access {
        match self.check(request.auth_token().as_deref()) {
            Some(name) => {
                self.admit(request.connection_id(), request.endpoint_id(), name);
                Access::Allow
            }
            None => Access::Deny { reason: Some("access token required".into()) },
        }
    }

    fn on_disconnect(&self, endpoint_id: EndpointId, connection_id: ConnectionId) {
        let mut state = self.state();
        state.live.remove(&connection_id);
        Self::touch(&mut state, endpoint_id);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tokens_are_added_found_and_removed() {
        let mut file = TokenFile::default();
        let home = file.add(" 우리집 ").unwrap();
        assert_eq!(home.name, "우리집");
        assert_eq!(home.token.len(), TOKEN_LEN);
        assert!(home.token.bytes().all(|byte| TOKEN_ALPHABET.contains(&byte)));
        assert!(file.add("우리집").is_err());
        assert!(file.add("  ").is_err());
        assert_eq!(file.find(&home.token).unwrap().name, "우리집");
        assert!(file.find("wrong").is_none());
        assert!(file.remove("우리집") && !file.remove("우리집"));
        assert!(file.find(&home.token).is_none());
    }

    #[test]
    fn gate_sees_cli_edits_without_restart() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("tokens.json");
        let gate = Gate::open(&path).unwrap();
        assert!(gate.check(Some("anything")).is_none());

        std::thread::sleep(std::time::Duration::from_millis(20));
        let mut file = TokenFile::load(&path).unwrap();
        let token = file.add("회사").unwrap();
        file.save(&path).unwrap();
        assert_eq!(gate.check(Some(&token.token)).as_deref(), Some("회사"));

        let added = gate.add_token("노트북").unwrap();
        assert_eq!(TokenFile::load(&path).unwrap().tokens.len(), 2);
        assert!(gate.remove_token("회사").unwrap());
        assert!(gate.check(Some(&token.token)).is_none());
        assert!(gate.check(Some(&added.token)).is_some());
        assert_eq!(gate.status()["denied"], 2);
    }

    #[test]
    fn a_broken_file_keeps_the_last_good_list() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("tokens.json");
        let gate = Gate::open(&path).unwrap();
        let token = gate.add_token("집").unwrap();
        std::thread::sleep(std::time::Duration::from_millis(20));
        std::fs::write(&path, "{ not json").unwrap();
        assert!(gate.check(Some(&token.token)).is_some());
    }
}
