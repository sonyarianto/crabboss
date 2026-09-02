# 🦀 CrabBoss

**Radio Automation Software** — built with Rust + Slint

A professional radio station management application inspired by RadioBoss, featuring audio playback, library management, playlist automation, and streaming capabilities.

## Features (In Progress)

- ✅ **Audio Playback** — Play, pause, stop, volume control via rodio
- ✅ **Music Library** — SQLite-backed database with metadata extraction
- ✅ **Playlist Management** — Create, edit, and manage playlists
- ✅ **Metadata Reading** — Auto-read ID3, Vorbis, and other tags via lofty
- ✅ **Dark Theme UI** — Modern dark radio-station theme via Slint

### Coming Soon

- 🔄 Crossfading engine
- 🔄 12-band equalizer
- 🔄 Playlist auto-generator with rotation rules
- 🔄 Advertisement scheduler
- 🔄 Icecast/Shoutcast streaming output
- 🔄 Cart wall for instant playback
- 🔄 Report generator (play logs → XLS/PDF)
- 🔄 Microphone/line-in input

## Tech Stack

| Layer | Technology |
|-------|-----------|
| UI Framework | [Slint](https://slint.dev/) (v1.17) |
| Audio Playback | [rodio](https://crates.io/crates/rodio) + cpal |
| Audio Decoding | [symphonia](https://crates.io/crates/symphonia) (via rodio) |
| Metadata | [lofty](https://crates.io/crates/lofty) |
| Database | [rusqlite](https://crates.io/crates/rusqlite) (SQLite) |
| Async Runtime | [tokio](https://crates.io/crates/tokio) |
| Logging | [tracing](https://crates.io/crates/tracing) |

## Project Structure

```
crabboss/
├── Cargo.toml          # Workspace root
├── crates/
│   ├── core/           # crabcore — audio engine, library, playlists
│   │   └── src/
│   │       ├── audio/  # Player, DSP, streaming
│   │       ├── library/ # SQLite library & metadata
│   │       └── playlist/ # Playlist management
│   └── ui/             # crabui — Slint desktop application
│       ├── ui/         # .slint UI files
│       │   ├── main.slint
│       │   ├── player.slint
│       │   ├── library.slint
│   │       │   ├── playlist.slint
│       │       │   └── common.slint
│       └── src/main.rs
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

## Supported Audio Formats

Thanks to symphonia and lofty, CrabBoss supports:

- **MP3** (MPEG-1 Audio Layer III)
- **FLAC** (Free Lossless Audio Codec)
- **AAC** (Advanced Audio Coding)
- **OGG Vorbis**
- **WAV** (Waveform Audio)
- **AIFF** (Audio Interchange File Format)
- **Opus**
- **WavPack**
- **Musepack**

## License

This project is licensed under the GNU General Public License v3.0 — see the LICENSE file for details.

---

🦀 Built with Rust + Slint
