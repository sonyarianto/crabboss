//! Voice tracking v1: record the mic to WAV + fire takes to air.
//!
//! Layout mirrors `cart/`:
//!
//! ```text
//! voice/
//!   mod.rs      # this file: take naming, tap, record budget
//!   wav.rs      # dependency-free PCM16 WAV writer
//!   manager.rs  # SQLite voice-track store (never library tracks)
//! ```
//!
//! Voice tracks are deliberately NOT library tracks: no music reports,
//! no Auto-DJ rotations, no loudness scans. The tick labels a promoted
//! voice deck via [`VoiceManager::find_by_path`] — the one place voice
//! meets the program display.

pub mod manager;
pub mod wav;

pub use manager::{VoiceManager, VoiceTrack};

/// Hard stop for one take: takes are talk segments, not shows. The UI
/// auto-stops at this budget with a status note.
pub const VOICE_MAX_RECORD_SECS: f64 = 600.0;

/// Suggested take filename (`voice-20260920-080000.wav`). The stem
/// doubles as the display label (stream metadata falls back to the
/// file name), so it carries a timestamp, not a counter.
pub fn suggest_filename(now: chrono::DateTime<chrono::Local>) -> String {
    format!("voice-{}.wav", now.format("%Y%m%d-%H%M%S"))
}

/// Suggested display name for a take.
pub fn suggest_name(now: chrono::DateTime<chrono::Local>) -> String {
    format!("Voice {}", now.format("%Y-%m-%d %H:%M"))
}

/// Producer side of the record tap: the output callback pushes the
/// live mic frame (post-ring, device rate, stereo f32) while a take is
/// rolling. Never blocks; on overflow the newest samples are dropped
/// (a gap in the take beats a glitch on air).
#[derive(Clone)]
pub struct VoiceTap {
    pub(crate) producer: std::sync::Arc<std::sync::Mutex<rtrb::Producer<f32>>>,
}

impl VoiceTap {
    /// Push staged interleaved stereo mic frames from the audio
    /// callback (callers batch per callback like the stream tap).
    /// Never blocks; fits what room exists (even count, pairs intact)
    /// and drops the rest (a gap in the take beats a glitch on air).
    pub fn push_slice(&self, interleaved: &[f32]) {
        if let Ok(mut p) = self.producer.try_lock() {
            let n = interleaved.len().min(p.slots() & !1);
            let _ = p.push_entire_slice(&interleaved[..n]);
        }
    }
}

/// A finished take handed back by `voice_record_stop`.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct VoiceTake {
    pub duration_secs: f64,
    pub sample_rate: u32,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn suggested_names_carry_timestamps() {
        let now = chrono::Local::now();
        let file = suggest_filename(now);
        assert!(file.starts_with("voice-"), "{file}");
        assert!(file.ends_with(".wav"), "{file}");
        let name = suggest_name(now);
        assert!(name.starts_with("Voice "), "{name}");
    }

    #[test]
    fn record_budget_is_ten_minutes() {
        assert_eq!(VOICE_MAX_RECORD_SECS, 600.0);
    }

    #[test]
    fn tap_stages_pairs_and_drops_overflow_without_blocking() {
        let (prod, mut cons) = rtrb::RingBuffer::new(8);
        let tap = VoiceTap {
            producer: std::sync::Arc::new(std::sync::Mutex::new(prod)),
        };
        tap.push_slice(&[0.1, 0.2, 0.3, 0.4]);
        assert_eq!(cons.slots(), 4);
        // Only 4 slots free: 6 more samples fit 4, pairs intact.
        tap.push_slice(&[0.5, 0.6, 0.7, 0.8, 0.9, 1.0]);
        assert_eq!(cons.slots(), 8);
        let mut got = Vec::new();
        while let Ok(s) = cons.pop() {
            got.push(s);
        }
        assert_eq!(got.len() % 2, 0, "pairs must stay intact");
        assert!((got[0] - 0.1).abs() < 1e-6);
    }
}
