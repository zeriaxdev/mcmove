# MIGRATION.md — Python+Go → Rust

mcmove is being rewritten in **Rust** as a Cargo workspace, so its core logic can be
embedded as a library into a separate **Minecraft launcher (Rust + GPUI)** for one-click
sync/update — not just run as a CLI.

## Why Rust (not Go)
The deciding factor is *library embedding*, not the binary. A Go binary forces the GPUI
launcher to shell out via subprocess + JSON IPC — a permanent seam. A Rust core crate lets
the launcher call functions directly with shared types and native async progress.

The old "Go cross-compiles SFTP more easily" concern is void: `russh` + `russh-sftp` are
pure-Rust, async, zero C deps, and cross-compile to a Windows `.exe` from macOS when paired
with `rustls` (not OpenSSL). Async also *fits* GPUI's runtime instead of fighting it. This is
an I/O-bound tool, so Rust buys no perf over Go here — the win is purely the embeddable crate.

## Architecture
Cargo workspace:

```
mcmove/
├─ Cargo.toml            # [workspace]
├─ crates/
│  ├─ mcmove-core/       # lib — sftp, nbt, modrinth, patcher, sync/pull/update
│  └─ mcmove-cli/        # bin — clap wrapper → the standalone .exe
```

**The core rule that keeps it launcher-ready:** `mcmove-core` **never prints and never
prompts.**
- Returns structured results; emits progress via a `Reporter` trait (CLI → `indicatif`;
  GPUI → native UI).
- Passwords come through a credential callback so GPUI can prompt natively.
- Everything `async` (tokio) so the launcher UI never blocks.

## Dependency choices (pinned in Cargo.toml)
- **HTTP/Modrinth:** `reqwest` (rustls-tls, no OpenSSL) + `serde`/`serde_json`
- **SFTP:** `russh` + `russh-sftp` (pure Rust, async)
- **NBT (playerdata):** `fastnbt` + `flate2` (gzip)
- **Archives:** `zip` (jar metadata + `.mcmpatch` bundles)
- **Hash:** `sha1`
- **CLI:** `clap` (derive) + `indicatif`
- **Misc:** `anyhow`/`thiserror`, `dirs`, `tokio`

## Cross-compile (the proof that Rust doesn't reintroduce the PyInstaller pain)
```sh
rustup target add x86_64-pc-windows-gnu
brew install mingw-w64
cargo build --release --target x86_64-pc-windows-gnu -p mcmove-cli
```
Produces `target/x86_64-pc-windows-gnu/release/mcmove-cli.exe` — dependency-free, for the
Windows friend.

## Staged plan (Python stays working until Rust hits parity)
- **Stage 0 — Setup.** Gitignore artifacts; commit Go sidecar. *(done — commit acf0941)*
- **Stage 1 — Scaffold.** Workspace + two crates; pin deps; prove the Windows cross-compile.
- **Stage 2 — Patcher first.** Port `create`/`share`/`apply` from `modpack_patch.go`
  (self-contained, proven, friend-facing — validates the whole toolchain).
- **Stage 3 — `mcmove-core` modules, one at a time, each verified live:**
  `modrinth` → `nbt`/playerdata → `sftp` → state manifest → commands
  (`whois`, `update`, `sync`, `pull`, `playerdata`, `move`, server profiles).
- **Stage 4 — CLI parity.** clap subcommands matching today's UX; colors auto-off on
  non-TTY / `NO_COLOR`.
- **Stage 5 — Parity check, then flip.** Run Rust vs Python on the same instance/server;
  confirm identical behavior; *then* delete `mcmove.py` + `modpack_patch.go` + the
  PyInstaller `.spec`. Likely a `1.0.0` tag.
- **Stage 6 — Packaging.** GitHub Actions matrix (mac/win/linux) → Releases; `cargo install`;
  optional Homebrew tap.
- **Stage 7 — Launcher integration.** The GPUI launcher adds `mcmove-core` as a path/git dep,
  wires its progress events into the UI.

## Loose ends to resolve during the migration (deliberately not yet deleted)
- `mcmove.py` — unwired `cmd_export`/`cmd_import` + a 0.5.0→0.6.0 bump (dead code, uncommitted).
- `AGENTS.md` — byte-for-byte CLAUDE.md copy with a bogus "Codex Opus 4.8" trailer.
- `modpack-patch.spec` — abandoned PyInstaller leftover.
