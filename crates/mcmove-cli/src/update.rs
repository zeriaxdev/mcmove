//! `mcmove update` — check Modrinth for newer mod versions and update the local
//! instance, with a per-mod version selector.

use std::fs;
use std::path::{Path, PathBuf};

use anyhow::bail;
use mcmove_core::config;
use mcmove_core::modrinth::{self, Version};
use mcmove_core::pack::{download_file, sha1_of};

use crate::util::{ask, clean_path, confirm};

struct Candidate {
    path: PathBuf,
    current: Version,
    newer: Vec<Version>,
    pick: Option<Version>,
}

pub async fn run(
    src: Option<String>,
    channel: String,
    all: bool,
    dry_run: bool,
) -> anyhow::Result<()> {
    let src = match src {
        Some(s) => clean_path(&s),
        None => {
            let cfg = config::load()?;
            clean_path(&ask(
                "Path to your local Modrinth/Minecraft instance",
                &config::remembered_src(&cfg),
            ))
        }
    };
    let src = Path::new(&src);
    if !src.is_dir() {
        bail!("not a folder: {}", src.display());
    }
    let mods_dir = src.join("mods");
    let mut jars: Vec<PathBuf> = fs::read_dir(&mods_dir)?
        .filter_map(|e| e.ok().map(|e| e.path()))
        .filter(|p| p.is_file() && p.extension().is_some_and(|e| e.eq_ignore_ascii_case("jar")))
        .collect();
    jars.sort();
    if jars.is_empty() {
        bail!("no .jar files in mods/");
    }

    println!(
        "Resolving {} mods on Modrinth (channel: {channel})...",
        jars.len()
    );
    let client = modrinth::client()?;
    let reporter = crate::report::CliReporter::new();
    let entries: Vec<(PathBuf, String)> = jars
        .into_iter()
        .map(|p| sha1_of(&p).map(|s| (p, s)))
        .collect::<Result<_, _>>()?;
    let hashes: Vec<String> = entries.iter().map(|(_, s)| s.clone()).collect();
    let current = modrinth::version_files(&client, &hashes, &reporter).await;

    let mut plan: Vec<Candidate> = Vec::new();
    let mut unknown = 0usize;
    let mut uptodate = 0usize;
    for (path, sha) in entries {
        let Some(v) = current.get(&sha) else {
            unknown += 1;
            continue;
        };
        let versions =
            modrinth::project_versions(&client, &v.project_id, &v.game_versions, &v.loaders).await;
        let newer: Vec<Version> = versions
            .into_iter()
            .filter(|x| {
                modrinth::channel_allows(&channel, &x.version_type)
                    && x.date_published > v.date_published
            })
            .collect();
        if newer.is_empty() {
            uptodate += 1;
        } else {
            plan.push(Candidate {
                path,
                current: v.clone(),
                newer,
                pick: None,
            });
        }
    }

    println!(
        "  {uptodate} up to date · {} with updates · {unknown} not on Modrinth",
        plan.len()
    );
    if plan.is_empty() {
        println!("\nEverything's current. Nothing to do.");
        return Ok(());
    }

    let mut chosen: Vec<Candidate> = Vec::new();
    if all {
        for mut m in plan {
            m.pick = Some(m.newer[0].clone());
            chosen.push(m);
        }
    } else {
        println!("\nFor each mod:  Enter = latest · number = pick a version · s = skip\n");
        for mut m in plan {
            let opts = &m.newer[..m.newer.len().min(12)];
            println!(
                "{}   (current {} [{}])",
                m.path.file_name().unwrap_or_default().to_string_lossy(),
                m.current.version_number,
                m.current.version_type
            );
            for (i, x) in opts.iter().enumerate() {
                let mark = if i == 0 { "  (latest)" } else { "" };
                println!(
                    "   {}) {:24} [{}] {}{mark}",
                    i + 1,
                    x.version_number,
                    x.version_type,
                    x.date_published.get(..10).unwrap_or(""),
                );
            }
            let raw = ask("   choose [1] / s", "");
            if raw.eq_ignore_ascii_case("s") {
                continue;
            }
            let idx: usize = raw.parse().unwrap_or(1);
            let idx = if (1..=opts.len()).contains(&idx) {
                idx
            } else {
                1
            };
            m.pick = Some(opts[idx - 1].clone());
            chosen.push(m);
        }
    }
    if chosen.is_empty() {
        println!("nothing selected");
        return Ok(());
    }

    println!("\nPlan:");
    for m in &chosen {
        let pick = m.pick.as_ref().unwrap();
        println!(
            "  ~ {}  {}  →  {} [{}]",
            m.path.file_name().unwrap_or_default().to_string_lossy(),
            m.current.version_number,
            pick.version_number,
            pick.version_type
        );
    }
    if dry_run {
        println!("\n(dry run — no changes made)");
        return Ok(());
    }
    if !confirm("\nDownload and apply these to the local instance?", true) {
        println!("aborted");
        return Ok(());
    }

    let tmp = tempfile::tempdir()?;
    for m in &chosen {
        let pick = m.pick.as_ref().unwrap();
        let Some(f) = pick.primary_file() else {
            println!("  ! {}: no downloadable file, skipping", m.path.display());
            continue;
        };
        if f.filename.contains('/') || f.filename.contains('\\') || f.filename.contains("..") {
            println!(
                "  ! {}: unsafe filename from Modrinth, skipping",
                f.filename
            );
            continue;
        }
        let staged = tmp.path().join(&f.filename);
        download_file(&client, &f.url, &staged).await?;
        let dest = mods_dir.join(&f.filename);
        let _ = fs::remove_file(&dest);
        if fs::rename(&staged, &dest).is_err() {
            fs::copy(&staged, &dest)?;
        }
        let old_name = m.path.file_name().unwrap_or_default().to_string_lossy();
        if old_name != f.filename.as_str() {
            let _ = fs::remove_file(&m.path);
        }
        println!("  ↑ {old_name} → {}", f.filename);
    }
    println!("\n✓ Local instance updated.  Run `mcmove sync` to push the changes to the server.");
    Ok(())
}
