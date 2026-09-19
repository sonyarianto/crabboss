# CrabBoss Architecture

Technical overview for contributors: what the stack is, where code lives,
and how audio flows. User-facing docs (features, setup, formats) stay in
the [README](../README.md); delivery milestones stay in the
[ROADMAP](../ROADMAP.md).

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

```text
crabboss/
├── crates/core/              # crabcore — engine, library, scheduler, streaming (no UI)
├── crates/ui/                # crabui — Iced desktop app (Elm: app/, screens/, widgets.rs)
├── settings.example.json     # documented defaults (copy it, never commit yours)
└── .github/workflows/ci.yml  # fmt + clippy -D warnings + tests
```

## Signal Chain

- Background loader: symphonia decode → loudness gain → rubato resample
  → dual-cursor deck (crossfade handoff, generation-guarded).
- Mixer, per cpal frame: 12-band EQ → blend → gain → soft-clip → limiter.
- Program out a cpal output stream. Monitor volume is local-only; the
  stream tap sits pre-volume so the broadcast keeps full level.
- Mic/line-in arrives via `rtrb` ring, is ducked against the music bed,
  and sums in pre-limiter/tap so the broadcast hears it.
- Stream tee: post-DSP tap → MP3/Opus sender thread with real-time
  pacing → Icecast (`PUT`/`SOURCE`, mount-based) or Shoutcast v1/v2
  (`password` + `icy-*`, MP3-only, titles via `admin.cgi`).
- Cue/PFL preview runs on its own output stream (second device), flat with
  ~30 ms click-free fades. It never feeds the stream tap, the mixer, the
  silence monitor, or the play log.
