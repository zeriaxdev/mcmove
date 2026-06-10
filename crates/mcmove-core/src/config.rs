//! Server profiles and paths under `~/.config/mcmove/` — the exact same files the
//! Python tool uses (`servers.json`, `state/`, `backups/`), so both coexist during
//! the migration.

use std::collections::BTreeMap;
use std::fs;
use std::path::PathBuf;

use serde::{Deserialize, Serialize};

use crate::{Error, Result};

pub fn config_dir() -> PathBuf {
    dirs::home_dir().unwrap_or_default().join(".config/mcmove")
}

pub fn state_dir() -> PathBuf {
    config_dir().join("state")
}

pub fn backup_dir() -> PathBuf {
    config_dir().join("backups")
}

fn config_file() -> PathBuf {
    config_dir().join("servers.json")
}

#[derive(Debug, Default, Serialize, Deserialize)]
pub struct Config {
    #[serde(default)]
    pub servers: BTreeMap<String, Profile>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Profile {
    pub host: String,
    #[serde(default = "default_port")]
    pub port: u16,
    pub username: String,
    #[serde(default)]
    pub key_path: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub last_src: String,
    /// Round-trip any fields this version doesn't know about.
    #[serde(flatten)]
    pub extra: serde_json::Map<String, serde_json::Value>,
}

fn default_port() -> u16 {
    2022
}

pub fn load() -> Result<Config> {
    let path = config_file();
    if !path.exists() {
        return Ok(Config::default());
    }
    Ok(serde_json::from_str(&fs::read_to_string(path)?)?)
}

pub fn save(cfg: &Config) -> Result<()> {
    fs::create_dir_all(config_dir())?;
    fs::write(config_file(), serde_json::to_vec_pretty(cfg)?)?;
    Ok(())
}

/// Parse `sftp://admin.100b3b70@node1.example.com:2022` → (host, port, username).
/// Scheme and port are optional; the username may be percent-encoded.
pub fn parse_sftp_url(url: &str) -> Result<(String, u16, String)> {
    let url = url.trim();
    let rest = url.split_once("://").map_or(url, |(_, r)| r);
    let (user, hostport) = match rest.rsplit_once('@') {
        Some((u, h)) => (percent_decode(u), h),
        None => (String::new(), rest),
    };
    let (host, port) = match hostport.rsplit_once(':') {
        Some((h, p)) => (
            h,
            p.parse::<u16>()
                .map_err(|_| Error::Other(format!("bad port in: {url}")))?,
        ),
        None => (hostport, 2022),
    };
    if host.is_empty() {
        return Err(Error::Other(format!("could not parse SFTP URL: {url}")));
    }
    Ok((host.to_string(), port, user))
}

fn percent_decode(s: &str) -> String {
    let bytes = s.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%' && i + 2 < bytes.len() {
            if let Ok(b) = u8::from_str_radix(&s[i + 1..i + 3], 16) {
                out.push(b);
                i += 3;
                continue;
            }
        }
        out.push(bytes[i]);
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

/// The most recently used local instance across all profiles (for prompts).
pub fn remembered_src(cfg: &Config) -> String {
    cfg.servers
        .values()
        .find(|s| !s.last_src.is_empty())
        .map(|s| s.last_src.clone())
        .unwrap_or_default()
}
