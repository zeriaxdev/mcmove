//! Server mod sync: classify local jars client/server, diff against the per-server
//! managed manifest + the server's /mods listing, and apply over SFTP.
//!
//! State lives at `~/.config/mcmove/state/<server>.json` — same file the Python
//! tool reads/writes, keyed by Modrinth project id / mod id.

use std::collections::{BTreeMap, HashSet};
use std::fs;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::config::state_dir;
use crate::progress::{Progress, Reporter};
use crate::sftp::{join, Sftp};
use crate::{modrinth, pack, Result};

/// Which side a local mod belongs to.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Side {
    /// Server-side (or both) — sync it.
    Keep,
    /// Client-only — never push to the server.
    Client,
    /// Couldn't determine — keep it, to be safe.
    Unknown,
}

#[derive(Debug, Clone)]
pub struct ModInfo {
    pub path: PathBuf,
    pub filename: String,
    pub sha1: String,
    pub key: String,
    pub side: Side,
}

#[derive(Debug, Default, Serialize, Deserialize)]
pub struct StateManifest {
    #[serde(default)]
    pub mods: BTreeMap<String, ManagedMod>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ManagedMod {
    pub filename: String,
    pub sha1: String,
    pub side: Side,
}

pub fn load_state(server: &str) -> Result<StateManifest> {
    let p = state_dir().join(format!("{server}.json"));
    if !p.exists() {
        return Ok(StateManifest::default());
    }
    Ok(serde_json::from_str(&fs::read_to_string(p)?)?)
}

pub fn save_state(server: &str, man: &StateManifest) -> Result<()> {
    fs::create_dir_all(state_dir())?;
    fs::write(
        state_dir().join(format!("{server}.json")),
        serde_json::to_vec_pretty(man)?,
    )?;
    Ok(())
}

/// Classify jars: Modrinth `server_side == "unsupported"` ⇒ client-only; offline
/// fallback reads the jar's own metadata (fabric `environment`; forge unknown).
pub async fn classify_mods(
    paths: &[PathBuf],
    client: &reqwest::Client,
    reporter: &dyn Reporter,
) -> Result<Vec<ModInfo>> {
    let mut infos = Vec::with_capacity(paths.len());
    for p in paths {
        infos.push(ModInfo {
            filename: p
                .file_name()
                .unwrap_or_default()
                .to_string_lossy()
                .into_owned(),
            sha1: pack::sha1_of(p)?,
            path: p.clone(),
            key: String::new(),
            side: Side::Unknown,
        });
    }
    let hashes: Vec<String> = infos.iter().map(|i| i.sha1.clone()).collect();
    let sides = modrinth::sides(client, &hashes, reporter).await;
    for i in &mut infos {
        if let Some((pid, server_side)) = sides.get(&i.sha1) {
            i.key = format!("modrinth:{pid}");
            i.side = if server_side.as_deref() == Some("unsupported") {
                Side::Client
            } else {
                Side::Keep
            };
            continue;
        }
        let (modid, env) = pack::jar_id_and_env(&i.path);
        i.key = match &modid {
            Some(id) => format!("mod:{id}"),
            None => format!("file:{}", i.filename),
        };
        i.side = match env.as_deref() {
            Some("client") => Side::Client,
            Some("server") | Some("both") => Side::Keep,
            _ => Side::Unknown,
        };
    }
    Ok(infos)
}

#[derive(Debug, Default)]
pub struct SyncPlan {
    /// New uploads (full local info, including path).
    pub add: Vec<ModInfo>,
    /// Changed uploads.
    pub update: Vec<ModInfo>,
    /// Remote filenames to delete (deduped, in order).
    pub remove: Vec<String>,
    /// Unchanged count.
    pub keep: usize,
    /// Client-only local filenames that were skipped.
    pub client: Vec<String>,
    /// Filenames whose side couldn't be determined (kept).
    pub unknown: Vec<String>,
}

impl SyncPlan {
    pub fn is_noop(&self) -> bool {
        self.add.is_empty() && self.update.is_empty() && self.remove.is_empty()
    }
}

/// Diff local mods vs the managed manifest + actual remote listing.
/// Returns the plan and the new managed map to persist after applying.
pub fn plan_sync(
    infos: &[ModInfo],
    manifest: &StateManifest,
    remote_files: &HashSet<String>,
) -> (SyncPlan, BTreeMap<String, ManagedMod>) {
    let managed = &manifest.mods;
    let mut plan = SyncPlan::default();
    let mut new_managed = BTreeMap::new();
    let mut seen = HashSet::new();

    for i in infos {
        seen.insert(i.key.clone());
        if i.side == Side::Client {
            // a previously-pushed copy (under either name) must come off the server
            let mut names: Vec<&str> = Vec::new();
            if let Some(prev) = managed.get(&i.key) {
                names.push(&prev.filename);
            }
            names.push(&i.filename);
            for fn_ in names {
                if remote_files.contains(fn_) && !plan.remove.iter().any(|r| r == fn_) {
                    plan.remove.push(fn_.to_string());
                }
            }
            plan.client.push(i.filename.clone());
            continue;
        }
        if i.side == Side::Unknown {
            plan.unknown.push(i.filename.clone());
        }
        match managed.get(&i.key) {
            Some(prev) => {
                if prev.sha1 == i.sha1 {
                    if remote_files.contains(&i.filename) {
                        plan.keep += 1;
                    } else {
                        plan.add.push(i.clone());
                    }
                } else {
                    if remote_files.contains(&prev.filename) && prev.filename != i.filename {
                        plan.remove.push(prev.filename.clone());
                    }
                    plan.update.push(i.clone());
                }
            }
            None => {
                if remote_files.contains(&i.filename) {
                    plan.keep += 1;
                } else {
                    plan.add.push(i.clone());
                }
            }
        }
        new_managed.insert(
            i.key.clone(),
            ManagedMod {
                filename: i.filename.clone(),
                sha1: i.sha1.clone(),
                side: i.side,
            },
        );
    }
    // mods that left the pack: drop the ones we previously managed
    for (key, m) in managed {
        if !seen.contains(key)
            && remote_files.contains(&m.filename)
            && !plan.remove.iter().any(|r| r == &m.filename)
        {
            plan.remove.push(m.filename.clone());
        }
    }
    (plan, new_managed)
}

/// Apply a confirmed sync plan to the server's /mods.
pub async fn execute_sync(sftp: &Sftp, plan: &SyncPlan, reporter: &dyn Reporter) -> Result<()> {
    sftp.mkdirs("/mods").await?;
    for fn_ in &plan.remove {
        let _ = sftp.remove_file(&join("/mods", fn_)).await;
        reporter.report(Progress::Info {
            message: format!("- {fn_}"),
        });
    }
    for i in plan.add.iter().chain(&plan.update) {
        sftp.put(&i.path, &join("/mods", &i.filename)).await?;
        reporter.report(Progress::Info {
            message: format!("↑ {}", i.filename),
        });
    }
    Ok(())
}

/// The server's current /mods listing — real `.jar` files only (panel SFTP
/// listings can contain junk).
pub async fn remote_mod_files(sftp: &Sftp) -> Result<HashSet<String>> {
    if !sftp.exists("/mods").await {
        return Ok(HashSet::new());
    }
    Ok(sftp
        .listdir("/mods")
        .await?
        .into_iter()
        .filter(|(n, is_dir)| !is_dir && n.ends_with(".jar"))
        .map(|(n, _)| n)
        .collect())
}

/// Jar paths in `<src>/mods`, sorted.
pub fn local_jars(src: &Path) -> Result<Vec<PathBuf>> {
    let mods_dir = src.join("mods");
    let mut paths: Vec<PathBuf> = fs::read_dir(&mods_dir)?
        .filter_map(|e| e.ok().map(|e| e.path()))
        .filter(|p| p.is_file() && p.extension().is_some_and(|e| e.eq_ignore_ascii_case("jar")))
        .collect();
    paths.sort();
    Ok(paths)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn info(key: &str, filename: &str, sha1: &str, side: Side) -> ModInfo {
        ModInfo {
            path: PathBuf::from(filename),
            filename: filename.into(),
            sha1: sha1.into(),
            key: key.into(),
            side,
        }
    }

    fn managed(filename: &str, sha1: &str, side: Side) -> ManagedMod {
        ManagedMod {
            filename: filename.into(),
            sha1: sha1.into(),
            side,
        }
    }

    fn remote(names: &[&str]) -> HashSet<String> {
        names.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn fresh_sync_adds_everything() {
        let infos = vec![
            info("modrinth:a", "a-1.jar", "s1", Side::Keep),
            info("mod:b", "b-1.jar", "s2", Side::Unknown),
        ];
        let (plan, new_managed) = plan_sync(&infos, &StateManifest::default(), &remote(&[]));
        assert_eq!(plan.add.len(), 2);
        assert!(plan.update.is_empty() && plan.remove.is_empty());
        assert_eq!(plan.unknown, vec!["b-1.jar"]);
        assert_eq!(new_managed.len(), 2);
    }

    #[test]
    fn client_mod_skipped_and_purged_from_remote() {
        let infos = vec![info("modrinth:iris", "iris.jar", "s1", Side::Client)];
        let mut man = StateManifest::default();
        // we previously pushed it under an old filename
        man.mods.insert(
            "modrinth:iris".into(),
            managed("iris-old.jar", "s0", Side::Keep),
        );
        let (plan, new_managed) = plan_sync(&infos, &man, &remote(&["iris-old.jar", "iris.jar"]));
        let mut removed = plan.remove.clone();
        removed.sort();
        assert_eq!(removed, vec!["iris-old.jar", "iris.jar"]);
        assert_eq!(plan.client, vec!["iris.jar"]);
        assert!(!new_managed.contains_key("modrinth:iris")); // client mods aren't managed
    }

    #[test]
    fn version_bump_updates_and_removes_old_name() {
        let infos = vec![info("modrinth:a", "a-2.jar", "s2", Side::Keep)];
        let mut man = StateManifest::default();
        man.mods
            .insert("modrinth:a".into(), managed("a-1.jar", "s1", Side::Keep));
        let (plan, _) = plan_sync(&infos, &man, &remote(&["a-1.jar"]));
        assert_eq!(plan.update.len(), 1);
        assert_eq!(plan.remove, vec!["a-1.jar"]);
    }

    #[test]
    fn mod_leaving_pack_is_removed() {
        let infos = vec![info("modrinth:a", "a-1.jar", "s1", Side::Keep)];
        let mut man = StateManifest::default();
        man.mods
            .insert("modrinth:a".into(), managed("a-1.jar", "s1", Side::Keep));
        man.mods.insert(
            "modrinth:gone".into(),
            managed("gone.jar", "s9", Side::Keep),
        );
        let (plan, new_managed) = plan_sync(&infos, &man, &remote(&["a-1.jar", "gone.jar"]));
        assert_eq!(plan.keep, 1);
        assert_eq!(plan.remove, vec!["gone.jar"]);
        assert!(!new_managed.contains_key("modrinth:gone"));
    }

    #[test]
    fn unmanaged_remote_files_are_untouched() {
        // a jar on the server that mcmove never managed must not be deleted
        let infos = vec![info("modrinth:a", "a-1.jar", "s1", Side::Keep)];
        let (plan, _) = plan_sync(&infos, &StateManifest::default(), &remote(&["mystery.jar"]));
        assert!(plan.remove.is_empty());
        assert_eq!(plan.add.len(), 1);
    }

    #[test]
    fn managed_but_missing_remote_is_readded() {
        // someone deleted the jar on the server -> we re-upload it
        let infos = vec![info("modrinth:a", "a-1.jar", "s1", Side::Keep)];
        let mut man = StateManifest::default();
        man.mods
            .insert("modrinth:a".into(), managed("a-1.jar", "s1", Side::Keep));
        let (plan, _) = plan_sync(&infos, &man, &remote(&[]));
        assert_eq!(plan.add.len(), 1);
        assert_eq!(plan.keep, 0);
    }
}
