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
- [x] Router: Home / Playout / Media Manager screens
- [x] License key activation (offline, `CB-XXXX-XXXX-XXXX`)
- [x] Library: `scan_directory()` via `walkdir`
- [ ] Crossfader + gapless
- [ ] 12-band EQ + limiter
- [ ] Playlist auto-generator with rotation rules
- [ ] Ad scheduler + cart wall
- [ ] Icecast/Shoutcast output
- [ ] Mic/line-in input with ducking
- [ ] Report generator (play logs → XLS/PDF)
- [ ] File dialog (`rfd`), progress timer in UI
- [ ] Settings screen (audio device, streaming, license details)
- [ ] Quality: `cargo fmt/clippy`, unit tests (`library`, `playlist`), CI
