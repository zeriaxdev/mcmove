//! mcmove CLI — a thin front-end over `mcmove-core`.
//!
//! All real work lives in the core crate. This binary only parses args, renders progress,
//! and prompts on the terminal.

mod color;
mod connect;
mod pack;
mod playerdata;
mod report;
mod servers;
mod synccmd;
mod update;
mod util;
mod whois;
mod wizard;

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
    /// Run the move wizard: push world / mods / config to a server.
    Move {
        /// Path to local instance (skips the prompt).
        #[arg(long)]
        src: Option<String>,
    },
    /// Push: patch a server's /mods to match the local instance.
    Sync {
        /// Saved server name (otherwise you'll be asked).
        #[arg(long)]
        server: Option<String>,
        /// Path to local instance (otherwise remembered/asked).
        #[arg(long)]
        src: Option<String>,
        /// Show the plan, change nothing.
        #[arg(long)]
        dry_run: bool,
    },
    /// Reverse: download the server's mods into your local instance.
    Pull {
        /// Saved server name (otherwise you'll be asked).
        #[arg(long)]
        server: Option<String>,
        /// Path to local instance (otherwise remembered/asked).
        #[arg(long)]
        src: Option<String>,
        /// Show the plan, change nothing.
        #[arg(long)]
        dry_run: bool,
        /// Also remove local SERVER-SIDE mods missing from the server
        /// (client-only mods are always kept).
        #[arg(long)]
        mirror: bool,
    },
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
    Playerdata {
        /// Path to a single-player level.dat (omit for interactive batch).
        #[arg(long)]
        level: Option<String>,
        /// Username or UUID this level.dat belongs to.
        #[arg(long)]
        player: Option<String>,
        /// Output dir (default ~/.config/mcmove/playerdata-out).
        #[arg(long)]
        out: Option<String>,
        /// Upload results to a server's <world>/playerdata.
        #[arg(long)]
        upload: bool,
        /// Server name for --upload.
        #[arg(long)]
        server: Option<String>,
        /// Server world folder (level-name) for --upload.
        #[arg(long)]
        world: Option<String>,
    },
    /// Resolve UUIDs to usernames (args, a folder, or a server's playerdata).
    Whois {
        /// One or more UUIDs to look up.
        uuid: Vec<String>,
        /// A local folder of <uuid>.dat files.
        #[arg(long)]
        dir: Option<String>,
        /// Read the server's <world>/playerdata listing.
        #[arg(long)]
        server: Option<String>,
        /// Server world folder (level-name).
        #[arg(long)]
        world: Option<String>,
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
        Command::Move { src } => wizard::run(src).await,
        Command::Update {
            src,
            channel,
            all,
            dry_run,
        } => update::run(src, channel, all, dry_run).await,
        Command::Whois {
            uuid,
            dir,
            server,
            world,
        } => whois::run(uuid, dir, server, world).await,
        Command::Sync {
            server,
            src,
            dry_run,
        } => synccmd::sync(server, src, dry_run).await,
        Command::Pull {
            server,
            src,
            dry_run,
            mirror,
        } => synccmd::pull(server, src, dry_run, mirror).await,
        Command::Playerdata {
            level,
            player,
            out,
            upload,
            server,
            world,
        } => {
            playerdata::run(playerdata::Args {
                level,
                player,
                out,
                upload,
                server,
                world,
            })
            .await
        }
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
    }
}
