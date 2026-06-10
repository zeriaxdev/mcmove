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
}

/// Shared HTTP client with the project User-Agent (Modrinth requires one).
pub fn client() -> Result<reqwest::Client> {
    Ok(reqwest::Client::builder()
        .user_agent(format!("mcmove/{} (github.com/zeriaxdev/mcmove)", crate::VERSION))
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
