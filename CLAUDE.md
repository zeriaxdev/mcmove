# CLAUDE.md — mcmove

Context for AI/dev sessions on this repo. Read this first.

## What this is
`mcmove` — a single-file Python CLI to move **worlds, mods, configs, and player
saves** between a local Minecraft instance (Modrinth App, Prism, etc.) and a
**Pelican / Pterodactyl** game server over **SFTP**. Repo: `github.com/zeriaxdev/mcmove`.

## Conventions (ALWAYS — non-negotiable)
- **Conventional Commits**: `feat:`, `fix:`, `docs:`, `refactor:`, `build:`, `chore:` …
- **SemVer**, annotated git tags `vX.Y.Z`. Backwards-compatible feature → **minor** bump.
- Commit trailer: `Co-Authored-By: Claude Opus 4.8 <noreply@anthropic.com>`.
- Push with `git push --follow-tags`.
- Version is single-sourced: `__version__` in `mcmove.py`; `pyproject.toml` reads it
  dynamically; surfaced via `mcmove --version`. Bump it in the same commit as the feature.

## Layout & architecture
- `mcmove.py` — the **entire** tool (one module, on purpose).
- `pyproject.toml` — setuptools, `py-modules = ["mcmove"]`, console entry `mcmove = "mcmove:cli"`.
- **Single-file is deliberate** (trivial `pipx install git+…`, easy to read/copy-run).
  Split into a package (natural seams: `sftp` / `modrinth` / `nbt` / `cli`) when it
  crosses **~2k lines** or when the **Pelican API** feature lands — do it as a
  dedicated `refactor:` commit.

## Commands (current, v0.5.0)
- `add-server` / `list` / `remove-server` — server profiles from panel SFTP details.
  Password is **never stored** (prompted per run, or use an SSH key path).
- `move` — interactive wizard: push world / mods / config (mods go through the patcher).
- `sync` — push: patch server `/mods` to match the local instance (add/update/remove;
  **skips client-only mods** via Modrinth; **guard** refuses to remove >50% of managed
  mods without extra confirmation — added after a wrong-instance near-miss).
- `pull [--mirror]` — reverse: server mods → local instance. **Additive + update only by
  default; never deletes local mods** (client-only safe). `--mirror` prunes local
  server-side mods gone from the server, still keeping client-only.
- `update [--channel release|beta|alpha] [--all] [--dry-run]` — Modrinth update checker;
  **per-mod version selector** with channel tags.
- `playerdata` — build server `playerdata/<uuid>.dat` from single-player `level.dat`
  (extract `Data→Player` NBT; bulk Mojang username→UUID; optional SFTP upload to
  `<world>/playerdata` with backups). Preserves inventory, gear, and **mod data
  attachments** (`neoforge:attachments`, e.g. Superb Warfare ammo).
- `whois` — UUID → username (args, `--dir`, or a server's playerdata listing).

## Dev & testing
- `python3 -m venv .venv && .venv/bin/pip install -e .`
- Run `python3 -m py_compile mcmove.py` after edits.
- No test suite yet (roadmap). We validate helpers live: Modrinth versions (real Sodium),
  Mojang lookups (Notch/jeb_), and synthetic `level.dat` NBT round-trips.
- Deps: `paramiko` (SFTP), `nbtlib` (NBT). `tomllib`/`nbtlib`/`paramiko` are import-guarded.

## Technical notes / gotchas
- Pelican/Pterodactyl SFTP is chrooted to the server root → paths like `/mods`,
  `/<world>/playerdata`. It's a custom subsystem: **no rsync**, and `listdir_attr` may
  include non-`.jar` entries (filter them on download).
- Mod side classification: Modrinth hash → `server_side` (`unsupported` = client-only);
  offline fallback reads `fabric.mod.json` `environment` (forge/neoforge → unknown → keep).
- Per-server mod manifest: `~/.config/mcmove/state/<server>.json` (tracks managed mods by
  Modrinth project id / mod id). Config + backups under `~/.config/mcmove/`.
- Colors auto-disable when stdout isn't a TTY or `NO_COLOR` is set.
- `playerdata`/`whois`: **online-mode (Mojang) UUIDs only**; offline-mode is roadmap.
- Player `.dat` = gzipped NBT with the Player compound as root; `level.dat` =
  root → `Data` → `Player`.

## Roadmap (banked, in rough priority)
1. **Auto-restart after sync** via Pelican client API (panel URL + API key). Also the
   right moment to modularize the codebase.
2. **`pull` for configs + worlds** (full two-way mirror).
3. **`export` / `import`** — share a modpack PC→PC (`.mrpack` or `.zip`), passwordless,
   launcher-agnostic. (Designed, paused by user.)
4. **PyPI publish + GitHub Actions CI** (so `pipx install mcmove` works without the git URL).
5. `status` (read-only diff), `doctor` (connectivity/health), `.mrpack` install.
6. Offline-mode UUIDs; reverse `playerdata` pull; OS-keychain password storage.
7. Delta uploads for worlds/configs.

## Origin
Built in one long session while debugging a NeoForge 1.21.1 "Create+" pack on a
DollarDeploy-hosted Pelican server: C2ME / Sable / Moonlight fake-level startup
deadlocks, Sable's UDP channel behind NAT, watchdog kills during Biolith worldgen, the
single-player → dedicated-server `playerdata` gap, and Xaero map migration. mcmove is the
durable artifact of all that.
