# CrabBoss ROADMAP

## Audio Backend: cpal (rodio removed)

**Status:** Done — migrated to `cpal` directly for low-level control;
the legacy rodio `Player` and the `rodio` dependency are removed.
`CpalEngine` is the sole backend (`--engine` flag kept as a no-op).

**Before (v0.1.0):**
- `crabcore::audio::Player` (`audio/player.rs`, since deleted) used
  `rodio 0.19` + `symphonia` (via rodio) + `lofty` for duration.
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
- `rubato` for resampling to device rate at decode time; background loader
  thread decodes full tracks off the UI thread; `rtrb` rings carry mic
  input and the stream tap (lock-free, audio-thread-safe).
- Keep `lofty` for metadata, `rusqlite` for library.

**As-built signal chain:**
```
background loader: symphonia decode -> loudness gain -> rubato resample ->
  install dual-cursor deck (crossfade handoff, generation-guarded)
Mixer [12-band EQ -> blend -> gain -> soft-clip -> limiter] per cpal frame ->
  cpal OutputStream (program, x monitor volume)
  + cpal InputStream (mic/line-in via rtrb, ducked, pre-limiter/tap)
  + Icecast tee (post-DSP tap via rtrb -> MP3 sender thread)
```

**Migration (kept `crabui` working throughout):**
1. ✅ Add `cpal, symphonia, rubato, rtrb` to `crates/core/Cargo.toml`; keep `rodio` temporarily.
2. ✅ Introduce `audio::Engine` trait; implement `CpalEngine` alongside legacy `Player`.
3. ✅ Switch `crates/ui/src/main.rs` to `Engine` trait — A/B via `--engine cpal` (default rodio).
4. ✅ Remove `rodio` dependency: delete `audio/player.rs`, move
   `PlayerState`/`TrackInfo` into `audio::engine`, cpal becomes the
   default and only backend.

## Broader Milestones (from README)

- [x] Fix `Player::stop()` sink recreation bug
- [x] `CpalEngine` MVP (play/pause/volume parity) — rubato resample TODO
- [x] Router: Home / Playout (stacked broadcast strip) / Library / Scheduler / Cart Wall / Reports / Ads / Settings screens + sidebar nav + on-air footer
- [x] License key activation (offline, `CB-XXXX-XXXX-XXXX`)
- [x] Library: `scan_directory()` via `walkdir`, live list + search model, aligned Kind/Title/Artist/Dur/Gain columns, `rfd` import dialog, tap-to-play
- [x] Playlist store wired (`PlaylistManager::open`, Home counts) + unit tests
- [x] Scheduler MVP: event list, Add/Edit dialog (time/action/days), auto-tick firing `generate`/`load`/`play`
- [x] Track kinds (music/jingle/ad): auto-classify on import, `set_kind`, pre-kind DB migration
- [x] Cart Wall MVP: 8 pads, instant play, jingle-first seeding/loading with kind badges
- [x] Crossfader + gapless (see §1.1 — stereo dual-cursor engine, equal-power/linear curves, background decode loader)
- [x] 12-band EQ + limiter (see §1.1)
- [x] Playlist auto-generator with rotation rules (engine done: repeat/separation/priority/daypart/jingles; Home panel fires 4 dayparts at once)
- [x] Auto-DJ continuity: 200 ms tick with live progress, prefetch handoff
      (cpal, 8 s horizon), single-outstanding prefetch guard (in-flight
      decodes count as pending — no duplicate queue storms), `RuleHistory`
      separation across picks + Coming-Up forecast list, live jingle
      insertion at the configured interval (shared slot logic with batch
      rotations), promotion reconcile (queued decks get logged + labeled
      with their source), EOF restart, Next/Prev, cold start (Play /
      Auto-DJ toggle begin the first pick), persisted ON/OFF + Up-next
- [x] Ad scheduler (dated blocks with intros/outros, chained breaks — see §1.3)
- [x] Icecast/Shoutcast output (see §1.5)
- [x] Mic/line-in input with ducking (see §1.6)
- [x] Report generator (play logs → CSV + screen; XLS/PDF open — see §1.9)
- [x] File dialog (`rfd`) + import progress in UI (see §1.9) — native multi-select dialog, chunked per-tick import with live status
- [x] Settings screen (device picker, live DSP prefs, license, streaming config — see §1.9)
- [x] Quality: `cargo fmt/clippy`, unit tests (`library`, `playlist`), CI (see §1.10)

## Gap Matrix vs RadioBOSS 7.x (2026)

Legend: ✅ done · 🟡 partial/scaffold · ❌ not started · — not previously in ROADMAP

| Area | RadioBOSS has | CrabBoss today | Status |
|---|---|---|---|
| Playback engine | Gapless, sample-accurate crossfade, curve choice | Stereo dual-cursor engine, equal-power/linear crossfade, background decode loader, gapless queued handoff | ✅ |
| EQ / dynamics | Full EQ, limiter, loudness normalization | 12-band peaking EQ (±12 dB) + brickwall limiter + BS.1770/R128 loudness normalization (library scan, per-track gain at decode) | ✅ |
| Playlist generator | Rotation, no-repeat, separation, playcount priority, dayparting, multi-playlist UI | Engine complete (repeat/separation/priority/daypart/jingles) + scheduler `generate` + Auto-DJ rotation; multi-preset UI open | 🟡 |
| Ad scheduler | Dated blocks, intros/outros, color-coded list | Dated blocks with intro→spot→outro chained breaks + engine pending queue | ✅ |
| Scheduler | Time+weekday, expirations, weekday column, insert-after | MVP + "valid until" expiry with row badges and warnings banner | ✅ |
| Cart wall | 8+ pads, hotkeys, progress, drag-drop, resize | 8 pads, hotkeys 1–8, per-pad progress + playing highlight, assign-from-library flow | ✅ |
| Voice tracking / teasers | Voice tracks, auto-intro, teasers | — | — |
| Streaming output | Icecast/Shoutcast + relay, listener stats, artwork | Icecast source client (MP3/LAME, PUT + SOURCE fallback, TLS, paced, reconnect, metadata) + Settings UI with live status + listener count + Playout cover art; Shoutcast/relay open | 🟡 |
| Mic / line-in | Mixed input, sidechain ducking, bed music | cpal input + `rtrb` ring summed pre-limiter/tap, voice-activated ducker, live device switching, Settings mic panel | ✅ |
| Silence detector | Dead-air auto-recovery | ✅ cpal mix-bus metering + filler recovery | ✅ |
| Remote control API | Playbackinfo, insert-after, scheduler on/off, requests | — (web remote UI in §2 instead) | — |
| Reporting | Play logs → XLS/PDF, royalty reports | Play logging on all paths + ranged reports + CSV export (jingles/ads excluded); XLS/PDF open | ✅ |
| Library depth | Mass tag editor, BPM scan, dupe detection, scheduled sync, health scan | Scan + missing-file health scan + loudness scan + kind auto-classify/repair + duplicate flag view + folder auto-sync; mass-tag/BPM open | 🟡 |
| Track health | Proactive missing/corrupt detection | ✅ `missing_files()` + startup/on-demand scan, `!` row flags | ✅ |
| UI niceties | Hotkeys, screen-reader a11y, drag-drop, waveform | Cart hotkeys 1–8 + sidebar nav + status footer; a11y/drag-drop/waveform open | 🟡 |
| Stream archive | Scheduled output recording | — | — |
| License | Offline key, holder, tier | MVP done: checksum keys + vendor `genkey`, status labels (checksum → ed25519 TODO). NOT enforced yet: `features_enabled()` unwired, holder hardcoded — enforcement, per-station names, and expiry gating parked until the business model is decided | 🟡 |
| File import UX | File dialog | Native `rfd` multi-select import with per-tick progress + report-export dialog | ✅ |
| Quality gates | — | Full suite green across all areas (library, playlist, scheduler, cart, mixer, license, stream, audio engine incl. lock-poisoning, settings); FK cascades proven; `cargo fmt` + `clippy -D warnings` in CI | ✅ |

Explicitly **out of scope**: DTMF phone-line control, CD-grabber (legacy hardware, see §2).

## 1. Parity Work (ordered by priority)

### 1.1 Finish the audio core (blocks everything downstream)
- [x] `CpalEngine`: stereo passthrough (no more mono downmix; mono devices get L+R mix-down)
- [x] `CpalEngine`: dual-cursor playback — `play()` while playing crossfades instead of cutting
- [x] Configurable crossfade curve (`CrossfadeCurve::EqualPower` default / `Linear`)
- [x] `rubato` sinc resampling to device rate at decode time
- [x] 12-band EQ insert (RBJ peaking biquad chain, ±12 dB, per-band
      Settings steppers, persisted + live-applied, cpal-only) + limiter
      (per-frame brickwall attack with metered release, ceiling stepper in dBFS)
- [x] Loudness normalization (ReplayGain-style): BS.1770 K-weighting +
       R128 gating meter (`LoudnessMeter`, validated against the ITU mono-sine
       −3.01 LUFS anchor), 🔊 Loudness scan button in the Library header writes
      per-track LUFS + gain toward the adjustable target (default −9 LUFS,
      RadioBOSS-style; −23 broadcast floor available) into the library (responsive
      background-thread scan with live progress), gain badge per library row, applied at
      decode time on every cpal play path behind a Settings ON/OFF toggle
- [x] Background decode loader: `play`/`queue` return instantly (idle play
      announces + `Buffering`); live handoffs keep the old deck sounding
      until the new deck lands as a crossfade, superseded/stopped loads are
      discarded, pause-mid-load sticks for an explicit resume
- [x] Panic-proof audio callback: every lock the realtime thread takes
      recovers from poisoning (`into_inner`) instead of killing the output
      stream; decode never runs there by construction (loader thread only).
      Poison survival is unit-tested; off-callback locks stay fail-fast
      on purpose (a UI panic is process death anyway)
- [x] Monitor-independent volume: the local knob dims speakers only; the
      program bus + stream tap stay at full level (monitor the delayed web
      stream at zero local volume without doubling or dimming the broadcast)
- [ ] Wire `library.search()` results into the Iced list (currently a no-op) — ✅ done (live list + search filter + tap-to-play)

### 1.2 Playlist Generator (real scope, not a checkbox)
- [x] No-repeat rules: artist, title, album — configurable lookback window
- [x] Separation rules (same genre gap)
- [x] Playcount-priority weighting (LeastPlayed MIN-style / MostPlayed MAX-style)
- [x] Dayparting: per-track hours + days, wrap-past-midnight aware (`set_daypart`, honored by generator)
- [x] Scheduler `generate` builds a real rotation and persists it as a playlist
- [x] One-at-a-time Auto-DJ primitives: `RuleHistory` threads no-repeat
      windows across picks (`generate_next`), `forecast_up_next` simulates
      coming picks on cloned history for the Coming-Up display, and the
      jingle slot fires at interval via logic shared with batch rotations
- [ ] Multi-playlist generation UI (several dayparts/rotations at once)

### 1.3 Ads, Scheduler & Cart depth
- [x] Ad blocks with start/end date ranges (validity window + weekday + HH:MM, full Add/Edit UI)
- [x] Intro/outro clips per ad block (engine pending-queue chains intro→spot→outro; spot play logged)
- [x] Scheduler event expiration ("valid until"): inclusive `YYYY-MM-DD`
      validity end honored by the auto-tick (`is_due` is date-aware), per-row
      badge (⏳ ≤7 days / ⚠ last day / expired) + warnings banner (⏳ ≤3 days,
      ⚠ last-day, expired-but-still-listed) and a Valid-until field in the
      Add/Edit dialog; empty = runs forever
- [x] "Insert after current track" (`queue` action + `Engine::queue`:
      blends at the boundary with end-of-track auto-fade)
- [x] Cart hotkeys (keys 1–8), per-pad progress bar +
      playing highlight, and assign flow (Assign → arm a library track →
      tap a pad) with pad-place
      API (`assign_at` replace-in-slot, fixed 8-pad wall)

### 1.4 Voice Tracking & Teasers
- [ ] Record a voice track that auto-ducks/overlaps between two library tracks
- [ ] Auto-intro: voice over outgoing song's tail, timed to end as next vocals start
- [ ] Teaser/promo clips scheduled between songs

### 1.5 Streaming Output
- [x] Icecast source client (encode + push): MP3/LAME CBR encoder tapped off the
      post-DSP cpal mix bus (pre-monitor-volume), lock-free `rtrb` ring → sender
      thread with real-time pacing, Icecast 2.4 `PUT` with legacy `SOURCE`
      fallback, mount in the request path, `100`-means-go handshake
      (the final 200 may only arrive at teardown; silence past a short
      grace proceeds optimistically, EOF stays a rejection), bounded
      TCP connect, optional TLS (OS-native stack, SNI) for HTTPS servers,
      in-band `StreamTitle` metadata, bounded reconnects (5, backoff)
- [x] Settings UI: STREAM ON/OFF toggle (auto-start on launch when enabled),
      host/port/mount/password/username fields (persisted), TLS toggle for
      HTTPS servers, bitrate ladder stepper (8–320 kbps), live status
      (⏳ Connecting/🔴 Live/⚠ error) + bytes/uptime stats; stream tap
      handle shared with the audio callback (fixed silent dead-air-while-Live)
- [x] Deployment lesson (verified live): L7 reverse proxies (Traefik/nginx)
      may pass source headers + statuses yet swallow the never-ending PUT
      body — server then kills the starved source on socket timeout while
      the client sees RST seconds after handshake. Run the source path
      direct to the pod (plain HTTP, e.g. :8000) or via TCP passthrough;
      keep HTTPS for listeners
- [ ] Shoutcast v1/v2 source client
- [x] Listener/connection stats in UI (local bytes/uptime plus listener
      count polled from the public status API while live; "—" when the
      API is disabled/unreachable — never an error state)
- [x] Artwork metadata forwarding (embedded art pulled via `lofty` at
      install, shown as cover in the Playout strip; the Icecast/Shoutcast
      source protocol is text-only (`StreamTitle`), so there is no
      encoder sink by design — not a gap)

### 1.6 Mic / Live Assist
- [x] Mic input via `cpal` input stream, mixed into program bus (device
      picker with live switching, f32 paths at the output rate, linear
      resample fallback, lock-free `rtrb` ring with a latency bound;
      summed pre-limiter + pre-stream-tap so the broadcast feed hears it)
- [x] Sidechain ducking: auto-lower music bed when mic is active
       (voice-activated peak envelope + threshold/depth/attack/release,
       live meter + `ducking` tag, duck ON/OFF)
- [x] Mic "bed" music under live breaks (voice sums over the ducked
      program bus and passes over a silent bed for talk breaks)
- [x] Settings UI: MIC ON/OFF toggle (auto-start on launch when enabled),
      input list, mic-level stepper, duck threshold/depth/attack/release
      steppers, live status (🎙 Live/⚠ error) + level meter

### 1.7 Reliability
- [x] Silence detector: `SilenceMonitor` meters the program bus pre-volume
      (−60 dBFS floor, 10 s default threshold — a muted monitor never
      alarms); the 200 ms UI tick auto-recovers dead air
      with a filler music track (rate-limited 1/min).
- [x] Background library health scan: `missing_files()` + startup scan, on-demand
      `✓ Health` button in the Library header, `!` prefixes on missing rows

### 1.8 Library depth
- [ ] Mass tag editor (multi-select batch edit)
- [ ] BPM detection/scan
- [x] Duplicate track detection (MVP: normalized title/artist + duration
      gate, "Duplicates only" flag view — human decides, nothing
      auto-deleted; content-hash confirmation open)
- [x] Scheduled folder auto-sync (watch folders + interval toggle on the
      Library screen; background walk queues new files through the normal
      import pump with progress; never deletes anything)

### 1.9 Reporting & Ops
- [x] Play logging on every play path (library tap, cart fire, scheduler run, silence filler)
- [x] Play-log reports: range presets (Today/7d/30d/All), jingle+ad exclusion, newest-100 list, CSV export
- [ ] XLS/PDF export (CSV done — spreadsheets/royalty bodies accept it; native XLS/PDF later)
- [x] Settings screen: output device picker (persisted, applies on restart, with
      unplugged-device fallback, live device highlighted), station name
      (persisted, dashboard header), section sub-pages with descriptions,
      engine display, crossfade + silence-alarm
      steppers (persisted, applied live), license section
- [x] `rfd` native file dialog for import (+ report export)

### 1.10 Quality gates
- [x] Unit tests for `library` and `playlist` (match scheduler/cart/mixer/license bar) — full suite green, no exceptions: kind classification + repair, loudness store/count, migrations, generator rules (incl. cross-pick `RuleHistory` + forecast + live-jingle cadence parity), manager CRUD, audio engine (loader generations, tap sharing, prefetch guard, handshake matrix, lock-poison survival), remove-track FK cascade across managers, settings (incl. example-file drift guard). (Counts intentionally unlisted — they rot every PR; CI is the source of truth.)
- [x] `cargo fmt` + `clippy` in CI (`-D warnings`, zero warnings) + `ci.yml` (fmt/clippy/test on push+PR)

## 2. Beyond Parity — Where CrabBoss Wins

RadioBOSS weaknesses: Windows-only, legacy Delphi UI, no ready remote UI,
closed codebase. Our stack (Rust + Iced, cross-platform, headless-ready)
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
