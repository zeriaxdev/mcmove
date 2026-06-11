//! The modpack patcher: create / share / apply `.mcmpatch` bundles for PC→PC mod updates.
//!
//! Port of the Go sidecar (`modpack_patch.go`). The on-disk format is unchanged
//! (`format: 1`), so patches made by either tool apply with the other: a zip holding
//! `manifest.json` plus `assets/mods/<sha1>-<name>.jar` for jars Modrinth can't identify.
//! Modrinth-recognized jars carry only a download ref and are fetched on the receiver.

use std::collections::HashMap;
use std::fs;
use std::io::{Read, Write};
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use sha1::{Digest, Sha1};
use zip::write::SimpleFileOptions;
use zip::{ZipArchive, ZipWriter};

use crate::modrinth;
use crate::progress::{Progress, Reporter};
use crate::{Error, Result};

pub const FORMAT: u32 = 1;
pub const FILEBIN: &str = "https://filebin.net";
pub const DEFAULT_REMOTE_NAME: &str = "pack.mcmpatch";

#[derive(Debug, Serialize, Deserialize)]
pub struct Manifest {
    pub format: u32,
    pub created_at: String,
    pub source_instance: String,
    pub mods: Vec<ModEntry>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ModEntry {
    /// Local path of the jar this entry was scanned from. Never serialized.
    #[serde(skip)]
    pub path: PathBuf,
    pub filename: String,
    pub sha1: String,
    pub size: u64,
    /// Identity across versions: `modrinth:<project>`, `mod:<modid>`, or `file:<name>`.
    pub key: String,
    /// `modrinth` (receiver downloads) or `bundled` (jar shipped inside the patch).
    pub source: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub project_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub version_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub version_number: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub version_type: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub game_versions: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub loaders: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub download: Option<DownloadRef>,
    #[serde(rename = "modid", default, skip_serializing_if = "Option::is_none")]
    pub mod_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub loader: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub asset: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DownloadRef {
    pub url: String,
    pub filename: String,
    pub size: u64,
    pub sha1: String,
}

#[derive(Debug, Default)]
pub struct Plan {
    pub add: Vec<ModEntry>,
    /// (old local entry, new desired entry)
    pub update: Vec<(ModEntry, ModEntry)>,
    pub remove: Vec<ModEntry>,
    pub keep: Vec<ModEntry>,
}

impl Plan {
    pub fn is_noop(&self) -> bool {
        self.add.is_empty() && self.update.is_empty() && self.remove.is_empty()
    }
}

/// Scan `<instance>/mods/*.jar`, identify each jar against Modrinth by sha1, and fall
/// back to in-jar metadata (`fabric.mod.json` / `mods.toml`) for a stable key.
pub async fn scan_mods(
    instance: &Path,
    client: &reqwest::Client,
    reporter: &dyn Reporter,
) -> Result<Vec<ModEntry>> {
    let mods_dir = instance.join("mods");
    if !mods_dir.is_dir() {
        return Err(Error::Other(format!(
            "no mods/ folder in {}",
            instance.display()
        )));
    }
    let mut paths: Vec<PathBuf> = fs::read_dir(&mods_dir)?
        .filter_map(|e| e.ok().map(|e| e.path()))
        .filter(|p| p.is_file() && p.extension().is_some_and(|e| e.eq_ignore_ascii_case("jar")))
        .collect();
    paths.sort();
    if paths.is_empty() {
        return Err(Error::Other(format!(
            "no .jar files in {}",
            mods_dir.display()
        )));
    }

    reporter.report(Progress::Phase {
        name: format!("Scanning {} jar(s)", paths.len()),
    });
    let mut entries = Vec::with_capacity(paths.len());
    let mut hashes = Vec::with_capacity(paths.len());
    for p in paths {
        let sha = sha1_of(&p)?;
        let size = fs::metadata(&p)?.len();
        hashes.push(sha.clone());
        entries.push(ModEntry {
            filename: file_name(&p),
            sha1: sha,
            size,
            path: p,
            ..ModEntry::default()
        });
    }

    let hits = modrinth::version_files(client, &hashes, reporter).await;
    for e in &mut entries {
        if let Some((v, f)) = hits
            .get(&e.sha1)
            .and_then(|v| v.file_with_sha1(&e.sha1).map(|f| (v, f)))
        {
            e.key = format!("modrinth:{}", v.project_id);
            e.source = "modrinth".into();
            e.project_id = Some(v.project_id.clone());
            e.version_id = Some(v.id.clone());
            e.version_number = Some(v.version_number.clone());
            e.version_type = Some(v.version_type.clone());
            e.game_versions = v.game_versions.clone();
            e.loaders = v.loaders.clone();
            e.download = Some(DownloadRef {
                url: f.url.clone(),
                filename: if f.filename.is_empty() {
                    e.filename.clone()
                } else {
                    f.filename.clone()
                },
                size: if f.size == 0 { e.size } else { f.size },
                sha1: e.sha1.clone(),
            });
        } else {
            let (mod_id, loader) = read_jar_meta(&e.path);
            e.key = match &mod_id {
                Some(id) => format!("mod:{id}"),
                None => format!("file:{}", e.filename),
            };
            e.source = "bundled".into();
            e.asset = Some(format!("assets/mods/{}-{}", e.sha1, e.filename));
            e.mod_id = mod_id;
            e.loader = loader;
        }
    }
    reporter.report(Progress::PhaseDone {
        name: format!("Scanned {} jar(s)", entries.len()),
    });
    Ok(entries)
}

/// Read a mod's id + loader from inside the jar, for jars Modrinth doesn't know.
fn read_jar_meta(path: &Path) -> (Option<String>, Option<String>) {
    let Ok(file) = fs::File::open(path) else {
        return (None, None);
    };
    let Ok(mut zip) = ZipArchive::new(file) else {
        return (None, None);
    };

    if let Ok(mut f) = zip.by_name("fabric.mod.json") {
        #[derive(Deserialize)]
        struct FabricMod {
            id: String,
        }
        let mut buf = String::new();
        if f.read_to_string(&mut buf).is_ok() {
            if let Ok(m) = serde_json::from_str::<FabricMod>(&buf) {
                return (Some(m.id), Some("fabric".into()));
            }
        }
    }
    for (name, loader) in [
        ("META-INF/neoforge.mods.toml", "neoforge"),
        ("META-INF/mods.toml", "forge"),
    ] {
        let Ok(f) = zip.by_name(name) else { continue };
        let mut buf = String::new();
        if f.take(1 << 20).read_to_string(&mut buf).is_ok() {
            if let Some(id) = toml_mod_id(&buf) {
                return (Some(id), Some(loader.into()));
            }
        }
    }
    (None, None)
}

/// Like `read_jar_meta`, but the second value is the mod's *environment* —
/// `client` / `server` / `both` from fabric.mod.json, `None` for forge/neoforge
/// (their toml has no reliable side info) or unparseable jars.
pub fn jar_id_and_env(path: &Path) -> (Option<String>, Option<String>) {
    let Ok(file) = fs::File::open(path) else {
        return (None, None);
    };
    let Ok(mut zip) = ZipArchive::new(file) else {
        return (None, None);
    };

    if let Ok(mut f) = zip.by_name("fabric.mod.json") {
        #[derive(Deserialize)]
        struct FabricMod {
            id: String,
            #[serde(default)]
            environment: Option<String>,
        }
        let mut buf = String::new();
        if f.read_to_string(&mut buf).is_ok() {
            if let Ok(m) = serde_json::from_str::<FabricMod>(&buf) {
                let env = match m.environment.as_deref() {
                    Some("client") => "client",
                    Some("server") => "server",
                    _ => "both",
                };
                return (Some(m.id), Some(env.into()));
            }
        }
    }
    for name in ["META-INF/neoforge.mods.toml", "META-INF/mods.toml"] {
        let Ok(f) = zip.by_name(name) else { continue };
        let mut buf = String::new();
        if f.take(1 << 20).read_to_string(&mut buf).is_ok() {
            if let Some(id) = toml_mod_id(&buf) {
                return (Some(id), None);
            }
        }
    }
    (None, None)
}

/// Extract the first `modId = "<id>"` from a mods.toml without a TOML parser.
fn toml_mod_id(toml: &str) -> Option<String> {
    for line in toml.lines() {
        let Some(rest) = line.trim_start().strip_prefix("modId") else {
            continue;
        };
        let Some(rest) = rest.trim_start().strip_prefix('=') else {
            continue;
        };
        let rest = rest.trim_start();
        let quote = rest.chars().next()?;
        if quote == '"' || quote == '\'' {
            let inner = &rest[1..];
            if let Some(end) = inner.find(quote) {
                return Some(inner[..end].to_string());
            }
        }
    }
    None
}

/// Write the `.mcmpatch` zip: manifest.json + the jars Modrinth can't provide.
pub fn write_bundle(
    entries: &[ModEntry],
    out_path: &Path,
    source_instance: &str,
) -> Result<Manifest> {
    let man = Manifest {
        format: FORMAT,
        created_at: chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Secs, true),
        source_instance: source_instance.to_string(),
        mods: entries.to_vec(),
    };
    let out = fs::File::create(out_path)?;
    let mut zw = ZipWriter::new(out);
    let opts = SimpleFileOptions::default().compression_method(zip::CompressionMethod::Deflated);
    zw.start_file("manifest.json", opts)?;
    zw.write_all(&serde_json::to_vec_pretty(&man)?)?;
    for e in entries.iter().filter(|e| e.source == "bundled") {
        let asset = e.asset.as_deref().ok_or_else(|| {
            Error::Other(format!("{}: bundled entry has no asset path", e.filename))
        })?;
        zw.start_file(asset, opts)?;
        let mut input = fs::File::open(&e.path)?;
        std::io::copy(&mut input, &mut zw)?;
    }
    zw.finish()?;
    Ok(man)
}

pub fn load_bundle(path: &Path) -> Result<(ZipArchive<fs::File>, Manifest)> {
    let file = fs::File::open(path)?;
    let mut zip = ZipArchive::new(file)
        .map_err(|_| Error::Other(format!("not a patch zip: {}", path.display())))?;
    let man: Manifest = {
        let f = zip
            .by_name("manifest.json")
            .map_err(|_| Error::Other("patch has no manifest.json".into()))?;
        serde_json::from_reader(f)?
    };
    if man.format != FORMAT {
        return Err(Error::Other(format!(
            "unsupported patch format: {}",
            man.format
        )));
    }
    Ok((zip, man))
}

/// Diff desired (patch) vs current (local) by key. Without `keep_extra`, local mods
/// absent from the patch — and duplicate keys — are removed (mirror semantics).
pub fn plan_apply(desired: &[ModEntry], current: &[ModEntry], keep_extra: bool) -> Plan {
    let mut current_by_key: HashMap<&str, &ModEntry> = HashMap::new();
    let mut duplicates = Vec::new();
    for e in current {
        if current_by_key.contains_key(e.key.as_str()) {
            duplicates.push(e);
        } else {
            current_by_key.insert(&e.key, e);
        }
    }
    let mut plan = Plan::default();
    let mut desired_keys: HashMap<&str, ()> = HashMap::new();
    for want in desired {
        desired_keys.insert(&want.key, ());
        match current_by_key.get(want.key.as_str()) {
            None => plan.add.push(want.clone()),
            Some(have) if have.sha1 == want.sha1 => plan.keep.push(want.clone()),
            Some(have) => plan.update.push(((*have).clone(), want.clone())),
        }
    }
    if !keep_extra {
        for have in current {
            if !desired_keys.contains_key(have.key.as_str()) {
                plan.remove.push(have.clone());
            }
        }
        for have in duplicates {
            if desired_keys.contains_key(have.key.as_str()) {
                plan.remove.push(have.clone());
            }
        }
    }
    plan
}

/// Execute a confirmed plan against `<mods_dir>`: remove, then update, then add.
/// Every fetched file is sha1-verified in a staging dir before it touches mods/.
pub async fn execute_plan(
    archive: &mut ZipArchive<fs::File>,
    plan: &Plan,
    mods_dir: &Path,
    client: &reqwest::Client,
    reporter: &dyn Reporter,
) -> Result<()> {
    let staging = tempfile::tempdir()?;
    for m in &plan.remove {
        if fs::remove_file(&m.path).is_ok() {
            reporter.report(Progress::Info {
                message: format!("- {}", m.filename),
            });
        }
    }
    for (old, new) in &plan.update {
        let got = fetch_desired(archive, new, staging.path(), client).await?;
        let dest = mods_dir.join(safe_name(&new.filename)?);
        if old.path != dest {
            let _ = fs::remove_file(&old.path);
        }
        move_replace(&got, &dest)?;
        reporter.report(Progress::Info {
            message: format!("~ {} -> {}", old.filename, new.filename),
        });
    }
    for m in &plan.add {
        let got = fetch_desired(archive, m, staging.path(), client).await?;
        move_replace(&got, &mods_dir.join(safe_name(&m.filename)?))?;
        reporter.report(Progress::Info {
            message: format!("+ {}", m.filename),
        });
    }
    Ok(())
}

/// Materialize one desired entry into `staging` — Modrinth download or bundled
/// extraction — and verify its sha1.
async fn fetch_desired(
    archive: &mut ZipArchive<fs::File>,
    entry: &ModEntry,
    staging: &Path,
    client: &reqwest::Client,
) -> Result<PathBuf> {
    let dest = staging.join(safe_name(&entry.filename)?);
    if entry.source == "modrinth" {
        let url = entry
            .download
            .as_ref()
            .map(|d| d.url.as_str())
            .filter(|u| !u.is_empty())
            .ok_or_else(|| {
                Error::Other(format!("{} has no Modrinth download URL", entry.filename))
            })?;
        download_file(client, url, &dest).await?;
    } else {
        let asset = entry.asset.as_deref().unwrap_or_default();
        if !asset.starts_with("assets/mods/") || asset.contains("..") {
            return Err(Error::Other(format!(
                "unsafe bundled asset path for {}",
                entry.filename
            )));
        }
        let mut f = archive
            .by_name(asset)
            .map_err(|_| Error::Other(format!("missing bundled asset for {}", entry.filename)))?;
        let mut out = fs::File::create(&dest)?;
        std::io::copy(&mut f, &mut out)?;
    }
    let got = sha1_of(&dest)?;
    if got != entry.sha1 {
        let _ = fs::remove_file(&dest);
        return Err(Error::Other(format!(
            "sha1 mismatch for {}: expected {}, got {got}",
            entry.filename, entry.sha1
        )));
    }
    Ok(dest)
}

pub async fn download_file(client: &reqwest::Client, url: &str, dest: &Path) -> Result<()> {
    let resp = client.get(url).send().await?.error_for_status()?;
    let bytes = resp.bytes().await?;
    fs::write(dest, &bytes)?;
    Ok(())
}

/// Turn an `apply` argument into a local patch path: an existing file, an http(s)
/// URL, or a Filebin short code. Returns the temp dir keeping a download alive.
pub async fn resolve_patch_source(
    arg: &str,
    client: &reqwest::Client,
    reporter: &dyn Reporter,
) -> Result<(PathBuf, Option<tempfile::TempDir>)> {
    if Path::new(arg).is_file() {
        return Ok((PathBuf::from(arg), None));
    }
    let url = if arg.starts_with("http://") || arg.starts_with("https://") {
        arg.to_string()
    } else if valid_bin(arg) {
        format!("{FILEBIN}/{arg}/{DEFAULT_REMOTE_NAME}")
    } else {
        return Err(Error::Other(format!("no such file or share code: {arg}")));
    };
    reporter.report(Progress::Info {
        message: format!("Downloading patch: {url}"),
    });
    // Filebin gates downloads: the first GET returns an HTML page and sets a
    // `verified` cookie; only a follow-up request carrying that cookie 302s to the
    // real file. The client's cookie store captures it, so we prime once then fetch.
    if url.contains("filebin.net") {
        let _ = client.get(&url).send().await;
    }
    let tmp = tempfile::tempdir()?;
    let dest = tmp.path().join(DEFAULT_REMOTE_NAME);
    download_file(client, &url, &dest).await?;
    Ok((dest, Some(tmp)))
}

/// Raw POST to filebin.net/{bin}/{filename}, then lock the bin read-only
/// (lock failure is non-fatal — the link still works).
pub async fn upload_filebin(
    client: &reqwest::Client,
    bin: &str,
    filename: &str,
    path: &Path,
) -> Result<String> {
    let url = format!("{FILEBIN}/{bin}/{filename}");
    let body = fs::read(path)?;
    let resp = client
        .post(&url)
        .header("Content-Type", "application/octet-stream")
        .body(body)
        .send()
        .await?;
    if !resp.status().is_success() {
        let status = resp.status();
        let text = resp.text().await.unwrap_or_default();
        return Err(Error::Other(format!(
            "filebin upload failed: {status} {}",
            text.trim()
        )));
    }
    let _ = client.put(format!("{FILEBIN}/{bin}")).send().await;
    Ok(url)
}

pub fn random_code() -> String {
    const ALPHABET: &[u8] = b"abcdefghijkmnopqrstuvwxyz23456789";
    let mut buf = [0u8; 8];
    if getrandom::fill(&mut buf).is_err() {
        let nanos = std::time::UNIX_EPOCH
            .elapsed()
            .map(|d| d.subsec_nanos())
            .unwrap_or(0);
        buf.copy_from_slice(&nanos.to_le_bytes().repeat(2)[..8]);
    }
    let code: String = buf
        .iter()
        .map(|b| ALPHABET[*b as usize % ALPHABET.len()] as char)
        .collect();
    format!("mcmove-{code}")
}

pub fn valid_bin(s: &str) -> bool {
    !s.is_empty()
        && s.len() <= 80
        && s.chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
}

/// Reject filenames that could escape mods/ when joined.
fn safe_name(name: &str) -> Result<&str> {
    if name.is_empty() || name.contains('/') || name.contains('\\') || name.contains("..") {
        return Err(Error::Other(format!("unsafe filename in patch: {name:?}")));
    }
    Ok(name)
}

fn move_replace(src: &Path, dest: &Path) -> Result<()> {
    let _ = fs::remove_file(dest);
    if fs::rename(src, dest).is_err() {
        // staging may be on another volume (common for temp dirs on Windows)
        fs::copy(src, dest)?;
        let _ = fs::remove_file(src);
    }
    Ok(())
}

pub fn sha1_of(path: &Path) -> Result<String> {
    let mut f = fs::File::open(path)?;
    let mut hasher = Sha1::new();
    std::io::copy(&mut f, &mut hasher)?;
    Ok(hex::encode(hasher.finalize()))
}

fn file_name(p: &Path) -> String {
    p.file_name()
        .unwrap_or_default()
        .to_string_lossy()
        .into_owned()
}
