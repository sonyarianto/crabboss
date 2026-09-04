//! Dead-air monitor: counts consecutive (near-)silent output frames.
//!
//! The engine feeds every rendered frame here; the UI polls [`SilenceMonitor::alarm`]
//! and recovers (skip / filler) when program output stays quiet too long.
//! Pure logic, no I/O — fully unit-testable.

/// Peak level below this counts as silence (≈ −60 dBFS).
pub const SILENCE_FLOOR: f32 = 0.001;

#[derive(Debug)]
pub struct SilenceMonitor {
    silent_frames: u64,
    threshold_secs: f32,
    rate: u32,
}

impl SilenceMonitor {
    pub fn new(rate: u32, threshold_secs: f32) -> Self {
        Self {
            silent_frames: 0,
            threshold_secs,
            rate,
        }
    }

    pub fn set_threshold_secs(&mut self, secs: f32) {
        self.threshold_secs = secs.clamp(1.0, 120.0);
    }

    /// Feed one rendered frame. `playing` = transport is Playing;
    /// anything else resets the counter (paused silence is intentional).
    pub fn push_frame(&mut self, playing: bool, l: f32, r: f32) {
        if playing && l.abs() < SILENCE_FLOOR && r.abs() < SILENCE_FLOOR {
            self.silent_frames += 1;
        } else {
            self.silent_frames = 0;
        }
    }

    pub fn reset(&mut self) {
        self.silent_frames = 0;
    }

    pub fn silent_secs(&self) -> f32 {
        self.silent_frames as f32 / self.rate.max(1) as f32
    }

    pub fn alarm(&self) -> bool {
        self.rate > 0 && self.silent_secs() >= self.threshold_secs
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn trips_after_threshold() {
        let mut m = SilenceMonitor::new(10, 1.0); // 10 fps, 1 s threshold
        for _ in 0..9 {
            m.push_frame(true, 0.0, 0.0);
        }
        assert!(!m.alarm());
        m.push_frame(true, 0.0, 0.0);
        assert!(m.alarm());
        assert!((m.silent_secs() - 1.0).abs() < 1e-6);
    }

    #[test]
    fn audio_and_pause_reset() {
        let mut m = SilenceMonitor::new(10, 1.0);
        for _ in 0..8 {
            m.push_frame(true, 0.0, 0.0);
        }
        m.push_frame(true, 0.5, 0.5);
        assert!(!m.alarm());
        assert_eq!(m.silent_secs(), 0.0);
        for _ in 0..20 {
            m.push_frame(true, 0.0, 0.0);
        }
        assert!(m.alarm());
        m.push_frame(false, 0.0, 0.0);
        assert!(!m.alarm(), "paused silence is intentional");
    }
}
