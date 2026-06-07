# mcmove

[![standard-readme compliant](https://img.shields.io/badge/readme%20style-standard-brightgreen.svg)](https://github.com/RichardLitt/standard-readme)

> Move worlds, mods, and configs from a local Minecraft instance into a Pelican / Pterodactyl server over SFTP.

`mcmove` is a small command-line tool for the chore of getting a single-player or
Modrinth instance onto a hosted game server. You pick a server, point it at your
local instance, and tick what to move — world, mods, configs. It uploads over the
panel's SFTP, so files land owned by the server user automatically (no root, no
`chown`), and it keeps `level-name` in `server.properties` in sync so the world
actually loads. It can back up the server's current files before overwriting.

Mods are handled as a **patch**, not a wipe-and-replace: `mcmove` diffs your local
instance against the server and only **adds**, **updates**, or **removes** what
changed — identical mods are left alone, mods you added directly on the server are
never touched, and **client-only mods** (shaders, minimaps, texture mods…) are
detected and kept off the server so it doesn't crash. The instance path is
remembered per server.

## Table of Contents

- [Background](#background)
- [Install](#install)
- [Usage](#usage)
- [Security](#security)
- [Roadmap](#roadmap)
- [Maintainers](#maintainers)
- [Contributing](#contributing)
- [License](#license)

## Background

Moving a world or a pile of mods onto a Pelican/Pterodactyl server usually means
either clicking through a slow web file manager, or copying files on the host as
root and then fixing ownership by hand — the classic "my mods/world don't load and
there's no error" trap is almost always a permissions or `level-name` mismatch.

`mcmove` does it over SFTP from your machine instead. SFTP writes as the server
user, so ownership is correct by construction, and the tool normalizes the world
name so it boots. It targets the SFTP endpoint every Pelican/Pterodactyl server
exposes (default port `2022`).

## Install

Requires Python 3.9+ and [paramiko](https://www.paramiko.org/).

```sh
git clone https://github.com/zeriaxdev/mcmove.git
cd mcmove
python3 -m venv .venv
source .venv/bin/activate
pip install -r requirements.txt
```

## Usage

### 1. Save a server

Grab the connection string from the panel: your server → **Settings → SFTP Details**.
Paste it once:

```sh
python3 mcmove.py add-server --url "sftp://admin.100b3b70@node1.example.com:2022"
```

Or run `python3 mcmove.py add-server` and paste it when prompted. Your panel
password is never stored — it is asked at connect time (or configure an SSH key).

### 2. Move things

```sh
python3 mcmove.py
```

The wizard will:

1. Let you pick a saved server.
2. Ask for your local instance folder (the one containing `mods/`, `config/`, `saves/`).
3. Let you tick **Mods**, **World**, and/or **Config**.
4. For a world, list the worlds in `saves/` and confirm the target `level-name`.
5. Offer to back up the server's current files first.

Then restart the server in the panel.

### Patch mods only

To just sync mods (the common case after you update your pack locally):

```sh
python3 mcmove.py sync                 # pick server, uses remembered instance path
python3 mcmove.py sync --dry-run       # show the add/update/remove plan, change nothing
python3 mcmove.py sync --server survival --src ~/packs/create
```

How the patch is decided, per mod:

| Situation | Action |
| --- | --- |
| Same mod, same version | left alone |
| Same mod, new version | old removed, new uploaded |
| New mod in your pack | added |
| Mod removed from your pack | removed |
| Client-only mod | skipped, and removed if present |
| Mod added directly on the server | never touched |

Detection uses the [Modrinth](https://modrinth.com) API (hash lookup for
`server_side` support) with an offline jar-metadata fallback. Mods whose side can't
be determined are kept, not dropped.

### Commands

| Command | Description |
| --- | --- |
| `mcmove.py` | Run the interactive move wizard |
| `mcmove.py sync` | Patch a server's mods to match a local instance |
| `mcmove.py list` | List saved servers |
| `mcmove.py add-server [--url URL]` | Save a server profile |
| `mcmove.py remove-server NAME` | Delete a saved server |
| `mcmove.py move --src PATH` | Skip the source-folder prompt |

Config, per-server mod state, and backups live in `~/.config/mcmove/`.

### Notes

- **level-name:** a world only loads if its folder name matches `level-name` in
  `server.properties`. `mcmove` sets `level-name` to the target name you choose.
- **Single-player → server worlds:** a single-player world keeps Nether/End nested
  inside the world folder. That is fine for modded (Forge/NeoForge) servers; pure
  vanilla expects separate `world_nether` / `world_the_end`.
- **Client-only mods:** copying a client `mods/` folder onto a dedicated server will
  crash it (client-only mods like shaders, minimaps, and texture mods load client
  classes the server rejects). For modpacks, install the `.mrpack` with an mrpack
  installer so only server-side mods are placed, then use `mcmove` for the world.
- **SFTP only:** Pelican/Pterodactyl SFTP is a custom subsystem, so `rsync` does not
  work over it.

## Security

- Your panel password is **never written to disk** — it is prompted per run, or you
  can point a saved profile at an SSH private key instead.
- Saved profiles in `~/.config/mcmove/servers.json` contain only host, port, username,
  and an optional key path.
- Treat backups under `~/.config/mcmove/backups/` like any world save — they may
  contain server data.

## Roadmap

Ideas and contributions welcome:

- **Install straight from a `.mrpack`** — resolve the modpack index and download only
  server-side mods, no manual instance folder needed.
- **Reverse sync** — pull a world or files back from the server (one-shot backups).
- **`pipx` install / `mcmove` entry point** — drop the `python3 mcmove.py` prefix.
- **Delta uploads for worlds/configs** — skip unchanged files (mods already do this).
- **Optional keychain integration** — store the password in the OS keychain for
  hands-free connects.
- **Fully non-interactive `move`** — flags for scripted world/config transfers.

## Maintainers

[@zeriaxdev](https://github.com/zeriaxdev)

## Contributing

PRs accepted. Open an [issue](https://github.com/zeriaxdev/mcmove/issues) first if
you want to discuss a larger change.

Small note: this project follows the
[standard-readme](https://github.com/RichardLitt/standard-readme) spec.

## License

[MIT](LICENSE) © zeriax
