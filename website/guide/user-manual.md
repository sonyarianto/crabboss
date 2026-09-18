# User Manual

The live workflow in one line: **import → cue on headphones → fire on air.**
The Library never touches the broadcast; the Playout desk owns it.

## Library — Manage & Preview

- **Import** audio (MP3, FLAC, WAV, OGG, AAC, M4A, Opus, AIFF, WavPack):
  pick files in the native dialog, watch per-file progress, then an
  automatic loudness scan analyzes every track.
- **Search, filter by kind** (Music/Jingles/Ads), flag **missing files**
  and **possible duplicates**, point **watch folders** at drop
  directories for timed auto-sync.
- Each row has a **Cue** button: it previews that track on your
  headphones only. Clicking a title just **selects** it (blue) — selection
  is what the Playout desk fires.

## Playout Desk — Go On Air

- The broadcast strip shows cover art (or a `No cover` placeholder),
  transport (`Prev / Play / Stop / Next`), progress, Auto-DJ toggle,
  **Coming Up** forecast, and monitor volume.
- **On Air** fires the selected track to program now, crossfading over
  whatever is live. Continuity follows the Auto-DJ toggle: on, the music
  keeps flowing after; off, it stops at the end — proper live-assist.
- Screens share one footer: `ON AIR: title` or `OFF AIR`.

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
