//! mcmove-core — UI-agnostic core for mcmove.
//!
//! This crate contains all the real logic (SFTP, NBT, Modrinth, the modpack patcher,
//! sync/pull/update). It is designed to be embedded: the `mcmove-cli` binary and a
//! separate GPUI Minecraft launcher both depend on it.
//!
//! Two rules keep it embeddable — see [`progress`] and [`Credentials`]:
//! - **It never prints.** Progress is emitted through a [`Reporter`]; callers render it
//!   however they like (CLI → `indicatif`, GPUI → native UI).
//! - **It never prompts.** Secrets are fetched through a [`Credentials`] callback so a GUI
//!   can present a native prompt.

pub mod actions;
pub mod config;
pub mod modrinth;
pub mod mojang;
pub mod nbt;
pub mod pack;
pub mod progress;
pub mod sftp;
pub mod syncmods;

pub use progress::{Progress, Reporter};

/// Error type surfaced across the crate boundary. Concrete enough for a UI to branch on.
#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("io: {0}")]
    Io(#[from] std::io::Error),
    #[error("http: {0}")]
    Http(#[from] reqwest::Error),
    #[error("json: {0}")]
    Json(#[from] serde_json::Error),
    #[error("zip: {0}")]
    Zip(#[from] zip::result::ZipError),
    #[error("{0}")]
    Other(String),
}

pub type Result<T> = std::result::Result<T, Error>;

/// How the core asks the embedder for secrets, instead of prompting on a TTY.
///
/// The CLI implements this by reading the terminal; a GPUI launcher implements it by
/// showing a native dialog. The core never touches stdin.
pub trait Credentials: Send + Sync {
    /// Return the SFTP password for `username@host`, or `None` to abort.
    fn sftp_password(&self, host: &str, username: &str) -> Option<String>;
}

/// Single-source crate version, surfaced via `mcmove --version`.
pub const VERSION: &str = env!("CARGO_PKG_VERSION");
