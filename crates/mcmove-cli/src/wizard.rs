//! `mcmove move` — the interactive wizard: push world / mods / config to a server,
//! with an optional pre-overwrite backup.

use std::fs;
use std::path::{Path, PathBuf};

use anyhow::bail;
use mcmove_core::{actions, config};

use crate::report::CliReporter;
use crate::util::{ask, clean_path, confirm};

pub async fn run(src_arg: Option<String>) -> anyhow::Result<()> {
    let mut cfg = config::load()?;
    if cfg.servers.is_empty() {
        println!("No servers yet. Let's add one.\n");
        crate::servers::add(None)?;
        cfg = config::load()?;
    }
    let (name, profile) = crate::servers::select(&cfg, None)?;

    let src = match src_arg {
        Some(s) => clean_path(&s),
        None => clean_path(&ask(
            "Path to your local Modrinth/Minecraft instance",
            &profile.last_src,
        )),
    };
    if !Path::new(&src).is_dir() {
        bail!("not a folder: {src}");
    }

    let options = [
        "Mods (patch — add/update/remove, skips client-only)",
        "World (from saves/)",
        "Config (config/)",
    ];
    println!("\nWhat do you want to move?");
    for (i, o) in options.iter().enumerate() {
        println!("  {}) {o}", i + 1);
    }
    let raw = ask("choose (e.g. 1,3 or 'all')", "");
    let actions_sel: Vec<usize> = if raw.eq_ignore_ascii_case("all") {
        vec![1, 2, 3]
    } else {
        raw.split(|c: char| c == ',' || c.is_whitespace())
            .filter_map(|t| t.parse().ok())
            .filter(|n| (1..=3).contains(n))
            .collect()
    };
    let (do_mods, do_world, do_config) = (
        actions_sel.contains(&1),
        actions_sel.contains(&2),
        actions_sel.contains(&3),
    );
    if !(do_mods || do_world || do_config) {
        bail!("nothing selected");
    }

    // World selection from saves/.
    let mut world_src: Option<PathBuf> = None;
    let mut level_name = String::new();
    if do_world {
        let saves = Path::new(&src).join("saves");
        let mut worlds: Vec<PathBuf> = if saves.is_dir() {
            fs::read_dir(&saves)?
                .filter_map(|e| e.ok().map(|e| e.path()))
                .filter(|p| p.is_dir())
                .collect()
        } else {
            Vec::new()
        };
        worlds.sort();
        if worlds.is_empty() {
            bail!("no worlds found in {}", saves.display());
        }
        println!("\nWhich world?");
        for (i, w) in worlds.iter().enumerate() {
            println!(
                "  {}) {}",
                i + 1,
                w.file_name().unwrap_or_default().to_string_lossy()
            );
        }
        let idx: usize = ask("choose", "1").parse().unwrap_or(0);
        if idx < 1 || idx > worlds.len() {
            bail!("no world selected");
        }
        world_src = Some(worlds[idx - 1].clone());
        level_name = ask("Target level-name on the server", "world");
    }

    let clear_world = do_world
        && confirm(
            &format!("Clear existing remote /{level_name} first?"),
            false,
        );
    // World/config overwrite, so offer a backup. Mods are patched (non-destructive
    // to unmanaged files), so they're excluded from the backup.
    let do_backup = (do_world || do_config)
        && confirm(
            "Back up the server's current world/config before overwriting?",
            true,
        );

    println!("\nPlan:");
    println!(
        "  server : {name}  ({}@{}:{})",
        profile.username, profile.host, profile.port
    );
    println!("  source : {src}");
    if do_mods {
        println!("  mods   : patch /mods (add/update/remove · skip client-only)");
    }
    if let Some(w) = &world_src {
        println!(
            "  world  : {}  ->  /{level_name}{}",
            w.file_name().unwrap_or_default().to_string_lossy(),
            if clear_world {
                "  (clearing target)"
            } else {
                ""
            }
        );
    }
    if do_config {
        println!("  config : config/  ->  /config");
    }
    println!("  backup : {}", if do_backup { "yes" } else { "no" });
    if !confirm("\nProceed?", true) {
        println!("aborted");
        return Ok(());
    }

    let reporter = CliReporter::new();
    let sftp = crate::connect::connect(profile).await?;
    let result = async {
        if do_backup {
            let mut targets = Vec::new();
            if do_world {
                targets.push(format!("/{level_name}"));
            }
            if do_config {
                targets.push("/config".to_string());
            }
            println!("\nBackup:");
            actions::backup_remote(&sftp, &name, &targets, &reporter).await?;
        }

        if do_mods {
            println!("\nMods (patch):");
            crate::synccmd::sync_over(&sftp, &name, &src, false).await?;
        }
        if do_config {
            println!("\nConfig:");
            let cfg_dir = Path::new(&src).join("config");
            if cfg_dir.is_dir() {
                sftp.upload_dir(&cfg_dir, "/config", &reporter).await?;
            } else {
                println!("  ! no config/ folder in {src}, skipping");
            }
        }
        if let Some(world) = &world_src {
            println!("\nWorld:");
            if clear_world {
                println!("  ✗ clearing remote /{level_name} ...");
                sftp.rm_rf(&format!("/{level_name}")).await?;
            }
            println!(
                "  moving world '{}' -> /{level_name} ...",
                world.file_name().unwrap_or_default().to_string_lossy()
            );
            sftp.upload_dir(world, &format!("/{level_name}"), &reporter)
                .await?;
            actions::set_level_name(&sftp, &level_name).await?;
            println!("  ✓ set level-name={level_name} in server.properties");
        }
        Ok::<(), anyhow::Error>(())
    }
    .await;
    sftp.close().await;
    result?;

    // Remember the source path for next time.
    let mut cfg = config::load()?;
    if let Some(p) = cfg.servers.get_mut(&name) {
        p.last_src = src.clone();
        config::save(&cfg)?;
    }

    println!("\n✓ Done. Restart the server in the panel to load the changes.");
    if do_world {
        println!("  Note: if this was a single-player world on vanilla, dimensions are nested");
        println!("  inside the world folder — that's fine for modded/Forge/NeoForge servers.");
    }
    Ok(())
}
