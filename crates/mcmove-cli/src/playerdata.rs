//! `mcmove playerdata` — build server playerdata/<uuid>.dat from single-player
//! level.dat files, with optional SFTP upload (existing files get .bak'd).

use std::path::{Path, PathBuf};

use anyhow::bail;
use mcmove_core::{config, modrinth, mojang, nbt, sftp};

use crate::util::{ask, clean_path, confirm};

pub struct Args {
    pub level: Option<String>,
    pub player: Option<String>,
    pub out: Option<String>,
    pub upload: bool,
    pub server: Option<String>,
    pub world: Option<String>,
}

pub async fn run(args: Args) -> anyhow::Result<()> {
    // Collect (level.dat, who) entries: one-shot via flags, or interactive batch.
    let mut entries: Vec<(String, String)> = Vec::new();
    if let Some(level) = &args.level {
        let who = args
            .player
            .clone()
            .unwrap_or_else(|| ask("Username or UUID for this level.dat", ""));
        entries.push((clean_path(level), who));
    } else {
        println!("Add each player: their single-player level.dat + who it belongs to.");
        println!("(level.dat lives in an instance under saves/<world>/level.dat)\n");
        loop {
            let lp = ask("level.dat path (blank to finish)", "");
            if lp.is_empty() {
                break;
            }
            let who = ask("  Minecraft username (or paste a UUID)", "");
            if !who.is_empty() {
                entries.push((clean_path(&lp), who));
            }
        }
    }
    if entries.is_empty() {
        bail!("nothing to do");
    }

    let client = modrinth::client()?;
    let names: Vec<String> = entries
        .iter()
        .filter(|(_, w)| !mojang::looks_like_uuid(w))
        .map(|(_, w)| w.clone())
        .collect();
    let name_map = mojang::resolve_uuids(&client, &names).await;
    let out_dir = match &args.out {
        Some(o) => PathBuf::from(clean_path(o)),
        None => config::config_dir().join("playerdata-out"),
    };

    let mut results: Vec<(String, PathBuf)> = Vec::new();
    for (lp, who) in &entries {
        if !Path::new(lp).is_file() {
            bail!("not a file: {lp}");
        }
        let uuid = if mojang::looks_like_uuid(who) {
            mojang::hyphenate_uuid(who)
        } else {
            match name_map.get(&who.to_lowercase()) {
                Some(u) => u.clone(),
                None => bail!(
                    "couldn't resolve a UUID for '{who}' (Mojang lookup failed — paste the UUID directly, or check spelling)"
                ),
            }
        };
        let player = nbt::extract_player(Path::new(lp))?;
        let path = nbt::write_playerdata(&player, &uuid, &out_dir)?;
        println!(
            "  ✓ {who:20} → {}",
            path.file_name().unwrap_or_default().to_string_lossy()
        );
        results.push((uuid, path));
    }

    println!(
        "\nWrote {} playerdata file(s) to {}",
        results.len(),
        out_dir.display()
    );
    println!("  Online-mode (Mojang-auth) servers only — offline-mode UUIDs differ.");

    let do_upload = args.upload
        || confirm(
            "\nUpload these into a server's <world>/playerdata now?",
            false,
        );
    if !do_upload {
        println!("  (copy them into <world>/playerdata/ yourself when ready)");
        return Ok(());
    }

    let cfg = config::load()?;
    let (_name, profile) = crate::servers::select(&cfg, args.server.as_deref())?;
    let world = args
        .world
        .clone()
        .unwrap_or_else(|| ask("Server world folder name (the level-name)", "world"));
    let conn = crate::connect::connect(profile).await?;
    let result = async {
        let remote_dir = format!("/{world}/playerdata");
        conn.mkdirs(&remote_dir).await?;
        let ts = chrono::Local::now().format("%Y%m%d-%H%M%S").to_string();
        for (uuid, path) in &results {
            let rp = sftp::join(&remote_dir, &format!("{uuid}.dat"));
            if conn.exists(&rp).await {
                // keep a backup of any existing file
                let _ = conn.rename(&rp, &format!("{rp}.{ts}.bak")).await;
            }
            conn.put(path, &rp).await?;
            println!("  ↑ {uuid}.dat");
        }
        Ok::<(), anyhow::Error>(())
    }
    .await;
    conn.close().await;
    result?;
    println!(
        "\n✓ Uploaded. Restart the server — those players keep their inventory and attributes."
    );
    Ok(())
}
