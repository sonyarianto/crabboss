//! Sample-accurate mixer: gain → crossfade → peak limiter.
//!
//! Pure DSP, no I/O. `CpalEngine` pulls frames through this;
//! unit-testable without an audio device.

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
    /// Limiter ceiling (linear, e.g. 0.99).
    pub ceiling: f32,
}

impl Default for Mixer {
    fn default() -> Self {
        Self {
            gain: 1.0,
            crossfade: 0.0,
            curve: CrossfadeCurve::EqualPower,
            ceiling: 0.99,
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

    /// Mix buses A and B with the configured curve, then gain + hard limiter.
    /// `a` / `b` are mono sources (duplicated to stereo); either may be `None`.
    pub fn process(&self, a: Option<f32>, b: Option<f32>) -> Frame {
        self.process_x(
            a.map(|v| Frame { l: v, r: v }),
            b.map(|v| Frame { l: v, r: v }),
            self.crossfade,
        )
    }

    /// Per-frame stereo crossfade at explicit position `x` (0.0 = full A).
    /// Used by the engine so every audio frame gets its own blend point.
    pub fn process_x(&self, a: Option<Frame>, b: Option<Frame>, x: f32) -> Frame {
        let x = x.clamp(0.0, 1.0);
        let (ga, gb) = Self::gains(self.curve, x);
        let a = a.unwrap_or_default();
        let b = b.unwrap_or_default();
        let l = soft_clip((a.l * ga + b.l * gb) * self.gain, self.ceiling);
        let r = soft_clip((a.r * ga + b.r * gb) * self.gain, self.ceiling);
        Frame { l, r }
    }

    /// Process an interleaved stereo slice in place (gain + limiter only).
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
        let m = Mixer::new(2.0);
        let f = m.process(Some(1.0), None);
        assert!(f.l <= 0.99 + f32::EPSILON);
    }

    #[test]
    fn silence_when_both_none() {
        let m = Mixer::default();
        let f = m.process(None, None);
        assert_eq!(f.l, 0.0);
        assert_eq!(f.r, 0.0);
    }

    #[test]
    fn stereo_endpoints_keep_channels() {
        let m = Mixer::default();
        let a = Frame { l: 0.8, r: 0.2 };
        let b = Frame { l: 0.1, r: 0.9 };
        let f = m.process_x(Some(a), Some(b), 0.0);
        assert!((f.l - 0.8).abs() < 1e-5 && (f.r - 0.2).abs() < 1e-5);
        let f = m.process_x(Some(a), Some(b), 1.0);
        assert!((f.l - 0.1).abs() < 1e-5 && (f.r - 0.9).abs() < 1e-5);
    }

    #[test]
    fn equal_power_midpoint_holds_level() {
        let m = Mixer::default();
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
}
