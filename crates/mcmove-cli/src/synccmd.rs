//! `mcmove sync` (local → server) and `mcmove pull` (server → local).

use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};

use anyhow::bail;
use mcmove_core::syncmods::{self, Side};
use mcmove_core::{config, modrinth, pack};

use crate::report::CliReporter;
use crate::util::{ask, clean_path, confirm};

/// Resolve the server profile + local instance path shared by sync/pull, and
/// remember the instance path on success.
fn resolve_src(profile_src: &str, src: Option<String>) -> anyhow::Result<String> {
    let src = match src {
        Some(s) => clean_path(&s),
        None => clean_path(&ask(
            "Path to your local Modrinth/Minecraft instance",
            profile_src,
        )),
    };
    if !Path::new(&src).is_dir() {
        bail!("not a folder: {src}");
    }
    Ok(src)
}

fn remember_src(server: &str, src: &str) -> anyhow::Result<()> {
    let mut cfg = config::load()?;
    if let Some(p) = cfg.servers.get_mut(server) {
        p.last_src = src.to_string();
        config::save(&cfg)?;
    }
    Ok(())
}

pub async fn sync(
    server: Option<String>,
    src: Option<String>,
    dry_run: bool,
) -> anyhow::Result<()> {
    let cfg = config::load()?;
    let (name, profile) = crate::servers::select(&cfg, server.as_deref())?;
    let src = resolve_src(&profile.last_src, src)?;

    let sftp = crate::connect::connect(profile).await?;
    let result = sync_over(&sftp, &name, &src, dry_run).await;
    sftp.close().await;
    if result? && !dry_run {
        remember_src(&name, &src)?;
    }
    Ok(())
}

/// The sync flow over an existing connection — scan, classify, plan, confirm,
/// apply. Returns whether changes were applied. Reused by the move wizard.
pub async fn sync_over(
    sftp: &mcmove_core::sftp::Sftp,
    name: &str,
    src: &str,
    dry_run: bool,
) -> anyhow::Result<bool> {
    let jars = syncmods::local_jars(Path::new(&src))?;
    if jars.is_empty() {
        bail!("no .jar files in {src}/mods");
    }
    println!(
        "Scanning {} local mods (resolving client/server via Modrinth)...",
        jars.len()
    );
    let client = modrinth::client()?;
    let reporter = CliReporter::new();
    let infos = syncmods::classify_mods(&jars, &client, &reporter).await?;
    let n_client = infos.iter().filter(|i| i.side == Side::Client).count();
    let n_unknown = infos.iter().filter(|i| i.side == Side::Unknown).count();
    print!(
        "  {} server-side · {n_client} client-only (skipped)",
        infos.len() - n_client
    );
    if n_unknown > 0 {
        print!(" · {n_unknown} undetermined (kept)");
    }
    println!();

    let manifest = syncmods::load_state(name)?;
    {
        let remote_files = syncmods::remote_mod_files(sftp).await?;
        let (plan, new_managed) = syncmods::plan_sync(&infos, &manifest, &remote_files);

        println!("\nPlan:");
        println!(
            "  add {} · update {} · remove {} · unchanged {} · client skipped {}",
            plan.add.len(),
            plan.update.len(),
            plan.remove.len(),
            plan.keep,
            plan.client.len()
        );
        for i in &plan.add {
            println!("  + add     {}", i.filename);
        }
        for i in &plan.update {
            println!("  ~ update  {}", i.filename);
        }
        for f in &plan.remove {
            println!("  - remove  {f}");
        }
        if !plan.unknown.is_empty() {
            let shown: Vec<&str> = plan.unknown.iter().take(8).map(|s| s.as_str()).collect();
            let more = if plan.unknown.len() > 8 { " ..." } else { "" };
            println!(
                "  ? kept (couldn't determine side): {}{more}",
                shown.join(", ")
            );
        }

        if plan.is_noop() {
            println!("\nServer mods already up to date. Nothing to do.");
            return Ok(false);
        }
        if dry_run {
            println!("\n(dry run — no changes made)");
            return Ok(false);
        }

        // Safety guard: a sync that removes a large share of managed mods almost
        // always means the wrong/incomplete local instance was selected.
        let n_remove = plan.remove.len();
        let managed_total = manifest.mods.len().max(1);
        if n_remove >= 15 && n_remove * 2 > managed_total {
            println!(
                "\n⚠  This would REMOVE {n_remove} mods from the server — more than half of what mcmove manages here."
            );
            println!("   That usually means this isn't the right/complete instance.");
            if !confirm("   Type y only if you're SURE. Proceed?", false) {
                println!("aborted");
                return Ok(false);
            }
        }
        if !confirm("\nApply this patch?", true) {
            println!("aborted");
            return Ok(false);
        }

        syncmods::execute_sync(sftp, &plan, &reporter).await?;
        let mut manifest = manifest;
        manifest.mods = new_managed;
        syncmods::save_state(name, &manifest)?;
        println!("\n✓ Mods patched. Restart the server to load changes.");
        Ok(true)
    }
}

pub async fn pull(
    server: Option<String>,
    src: Option<String>,
    dry_run: bool,
    mirror: bool,
) -> anyhow::Result<()> {
    let cfg = config::load()?;
    let (name, profile) = crate::servers::select(&cfg, server.as_deref())?;
    let src = resolve_src(&profile.last_src, src)?;
    let mods_dir = Path::new(&src).join("mods");
    if !mods_dir.is_dir() {
        bail!("no mods/ folder in {src}");
    }

    let local_paths = syncmods::local_jars(Path::new(&src))?;
    let local_files: Vec<String> = local_paths
        .iter()
        .map(|p| {
            p.file_name()
                .unwrap_or_default()
                .to_string_lossy()
                .into_owned()
        })
        .collect();
    let mut local_by_key: HashMap<String, PathBuf> = HashMap::new();
    for p in &local_paths {
        let (modid, _) = pack::jar_id_and_env(p);
        let key = match modid {
            Some(id) => format!("mod:{id}"),
            None => format!(
                "file:{}",
                p.file_name().unwrap_or_default().to_string_lossy()
            ),
        };
        local_by_key.insert(key, p.clone());
    }

    let client = modrinth::client()?;
    let reporter = CliReporter::new();
    let sftp = crate::connect::connect(profile).await?;
    let tmp = tempfile::tempdir()?;
    let result = async {
        let mut server_files: Vec<String> = syncmods::remote_mod_files(&sftp)
            .await?
            .into_iter()
            .collect();
        server_files.sort();
        if server_files.is_empty() {
            bail!("server has no .jar mods in /mods");
        }

        // add = new to us; update = same mod id under a different filename
        let mut add: Vec<(String, PathBuf)> = Vec::new();
        let mut update: Vec<(String, PathBuf, PathBuf)> = Vec::new(); // (server name, old local, staged)
        let mut skip = 0usize;
        for sf in &server_files {
            if local_files.contains(sf) {
                skip += 1; // same filename = same version
                continue;
            }
            let staged = tmp.path().join(sf);
            if let Err(e) = sftp
                .get(&mcmove_core::sftp::join("/mods", sf), &staged)
                .await
            {
                println!("  ! couldn't download {sf}: {e} — skipping");
                continue;
            }
            let (modid, _) = pack::jar_id_and_env(&staged);
            let key = match modid {
                Some(id) => format!("mod:{id}"),
                None => format!("file:{sf}"),
            };
            match local_by_key.get(&key) {
                Some(old)
                    if old.file_name().unwrap_or_default().to_string_lossy() != sf.as_str() =>
                {
                    update.push((sf.clone(), old.clone(), staged));
                }
                _ => add.push((sf.clone(), staged)),
            }
        }

        // --mirror: also remove local SERVER-SIDE mods gone from the server;
        // client-only mods are always protected.
        let mut remove: Vec<PathBuf> = Vec::new();
        if mirror {
            println!("Resolving which local mods are server-side (client-only are protected)...");
            for i in syncmods::classify_mods(&local_paths, &client, &reporter).await? {
                if i.side == Side::Keep && !server_files.contains(&i.filename) {
                    remove.push(i.path);
                }
            }
        }

        println!("\nPlan (server → local instance):");
        let mut line = format!(
            "  add {} · update {} · unchanged {skip}",
            add.len(),
            update.len()
        );
        if mirror {
            line.push_str(&format!(" · remove {}", remove.len()));
        }
        println!("{line}");
        for (sf, _) in &add {
            println!("  + add     {sf}");
        }
        for (sf, old, _) in &update {
            println!(
                "  ~ update  {}  →  {sf}",
                old.file_name().unwrap_or_default().to_string_lossy()
            );
        }
        for p in &remove {
            println!(
                "  - remove  {}",
                p.file_name().unwrap_or_default().to_string_lossy()
            );
        }

        if add.is_empty() && update.is_empty() && remove.is_empty() {
            println!("\nYour instance already matches the server. Nothing to do.");
            return Ok(false);
        }
        if dry_run {
            println!("\n(dry run — no changes made)");
            return Ok(false);
        }
        if !confirm("\nApply to your LOCAL instance?", true) {
            println!("aborted");
            return Ok(false);
        }

        for (sf, staged) in &add {
            move_into(staged, &mods_dir.join(sf))?;
            println!("  + {sf}");
        }
        for (sf, old, staged) in &update {
            let _ = fs::remove_file(old);
            move_into(staged, &mods_dir.join(sf))?;
            println!(
                "  ~ {} → {sf}",
                old.file_name().unwrap_or_default().to_string_lossy()
            );
        }
        for p in &remove {
            let _ = fs::remove_file(p);
            println!(
                "  - {}",
                p.file_name().unwrap_or_default().to_string_lossy()
            );
        }
        println!("\n✓ Local instance patched from the server. Restart your game.");
        Ok::<bool, anyhow::Error>(true)
    }
    .await;
    sftp.close().await;
    if result? && !dry_run {
        remember_src(&name, &src)?;
    }
    Ok(())
}

fn move_into(src: &Path, dest: &Path) -> std::io::Result<()> {
    let _ = fs::remove_file(dest);
    if fs::rename(src, dest).is_err() {
        fs::copy(src, dest)?;
        let _ = fs::remove_file(src);
    }
    Ok(())
}
