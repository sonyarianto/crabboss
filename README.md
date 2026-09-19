[![CI](https://github.com/sonyarianto/crabboss/actions/workflows/ci.yml/badge.svg)](https://github.com/sonyarianto/crabboss/actions/workflows/ci.yml)
[![GitHub Sponsors](https://img.shields.io/github/sponsors/sonyarianto?style=social)](https://github.com/sponsors/sonyarianto)
[![Buy Me A Coffee](https://img.shields.io/badge/Buy%20Me%20a%20Coffee-support-FFDD00?style=flat&logo=buy-me-a-coffee&logoColor=black)](https://buymeacoffee.com/sonyarianto)
[![Ko-fi](https://img.shields.io/badge/Ko--fi-support-FF5E5B?style=flat&logo=ko-fi&logoColor=white)](https://ko-fi.com/sonyarianto)

# 🦀 CrabBoss

**Radio Automation Software** — 🌐 Official website: https://crabboss.vercel.app/

A professional radio station management application featuring audio playback, library management, playlist automation, and streaming capabilities.

## Features

- ✅ **Audio Playback** — Play, pause, stop, volume via cpal; stereo gapless engine with equal-power crossfade and background decoding (UI never freezes)
- ✅ **Cue (PFL) + On Air Gate** — Private headphone audition bus on a second output (click-free fades, never touches program/stream/reports); deliberate crossfaded on-air cut of the selected track from the Playout strip
- ✅ **Music Library** — SQLite database with metadata extraction, search, health scan, loudness analysis, auto-classification (music/jingle/ad)
- ✅ **Playlist Management** — Manual builder (create named playlists, add from Library, reorder Up/Down, remove, delete) + auto-generator with rotation rules (no-repeat, separation, playcount priority, dayparting, jingle slots); saved rotations fire to air in stored order (Home button or scheduler `load`), missing files skipped with a count
- ✅ **Scheduler** — Time + weekday events with `play`/`load`/`generate`/`queue` actions (`load` fires a named playlist in stored order), valid-until expiry with badges, Auto-DJ continuity with prefetch handoff and Coming-Up forecast
- ✅ **Advertisement Scheduler** — Dated blocks with intro→spot→outro chained breaks
- ✅ **Cart Wall** — 8 pads with hotkeys, progress, assign-from-library
- ✅ **12-Band EQ + Limiter + Loudness** — Program EQ, brickwall limiter, BS.1770/R128 normalization
- ✅ **Streaming Output** — Icecast + Shoutcast v1/v2 source client (MP3/Opus on Icecast, MP3 on Shoutcast, plain + TLS) with auto-reconnect, live-encoder indicator, one-click apply-and-restart, and Settings UI
- ✅ **Microphone/Line-In** — Live input with voice-activated ducking and bed mix
- ✅ **Reports** — Play logs with ranged reports + CSV/XLSX export
- ✅ **Dark Theme UI** — Modern dark radio-station theme via Iced (sidebar navigation, on-air status footer, aligned library table)

### Coming Soon

- 🔄 Voice tracking & teasers
- 🔄 Library depth (mass tag editor, BPM scan)
- 🔄 PDF report export
- 🔄 Headless/server mode + web remote UI

## Documentation

Technical overview for contributors (stack, layout, signal chain):
[docs/architecture.md](docs/architecture.md).
Milestones live in [ROADMAP.md](ROADMAP.md).

## Getting Started

### Prerequisites

- Rust (latest stable)
- ALSA development libraries (Linux):
  ```bash
  # Ubuntu/Debian
  sudo apt-get install libasound2-dev

  # Fedora
  sudo dnf install alsa-lib-devel
  ```
- CMake + a C toolchain (Windows: VS Build Tools; `winget install Kitware.CMake`):
  needed to compile the bundled Opus encoder for the Opus stream output.
  MP3-only builds don't need it, but the workspace always builds both.

### Build & Run

```bash
# Check
cargo check --workspace

# Build
cargo build --workspace

# Run
cargo run -p crabui
```

> Prefs live in `settings.json` under the per-user data dir
> (`%LOCALAPPDATA%\CrabBoss` on Windows — shown in Settings → Station
> as "Data:"), auto-created on first change. On first run after this
> change, existing `settings.json` / `crabboss.db` next to the app are
> copied there once (never overwritten). `--data-dir <path>` overrides
> the location for portable installs. Never commit yours: it holds
> machine devices and secrets. `settings.example.json` documents every
> tunable with defaults.
>
> > **Security note (Stage A):** the stream source password is stored
> > **plaintext** in the local `settings.json` (and in backups of it).
> > Anyone who can read that file can impersonate your stream source.
> > A platform credential store (Stage B) will replace this with a
> > reference; until then, protect the file with OS permissions and do
> > not share it.

## Supported Audio Formats

Thanks to symphonia and lofty, CrabBoss supports:

- **MP3** (MPEG-1 Audio Layer III)
- **FLAC** (Free Lossless Audio Codec)
- **AAC** (Advanced Audio Coding)
- **OGG Vorbis**
- **WAV** (Waveform Audio)
- **AIFF** (Audio Interchange File Format)
- **M4A** (MPEG-4 Audio)
- **Opus**
- **WavPack**
- **Musepack**

## Support

CrabBoss is free and open source (MIT). If it powers your station, please support development:

- 💖 [GitHub Sponsors](https://github.com/sponsors/sonyarianto)
- ☕ [Buy Me a Coffee](https://buymeacoffee.com/sonyarianto)
- 🧋 [Ko-fi](https://ko-fi.com/sonyarianto)

## License

This project is licensed under the MIT License — see the LICENSE file for details.
