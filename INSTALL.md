# Installing mcmove

mcmove ships as a single self-contained binary — no Python, no runtime deps.

## Windows (recommended — one line)

Open PowerShell and run:

```powershell
powershell -ExecutionPolicy Bypass -Command "irm https://raw.githubusercontent.com/zeriaxdev/mcmove/main/install.ps1 | iex"
```

This downloads the latest `mcmove.exe` from [Releases](https://github.com/zeriaxdev/mcmove/releases),
installs it to `%LOCALAPPDATA%\Programs\mcmove`, and adds it to your user PATH. Open a new
terminal and `mcmove` works anywhere. Re-run it any time to update to the newest release.

> First launch may trip SmartScreen ("unknown publisher") because the binary is unsigned —
> click **More info → Run anyway** once.

## macOS / Linux — prebuilt binary

Grab the binary for your platform from the [latest release](https://github.com/zeriaxdev/mcmove/releases/latest)
(`mcmove-aarch64-apple-darwin`, `mcmove-x86_64-apple-darwin`, or
`mcmove-x86_64-unknown-linux-gnu`), then:

```sh
chmod +x mcmove-*               # make it executable
sudo mv mcmove-* /usr/local/bin/mcmove
# macOS only, if Gatekeeper complains about an unsigned binary:
xattr -d com.apple.quarantine /usr/local/bin/mcmove
mcmove --version
```

## From source with Cargo (any platform)

Needs a [Rust toolchain](https://rustup.rs):

```sh
cargo install --git https://github.com/zeriaxdev/mcmove mcmove-cli
```

This builds and installs the `mcmove` binary into `~/.cargo/bin`. To build from a local
checkout instead:

```sh
cargo install --path crates/mcmove-cli
```

## Build a Windows .exe from macOS/Linux

```sh
rustup target add x86_64-pc-windows-gnu
brew install mingw-w64                       # or your distro's mingw-w64 package
cargo build --release --target x86_64-pc-windows-gnu -p mcmove-cli
# -> target/x86_64-pc-windows-gnu/release/mcmove.exe
```
