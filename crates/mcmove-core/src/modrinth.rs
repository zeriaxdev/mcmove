//! Minimal Modrinth API client — only what the patcher needs today.
//! `update`/`sync` will extend this when they are ported.

use std::collections::HashMap;
use std::time::Duration;

use serde::Deserialize;

use crate::progress::{Progress, Reporter};
use crate::Result;

pub const API: &str = "https://api.modrinth.com/v2";

#[derive(Debug, Clone, Deserialize)]
pub struct Version {
    pub id: String,
    pub project_id: String,
    pub version_number: String,
    pub version_type: String,
    #[serde(default)]
    pub date_published: String,
    #[serde(default)]
    pub game_versions: Vec<String>,
    #[serde(default)]
    pub loaders: Vec<String>,
    #[serde(default)]
    pub files: Vec<VersionFile>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct VersionFile {
    pub url: String,
    pub filename: String,
    #[serde(default)]
    pub primary: bool,
    #[serde(default)]
    pub size: u64,
    #[serde(default)]
    pub hashes: HashMap<String, String>,
}

impl Version {
    /// The file in this version whose sha1 matches, if it has a usable URL.
    pub fn file_with_sha1(&self, sha1: &str) -> Option<&VersionFile> {
        self.files
            .iter()
            .find(|f| !f.url.is_empty() && f.hashes.get("sha1").is_some_and(|h| h == sha1))
    }

    /// The file marked primary, or the first one.
    pub fn primary_file(&self) -> Option<&VersionFile> {
        self.files.iter().find(|f| f.primary).or(self.files.first())
    }
}

/// Release channel = which version types are acceptable, widest channel last.
pub fn channel_allows(channel: &str, version_type: &str) -> bool {
    match channel {
        "release" => version_type == "release",
        "beta" => matches!(version_type, "release" | "beta"),
        "alpha" => matches!(version_type, "release" | "beta" | "alpha"),
        _ => false,
    }
}

/// Shared HTTP client with the project User-Agent (Modrinth requires one).
pub fn client() -> Result<reqwest::Client> {
    Ok(reqwest::Client::builder()
        .user_agent(format!(
            "mcmove/{} (github.com/zeriaxdev/mcmove)",
            crate::VERSION
        ))
        .timeout(Duration::from_secs(120))
        .build()?)
}

/// Batch sha1 → version lookup via POST /version_files, 100 hashes per request.
/// Failed chunks are reported as warnings and skipped so unknown jars degrade to
/// "bundled" instead of failing the whole scan.
pub async fn version_files(
    client: &reqwest::Client,
    hashes: &[String],
    reporter: &dyn Reporter,
) -> HashMap<String, Version> {
    let mut out = HashMap::new();
    for chunk in hashes.chunks(100) {
        let payload = serde_json::json!({ "hashes": chunk, "algorithm": "sha1" });
        let result = async {
            let resp = client
                .post(format!("{API}/version_files"))
                .json(&payload)
                .send()
                .await?
                .error_for_status()?;
            resp.json::<HashMap<String, Version>>().await
        }
        .await;
        match result {
            Ok(map) => out.extend(map),
            Err(e) => reporter.report(Progress::Warn {
                message: format!("Modrinth lookup failed for {} file(s): {e}", chunk.len()),
            }),
        }
    }
    out
}

/// All versions of a project filtered to game version + loader, newest first.
/// Best-effort: empty on failure.
pub async fn project_versions(
    client: &reqwest::Client,
    project_id: &str,
    game_versions: &[String],
    loaders: &[String],
) -> Vec<Version> {
    let mut url = format!("{API}/project/{project_id}/version");
    let mut params = Vec::new();
    for (name, values) in [("game_versions", game_versions), ("loaders", loaders)] {
        if !values.is_empty() {
            let mut sorted: Vec<&String> = values.iter().collect();
            sorted.sort();
            sorted.dedup();
            let json = serde_json::to_string(&sorted).unwrap_or_default();
            params.push(format!("{name}={}", urlencode(&json)));
        }
    }
    if !params.is_empty() {
        url.push('?');
        url.push_str(&params.join("&"));
    }
    let result = async {
        client
            .get(&url)
            .send()
            .await?
            .error_for_status()?
            .json::<Vec<Version>>()
            .await
    }
    .await;
    let mut versions = result.unwrap_or_default();
    versions.sort_by(|a, b| b.date_published.cmp(&a.date_published));
    versions
}

/// sha1 → (project_id, server_side) for client/server classification.
/// `server_side == "unsupported"` means client-only. Best-effort; empty on failure
/// (callers fall back to in-jar metadata).
pub async fn sides(
    client: &reqwest::Client,
    hashes: &[String],
    reporter: &dyn Reporter,
) -> HashMap<String, (String, Option<String>)> {
    #[derive(Deserialize)]
    struct Project {
        id: String,
        #[serde(default)]
        server_side: Option<String>,
    }
    if hashes.is_empty() {
        return HashMap::new();
    }
    let versions = version_files(client, hashes, reporter).await;
    let mut pids: Vec<&str> = versions.values().map(|v| v.project_id.as_str()).collect();
    pids.sort();
    pids.dedup();
    let mut side_by_pid: HashMap<String, Option<String>> = HashMap::new();
    if !pids.is_empty() {
        let ids = serde_json::to_string(&pids).unwrap_or_default();
        let result = async {
            client
                .get(format!("{API}/projects?ids={}", urlencode(&ids)))
                .send()
                .await?
                .error_for_status()?
                .json::<Vec<Project>>()
                .await
        }
        .await;
        if let Ok(projects) = result {
            for p in projects {
                side_by_pid.insert(p.id, p.server_side);
            }
        }
    }
    versions
        .into_iter()
        .map(|(h, v)| {
            let side = side_by_pid.get(&v.project_id).cloned().flatten();
            (h, (v.project_id, side))
        })
        .collect()
}

fn urlencode(s: &str) -> String {
    s.bytes()
        .map(|b| match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                (b as char).to_string()
            }
            _ => format!("%{b:02X}"),
        })
        .collect()
}
