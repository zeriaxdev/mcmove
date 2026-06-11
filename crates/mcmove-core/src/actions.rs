//! Composite server actions used by the move wizard: pre-overwrite backups and
//! server.properties level-name updates.

use std::fs;
use std::path::PathBuf;

use flate2::write::GzEncoder;
use flate2::Compression;

use crate::config::backup_dir;
use crate::progress::{Progress, Reporter};
use crate::sftp::Sftp;
use crate::Result;

/// Download each existing remote target into a staging dir, tar.gz it under
/// `~/.config/mcmove/backups/<server>-<ts>.tar.gz`, and return the archive path.
/// Returns None when none of the targets exist yet.
pub async fn backup_remote(
    sftp: &Sftp,
    server_name: &str,
    targets: &[String],
    reporter: &dyn Reporter,
) -> Result<Option<PathBuf>> {
    let mut present = Vec::new();
    for t in targets {
        if sftp.exists(t).await {
            present.push(t.clone());
        }
    }
    if present.is_empty() {
        reporter.report(Progress::Info {
            message: "(nothing to back up yet)".into(),
        });
        return Ok(None);
    }
    let ts = chrono::Local::now().format("%Y%m%d-%H%M%S").to_string();
    let staging = backup_dir().join(format!("{server_name}-{ts}"));
    for t in &present {
        reporter.report(Progress::Info {
            message: format!("⇣ backing up {t} ..."),
        });
        sftp.download_dir(t, &staging.join(t.trim_matches('/')), reporter)
            .await?;
    }
    let tar_path = backup_dir().join(format!("{server_name}-{ts}.tar.gz"));
    let tar_gz = fs::File::create(&tar_path)?;
    let enc = GzEncoder::new(tar_gz, Compression::default());
    let mut tar = tar::Builder::new(enc);
    tar.append_dir_all(format!("{server_name}-{ts}"), &staging)?;
    tar.into_inner()?.finish()?;
    fs::remove_dir_all(&staging)?;
    reporter.report(Progress::Info {
        message: format!("✓ backup saved: {}", tar_path.display()),
    });
    Ok(Some(tar_path))
}

/// Set `level-name=<name>` in the server's /server.properties (creating the file
/// or appending the key if missing).
pub async fn set_level_name(sftp: &Sftp, name: &str) -> Result<()> {
    let path = "/server.properties";
    let content = if sftp.exists(path).await {
        String::from_utf8_lossy(&sftp.read(path).await?).into_owned()
    } else {
        String::new()
    };
    let mut out: Vec<String> = Vec::new();
    let mut found = false;
    for ln in content.lines() {
        if ln.starts_with("level-name=") {
            out.push(format!("level-name={name}"));
            found = true;
        } else {
            out.push(ln.to_string());
        }
    }
    if !found {
        out.push(format!("level-name={name}"));
    }
    sftp.write(path, (out.join("\n") + "\n").as_bytes()).await
}
