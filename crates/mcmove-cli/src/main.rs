//! mcmove CLI — a thin front-end over `mcmove-core`.
//!
//! All real work lives in the core crate. This binary only parses args, renders progress,
//! and prompts for secrets on the terminal. Commands are stubbed during the Rust migration
//! (see MIGRATION.md) and filled in stage by stage.

mod report;

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
    Create,
    /// Create a patch and upload it for a short share code.
    Share,
    /// Apply a .mcmpatch (path, URL, or share code) to an instance.
    Apply,
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let cli = Cli::parse();
    let _reporter = report::CliReporter::new();

    match cli.command {
        Command::Sync
        | Command::Pull
        | Command::Update
        | Command::Playerdata
        | Command::Whois
        | Command::Pack { .. } => {
            anyhow::bail!("not yet ported to Rust — see MIGRATION.md (still available via mcmove.py)");
        }
    }
}
