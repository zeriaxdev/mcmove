//! `mcmove list | add-server | remove-server` — server profile management.

use anyhow::bail;
use mcmove_core::config::{self, Config, Profile};

use crate::util::{ask, clean_path};

pub fn list() -> anyhow::Result<()> {
    let cfg = config::load()?;
    if cfg.servers.is_empty() {
        println!("No servers configured. Add one with:  mcmove add-server");
        return Ok(());
    }
    println!("Configured servers:");
    for (name, s) in &cfg.servers {
        let auth = if s.key_path.is_empty() {
            "password"
        } else {
            "key"
        };
        let src = if s.last_src.is_empty() {
            String::new()
        } else {
            format!("  src={}", s.last_src)
        };
        println!(
            "  {name:16} {}@{}:{}  ({auth}){src}",
            s.username, s.host, s.port
        );
    }
    Ok(())
}

pub fn add(url: Option<String>) -> anyhow::Result<()> {
    let mut cfg = config::load()?;
    println!("Add a server. Grab these from the panel: your server → Settings → SFTP Details.\n");

    let url = url.unwrap_or_else(|| {
        ask(
            "Paste SFTP URL (sftp://user@host:port), or leave blank to type fields manually",
            "",
        )
    });
    let (mut host, mut port, mut username) = (String::new(), 0u16, String::new());
    if !url.is_empty() {
        (host, port, username) = config::parse_sftp_url(&url)?;
        println!("  parsed → host={host} port={port} username={username}");
    }

    let default_name = username.rsplit('.').next().unwrap_or_default().to_string();
    let name = ask("Profile name (e.g. survival)", &default_name);
    if name.is_empty() {
        bail!("name required");
    }
    if host.is_empty() {
        host = ask("SFTP host", "");
    }
    if port == 0 {
        port = ask("SFTP port", "2022").parse().unwrap_or(2022);
    }
    if username.is_empty() {
        username = ask("SFTP username (looks like admin.ab12cd34)", "");
    }
    let key_path = ask(
        "Path to SSH private key (blank = use password each run)",
        "",
    );
    let has_key = !key_path.is_empty();
    cfg.servers.insert(
        name.clone(),
        Profile {
            host,
            port,
            username,
            key_path: if has_key {
                clean_path(&key_path)
            } else {
                String::new()
            },
            ..Profile::default()
        },
    );
    config::save(&cfg)?;
    if has_key {
        println!("\nSaved '{name}'.");
    } else {
        println!("\nSaved '{name}'. Password is never stored — you'll be prompted at connect time");
    }
    Ok(())
}

pub fn remove(name: &str) -> anyhow::Result<()> {
    let mut cfg = config::load()?;
    if cfg.servers.remove(name).is_none() {
        bail!("no such server: {name}");
    }
    config::save(&cfg)?;
    println!("removed {name}");
    Ok(())
}

/// Pick a profile by name, or interactively when `name` is None.
#[allow(dead_code)] // used once sync/pull/playerdata are ported (SFTP batch)
pub fn select<'a>(cfg: &'a Config, name: Option<&str>) -> anyhow::Result<(String, &'a Profile)> {
    if cfg.servers.is_empty() {
        bail!("no servers configured; add one with:  mcmove add-server");
    }
    if let Some(n) = name {
        return match cfg.servers.get(n) {
            Some(p) => Ok((n.to_string(), p)),
            None => bail!("no such server: {n}"),
        };
    }
    let names: Vec<&String> = cfg.servers.keys().collect();
    if names.len() == 1 {
        return Ok((names[0].clone(), &cfg.servers[names[0]]));
    }
    println!("Target server:");
    for (i, n) in names.iter().enumerate() {
        let s = &cfg.servers[*n];
        println!("  {}) {n}  ({}@{})", i + 1, s.username, s.host);
    }
    let raw = ask("choose", "1");
    let idx: usize = raw.parse().unwrap_or(0);
    if idx < 1 || idx > names.len() {
        bail!("no server selected");
    }
    Ok((names[idx - 1].clone(), &cfg.servers[names[idx - 1]]))
}
