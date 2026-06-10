//! `mcmove whois` — resolve UUIDs to usernames from args, a folder of `<uuid>.dat`
//! files, or a server's `<world>/playerdata` listing.

use std::fs;
use std::path::Path;

use anyhow::bail;
use mcmove_core::{config, modrinth, mojang};

use crate::util::{ask, clean_path};

pub async fn run(
    uuids: Vec<String>,
    dir: Option<String>,
    server: Option<String>,
    world: Option<String>,
) -> anyhow::Result<()> {
    let mut all = uuids;
    if let Some(dir) = dir {
        let dir = clean_path(&dir);
        for entry in fs::read_dir(Path::new(&dir))? {
            let p = entry?.path();
            if p.extension().is_some_and(|e| e == "dat") {
                if let Some(stem) = p.file_stem() {
                    all.push(stem.to_string_lossy().into_owned());
                }
            }
        }
    }
    if server.is_some() || world.is_some() {
        let cfg = config::load()?;
        let (_name, profile) = crate::servers::select(&cfg, server.as_deref())?;
        let world =
            world.unwrap_or_else(|| ask("Server world folder name (the level-name)", "world"));
        let conn = crate::connect::connect(profile).await?;
        let rd = format!("/{world}/playerdata");
        if conn.exists(&rd).await {
            for (name, is_dir) in conn.listdir(&rd).await? {
                if !is_dir {
                    if let Some(stem) = name.strip_suffix(".dat") {
                        all.push(stem.to_string());
                    }
                }
            }
        }
        conn.close().await;
    }

    let mut seen: Vec<String> = Vec::new();
    for u in all {
        if mojang::looks_like_uuid(&u) && !seen.contains(&u) {
            seen.push(u);
        }
    }
    if seen.is_empty() {
        bail!("no UUIDs to look up — pass UUIDs, --dir, or --server/--world");
    }

    let client = modrinth::client()?;
    println!("  {:<18} UUID", "USERNAME");
    for u in &seen {
        match mojang::uuid_to_name(&client, u).await {
            Some(name) => println!("  {name:<18} {u}"),
            None => println!("  {:<18} {u}", "(unknown)"),
        }
    }
    Ok(())
}
