//! `mcmove whois` — resolve UUIDs to usernames from args or a folder of `<uuid>.dat`
//! files. (Reading a server's playerdata listing arrives with the SFTP port.)

use std::fs;
use std::path::Path;

use anyhow::bail;
use mcmove_core::{modrinth, mojang};

use crate::util::clean_path;

pub async fn run(uuids: Vec<String>, dir: Option<String>) -> anyhow::Result<()> {
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

    let mut seen: Vec<String> = Vec::new();
    for u in all {
        if mojang::looks_like_uuid(&u) && !seen.contains(&u) {
            seen.push(u);
        }
    }
    if seen.is_empty() {
        bail!("no UUIDs to look up — pass UUIDs or --dir");
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
