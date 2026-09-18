# Getting Started

CrabBoss is a native desktop app built in Rust. You build it from source —
no installer yet.

## Prerequisites

- **Rust** (latest stable): https://rustup.rs
- **Linux only** — ALSA development libraries:
  - Ubuntu/Debian: `sudo apt-get install libasound2-dev`
  - Fedora: `sudo dnf install alsa-lib-devel`
- **Opus encoder build tools** — CMake + a C toolchain, because the
  workspace always builds the bundled Opus encoder for Opus streaming:
  - Windows: VS Build Tools (`winget install Kitware.CMake`)
  - Linux/macOS: CMake + gcc/clang

## Build & Run

```bash
# Sanity check
cargo check --workspace

# Build
cargo build --workspace

# Run
cargo run -p crabui
```

## First Run

- Preferences live in `settings.json` under the per-user data dir
  (`%LOCALAPPDATA%\CrabBoss` on Windows — shown in Settings → Station
  as "Data:"), auto-created on first change.
- A legacy `settings.json` / `crabboss.db` next to the app is copied
  there once on first run (never overwritten).
- `--data-dir <path>` overrides the location for portable installs.
- Every tunable with its default is documented in
  [`settings.example.json`](https://github.com/sonyarianto/crabboss/blob/main/settings.example.json).
  Never commit your own `settings.json`: it holds machine devices and
  the plaintext Icecast password — protect it with OS permissions.

Next: [User Manual](./user-manual.md).
