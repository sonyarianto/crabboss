//! Sample-accurate mixer: EQ → gain → crossfade → limiter.
//!
//! Pure DSP, no I/O. `CpalEngine` pulls frames through this;
//! unit-testable without an audio device.
//!
//! Signal order per frame: 12-band biquad EQ (per channel state) →
//! A/B crossfade blend → master gain → lookahead limiter.

// ---------------------------------------------------------------- EQ ----

/// Number of EQ bands (1 kHz-ish spaced octaves from 31 Hz to 16 kHz).
pub const EQ_BAND_COUNT: usize = 12;

/// Center frequencies of the 12 bands (Hz): 31, 62, 125, 250, 500, 1k,
/// 2k, 4k, 8k, 16k, plus two half-octave voice presence bands.
pub const EQ_CENTER_HZ: [f32; EQ_BAND_COUNT] = [
    31.0, 44.0, 62.0, 125.0, 250.0, 500.0, 1_000.0, 2_000.0, 3_150.0, 4_000.0, 8_000.0, 16_000.0,
];

/// One peaking EQ band: gain in dB (±12), Q ~1.0. Frequencies are fixed.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct EqBand {
    /// Boost/cut in decibels, clamped to ±12.0.
    pub gain_db: f32,
    /// Resonance; 1.0 ≈ musical, higher = narrower notch.
    pub q: f32,
}

impl Default for EqBand {
    fn default() -> Self {
        Self {
            gain_db: 0.0,
            q: 1.0,
        }
    }
}

impl EqBand {
    pub fn is_neutral(&self) -> bool {
        self.gain_db.abs() < 0.05
    }
}

/// RBJ "Cookbook" peaking biquad (audio EQ design formulas, direct form 1).
#[derive(Debug, Clone, Copy)]
struct Biquad {
    b0: f32,
    b1: f32,
    b2: f32,
    a1: f32,
    a2: f32,
    // Direct-form-1 state (x/y history).
    x1: f32,
    x2: f32,
    y1: f32,
    y2: f32,
}

impl Biquad {
    fn neutral() -> Self {
        Self {
            b0: 1.0,
            b1: 0.0,
            b2: 0.0,
            a1: 0.0,
            a2: 0.0,
            x1: 0.0,
            x2: 0.0,
            y1: 0.0,
            y2: 0.0,
        }
    }

    /// Peaking EQ at `center` Hz for the given sample rate
    /// (RBJ audio-EQ-cookbook, direct form 1, a0-normalized).
    fn peaking(center: f32, rate: f32, gain_db: f32, q: f32) -> Self {
        if center <= 0.0 || center >= rate * 0.5 || q <= 0.0 {
            return Self::neutral();
        }
        let a = 10f32.powf(gain_db / 40.0); // amplitude ratio
        let w0 = std::f32::consts::TAU * center / rate;
        let cw = w0.cos();
        let alpha = w0.sin() / (2.0 * q);

        let b0 = 1.0 + alpha * a;
        let b1 = -2.0 * cw;
        let b2 = 1.0 - alpha * a;
        let a0 = 1.0 + alpha / a;
        let a1 = -2.0 * cw;
        let a2 = 1.0 - alpha / a;

        Self {
            b0: b0 / a0,
            b1: b1 / a0,
            b2: b2 / a0,
            a1: a1 / a0,
            a2: a2 / a0,
            x1: 0.0,
            x2: 0.0,
            y1: 0.0,
            y2: 0.0,
        }
    }

    /// Process one sample with direct-form-1 state.
    fn tick(&mut self, x: f32) -> f32 {
        let y = self.b0 * x + self.b1 * self.x1 + self.b2 * self.x2
            - self.a1 * self.y1
            - self.a2 * self.y2;
        self.x2 = self.x1;
        self.x1 = x;
        self.y2 = self.y1;
        self.y1 = y;
        y
    }
}

impl Default for Biquad {
    fn default() -> Self {
        Self::neutral()
    }
}

/// Per-channel 12-band EQ processor: biquad chain + band gains.
#[derive(Debug, Clone)]
pub struct EqChain {
    bands: [EqBand; EQ_BAND_COUNT],
    /// Coefficient cache matching `bands` (rebuilt on set + rate change).
    coeffs: Vec<Biquad>,
    rate: f32,
    enabled: bool,
}

impl Default for EqChain {
    fn default() -> Self {
        Self {
            bands: [EqBand::default(); EQ_BAND_COUNT],
            coeffs: (0..EQ_BAND_COUNT).map(|_| Biquad::neutral()).collect(),
            rate: 48_000.0,
            enabled: false,
        }
    }
}

impl EqChain {
    pub fn new(rate: u32) -> Self {
        Self {
            rate: rate as f32,
            ..Self::default()
        }
    }

    /// True when the chain is bypassed (disabled or every band neutral).
    pub fn is_bypassed(&self) -> bool {
        !self.enabled || self.bands.iter().all(EqBand::is_neutral)
    }

    pub fn enabled(&self) -> bool {
        self.enabled
    }

    pub fn set_enabled(&mut self, on: bool) {
        self.enabled = on;
    }

    pub fn bands(&self) -> &[EqBand; EQ_BAND_COUNT] {
        &self.bands
    }

    /// Set one band's gain (dB, clamped ±12) and rebuild its coefficients.
    pub fn set_band(&mut self, idx: usize, gain_db: f32) {
        if idx >= EQ_BAND_COUNT {
            return;
        }
        let q = self.bands[idx].q;
        self.bands[idx].gain_db = gain_db.clamp(-12.0, 12.0);
        self.coeffs[idx] =
            Biquad::peaking(EQ_CENTER_HZ[idx], self.rate, self.bands[idx].gain_db, q);
    }

    /// Zero every band (flat response) and rebuild.
    pub fn reset(&mut self) {
        self.bands = [EqBand::default(); EQ_BAND_COUNT];
        self.coeffs = vec![Biquad::neutral(); EQ_BAND_COUNT];
    }

    /// Recompute all coefficients for a new device rate (clears filter state).
    pub fn set_rate(&mut self, rate: u32) {
        if self.rate == rate as f32 {
            return;
        }
        self.rate = rate as f32;
        for (idx, band) in self.bands.iter().enumerate() {
            self.coeffs[idx] = if band.is_neutral() {
                Biquad::neutral()
            } else {
                Biquad::peaking(EQ_CENTER_HZ[idx], self.rate, band.gain_db, band.q)
            };
        }
    }

    /// Process one sample through the chain. Cheap when bypassed.
    fn tick(&mut self, x: f32) -> f32 {
        if self.is_bypassed() {
            return x;
        }
        let mut y = x;
        for c in self.coeffs.iter_mut() {
            y = c.tick(y);
        }
        y
    }
}

// ----------------------------------------------------------- limiter ----

/// Transparent peak limiter: brickwall per-frame attack (a frame can never
/// exceed the ceiling) with a metered release back to unity, so sustained
/// peaks get steady gain reduction instead of hard clipping.
#[derive(Debug)]
pub struct Limiter {
    ceiling: f32,
    /// Gain-reduction recovery per second (e.g. 5.0 → 200 ms back to unity).
    release_per_sec: f32,
    /// 1 / device sample rate (per-frame release step scaling).
    inv_rate: f32,
    /// Current applied gain 0..1 (smoothed).
    gain: f32,
}

impl Default for Limiter {
    fn default() -> Self {
        Self::new(0.99, 48_000)
    }
}

impl Limiter {
    /// `ceiling`: max output amplitude (linear); `rate`: device sample rate.
    pub fn new(ceiling: f32, rate: u32) -> Self {
        Self {
            ceiling: ceiling.clamp(0.1, 1.0),
            release_per_sec: 5.0,
            inv_rate: 1.0 / rate.max(8_000) as f32,
            gain: 1.0,
        }
    }

    pub fn ceiling(&self) -> f32 {
        self.ceiling
    }

    /// Set a new ceiling (linear 0.1..1.0).
    pub fn set_ceiling(&mut self, c: f32) {
        self.ceiling = c.clamp(0.1, 1.0);
    }

    /// Instantaneous gain reduction in dB (for UI metering).
    pub fn reduction_db(&self) -> f32 {
        20.0 * self.gain.max(f32::EPSILON).log10()
    }

    /// Process one stereo frame; returns limited (l, r).
    pub fn process(&mut self, l: f32, r: f32) -> (f32, f32) {
        // Peak of the incoming frame drives the target gain.
        let peak = l.abs().max(r.abs()).max(f32::EPSILON);
        let target = if peak > self.ceiling {
            self.ceiling / peak
        } else {
            1.0
        };

        // Attack is instant (brickwall), release is metered.
        if target < self.gain {
            self.gain = target;
        } else {
            // Frame-based smoothing: release_per_sec per second.
            let step = self.release_per_sec * self.inv_rate;
            self.gain = (self.gain + step).min(1.0);
        }

        let lim = self.ceiling;
        let out = |s: f32| (s * self.gain).clamp(-lim, lim);
        (out(l), out(r))
    }

    /// Clear internal state (on stop/seek so old gain reduction never
    /// carries into the next track).
    pub fn reset(&mut self) {
        self.gain = 1.0;
    }
}

/// Stereo frame.
#[derive(Debug, Clone, Copy, Default)]
pub struct Frame {
    pub l: f32,
    pub r: f32,
}

/// Crossfade loudness curve.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum CrossfadeCurve {
    /// Constant-power (equal-power): no dip at the midpoint. Default.
    #[default]
    EqualPower,
    /// Straight linear blend (slight dip at midpoint, classic DJ feel).
    Linear,
}

/// Mixer state for program bus.
#[derive(Debug)]
pub struct Mixer {
    /// Master gain 0.0..1.0 (+ headroom above 1.0 allowed internally).
    pub gain: f32,
    /// Crossfade 0.0 = full A, 1.0 = full B.
    pub crossfade: f32,
    /// Curve used for the A/B blend.
    pub curve: CrossfadeCurve,
    /// Soft-clip safety net (linear, e.g. 0.99); mirrors the limiter ceiling.
    pub ceiling: f32,
    /// 12-band program EQ insert (before gain).
    pub eq: EqChain,
    /// Lookahead peak limiter (final stage).
    pub limiter: Limiter,
}

impl Default for Mixer {
    fn default() -> Self {
        Self {
            gain: 1.0,
            crossfade: 0.0,
            curve: CrossfadeCurve::EqualPower,
            ceiling: 0.99,
            eq: EqChain::default(),
            limiter: Limiter::default(),
        }
    }
}

impl Mixer {
    pub fn new(gain: f32) -> Self {
        Self {
            gain: gain.clamp(0.0, 1.5),
            ..Self::default()
        }
    }

    pub fn set_gain(&mut self, gain: f32) {
        self.gain = gain.clamp(0.0, 1.5);
    }

    pub fn set_crossfade(&mut self, x: f32) {
        self.crossfade = x.clamp(0.0, 1.0);
    }

    pub fn set_curve(&mut self, curve: CrossfadeCurve) {
        self.curve = curve;
    }

    /// Ceiling for both the soft-clip net and the limiter.
    pub fn set_ceiling(&mut self, c: f32) {
        self.ceiling = c.clamp(0.1, 1.0);
        self.limiter.set_ceiling(self.ceiling);
    }

    /// Recompute EQ coefficients for the device rate (on stream open).
    pub fn set_eq_rate(&mut self, rate: u32) {
        self.eq.set_rate(rate);
    }

    pub fn set_eq_enabled(&mut self, on: bool) {
        self.eq.set_enabled(on);
    }

    pub fn eq_enabled(&self) -> bool {
        self.eq.enabled()
    }

    /// Set one EQ band's gain in dB (clamped ±12 by the chain).
    pub fn set_eq_band(&mut self, band: usize, gain_db: f32) {
        self.eq.set_band(band, gain_db);
    }

    /// Current limiter gain reduction in dB (UI metering).
    pub fn limiter_reduction_db(&self) -> f32 {
        self.limiter.reduction_db()
    }

    /// Clear DSP state (on stop): flatten EQ band gains and limiter
    /// gain reduction so nothing carries into the next playback.
    pub fn reset(&mut self) {
        self.eq.reset();
        self.limiter.reset();
    }

    fn gains(curve: CrossfadeCurve, x: f32) -> (f32, f32) {
        match curve {
            CrossfadeCurve::Linear => (1.0 - x, x),
            CrossfadeCurve::EqualPower => {
                use std::f32::consts::FRAC_PI_2;
                let a = x * FRAC_PI_2;
                (a.cos(), a.sin())
            }
        }
    }

    /// Mix buses A and B with the configured curve, then EQ + gain + limiter.
    /// `a` / `b` are mono sources (duplicated to stereo); either may be `None`.
    pub fn process(&mut self, a: Option<f32>, b: Option<f32>) -> Frame {
        self.process_x(
            a.map(|v| Frame { l: v, r: v }),
            b.map(|v| Frame { l: v, r: v }),
            self.crossfade,
        )
    }

    /// Per-frame stereo crossfade at explicit position `x` (0.0 = full A).
    /// Used by the engine so every audio frame gets its own blend point.
    /// Chain: EQ insert → blend → gain → soft-clip net → limiter.
    pub fn process_x(&mut self, a: Option<Frame>, b: Option<Frame>, x: f32) -> Frame {
        let x = x.clamp(0.0, 1.0);
        let (ga, gb) = Self::gains(self.curve, x);
        let a = a.unwrap_or_default();
        let b = b.unwrap_or_default();
        let l = self.eq.tick(a.l * ga + b.l * gb) * self.gain;
        let r = self.eq.tick(a.r * ga + b.r * gb) * self.gain;
        let l = soft_clip(l, self.ceiling);
        let r = soft_clip(r, self.ceiling);
        let (l, r) = self.limiter.process(l, r);
        Frame { l, r }
    }

    /// Process an interleaved stereo slice in place (legacy rodio/decode
    /// path: gain + soft-clip safety net only — no EQ/limiter there).
    pub fn process_stereo_in_place(&self, buf: &mut [f32]) {
        for s in buf.iter_mut() {
            *s = soft_clip(*s * self.gain, self.ceiling);
        }
    }
}

fn soft_clip(x: f32, ceiling: f32) -> f32 {
    if x > ceiling {
        ceiling
    } else if x < -ceiling {
        -ceiling
    } else {
        x
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn crossfade_endpoints() {
        let mut m = Mixer::default();
        m.set_crossfade(0.0);
        let f = m.process(Some(1.0), Some(0.0));
        assert!(f.l > 0.98, "full A at x=0");

        m.set_crossfade(1.0);
        let f = m.process(Some(0.0), Some(1.0));
        assert!(f.l > 0.98, "full B at x=1");
    }

    #[test]
    fn limiter_clamps() {
        let mut m = Mixer::new(2.0);
        let f = m.process(Some(1.0), None);
        assert!(f.l <= 0.99 + f32::EPSILON);
    }

    #[test]
    fn silence_when_both_none() {
        let mut m = Mixer::default();
        let f = m.process(None, None);
        assert_eq!(f.l, 0.0);
        assert_eq!(f.r, 0.0);
    }

    #[test]
    fn stereo_endpoints_keep_channels() {
        let mut m = Mixer::default();
        let a = Frame { l: 0.8, r: 0.2 };
        let b = Frame { l: 0.1, r: 0.9 };
        let f = m.process_x(Some(a), Some(b), 0.0);
        assert!((f.l - 0.8).abs() < 1e-5 && (f.r - 0.2).abs() < 1e-5);
        let f = m.process_x(Some(a), Some(b), 1.0);
        assert!((f.l - 0.1).abs() < 1e-5 && (f.r - 0.9).abs() < 1e-5);
    }

    #[test]
    fn equal_power_midpoint_holds_level() {
        let mut m = Mixer::default();
        let a = Frame { l: 0.5, r: 0.5 };
        let b = Frame { l: 0.5, r: 0.5 };
        let f = m.process_x(Some(a), Some(b), 0.5);
        // cos45 + sin45 ≈ √2, times 0.5 per bus sum
        assert!(
            (f.l - std::f32::consts::FRAC_1_SQRT_2).abs() < 1e-6,
            "got {}",
            f.l
        );
    }

    #[test]
    fn linear_curve_dips_at_midpoint() {
        let mut m = Mixer::default();
        m.set_curve(CrossfadeCurve::Linear);
        let a = Frame { l: 0.5, r: 0.5 };
        let b = Frame { l: 0.5, r: 0.5 };
        let f = m.process_x(Some(a), Some(b), 0.5);
        assert!((f.l - 0.5).abs() < 1e-5, "got {}", f.l);
    }

    #[test]
    fn eq_bypassed_is_identity() {
        let mut m = Mixer::default();
        m.set_eq_band(6, 9.0); // +9 dB @ 1 kHz but EQ disabled by default
        let f = m.process_x(Some(Frame { l: 0.25, r: -0.25 }), None, 0.0);
        assert!((f.l - 0.25).abs() < 1e-6 && (f.r + 0.25).abs() < 1e-6);
    }

    #[test]
    fn eq_neutral_bands_are_identity_even_when_enabled() {
        let mut m = Mixer::default();
        m.set_eq_enabled(true);
        assert!(m.eq.is_bypassed(), "all-flat chain should bypass");
        let f = m.process_x(Some(Frame { l: 0.5, r: 0.5 }), None, 0.0);
        assert!((f.l - 0.5).abs() < 1e-6);
    }

    #[test]
    fn eq_cut_lowers_band_energy() {
        // A 1 kHz tone must lose energy when its band is cut, and gain
        // energy (pre-limiter; clamped here) when boosted.
        let rate = 48_000.0;
        let tone = |m: &mut Mixer| -> f32 {
            let f0 = 1_000.0;
            let mut energy = 0.0f64;
            for n in 0..(rate as usize / 2) {
                let s = (std::f32::consts::TAU * f0 * n as f32 / rate).sin() * 0.5;
                let out = m.process_x(Some(Frame { l: s, r: s }), None, 0.0);
                energy += (out.l as f64) * (out.l as f64);
            }
            (energy / rate as f64 / 2.0).sqrt() as f32
        };
        let mut flat = Mixer::default();
        flat.set_eq_enabled(true);
        let mut cut = Mixer::default();
        cut.set_eq_enabled(true);
        cut.set_eq_band(6, -12.0); // −12 dB at 1 kHz
        let mut boosted = Mixer::default();
        boosted.set_eq_enabled(true);
        boosted.set_eq_band(6, 9.0); // +9 dB at 1 kHz
        let e_flat = tone(&mut flat);
        let e_cut = tone(&mut cut);
        let e_boost = tone(&mut boosted);
        assert!(
            e_cut < e_flat * 0.9,
            "1 kHz cut should lower energy: flat={e_flat} cut={e_cut}"
        );
        assert!(
            e_boost > e_flat,
            "1 kHz boost should raise energy: flat={e_flat} boost={e_boost}"
        );
    }

    #[test]
    fn eq_cut_then_reset_restores_level() {
        let mut m = Mixer::default();
        m.set_eq_enabled(true);
        for b in 0..EQ_BAND_COUNT {
            m.set_eq_band(b, -6.0);
        }
        let limiter_reduction = m.limiter_reduction_db();
        assert!(limiter_reduction.abs() < 1e-6, "no GR at unity");
        m.reset();
        let f = m.process_x(Some(Frame { l: 0.5, r: 0.5 }), None, 0.0);
        assert!((f.l - 0.5).abs() < 1e-6, "reset must be flat again");
    }

    #[test]
    fn eq_rate_change_rebuilds_coefficients() {
        let mut eq = EqChain::new(48_000);
        eq.set_enabled(true);
        eq.set_band(0, 6.0);
        eq.tick(0.5); // leave some filter state behind
        eq.set_rate(44_100);
        let f = eq.tick(0.25);
        assert!(f.is_finite());
    }

    #[test]
    fn limiter_brickwalls_above_ceiling() {
        let mut lim = Limiter::new(0.9, 48_000);
        // 3 s of constant 2.0-amplitude tone: every sample must be ≤ ceiling.
        for n in 0..(48_000 * 3) {
            let s = 2.0 * (std::f32::consts::TAU * 220.0 * n as f32 / 48_000.0).sin();
            let (l, r) = lim.process(s, -s);
            assert!(l.abs() <= 0.9 + 1e-6 && r.abs() <= 0.9 + 1e-6);
        }
        // Sustained gain reduction must be metered and sensible.
        let gr = lim.reduction_db();
        assert!(gr < -3.0, "expected >3 dB GR, got {gr}");
    }

    #[test]
    fn limiter_releases_and_resets() {
        let mut lim = Limiter::new(0.9, 48_000);
        for _ in 0..48_000 {
            lim.process(2.0, 2.0);
        }
        assert!(lim.reduction_db() < 0.0);
        // Quiet signal: gain recovers toward unity within ~0.5 s.
        for _ in 0..(48_000 / 2) {
            lim.process(0.1, 0.1);
        }
        assert!(
            lim.reduction_db() > -1.0,
            "GR should recover, got {}",
            lim.reduction_db()
        );
        lim.reset();
        assert_eq!(lim.reduction_db(), 0.0);
    }

    #[test]
    fn mixer_ceiling_syncs_limiter() {
        let mut m = Mixer::default();
        m.set_ceiling(0.5);
        let f = m.process_x(Some(Frame { l: 1.0, r: 1.0 }), None, 0.0);
        assert!(f.l <= 0.5 + 1e-6 && f.r <= 0.5 + 1e-6);
        assert_eq!(m.limiter.ceiling(), 0.5);
    }

    #[test]
    fn eq_passthrough_when_disabled_keeps_polarity() {
        let mut m = Mixer::default();
        m.set_eq_band(11, -12.0);
        let f = m.process_x(Some(Frame { l: -0.4, r: 0.4 }), None, 0.0);
        assert!((f.l + 0.4).abs() < 1e-6 && (f.r - 0.4).abs() < 1e-6);
    }
}
