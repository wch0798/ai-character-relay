//! `/etc/aic-relay/config.toml`.

use serde::{Deserialize, Serialize};
use std::net::SocketAddr;
use std::path::{Path, PathBuf};

pub const DEFAULT_CONFIG_PATH: &str = "/etc/aic-relay/config.toml";

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct Config {
    /// The relay's domain; its DNS A record must point at this server.
    pub domain: String,
    /// Let's Encrypt contact address.
    pub email: String,
    #[serde(default = "default_data_dir")]
    pub data_dir: PathBuf,
    #[serde(default = "default_http_bind")]
    pub http_bind: SocketAddr,
    #[serde(default = "default_https_bind")]
    pub https_bind: SocketAddr,
    /// QUIC address discovery helps two computers reach each other directly.
    #[serde(default = "yes")]
    pub enable_quic: bool,
    #[serde(default = "default_quic_bind")]
    pub quic_bind: SocketAddr,
    /// The admin page has no TLS of its own, so keep it on 127.0.0.1 and reach it over SSH.
    #[serde(default = "default_admin_bind")]
    pub admin_bind: SocketAddr,
    pub admin_password: String,
    /// Also serve the admin page at `https://<domain>/admin/` (same password, guessing gets blocked).
    #[serde(default)]
    pub https_admin: bool,
    /// The phone page at `https://<domain>/app/` (static files; chats stay end-to-end encrypted).
    #[serde(default = "yes")]
    pub phone_web: bool,
    /// Plain HTTP on `http_bind` only, no certificates. For local tests only.
    #[serde(default)]
    pub dev: bool,
    #[serde(default = "yes")]
    pub production_certs: bool,
    /// Your own certificate (PEM chain) and key instead of Let's Encrypt. Set both or neither.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tls_cert: Option<PathBuf>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tls_key: Option<PathBuf>,
}

fn yes() -> bool {
    true
}

fn default_data_dir() -> PathBuf {
    PathBuf::from("/var/lib/aic-relay")
}

fn default_http_bind() -> SocketAddr {
    "[::]:80".parse().unwrap()
}

fn default_https_bind() -> SocketAddr {
    "[::]:443".parse().unwrap()
}

fn default_quic_bind() -> SocketAddr {
    "[::]:7842".parse().unwrap()
}

fn default_admin_bind() -> SocketAddr {
    "127.0.0.1:7843".parse().unwrap()
}

impl Config {
    pub fn new(domain: &str, email: &str, admin_password: String) -> Self {
        Self {
            domain: domain.to_string(),
            email: email.to_string(),
            data_dir: default_data_dir(),
            http_bind: default_http_bind(),
            https_bind: default_https_bind(),
            enable_quic: true,
            quic_bind: default_quic_bind(),
            admin_bind: default_admin_bind(),
            admin_password,
            https_admin: false,
            phone_web: true,
            dev: false,
            production_certs: true,
            tls_cert: None,
            tls_key: None,
        }
    }

    pub fn parse(text: &str) -> Result<Self, String> {
        let config: Self = toml::from_str(text).map_err(|error| format!("설정 파일을 읽지 못했습니다: {error}"))?;
        config.validate()?;
        Ok(config)
    }

    pub fn load(path: &Path) -> Result<Self, String> {
        let text = std::fs::read_to_string(path).map_err(|error| format!("{} 을(를) 열지 못했습니다: {error} (먼저 `aic-relay init` 을 실행하세요)", path.display()))?;
        Self::parse(&text)
    }

    pub fn to_toml(&self) -> String {
        toml::to_string_pretty(self).unwrap_or_default()
    }

    pub fn validate(&self) -> Result<(), String> {
        if !valid_domain(&self.domain) {
            return Err(format!("도메인 형식이 아닙니다: {}", self.domain));
        }
        if !self.dev && !valid_email(&self.email) {
            return Err(format!("이메일 형식이 아닙니다: {}", self.email));
        }
        if self.admin_password.len() < 12 {
            return Err("admin_password 는 12자 이상이어야 합니다.".into());
        }
        if self.tls_cert.is_some() != self.tls_key.is_some() {
            return Err("tls_cert 와 tls_key 는 함께 넣거나 둘 다 빼야 합니다.".into());
        }
        Ok(())
    }

    pub fn tokens_path(&self) -> PathBuf {
        self.data_dir.join("tokens.json")
    }

    pub fn blocks_path(&self) -> PathBuf {
        self.data_dir.join("blocks.json")
    }

    pub fn cert_dir(&self) -> PathBuf {
        self.data_dir.join("certs")
    }
}

pub fn valid_domain(domain: &str) -> bool {
    !domain.is_empty()
        && domain.len() <= 253
        && !domain.starts_with(['.', '-'])
        && !domain.ends_with(['.', '-'])
        && domain.chars().all(|c| c.is_ascii_alphanumeric() || c == '.' || c == '-')
}

pub fn valid_email(email: &str) -> bool {
    match email.split_once('@') {
        Some((user, host)) => !user.is_empty() && host.contains('.') && !email.chars().any(char::is_whitespace),
        None => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn minimal_config_gets_defaults() {
        let config = Config::parse("domain = \"relay.example.com\"\nemail = \"me@example.com\"\nadmin_password = \"abcdefghijklmnop\"\n").unwrap();
        assert_eq!(config.https_bind.port(), 443);
        assert_eq!(config.quic_bind.port(), 7842);
        assert!(config.admin_bind.ip().is_loopback());
        assert_eq!(config.tokens_path(), PathBuf::from("/var/lib/aic-relay/tokens.json"));
        assert!(config.enable_quic && config.production_certs && !config.dev && !config.https_admin);
        assert_eq!(Config::parse(&config.to_toml()).unwrap(), config);
    }

    #[test]
    fn bad_values_are_explained() {
        assert!(Config::parse("domain = \"bad domain\"\nemail = \"me@example.com\"\nadmin_password = \"abcdefghijklmnop\"\n").is_err());
        assert!(Config::parse("domain = \"relay.example.com\"\nemail = \"nope\"\nadmin_password = \"abcdefghijklmnop\"\n").is_err());
        assert!(Config::parse("domain = \"relay.example.com\"\nemail = \"me@example.com\"\nadmin_password = \"short\"\n").is_err());
        assert!(valid_domain("relay.my-domain.co.kr") && !valid_domain("-x.com") && !valid_domain("a/b"));
    }
}
