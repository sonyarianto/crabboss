//! Mic / line-in input with sidechain ducking (ROADMAP §1.6).
//!
//! The cpal input stream writes stereo-interleaved f32 (resampled to the
//! device rate) into a lock-free [`rtrb`] ring; the output callback drains
//! it, eases the music bed down while the DJ talks ([`Ducker`]), and sums
//! the voice into the program bus ahead of the limiter + stream tap — so
//! the broadcast feed hears the mic too.

use serde::{Deserialize, Serialize};

/// Ring capacity in f32 samples (2^17 ≈ 1.4 s of stereo @ 48 kHz).
/// Small on purpose: a deep ring would add audible monitoring latency,
/// and unlike the stream tap there is no reconnect to ride out — drops
/// beat delay. (Power of two, per `rtrb` convention.)
pub const MIC_RING_SAMPLES: usize = 1 << 17;

/// Persisted mic preferences (lives in `AppSettings::mic`).
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct MicConfig {
    /// Master switch: the input stream runs while true.
    pub enabled: bool,
    /// Input device name (`None` = system default).
    pub device: Option<String>,
    /// Mic gain 0.0..1.5, applied before the mix + voice detector.
    pub level: f32,
    /// Sidechain ducking: lower the music bed while the mic is active.
    pub duck_enabled: bool,
    /// Voice-activity threshold in dBFS (envelope above this = talking).
    pub duck_threshold_db: f32,
    /// How far the music bed drops while talking, in dB.
    pub duck_depth_db: f32,
    /// Duck fade-down time in ms.
    pub attack_ms: f32,
    /// Duck fade-up time in ms.
    pub release_ms: f32,
}

impl Default for MicConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            device: None,
            level: 1.0,
            duck_enabled: true,
            duck_threshold_db: -30.0,
            duck_depth_db: 12.0,
            attack_ms: 10.0,
            release_ms: 400.0,
        }
    }
}

impl MicConfig {
    /// Clamp user input to sane ranges.
    pub fn sanitized(mut self) -> Self {
        self.level = self.level.clamp(0.0, 1.5);
        self.duck_threshold_db = self.duck_threshold_db.clamp(-60.0, 0.0);
        self.duck_depth_db = self.duck_depth_db.clamp(0.0, 24.0);
        self.attack_ms = self.attack_ms.clamp(1.0, 500.0);
        self.release_ms = self.release_ms.clamp(10.0, 3000.0);
        if self.duck_depth_db == 0.0 {
            self.duck_enabled = false;
        }
        self
    }
}

/// Live state of the mic pipeline (surfaced in Settings).
#[derive(Debug, Clone, PartialEq)]
pub enum MicState {
    /// Master switch off / input never started.
    Off,
    /// Input stream running and feeding the program bus.
    Live,
    /// Last start (re)open failed; `String` carries the message.
    Error(String),
}

impl MicState {
    /// Short one-line status for the Settings UI.
    pub fn label(&self) -> String {
        match self {
            MicState::Off => "⏸ Off".into(),
            MicState::Live => "🎙 Live".into(),
            MicState::Error(e) => format!("⚠ {e}"),
        }
    }

    pub fn is_live(&self) -> bool {
        matches!(self, MicState::Live)
    }
}

/// Voice-activated music-bed ducker.
///
/// A peak envelope follower tracks the post-gain mic signal; while the
/// envelope sits above the threshold the music gain eases toward
/// −depth dB (attack = fade-down, release = fade-up). All smoothing is
/// one-pole per-sample, so it is cheap enough for the audio callback.
#[derive(Debug, Clone)]
pub struct Ducker {
    rate: u32,
    enabled: bool,
    threshold_lin: f32,
    /// Linear music gain at full duck (10^(-depth/20)).
    depth_gain: f32,
    attack_coef: f32,
    release_coef: f32,
    /// Mic envelope (linear amplitude).
    env: f32,
    /// Current music gain (linear, ≤ 1).
    gain: f32,
}

impl Ducker {
    pub fn new(rate: u32) -> Self {
        let mut d = Self {
            rate: rate.max(1),
            enabled: true,
            threshold_lin: 0.0,
            depth_gain: 0.0,
            attack_coef: 0.0,
            release_coef: 0.0,
            env: 0.0,
            gain: 1.0,
        };
        d.configure(-30.0, 12.0, 10.0, 400.0);
        d
    }

    /// One-pole coefficient for a time constant in ms at the device rate.
    fn coef(rate: u32, ms: f32) -> f32 {
        let n = (ms.max(0.5) / 1000.0 * rate as f32).max(1.0);
        (-1.0 / n).exp()
    }

    pub fn set_rate(&mut self, rate: u32) {
        // Preserve ms timings across the rate change by re-deriving
        // coefficients from the stored dB config is overkill — callers
        // re-apply `configure` after `set_rate` when timings matter.
        // Here we keep it simple: keep coefs, just track the rate.
        self.rate = rate.max(1);
    }

    pub fn rate(&self) -> u32 {
        self.rate
    }

    pub fn set_enabled(&mut self, on: bool) {
        self.enabled = on;
        if !on {
            self.gain = 1.0;
        }
    }

    pub fn configure(&mut self, threshold_db: f32, depth_db: f32, attack_ms: f32, release_ms: f32) {
        self.threshold_lin = 10f32.powf(threshold_db.clamp(-60.0, 0.0) / 20.0);
        self.depth_gain = 10f32.powf(-depth_db.clamp(0.0, 24.0) / 20.0);
        self.attack_coef = Self::coef(self.rate, attack_ms.clamp(1.0, 500.0));
        self.release_coef = Self::coef(self.rate, release_ms.clamp(10.0, 3000.0));
    }

    /// Feed one post-gain mic frame; returns the music gain (linear ≤ 1).
    pub fn process(&mut self, l: f32, r: f32) -> f32 {
        if !self.enabled {
            return 1.0;
        }
        let peak = l.abs().max(r.abs());
        // Envelope: instant attack so onsets duck immediately, release-time
        // decay so the bed doesn't pump between syllables.
        if peak > self.env {
            self.env = peak;
        } else {
            self.env = peak + (self.env - peak) * self.release_coef;
        }
        let target = if self.env >= self.threshold_lin {
            self.depth_gain
        } else {
            1.0
        };
        let coef = if target < self.gain {
            self.attack_coef
        } else {
            self.release_coef
        };
        self.gain = target + (self.gain - target) * coef;
        self.gain
    }

    /// Current music gain reduction in dB (≤ 0; UI ducking indicator).
    pub fn reduction_db(&self) -> f32 {
        20.0 * self.gain.max(1e-6).log10()
    }

    /// Mic envelope in dBFS (floored; UI level meter).
    pub fn input_level_db(&self) -> f32 {
        if self.env <= 1e-6 {
            -99.0
        } else {
            20.0 * self.env.log10()
        }
    }

    /// True while the bed is audibly ducked (> 0.5 dB reduction).
    pub fn ducking(&self) -> bool {
        self.enabled && self.gain < 0.944
    }

    pub fn reset(&mut self) {
        self.env = 0.0;
        self.gain = 1.0;
    }
}

impl Default for Ducker {
    fn default() -> Self {
        Self::new(48_000)
    }
}

/// Linear interpolating resampler for the mic path (input rate → device
/// rate). Voice-grade and ~zero latency — unlike the sinc resampler on
/// the file-decode path, this runs sample-by-sample inside the realtime
/// input callback, so it must be stateful and allocation-free per call.
#[derive(Debug, Clone)]
pub struct MicResampler {
    /// Output frames per input frame (`to / from`).
    out_per_in: f64,
    /// Accumulated output credit (fractional).
    credit: f64,
    prev_l: f32,
    prev_r: f32,
    has_prev: bool,
}

impl MicResampler {
    pub fn new(from: u32, to: u32) -> Self {
        Self {
            out_per_in: to.max(1) as f64 / from.max(1) as f64,
            credit: 0.0,
            prev_l: 0.0,
            prev_r: 0.0,
            has_prev: false,
        }
    }

    /// Feed one input frame; appends 0+ device-rate frames to `out`
    /// (caller reuses the buffer across calls). Rate-matched streams
    /// pass through with zero added latency.
    pub fn feed(&mut self, l: f32, r: f32, out: &mut Vec<(f32, f32)>) {
        if !self.has_prev {
            self.prev_l = l;
            self.prev_r = r;
            self.has_prev = true;
        }
        self.credit += self.out_per_in;
        while self.credit >= 1.0 {
            self.credit -= 1.0;
            // `t` sweeps prev → cur across the emitted frames.
            let t = (1.0 - self.credit) as f32;
            out.push((
                self.prev_l + (l - self.prev_l) * t,
                self.prev_r + (r - self.prev_r) * t,
            ));
        }
        self.prev_l = l;
        self.prev_r = r;
    }

    pub fn reset(&mut self) {
        self.credit = 0.0;
        self.has_prev = false;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn config_sanitizes_ranges() {
        let c = MicConfig {
            level: 9.0,
            duck_threshold_db: 5.0,
            duck_depth_db: 99.0,
            attack_ms: 0.0,
            release_ms: 99999.0,
            ..Default::default()
        }
        .sanitized();
        assert_eq!(c.level, 1.5);
        assert_eq!(c.duck_threshold_db, 0.0);
        assert_eq!(c.duck_depth_db, 24.0);
        assert_eq!(c.attack_ms, 1.0);
        assert_eq!(c.release_ms, 3000.0);
    }

    #[test]
    fn config_roundtrips_through_json() {
        let c = MicConfig {
            enabled: true,
            device: Some("USB Mic".into()),
            level: 0.8,
            duck_depth_db: 9.0,
            ..Default::default()
        };
        let json = serde_json::to_string(&c).unwrap();
        let back: MicConfig = serde_json::from_str(&json).unwrap();
        assert!(back.enabled);
        assert_eq!(back.device.as_deref(), Some("USB Mic"));
        assert!((back.level - 0.8).abs() < 1e-6);
        // Missing fields fall back to defaults (serde(default)).
        let partial: MicConfig = serde_json::from_str(r#"{"enabled":true}"#).unwrap();
        assert!(partial.enabled);
        assert!(partial.duck_enabled);
    }

    #[test]
    fn silence_keeps_unity_gain() {
        let mut d = Ducker::new(48_000);
        for _ in 0..48_000 {
            let g = d.process(0.0, 0.0);
            assert!((g - 1.0).abs() < 1e-6);
        }
        assert!(!d.ducking());
        assert_eq!(d.reduction_db(), 0.0);
    }

    #[test]
    fn loud_mic_ducks_toward_configured_depth() {
        let mut d = Ducker::new(48_000);
        d.configure(-30.0, 12.0, 10.0, 400.0);
        // Full-scale voice for 2 s: gain must converge near −12 dB (0.251).
        let mut g = 1.0;
        for _ in 0..96_000 {
            g = d.process(0.9, 0.9);
        }
        assert!((g - 0.251).abs() < 0.01, "gain {g}");
        assert!(d.ducking());
        assert!(d.reduction_db() < -11.0);
        assert!(d.input_level_db() > -2.0);
    }

    #[test]
    fn quiet_mic_below_threshold_does_not_duck() {
        let mut d = Ducker::new(48_000);
        d.configure(-30.0, 12.0, 10.0, 400.0);
        // −40 dBFS murmur stays under the −30 dB threshold.
        for _ in 0..48_000 {
            let g = d.process(0.01, 0.01);
            assert!((g - 1.0).abs() < 1e-6);
        }
        assert!(!d.ducking());
    }

    #[test]
    fn release_returns_to_unity() {
        let mut d = Ducker::new(48_000);
        d.configure(-30.0, 12.0, 10.0, 50.0);
        for _ in 0..48_000 {
            d.process(0.9, 0.9);
        }
        assert!(d.ducking());
        // 1 s of silence with a 50 ms release: fully recovered.
        for _ in 0..48_000 {
            d.process(0.0, 0.0);
        }
        let g = d.process(0.0, 0.0);
        assert!((g - 1.0).abs() < 0.001, "gain {g}");
        assert!(!d.ducking());
    }

    #[test]
    fn disabled_never_ducks() {
        let mut d = Ducker::new(48_000);
        d.set_enabled(false);
        for _ in 0..1000 {
            assert_eq!(d.process(1.0, 1.0), 1.0);
        }
        assert!(!d.ducking());
    }

    #[test]
    fn resampler_passthrough_at_matching_rates() {
        let mut r = MicResampler::new(48_000, 48_000);
        let mut out = Vec::new();
        for i in 0..100 {
            r.feed(i as f32, -(i as f32), &mut out);
        }
        assert_eq!(out.len(), 100);
        assert_eq!(out[50], (50.0, -50.0));
    }

    #[test]
    fn resampler_upsample_produces_expected_frame_count() {
        let mut r = MicResampler::new(24_000, 48_000);
        let mut out = Vec::new();
        for _ in 0..240 {
            r.feed(1.0, 1.0, &mut out);
        }
        assert_eq!(out.len(), 480);
    }

    #[test]
    fn resampler_downsample_produces_expected_frame_count() {
        let mut r = MicResampler::new(48_000, 24_000);
        let mut out = Vec::new();
        for _ in 0..480 {
            r.feed(1.0, 1.0, &mut out);
        }
        assert_eq!(out.len(), 240);
    }

    #[test]
    fn mic_state_labels() {
        assert_eq!(MicState::Off.label(), "⏸ Off");
        assert_eq!(MicState::Live.label(), "🎙 Live");
        assert!(MicState::Error("boom".into()).label().contains("boom"));
        assert!(MicState::Live.is_live());
        assert!(!MicState::Off.is_live());
    }
}
