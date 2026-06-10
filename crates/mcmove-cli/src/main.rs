//! mcmove CLI — a thin front-end over `mcmove-core`.
//!
//! All real work lives in the core crate. This binary only parses args, renders progress,
//! and prompts on the terminal. Remaining Python-only commands are stubbed during the
//! migration (see MIGRATION.md) and filled in stage by stage.

mod pack;
mod report;
mod servers;
mod update;
mod util;
mod whois;

use std::path::PathBuf;

use clap::{Parser, Subcommand};

#[derive(Parser)]
#[command(name = "mcmove", version = mcmove_core::VERSION, about = "Move Minecraft mods, worlds, configs, and saves between local instances and servers.")]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// List configured servers.
    List,
    /// Add a server profile (from the panel's SFTP Details).
    AddServer {
        /// Paste the panel's sftp://user@host:port string.
        #[arg(long)]
        url: Option<String>,
    },
    /// Remove a server profile.
    RemoveServer { name: String },
    /// Push: patch a server's /mods to match the local instance.
    Sync,
    /// Reverse: server mods → local instance.
    Pull,
    /// Check Modrinth for newer mod versions and update the local instance.
    Update {
        /// Path to local instance (otherwise remembered/asked).
        #[arg(long)]
        src: Option<String>,
        /// Newest release channel to allow.
        #[arg(long, default_value = "release", value_parser = ["release", "beta", "alpha"])]
        channel: String,
        /// Take the latest in-channel for every mod (no per-mod prompts).
        #[arg(long)]
        all: bool,
        /// Show the plan, change nothing.
        #[arg(long)]
        dry_run: bool,
    },
    /// Build server playerdata/<uuid>.dat from single-player level.dat.
    Playerdata,
    /// Resolve UUIDs to usernames (args or a folder of <uuid>.dat files).
    Whois {
        /// One or more UUIDs to look up.
        uuid: Vec<String>,
        /// A local folder of <uuid>.dat files.
        #[arg(long)]
        dir: Option<String>,
    },
    /// Modpack patcher: create/share/apply a PC→PC mod-folder patch.
    Pack {
        #[command(subcommand)]
        action: PackAction,
    },
}

#[derive(Subcommand)]
enum PackAction {
    /// Snapshot the local instance's mods into a .mcmpatch.
    Create {
        /// Path to the local Minecraft instance (the folder containing mods/).
        instance: PathBuf,
        /// Output patch path (default: <instance-name>.mcmpatch).
        #[arg(short, long)]
        out: Option<PathBuf>,
    },
    /// Create a patch and upload it to Filebin for a short share code.
    Share {
        /// Path to the local Minecraft instance.
        instance: PathBuf,
        /// Filebin bin code to upload into (default: random mcmove-XXXXXXXX).
        #[arg(long)]
        bin: Option<String>,
        /// Filename inside the bin.
        #[arg(long, default_value = "pack.mcmpatch")]
        filename: String,
    },
    /// Apply a .mcmpatch (path, URL, or share code) to an instance.
    Apply {
        /// Patch source: a .mcmpatch path, an https URL, or a share code.
        patch: String,
        /// Path to the instance to patch.
        instance: PathBuf,
        /// Show the plan without changing anything.
        #[arg(long)]
        dry_run: bool,
        /// Keep local mods that are not in the patch (default mirrors removals).
        #[arg(long)]
        keep_extra: bool,
        /// Skip the confirmation prompt.
        #[arg(short, long)]
        yes: bool,
    },
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let cli = Cli::parse();

    match cli.command {
        Command::List => servers::list(),
        Command::AddServer { url } => servers::add(url),
        Command::RemoveServer { name } => servers::remove(&name),
        Command::Update {
            src,
            channel,
            all,
            dry_run,
        } => update::run(src, channel, all, dry_run).await,
        Command::Whois { uuid, dir } => whois::run(uuid, dir).await,
        Command::Pack { action } => match action {
            PackAction::Create { instance, out } => pack::create(&instance, out).await,
            PackAction::Share {
                instance,
                bin,
                filename,
            } => pack::share(&instance, bin, filename).await,
            PackAction::Apply {
                patch,
                instance,
                dry_run,
                keep_extra,
                yes,
            } => pack::apply(&patch, &instance, dry_run, keep_extra, yes).await,
        },
        Command::Sync | Command::Pull | Command::Playerdata => {
            anyhow::bail!(
                "not yet ported to Rust — see MIGRATION.md (still available via mcmove.py)"
            );
        }
    }
}
