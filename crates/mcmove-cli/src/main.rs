//! mcmove CLI — a thin front-end over `mcmove-core`.
//!
//! All real work lives in the core crate. This binary only parses args, renders progress,
//! and prompts for secrets on the terminal. Commands are stubbed during the Rust migration
//! (see MIGRATION.md) and filled in stage by stage.

mod pack;
mod report;

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
    /// Push: patch a server's /mods to match the local instance.
    Sync,
    /// Reverse: server mods → local instance.
    Pull,
    /// Check Modrinth for newer mod versions and update the local instance.
    Update,
    /// Build server playerdata/<uuid>.dat from single-player level.dat.
    Playerdata,
    /// UUID → username lookup.
    Whois,
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
        Command::Pack { action } => match action {
            PackAction::Create { instance, out } => pack::create(&instance, out).await,
            PackAction::Share { instance, bin, filename } => pack::share(&instance, bin, filename).await,
            PackAction::Apply { patch, instance, dry_run, keep_extra, yes } => {
                pack::apply(&patch, &instance, dry_run, keep_extra, yes).await
            }
        },
        Command::Sync | Command::Pull | Command::Update | Command::Playerdata | Command::Whois => {
            anyhow::bail!("not yet ported to Rust — see MIGRATION.md (still available via mcmove.py)");
        }
    }
}
