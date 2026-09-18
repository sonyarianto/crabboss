# User Manual

The live workflow in one line: **import → cue on headphones → fire on air.**
The Library never touches the broadcast; the Playout desk owns it.

## Library — Manage & Preview

- **Import** audio (MP3, FLAC, WAV, OGG, AAC, M4A, Opus, AIFF, WavPack, Musepack):
  pick files in the native dialog, watch per-file progress, then an
  automatic loudness scan analyzes every track.
- **Search, filter by kind** (Music/Jingles/Ads), flag **missing files**
  and **possible duplicates**, point **watch folders** at drop
  directories for timed auto-sync.
- Each row has a **Cue** button: it previews that track on your
  headphones only. Clicking a title just **selects** it (blue) — selection
  is what the Playout desk fires.

## Home — Rotations & Saved Playlists

- **Rotation Generator**: one row per daypart (Morning/Midday/Evening/Night)
  with an hour stepper and a track-count stepper. `Generate` fires one
  row, `Generate all dayparts` fires all four. Each fire persists a
  timestamped playlist (`Daypart HH:MM`).
- **Saved Playlists**: every rotation — manual or generated — lands here.
  `Create` makes an empty named playlist. `Edit`/`Close` expands it in
  stored order, `Queue to Air` fires it, `Delete` removes it with its items.
- **Manual builder**: pick a track in Library, go back to Home, press
  `Add here` inside the expanded playlist. Reorder with `↑`/`↓`, drop a
  row with `Remove`. Missing files stay listed as `(! missing)` and the
  header counts them — firing skips them with a count instead of failing.
- **Queue to Air** semantics: the first ready track plays now (crossfades
  over live audio, starts from silence when idle), the rest queue behind
  it in stored order. If an Auto-DJ/scheduler deck is still pending, the
  status says `queued deck plays first` instead of surprising you.

## Playout Desk — Go On Air

- The broadcast strip shows cover art (or a `No cover` placeholder),
  transport (`Prev / Play / Stop / Next`), progress, Auto-DJ toggle,
  **Coming Up** forecast, and monitor volume.
- **On Air** fires the selected track to program now, crossfading over
  whatever is live. Continuity follows the Auto-DJ toggle: on, the music
  keeps flowing after; off, it stops at the end — proper live-assist.
- Screens share one footer: `ON AIR: title` or `OFF AIR`.

## Scheduler — Autopilot

- Master **Enabled** toggle plus per-event Enable/Disable. `Run` fires any
  event immediately for testing; the 200 ms tick fires due events live.
- Actions (`<`/`>` in the editor):
  - `play` — a file path straight to program.
  - `load` — a **saved playlist display name** (exact, case-sensitive, as
    shown on Home), fired in stored order like Queue to Air. Unknown names
    fall back to the single-file `play` path.
  - `generate` — builds a 10-track rotation for the current hour/weekday
    and persists it as `<target> HH:MM`.
  - `queue` — a file path inserted after the current track.
  - `command` — logged no-op in this build.
- Editor fields: Name, `HH:MM`, action, Target (file / playlist / preset),
  `Valid until YYYY-MM-DD` (empty = runs forever), weekdays Mon–Sun.
- Expiry is visible: rows badge `expiring` (≤7 days) / `last day` /
  `expired`, with a warnings banner for soon-to-expire and expired events.

## Cart Wall — Jingles & Stingers

- 8 pads with per-pad progress, playing highlight, and kind badges.
  `Play` fires instantly (logged), `Del` clears the pad.
- Assign flow: select a track in Media, turn **Assign ON**, then
  **Place here** on a pad. Hotkeys `1`–`8` fire pads 1–8.

## Ads — Dated Blocks

- Blocks chain **intro → spot → outro** with a validity window
  (start/end `YYYY-MM-DD`), `HH:MM` play time, and weekdays.
- Intro/outro paths are optional; the spot is required. Enable/Disable
  per block, `Run` to test-fire now, `Edit`/`Del` to manage.

## Reports — Proof of Play

- Range presets Today / Last 7 days / Last 30 days / All time, plus a
  `Recently played (24h)` strip. The main list excludes jingles and ads;
  cue previews never enter the log.
- `Export CSV/XLSX` opens a native save dialog and writes the current range.

## Cue (PFL) — Headphones

- Pick the headphone device in **Settings → Audio Device** (e.g. laptop
  output while program plays on monitor speakers). Switching is live —
  no restart.
- A same-device warning appears if cue equals the program device:
  workable for testing, but the room hears your preview.
- Cue volume is independent; cueing never touches the program bus, the
  stream, or play reports. Stops and track ends fade ~30 ms — no clicks.

## Streaming

- **Settings → Streaming**: Icecast host/port/mount/credentials, TLS,
  format (**MP3**/**Opus**) and bitrate. Opus snaps to its own ladder
  (24–160 kbps) and needs far fewer bits for the same quality.
- The status names the live encoder (`🔴 Live — Opus 128 kbps`). Format,
  bitrate, and connection edits apply on **restart** — while live, a red
  **Apply & restart** button appears for pending changes. Every restart
  rebuffers listeners, so batch your edits.

## Sound

- **Settings → Equalizer**: 12-band graphic-EQ fader strip. Drag applies
  live; values persist on release. The header tells `Flat — bypassed`
  vs `Custom curve`; the limiter ceiling sits below.
- Loudness normalization (BS.1770 toward an adjustable LUFS target) is
  applied per track at decode time from the library scan.
