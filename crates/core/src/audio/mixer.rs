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

/// Mixer state for program bus.
#[derive(Debug)]
pub struct Mixer {
    /// Master gain 0.0..1.0 (+ headroom above 1.0 allowed internally).
    pub gain: f32,
    /// Crossfade 0.0 = full A, 1.0 = full B.
    pub crossfade: f32,
    /// Limiter ceiling (linear, e.g. 0.99).
    pub ceiling: f32,
}

impl Default for Mixer {
    fn default() -> Self {
        Self {
            gain: 1.0,
            crossfade: 0.0,
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

    /// Mix buses A and B with equal-power crossfade, then gain + hard limiter.
    /// `a` / `b` are mono sources (duplicated to stereo); either may be `None`.
    pub fn process(&self, a: Option<f32>, b: Option<f32>) -> Frame {
        use std::f32::consts::FRAC_PI_2;
        let x = self.crossfade * FRAC_PI_2;
        let (ga, gb) = (x.cos(), x.sin());
        let m = a.unwrap_or(0.0) * ga + b.unwrap_or(0.0) * gb;
        let m = m * self.gain;
        let l = soft_clip(m, self.ceiling);
        Frame { l, r: l }
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
}
