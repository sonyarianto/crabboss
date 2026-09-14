# 🦀 CrabBoss

**Radio Automation Software**

A professional radio station management application featuring audio playback, library management, playlist automation, and streaming capabilities.

## Features

- ✅ **Audio Playback** — Play, pause, stop, volume via cpal; stereo gapless engine with equal-power crossfade and background decoding (UI never freezes)
- ✅ **Music Library** — SQLite database with metadata extraction, search, health scan, loudness analysis, auto-classification (music/jingle/ad)
- ✅ **Playlist Management** — Create, edit, and manage playlists; auto-generator with rotation rules (no-repeat, separation, playcount priority, dayparting, jingle slots)
- ✅ **Scheduler** — Time + weekday events with expiration, Auto-DJ continuity with prefetch handoff and Coming-Up forecast
- ✅ **Advertisement Scheduler** — Dated blocks with intro→spot→outro chained breaks
- ✅ **Cart Wall** — 8 pads with hotkeys, progress, assign-from-library
- ✅ **12-Band EQ + Limiter + Loudness** — Program EQ, brickwall limiter, BS.1770/R128 normalization
- ✅ **Streaming Output** — Icecast source client (MP3, plain + TLS) with auto-reconnect and Settings UI
- ✅ **Microphone/Line-In** — Live input with voice-activated ducking and bed mix
- ✅ **Reports** — Play logs with ranged reports + CSV export
- ✅ **Dark Theme UI** — Modern dark radio-station theme via Iced (sidebar navigation, on-air status footer, aligned library table)

### Coming Soon

- 🔄 Shoutcast output + listener stats
- 🔄 Voice tracking & teasers
- 🔄 Library depth (mass tag editor, BPM scan, dupe detection, auto-sync)
- 🔄 XLS/PDF report export
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
│   │       └── license.rs # Offline license keys
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

### Build & Run

```bash
# Check
cargo check --workspace

# Build
cargo build --workspace

# Run
cargo run -p crabui
```

> Prefs live in `settings.json` (auto-created on first change — never commit
> yours, it holds machine devices and secrets). `settings.example.json`
> documents every tunable with defaults.

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
