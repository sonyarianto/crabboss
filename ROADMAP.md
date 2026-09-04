# CrabBoss ROADMAP

## Audio Backend Decision: rodio → cpal

**Status:** Decided — migrate to `cpal` directly for low-level control.

**Current (v0.1.0):**
- `crabcore::audio::Player` (`crates/core/src/audio/player.rs`) uses `rodio 0.19` + `symphonia` (via rodio) + `lofty` for duration.
- Good for: play / pause / resume / volume.
- Not enough for radio automation.

**Why rodio is insufficient:**
- `Sink::append()` — no sample-accurate gapless / crossfade control.
- No DSP insert point — can't do 12-band EQ, ducking, limiter, loudness.
- Single output only — can't do program + cue/monitor buses, mic/line-in mix, Icecast/Shoutcast tee.
- Device / latency / exclusive-mode control hidden.
- Current `stop()` takes the `Sink` and never recreates it (replay goes silent) — symptom of fighting the abstraction.

**Target:**
- `cpal` for I/O (output + input streams, device enum, format/latency control).
- `symphonia` directly for decode (mp3/flac/aac/ogg/wav/aiff/opus).
- `rubato` for resampling to device rate, `rtrb` for decoder→mixer ring buffer.
- Keep `lofty` for metadata, `rusqlite` for library.

**Target signal chain:**
```
symphonia decoder thread -> f32 PCM -> rtrb ringbuf ->
  Mixer [gain -> 12-band EQ (biquad) -> crossfade -> limiter] ->
    cpal OutputStream (program)
    + cpal InputStream (mic/line-in, mixed/ducked)
    + Icecast/Shoutcast tee (encoded stream)
```

**Migration (keep `crabui` working):**
1. ✅ Add `cpal, symphonia, rubato, rtrb` to `crates/core/Cargo.toml`; keep `rodio` temporarily.
2. ✅ Introduce `audio::Engine` trait; implement `CpalEngine` alongside legacy `Player`.
3. ✅ Switch `crates/ui/src/main.rs` to `Engine` trait — A/B via `--engine cpal` (default rodio).
4. Remove `rodio` dependency.

## Broader Milestones (from README)

- [x] Fix `Player::stop()` sink recreation bug
- [x] `CpalEngine` MVP (play/pause/volume parity) — rubato resample TODO
- [x] Router: Home / Playout / Media Manager / Scheduler / Cart Wall screens
- [x] License key activation (offline, `CB-XXXX-XXXX-XXXX`)
- [x] Library: `scan_directory()` via `walkdir`, live list + search model, `rfd` import dialog, tap-to-play
- [x] Playlist store wired (`PlaylistManager::open`, Home counts) + unit tests
- [x] Scheduler MVP: event list, Add/Edit dialog (time/action/days), auto-tick firing `generate`/`load`/`play`
- [x] Track kinds (music/jingle/ad): auto-classify on import, `set_kind`, pre-kind DB migration
- [x] Cart Wall MVP: 8 pads, instant play, jingle-first seeding/loading with kind badges
- [ ] Crossfader + gapless (see §1.1 — full scope below)
- [ ] 12-band EQ + limiter (see §1.1)
- [x] Playlist auto-generator with rotation rules (engine done: repeat/separation/priority/daypart/jingles; UI presets open)
- [x] Ad scheduler (dated blocks with intros/outros, chained breaks — see §1.3)
- [ ] Icecast/Shoutcast output (see §1.5)
- [ ] Mic/line-in input with ducking (see §1.6)
- [x] Report generator (play logs → CSV + screen; XLS/PDF open — see §1.9)
- [ ] File dialog (`rfd`), progress timer in UI (see §1.9) — `rfd` import done, progress timer still open
- [x] Settings screen (device picker, live DSP prefs, license — streaming config still open, see §1.9)
- [ ] Quality: `cargo fmt/clippy`, unit tests (`library`, `playlist`), CI (see §1.10)

## Gap Matrix vs RadioBOSS 7.x (2026)

Legend: ✅ done · 🟡 partial/scaffold · ❌ not started · — not previously in ROADMAP

| Area | RadioBOSS has | CrabBoss today | Status |
|---|---|---|---|
| Playback engine | Gapless, sample-accurate crossfade, curve choice | `Mixer` DSP exists, not wired to two cursors; mono-only `CpalEngine` | 🟡 |
| EQ / dynamics | Full EQ, limiter, loudness normalization | Unchecked | ❌ |
| Playlist generator | Rotation, no-repeat, separation, playcount priority, dayparting, multi-playlist UI | Checkbox only (+ kind-aware counting) | ❌ |
| Ad scheduler | Dated blocks, intros/outros, color-coded list | Unchecked | ❌ |
| Scheduler | Time+weekday, expirations, weekday column, insert-after | MVP done | ✅ + depth TODO (§1.3) |
| Cart wall | 8+ pads, hotkeys, progress, drag-drop, resize | 8 pads, instant play, kind badges | ✅ + depth TODO (§1.3) |
| Voice tracking / teasers | Voice tracks, auto-intro, teasers | — | — |
| Streaming output | Icecast/Shoutcast + relay, listener stats, artwork | Unchecked | ❌ |
| Mic / line-in | Mixed input, sidechain ducking, bed music | Unchecked | ❌ |
| Silence detector | Dead-air auto-recovery | ✅ cpal mix-bus metering + filler recovery | ✅ |
| Remote control API | Playbackinfo, insert-after, scheduler on/off, requests | — (web remote UI in §2 instead) | — |
| Reporting | Play logs → XLS/PDF, royalty reports | Unchecked | ❌ |
| Library depth | Mass tag editor, BPM scan, dupe detection, scheduled sync, health scan | `scan_directory()` only | 🟡 |
| Track health | Proactive missing/corrupt detection | ✅ `missing_files()` + startup/on-demand scan, ⚠ row flags | ✅ |
| UI niceties | Hotkeys, screen-reader a11y, drag-drop, waveform | — | — |
| Stream archive | Scheduled output recording | — | — |
| License | Offline key, holder, tier | MVP done (checksum → ed25519 TODO) | ✅ |
| File import UX | File dialog | `rfd` unchecked | ❌ |
| Quality gates | — | Tests in scheduler/cart/mixer/license; none for library/playlist; no CI | 🟡 |

Explicitly **out of scope**: DTMF phone-line control, CD-grabber (legacy hardware, see §2).

## 1. Parity Work (ordered by priority)

### 1.1 Finish the audio core (blocks everything downstream)
- [x] `CpalEngine`: stereo passthrough (no more mono downmix; mono devices get L+R mix-down)
- [x] `CpalEngine`: dual-cursor playback — `play()` while playing crossfades instead of cutting
- [x] Configurable crossfade curve (`CrossfadeCurve::EqualPower` default / `Linear`)
- [x] `rubato` sinc resampling to device rate at decode time
- [ ] 12-band EQ insert (biquad chain) + limiter tuning
- [ ] Loudness normalization (ReplayGain-style)
- [ ] Wire `library.search()` results into the Slint model (currently a no-op) — ✅ done (live list + search filter + tap-to-play)

### 1.2 Playlist Generator (real scope, not a checkbox)
- [x] No-repeat rules: artist, title, album — configurable lookback window
- [x] Separation rules (same genre gap)
- [x] Playcount-priority weighting (LeastPlayed MIN-style / MostPlayed MAX-style)
- [x] Dayparting: per-track hours + days, wrap-past-midnight aware (`set_daypart`, honored by generator)
- [x] Scheduler `generate` builds a real rotation and persists it as a playlist
- [ ] Multi-playlist generation UI (several dayparts/rotations at once)

### 1.3 Ads, Scheduler & Cart depth
- [x] Ad blocks with start/end date ranges (validity window + weekday + HH:MM, full Add/Edit UI)
- [x] Intro/outro clips per ad block (engine pending-queue chains intro→spot→outro; spot play logged)
- [ ] Scheduler event expiration ("valid until") + warnings
- [x] "Insert after current track" (`queue` action + `Engine::queue`: cpal blends
      at the boundary with end-of-track auto-fade; rodio degrades to immediate play)
- [ ] Cart hotkeys, drag-drop from library, per-pad progress bar

### 1.4 Voice Tracking & Teasers
- [ ] Record a voice track that auto-ducks/overlaps between two library tracks
- [ ] Auto-intro: voice over outgoing song's tail, timed to end as next vocals start
- [ ] Teaser/promo clips scheduled between songs

### 1.5 Streaming Output
- [ ] Icecast source client (encode + push)
- [ ] Shoutcast v1/v2 source client
- [ ] Listener/connection stats in UI
- [ ] Artwork metadata forwarding to encoders

### 1.6 Mic / Live Assist
- [ ] Mic input via `cpal` input stream, mixed into program bus
- [ ] Sidechain ducking: auto-lower music bed when mic is active (voice-activated)
- [ ] Mic "bed" music under live breaks

### 1.7 Reliability
- [x] Silence detector: `SilenceMonitor` meters the cpal mix bus (−60 dBFS floor,
      10 s default threshold); UI polls every 5 s and auto-recovers dead air
      with a filler music track (rate-limited 1/min). Metering is cpal-only —
      rodio (current default) reports no alarm.
- [x] Background library health scan: `missing_files()` + startup scan, on-demand
      `✓ Health` button in Media/Playout, ⚠ prefixes on missing rows

### 1.8 Library depth
- [ ] Mass tag editor (multi-select batch edit)
- [ ] BPM detection/scan
- [ ] Duplicate track detection
- [ ] Scheduled folder auto-sync (timer re-scan, not just manual import)

### 1.9 Reporting & Ops
- [x] Play logging on every play path (library tap, cart fire, scheduler run, silence filler)
- [x] Play-log reports: range presets (Today/7d/30d/All), jingle+ad exclusion, newest-100 list, CSV export
- [ ] XLS/PDF export (CSV done — spreadsheets/royalty bodies accept it; native XLS/PDF later)
- [x] Settings screen: output device picker (persisted, applies on restart, with
      unplugged-device fallback), engine display, crossfade + silence-alarm
      steppers (persisted, applied live), license section
- [x] `rfd` native file dialog for import (+ report export)

### 1.10 Quality gates
- [ ] Unit tests for `library` and `playlist` (match scheduler/cart/mixer/license bar) — `playlist` done, `library` partial (kind tests only)
- [x] `cargo fmt` + `clippy` in CI (`-D warnings`, zero warnings) + `ci.yml` (fmt/clippy/test on push+PR)

## 2. Beyond Parity — Where CrabBoss Wins

RadioBOSS weaknesses: Windows-only, legacy Delphi UI, no ready remote UI,
closed codebase. Our stack (Rust + Slint, cross-platform, headless-ready)
beats it on these axes instead of just chasing feature count:

- [ ] **Headless/server mode as first-class target** — `crabboss --headless
      --config station.toml` on a cheap Linux VPS, no display, no Windows
      license. RadioBOSS cannot do this.
- [ ] **Web remote-control UI** — embedded HTTP server (now-playing,
      scheduler, carts, library search) for a DJ's phone browser. RadioBOSS
      has a raw command protocol, not a ready UI.
- [ ] **Config-as-code** — station config, scheduler events, rotation rules
      as version-controllable TOML/JSON instead of GUI-only config.
- [ ] **Open plugin points** in `Engine`/manager traits (custom scheduler
      actions, streaming targets, import sources) without forking.
- [ ] **Fair licensing** — genuine free tier + transparent paid tier vs
      flat $149.95, aimed at hobbyist/community radio.

## 3. Phase Sequencing

1. **Phase 1 (parity foundation):** §1.1 audio core — nothing else matters
   until crossfade/stereo/EQ work.
2. **Phase 2 (operational parity):** §1.2–1.4 (playlist rules, ads, voice
   tracking) — separates "plays files" from "radio automation."
3. **Phase 3 (broadcast parity):** §1.5–1.7 (streaming, mic, reliability) —
   required before real on-air use.
4. **Phase 4 (polish/ops):** §1.8–1.10 (library depth, reporting, CI).
5. **Phase 5 (differentiation):** §2 — wins users away instead of matching.
