//! Mojang API lookups: username ↔ UUID. Online-mode (Mojang-auth) UUIDs only.

use std::collections::HashMap;

use serde::Deserialize;

pub fn looks_like_uuid(s: &str) -> bool {
    let hex: String = s.trim().chars().filter(|c| *c != '-').collect();
    hex.len() == 32 && hex.chars().all(|c| c.is_ascii_hexdigit())
}

pub fn hyphenate_uuid(u: &str) -> String {
    let u: String = u
        .chars()
        .filter(|c| *c != '-')
        .collect::<String>()
        .to_lowercase();
    format!(
        "{}-{}-{}-{}-{}",
        &u[0..8],
        &u[8..12],
        &u[12..16],
        &u[16..20],
        &u[20..32]
    )
}

/// Bulk username → hyphenated UUID, 10 per request (the API's cap). Best-effort:
/// unknown names and failed chunks are simply absent from the result.
pub async fn resolve_uuids(client: &reqwest::Client, names: &[String]) -> HashMap<String, String> {
    #[derive(Deserialize)]
    struct Entry {
        id: String,
        name: String,
    }
    let mut unique: Vec<&String> = names.iter().collect();
    unique.sort();
    unique.dedup();
    let mut out = HashMap::new();
    for chunk in unique.chunks(10) {
        let result = async {
            client
                .post("https://api.mojang.com/profiles/minecraft")
                .json(&chunk)
                .send()
                .await?
                .error_for_status()?
                .json::<Vec<Entry>>()
                .await
        }
        .await;
        if let Ok(entries) = result {
            for e in entries {
                out.insert(e.name.to_lowercase(), hyphenate_uuid(&e.id));
            }
        }
    }
    out
}

/// Reverse: UUID (hyphenated or plain) → current username, or None.
pub async fn uuid_to_name(client: &reqwest::Client, uuid: &str) -> Option<String> {
    #[derive(Deserialize)]
    struct Profile {
        name: String,
    }
    let u: String = uuid.chars().filter(|c| *c != '-').collect();
    let url = format!("https://sessionserver.mojang.com/session/minecraft/profile/{u}");
    let resp = client.get(url).send().await.ok()?.error_for_status().ok()?;
    resp.json::<Profile>().await.ok().map(|p| p.name)
}
