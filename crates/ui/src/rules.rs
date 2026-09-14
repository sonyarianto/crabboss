//! Pure state-transition rules extracted from `update`/`on_tick`.
//!
//! No Iced, no audio device, no clock: every rule takes plain inputs so
//! `cargo test` can pin the station's critical behavior — Auto-DJ
//! continuity without double-queueing, once-per-minute scheduler firing,
//! pending-source handoff, and editor day conversion. The live code in
//! `app.rs` calls these; the tests below lock them.

use std::collections::HashMap;
use std::path::PathBuf;

/// How far ahead of the playhead Auto-DJ prefetches the next deck.
pub(crate) const PREFETCH_HORIZON_SECS: f64 = 8.0;

// ---------------------------------------------------------------------------
// Auto-DJ continuity
// ---------------------------------------------------------------------------

/// Snapshot of the tick inputs the Auto-DJ tail of `on_tick` decides on.
pub(crate) struct AutodjTick {
    pub autodj: bool,
    pub auto_continue: bool,
    pub playing: bool,
    pub finished: bool,
    pub was_playing: bool,
    pub pos: f64,
    pub dur: Option<f64>,
    /// Installed-but-unplayed decks plus decode jobs still in flight.
    pub pending: usize,
    pub has_queue: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum AutodjAction {
    /// Nothing to do this tick (but `was_playing` still advances).
    Idle,
    /// Track ended while we were responsible: play the next pick now.
    PlayNow,
    /// Inside the prefetch horizon with room: queue one deck.
    Prefetch,
}

/// Decide the Auto-DJ tick action. Returns the action plus the next
/// `was_playing` (always the current `playing`: every path advances it).
pub(crate) fn autodj_tick_action(t: &AutodjTick) -> (AutodjAction, bool) {
    if !t.autodj || !t.auto_continue {
        return (AutodjAction::Idle, t.playing);
    }
    if t.finished && (t.playing || t.was_playing) {
        return (AutodjAction::PlayNow, t.playing);
    }
    if !t.playing {
        return (AutodjAction::Idle, t.playing);
    }
    if crabcore::audio::needs_prefetch(t.pos, t.dur, t.pending, t.has_queue, PREFETCH_HORIZON_SECS)
    {
        (AutodjAction::Prefetch, t.playing)
    } else {
        (AutodjAction::Idle, t.playing)
    }
}

// ---------------------------------------------------------------------------
// Once-per-minute fire dedupe (scheduler + ad blocks)
// ---------------------------------------------------------------------------

/// Dedupe key for a wall-clock minute. Takes the timestamp as input so
/// tests pin behavior without touching the real clock.
pub(crate) fn minute_key(at: &chrono::NaiveDateTime) -> String {
    at.format("%Y-%m-%d %H:%M").to_string()
}

/// Has `id` already fired inside `minute`?
pub(crate) fn fired_this_minute(fired: &HashMap<String, String>, id: &str, minute: &str) -> bool {
    fired.get(id).map(|m| m == minute).unwrap_or(false)
}

/// Claim the fire slot; `false` means "already fired this minute, skip".
/// The live loop pre-filters with [`fired_this_minute`] and claims again
/// while firing, so a double-fire needs two independent bugs.
pub(crate) fn claim_fire_slot(fired: &mut HashMap<String, String>, id: &str, minute: &str) -> bool {
    if fired_this_minute(fired, id, minute) {
        return false;
    }
    fired.insert(id.to_string(), minute.to_string());
    true
}

// ---------------------------------------------------------------------------
// Queued-deck promotion
// ---------------------------------------------------------------------------

/// The pending source (recorded at `queue()` time) follows the deck onto
/// the air so the `· via X` tag stays truthful; consumed either way, and
/// a deck nobody queued is Auto-DJ's by elimination.
pub(crate) fn take_pending_source(pending: &mut Option<String>) -> String {
    pending.take().unwrap_or_else(|| "Auto-DJ".into())
}

// ---------------------------------------------------------------------------
// Editor day conversion ([bool; 7] <-> bitmask, Mon = bit 0)
// ---------------------------------------------------------------------------

/// Checkbox week to bitmask for [`crabcore::scheduler::days_from_mask`].
pub(crate) fn days_to_mask(days: [bool; 7]) -> u8 {
    let mut mask = 0u8;
    for (i, on) in days.iter().enumerate() {
        if *on {
            mask |= 1 << i;
        }
    }
    mask
}

/// Bitmask back to checkboxes. Bit 7+ is not a weekday and never set by
/// [`days_to_mask`]; it is ignored here.
pub(crate) fn days_from_bits(mask: u8) -> [bool; 7] {
    let mut days = [false; 7];
    for (b, day) in days.iter_mut().enumerate() {
        *day = mask & (1 << b) != 0;
    }
    days
}

/// Resolve a queued pick to an engine path, mirroring the live guards:
/// a pick with no file on disk is skipped, never queued.
pub(crate) fn pick_engine_path(file_path: &str) -> Option<PathBuf> {
    let path = PathBuf::from(file_path);
    if !path.is_file() {
        return None;
    }
    Some(path)
}

// ---------------------------------------------------------------------------
// Folder auto-sync fire rule
// ---------------------------------------------------------------------------

/// A sync pass may start when the toggle is on, at least one folder is
/// watched, no import/scan/sync worker is running, and the interval
/// elapsed (`None` = never synced: fire promptly). Pure: the tick
/// passes the clock in as seconds so tests pin it without sleeping.
pub(crate) fn autosync_due(
    enabled: bool,
    has_folders: bool,
    busy: bool,
    elapsed_secs: Option<u64>,
    interval_secs: u64,
) -> bool {
    if !enabled || !has_folders || busy {
        return false;
    }
    match elapsed_secs {
        None => true,
        Some(e) => e >= interval_secs,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tick() -> AutodjTick {
        AutodjTick {
            autodj: true,
            auto_continue: true,
            playing: true,
            finished: false,
            was_playing: true,
            pos: 200.0,
            dur: Some(240.0),
            pending: 0,
            // The real engine always supports queueing; `false` models a
            // queue-less backend where prefetch must stay off.
            has_queue: true,
        }
    }

    #[test]
    fn autodj_stays_idle_when_off_or_paused() {
        let mut t = tick();
        t.autodj = false;
        assert_eq!(autodj_tick_action(&t), (AutodjAction::Idle, true));
        let mut t = tick();
        t.auto_continue = false;
        assert_eq!(autodj_tick_action(&t), (AutodjAction::Idle, true));
        // Paused mid-track: idle, and was_playing tracks the pause.
        let mut t = tick();
        t.playing = false;
        t.was_playing = true;
        assert_eq!(autodj_tick_action(&t), (AutodjAction::Idle, false));
    }

    #[test]
    fn eof_transition_plays_now_even_from_pause_edge() {
        // Finished while playing: the classic handoff.
        let mut t = tick();
        t.finished = true;
        assert_eq!(autodj_tick_action(&t), (AutodjAction::PlayNow, true));
        // Finished flag set but we had already paused: the deck ended on
        // our watch (was_playing), so still take over.
        let mut t = tick();
        t.finished = true;
        t.playing = false;
        t.was_playing = true;
        assert_eq!(autodj_tick_action(&t), (AutodjAction::PlayNow, false));
        // Finished while never playing: not our deck, stay idle.
        let mut t = tick();
        t.finished = true;
        t.playing = false;
        t.was_playing = false;
        assert_eq!(autodj_tick_action(&t), (AutodjAction::Idle, false));
    }

    #[test]
    fn prefetch_fires_once_then_holds_while_in_flight() {
        // Near the end, nothing installed or decoding: prefetch one deck.
        let mut t = tick();
        t.pos = 235.0;
        assert_eq!(autodj_tick_action(&t), (AutodjAction::Prefetch, true));
        // Same tick but a decode job in flight: hold. Without this the
        // 200 ms tick would stack the same pick behind the live deck
        // until the first decode lands.
        let mut t = tick();
        t.pos = 235.0;
        t.pending = 1;
        assert_eq!(autodj_tick_action(&t), (AutodjAction::Idle, true));
        // Same, but the pending unit is an installed deck rather than an
        // in-flight decode: still hold — one deck ahead is enough.
        // (`pending` counts both; the engine reports their sum.)
        let mut t = tick();
        t.pos = 235.0;
        t.pending = 2;
        assert_eq!(autodj_tick_action(&t), (AutodjAction::Idle, true));
        // Queue-less backend: never prefetch, even inside the horizon.
        let mut t = tick();
        t.pos = 235.0;
        t.has_queue = false;
        assert_eq!(autodj_tick_action(&t), (AutodjAction::Idle, true));
        // Mid-track, far from the horizon: idle.
        assert_eq!(autodj_tick_action(&tick()), (AutodjAction::Idle, true));
    }

    fn minute(h: u32, min: u32) -> chrono::NaiveDateTime {
        chrono::NaiveDate::from_ymd_opt(2026, 9, 14)
            .unwrap()
            .and_hms_opt(h, min, 0)
            .unwrap()
    }

    #[test]
    fn minute_key_and_fire_slots() {
        assert_eq!(minute_key(&minute(8, 5)), "2026-09-14 08:05");
        let mut fired = HashMap::new();
        assert!(!fired_this_minute(&fired, "e1", "2026-09-14 08:05"));
        // First claim wins…
        assert!(claim_fire_slot(&mut fired, "e1", "2026-09-14 08:05"));
        // …the 200 ms tick re-firing the same minute loses…
        assert!(!claim_fire_slot(&mut fired, "e1", "2026-09-14 08:05"));
        assert!(fired_this_minute(&fired, "e1", "2026-09-14 08:05"));
        // …a different event is unaffected…
        assert!(claim_fire_slot(&mut fired, "e2", "2026-09-14 08:05"));
        // …and the next minute re-arms.
        assert!(claim_fire_slot(&mut fired, "e1", "2026-09-14 08:06"));
    }

    #[test]
    fn pending_source_handoff() {
        let mut p = Some("Scheduler".to_string());
        assert_eq!(take_pending_source(&mut p), "Scheduler");
        assert!(p.is_none(), "consumed either way");
        let mut p: Option<String> = None;
        assert_eq!(take_pending_source(&mut p), "Auto-DJ");
    }

    #[test]
    fn day_masks_roundtrip() {
        assert_eq!(days_to_mask([true; 7]), 127);
        assert_eq!(days_to_mask([false; 7]), 0);
        assert_eq!(
            days_to_mask([true, false, true, false, false, false, false]),
            0b101
        );
        assert_eq!(days_from_bits(127), [true; 7]);
        assert_eq!(days_from_bits(0), [false; 7]);
        assert_eq!(
            days_from_bits(0b101),
            [true, false, true, false, false, false, false]
        );
        // Round-trips through the core string form too.
        for mask in [0u8, 1, 5, 64, 96, 126, 127] {
            let back = days_to_mask(days_from_bits(mask));
            assert_eq!(back, if mask == 0 { 0 } else { mask & 127 });
        }
        // …except all-false, which the core reads as Daily.
        assert_eq!(
            crabcore::scheduler::days_from_mask(days_to_mask([false; 7])),
            "Daily"
        );
    }

    #[test]
    fn missing_pick_file_is_skipped() {
        assert!(pick_engine_path("/no/such/file-xyz.mp3").is_none());
        let dir = std::env::temp_dir();
        let p = dir.join("crabboss-rules-pick.mp3");
        std::fs::write(&p, b"x").unwrap();
        assert_eq!(pick_engine_path(&p.to_string_lossy()), Some(p.clone()));
        std::fs::remove_file(&p).ok();
    }

    #[test]
    fn autosync_fires_when_due_and_idle() {
        // First pass with folders watched: fire promptly.
        assert!(autosync_due(true, true, false, None, 3600));
        // Interval elapsed: fire; a second early: hold.
        assert!(autosync_due(true, true, false, Some(3600), 3600));
        assert!(!autosync_due(true, true, false, Some(3599), 3600));
        // Guards: toggle off, no folders, or anything running.
        assert!(!autosync_due(false, true, false, None, 3600));
        assert!(!autosync_due(true, false, false, None, 3600));
        assert!(!autosync_due(true, true, true, None, 3600));
        assert!(!autosync_due(true, true, true, Some(99999), 3600));
    }
}
