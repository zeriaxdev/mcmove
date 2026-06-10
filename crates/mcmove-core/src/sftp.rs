//! SFTP transport for Pelican/Pterodactyl panels (russh + russh-sftp, pure Rust).
//!
//! The panel's SFTP is chrooted to the server root, so remote paths look like
//! `/mods` or `/<world>/playerdata`. It's a custom subsystem: no rsync, and
//! directory listings may contain non-jar junk — callers filter.
//!
//! Like the Python tool (paramiko with no host-key policy), the server's host key
//! is not verified — panel hosts are typically reached by IP/short-lived DNS and
//! users never have them in known_hosts.

use std::path::Path;
use std::sync::Arc;

use russh::client;
use russh_sftp::client::SftpSession;
use tokio::io::AsyncWriteExt;

use crate::config::Profile;
use crate::progress::{Progress, Reporter};
use crate::{Error, Result};

/// How to authenticate. The CLI resolves the password (or key path) *before*
/// connecting, so the core never prompts.
pub enum Auth {
    Password(String),
    KeyFile(String),
}

struct Handler;

#[async_trait::async_trait]
impl client::Handler for Handler {
    type Error = russh::Error;

    async fn check_server_key(
        &mut self,
        _key: &russh::keys::key::PublicKey,
    ) -> std::result::Result<bool, Self::Error> {
        Ok(true)
    }
}

pub struct Sftp {
    handle: client::Handle<Handler>,
    pub session: SftpSession,
}

impl Sftp {
    pub async fn connect(profile: &Profile, auth: Auth) -> Result<Self> {
        let config = Arc::new(client::Config::default());
        let addr = (profile.host.as_str(), profile.port);
        let mut handle = client::connect(config, addr, Handler)
            .await
            .map_err(|e| Error::Other(format!("could not connect: {e}")))?;
        let ok = match auth {
            Auth::Password(pw) => handle
                .authenticate_password(&profile.username, pw)
                .await
                .map_err(|e| Error::Other(format!("could not connect: {e}")))?,
            Auth::KeyFile(path) => {
                let key = russh::keys::load_secret_key(&path, None)
                    .map_err(|e| Error::Other(format!("could not load private key {path}: {e}")))?;
                handle
                    .authenticate_publickey(&profile.username, Arc::new(key))
                    .await
                    .map_err(|e| Error::Other(format!("could not connect: {e}")))?
            }
        };
        if !ok {
            return Err(Error::Other(
                "authentication failed — check username/password (it's your PANEL password)."
                    .into(),
            ));
        }
        let channel = handle
            .channel_open_session()
            .await
            .map_err(|e| Error::Other(format!("could not open channel: {e}")))?;
        channel
            .request_subsystem(true, "sftp")
            .await
            .map_err(|e| Error::Other(format!("could not start sftp: {e}")))?;
        let session = SftpSession::new(channel.into_stream())
            .await
            .map_err(|e| Error::Other(format!("could not start sftp: {e}")))?;
        Ok(Self { handle, session })
    }

    pub async fn close(self) {
        let _ = self.session.close().await;
        let _ = self
            .handle
            .disconnect(russh::Disconnect::ByApplication, "", "en")
            .await;
    }

    pub async fn exists(&self, path: &str) -> bool {
        self.session.try_exists(path).await.unwrap_or(false)
    }

    /// mkdir -p over SFTP.
    pub async fn mkdirs(&self, remote: &str) -> Result<()> {
        let mut cur = String::new();
        for part in remote
            .trim_matches('/')
            .split('/')
            .filter(|p| !p.is_empty())
        {
            cur.push('/');
            cur.push_str(part);
            if !self.exists(&cur).await {
                let _ = self.session.create_dir(&cur).await;
            }
        }
        Ok(())
    }

    /// (name, is_dir) for each entry of a remote directory.
    pub async fn listdir(&self, remote: &str) -> Result<Vec<(String, bool)>> {
        let rd = self
            .session
            .read_dir(remote)
            .await
            .map_err(|e| Error::Other(format!("listing {remote}: {e}")))?;
        Ok(rd
            .map(|e| (e.file_name(), e.metadata().is_dir()))
            .filter(|(n, _)| n != "." && n != "..")
            .collect())
    }

    /// Recursively delete the *contents* of `remote` (like the Python tool: the
    /// top-level directory itself is kept).
    pub async fn rm_rf(&self, remote: &str) -> Result<()> {
        if !self.exists(remote).await {
            return Ok(());
        }
        Box::pin(self.rm_rf_inner(remote)).await
    }

    async fn rm_rf_inner(&self, remote: &str) -> Result<()> {
        for (name, is_dir) in self.listdir(remote).await? {
            let rp = join(remote, &name);
            if is_dir {
                Box::pin(self.rm_rf_inner(&rp)).await?;
                let _ = self.session.remove_dir(&rp).await;
            } else {
                let _ = self.session.remove_file(&rp).await;
            }
        }
        Ok(())
    }

    pub async fn put(&self, local: &Path, remote: &str) -> Result<()> {
        let mut src = tokio::fs::File::open(local).await?;
        let mut dst = self
            .session
            .create(remote)
            .await
            .map_err(|e| Error::Other(format!("creating {remote}: {e}")))?;
        tokio::io::copy(&mut src, &mut dst).await?;
        dst.shutdown().await?;
        Ok(())
    }

    pub async fn get(&self, remote: &str, local: &Path) -> Result<()> {
        let mut src = self
            .session
            .open(remote)
            .await
            .map_err(|e| Error::Other(format!("opening {remote}: {e}")))?;
        let mut dst = tokio::fs::File::create(local).await?;
        tokio::io::copy(&mut src, &mut dst).await?;
        Ok(())
    }

    pub async fn read(&self, remote: &str) -> Result<Vec<u8>> {
        self.session
            .read(remote)
            .await
            .map_err(|e| Error::Other(format!("reading {remote}: {e}")))
    }

    pub async fn write(&self, remote: &str, data: &[u8]) -> Result<()> {
        use russh_sftp::protocol::OpenFlags;
        // SftpSession::write opens WRITE-only, which fails for files that don't
        // exist yet — open with CREATE|TRUNCATE ourselves.
        let mut f = self
            .session
            .open_with_flags(
                remote,
                OpenFlags::WRITE | OpenFlags::CREATE | OpenFlags::TRUNCATE,
            )
            .await
            .map_err(|e| Error::Other(format!("writing {remote}: {e}")))?;
        f.write_all(data)
            .await
            .map_err(|e| Error::Other(format!("writing {remote}: {e}")))?;
        f.shutdown()
            .await
            .map_err(|e| Error::Other(format!("writing {remote}: {e}")))?;
        Ok(())
    }

    pub async fn rename(&self, from: &str, to: &str) -> Result<()> {
        self.session
            .rename(from, to)
            .await
            .map_err(|e| Error::Other(format!("renaming {from}: {e}")))
    }

    pub async fn remove_file(&self, remote: &str) -> Result<()> {
        self.session
            .remove_file(remote)
            .await
            .map_err(|e| Error::Other(format!("removing {remote}: {e}")))
    }

    /// Upload every file under `local` into `remote`, creating directories as needed.
    pub async fn upload_dir(
        &self,
        local: &Path,
        remote: &str,
        reporter: &dyn Reporter,
    ) -> Result<()> {
        let files = walk_local(local)?;
        self.mkdirs(remote).await?;
        reporter.report(Progress::Phase {
            name: format!("↑ {remote}"),
        });
        reporter.report(Progress::Total {
            units: files.len() as u64,
        });
        for p in &files {
            let rel = p.strip_prefix(local).unwrap_or(p);
            let rel = rel.to_string_lossy().replace('\\', "/");
            let rp = join(remote, &rel);
            if let Some(dir) = rp.rsplit_once('/').map(|(d, _)| d) {
                if !dir.is_empty() {
                    self.mkdirs(dir).await?;
                }
            }
            self.put(p, &rp).await?;
            reporter.report(Progress::Advance {
                units: 1,
                label: rel,
            });
        }
        reporter.report(Progress::PhaseDone {
            name: format!("↑ {remote}: {} files", files.len()),
        });
        Ok(())
    }

    /// Upload a flat list of files into `remote` by basename.
    pub async fn upload_files(
        &self,
        files: &[std::path::PathBuf],
        remote: &str,
        reporter: &dyn Reporter,
    ) -> Result<()> {
        self.mkdirs(remote).await?;
        reporter.report(Progress::Phase {
            name: format!("↑ {remote}"),
        });
        reporter.report(Progress::Total {
            units: files.len() as u64,
        });
        for p in files {
            let name = p.file_name().unwrap_or_default().to_string_lossy();
            self.put(p, &join(remote, &name)).await?;
            reporter.report(Progress::Advance {
                units: 1,
                label: name.into_owned(),
            });
        }
        reporter.report(Progress::PhaseDone {
            name: format!("↑ {remote}: {} files", files.len()),
        });
        Ok(())
    }

    /// Download a remote tree into a local directory.
    pub async fn download_dir(
        &self,
        remote: &str,
        local: &Path,
        reporter: &dyn Reporter,
    ) -> Result<()> {
        Box::pin(self.download_dir_inner(remote, local, reporter)).await
    }

    async fn download_dir_inner(
        &self,
        remote: &str,
        local: &Path,
        reporter: &dyn Reporter,
    ) -> Result<()> {
        std::fs::create_dir_all(local)?;
        for (name, is_dir) in self.listdir(remote).await? {
            let rp = join(remote, &name);
            if is_dir {
                Box::pin(self.download_dir_inner(&rp, &local.join(&name), reporter)).await?;
            } else {
                self.get(&rp, &local.join(&name)).await?;
                reporter.report(Progress::Advance {
                    units: 1,
                    label: rp,
                });
            }
        }
        Ok(())
    }
}

/// POSIX path join for remote paths.
pub fn join(base: &str, name: &str) -> String {
    if base.ends_with('/') {
        format!("{base}{name}")
    } else {
        format!("{base}/{name}")
    }
}

fn walk_local(root: &Path) -> Result<Vec<std::path::PathBuf>> {
    let mut out = Vec::new();
    let mut stack = vec![root.to_path_buf()];
    while let Some(dir) = stack.pop() {
        for entry in std::fs::read_dir(&dir)? {
            let p = entry?.path();
            if p.is_dir() {
                stack.push(p);
            } else if p.is_file() {
                out.push(p);
            }
        }
    }
    out.sort();
    Ok(out)
}
