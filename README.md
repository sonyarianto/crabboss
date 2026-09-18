[![CI](https://github.com/sonyarianto/crabboss/actions/workflows/ci.yml/badge.svg)](https://github.com/sonyarianto/crabboss/actions/workflows/ci.yml)

# 🦀 CrabBoss

**Radio Automation Software**

A professional radio station management application featuring audio playback, library management, playlist automation, and streaming capabilities.

## Features

- ✅ **Audio Playback** — Play, pause, stop, volume via cpal; stereo gapless engine with equal-power crossfade and background decoding (UI never freezes)
- ✅ **Cue (PFL) + On Air Gate** — Private headphone audition bus on a second output (click-free fades, never touches program/stream/reports); deliberate crossfaded on-air cut of the selected track from the Playout strip
- ✅ **Music Library** — SQLite database with metadata extraction, search, health scan, loudness analysis, auto-classification (music/jingle/ad)
- ✅ **Playlist Management** — Create, edit, and manage playlists; auto-generator with rotation rules (no-repeat, separation, playcount priority, dayparting, jingle slots)
- ✅ **Scheduler** — Time + weekday events with expiration, Auto-DJ continuity with prefetch handoff and Coming-Up forecast
- ✅ **Advertisement Scheduler** — Dated blocks with intro→spot→outro chained breaks
- ✅ **Cart Wall** — 8 pads with hotkeys, progress, assign-from-library
- ✅ **12-Band EQ + Limiter + Loudness** — Program EQ, brickwall limiter, BS.1770/R128 normalization
- ✅ **Streaming Output** — Icecast source client (MP3/Opus, plain + TLS) with auto-reconnect, live-encoder indicator, one-click apply-and-restart, and Settings UI
- ✅ **Microphone/Line-In** — Live input with voice-activated ducking and bed mix
- ✅ **Reports** — Play logs with ranged reports + CSV/XLSX export
- ✅ **Dark Theme UI** — Modern dark radio-station theme via Iced (sidebar navigation, on-air status footer, aligned library table)

### Coming Soon

- 🔄 Shoutcast output
- 🔄 Voice tracking & teasers
- 🔄 Library depth (mass tag editor, BPM scan)
- 🔄 PDF report export
- 🔄 Headless/server mode + web remote UI

## Tech Stack

| Layer | Technology |
|-------|-----------|
| UI Framework | [Iced](https://iced.rs/) (Elm architecture) |
| Audio Playback | [cpal](https://github.com/RustAudio/cpal) |
| Audio Decoding | [symphonia](https://crates.io/crates/symphonia) |
| Metadata | [lofty](https://crates.io/crates/lofty) |
| Database | [rusqlite](https://crates.io/crates/rusqlite) (SQLite) |
| Concurrency | std threads + channels (UI thread never blocks on decode/analysis) |
| Logging | [tracing](https://crates.io/crates/tracing) |

## Project Structure

```
crabboss/
├── Cargo.toml          # Workspace root
├── crates/
│   ├── core/           # crabcore — audio engine, library, playlists
│   │   └── src/
│   │       ├── audio/  # Player, DSP, streaming, mic
│   │       ├── library/ # SQLite library & metadata
│   │       ├── playlist/ # Playlist management + generator
│   │       ├── scheduler/ # Timed events + expiration
│   │       ├── cart/    # Cart wall pads
│   │       ├── ads/     # Dated ad blocks
│   │       ├── stream/  # Icecast source client
│   │       ├── report.rs # Play-log reports
│   │       ├── settings.rs # Persisted prefs (incl. station name)
│   │       ├── license.rs # Offline license keys
│   │       └── examples/ # Vendor key generator (`genkey`)
│   └── ui/             # crabui — Iced desktop application
│       └── src/main.rs   # Elm app: 8 screens + tick subscriptions
└── README.md
```

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
> > **Security note (Stage A):** the Icecast source password is stored
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

## License

This project is licensed under the MIT License — see the LICENSE file for details.
