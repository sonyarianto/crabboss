//! Loudness measurement (ITU-R BS.1770-4 / EBU R128) for ReplayGain-style
//! normalization.
//!
//! Signal path: K-weighting (high shelf + high-pass, RBJ biquads at the
//! file's native rate) → 400 ms blocks with 75% overlap → BS.1770 gating
//! (absolute −70 LUFS, relative −10 LU) → integrated LUFS.
//!
//! Pure DSP + decode, no I/O beyond reading the file — unit-testable with
//! synthetic tones.

use std::path::Path;

use crate::audio::cpal_engine::decode_to_stereo;
use crate::audio::mixer::Biquad;
use crate::error::Result;

/// Normalization target: EBU R128 broadcast level (RadioBOSS-style stations).
pub const TARGET_LUFS: f32 = -23.0;

/// Safety clamp on applied correction (±24 dB covers practically anything).
pub const MAX_GAIN_DB: f32 = 24.0;

/// Result of analyzing one file.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct LoudnessAnalysis {
    /// Integrated loudness (gated) in LUFS.
    pub integrated_lufs: f32,
    /// Linear gain in dB that brings the track to [`TARGET_LUFS`].
    pub gain_db: f32,
}

/// BS.1770 K-weighting pre-filter (per channel state) at an arbitrary rate.
// Constants are the published BS.1770 coefficients (f64 precision kept
// verbatim on purpose; f32 truncation is irrelevant at audio rates).
#[allow(clippy::excessive_precision)]
fn k_weighting(rate: f32) -> [Biquad; 2] {
    // Stage 1: high shelf (+4 dB above ≈1.68 kHz) — tangent form as used by
    // libebur128. Verifiable at the band edges: H(0) = 1, H(π) = Vh.
    let vh = 10f32.powf(3.99984385397 / 20.0);
    let vb = vh.powf(0.4996667741545416);
    let k = (std::f32::consts::PI * 1681.9744509555319 / rate).tan();
    let kq = k / 0.7071752369554196;
    let k2 = k * k;
    let shelf = Biquad::from_raw(
        vh + vb * kq + k2,
        2.0 * (k2 - vh),
        vh - vb * kq + k2,
        1.0 + kq + k2,
        2.0 * (k2 - 1.0),
        1.0 - kq + k2,
    );

    // Stage 2: high-pass, fc ≈ 38.13 Hz, Q ≈ 0.5003.
    let w0 = std::f32::consts::TAU * 38.13547087602444 / rate;
    let cw = w0.cos();
    let alpha = w0.sin() / (2.0 * 0.5003270373238773);
    let hp = Biquad::from_raw(
        (1.0 + cw) / 2.0,
        -(1.0 + cw),
        (1.0 + cw) / 2.0,
        1.0 + alpha,
        -2.0 * cw,
        1.0 - alpha,
    );

    [shelf, hp]
}

/// Running BS.1770 loudness meter. Feed every frame with [`push`](Self::push),
/// then read [`integrated_lufs`](Self::integrated_lufs).
pub struct LoudnessMeter {
    /// 100 ms hop length in frames.
    hop_frames: usize,
    /// Power sums (K-weighted) for the last 4 hops = one 400 ms block.
    recent: Vec<(f64, f64)>,
    /// Power sums of the hop currently being accumulated.
    cur: (f64, f64),
    frames_in_hop: usize,
    /// Channel power of every completed block (for gating).
    block_powers: Vec<f64>,
    k_l: [Biquad; 2],
    k_r: [Biquad; 2],
}

impl LoudnessMeter {
    pub fn new(rate: u32) -> Self {
        let hop_frames = ((rate as f32) * 0.1).round().max(1.0) as usize;
        Self {
            hop_frames,
            recent: Vec::new(),
            cur: (0.0, 0.0),
            frames_in_hop: 0,
            block_powers: Vec::new(),
            k_l: k_weighting(rate as f32),
            k_r: k_weighting(rate as f32),
        }
    }

    /// K-weight one channel sample through the 2-biquad chain.
    fn k_filter(chain: &mut [Biquad; 2], x: f32) -> f32 {
        let hp = chain[1].tick(x);
        chain[0].tick(hp)
    }

    /// Push one stereo frame.
    pub fn push(&mut self, l: f32, r: f32) {
        let kl = Self::k_filter(&mut self.k_l, l);
        let kr = Self::k_filter(&mut self.k_r, r);
        self.cur.0 += (kl as f64) * (kl as f64);
        self.cur.1 += (kr as f64) * (kr as f64);
        self.frames_in_hop += 1;
        if self.frames_in_hop >= self.hop_frames {
            self.recent.push(self.cur);
            self.cur = (0.0, 0.0);
            self.frames_in_hop = 0;
            if self.recent.len() > 4 {
                self.recent.remove(0);
            }
            if self.recent.len() == 4 {
                let (pl, pr): (f64, f64) = self
                    .recent
                    .iter()
                    .fold((0.0, 0.0), |(a, b), (l, r)| (a + l, b + r));
                self.block_powers
                    .push((pl + pr) / (4.0 * self.hop_frames as f64));
            }
        }
    }

    /// Loudness of one block power: −0.691 + 10·log₁₀(power).
    fn block_lufs(power: f64) -> f32 {
        (-0.691 + 10.0 * power.max(f64::EPSILON).log10()) as f32
    }

    /// Integrated (gated) loudness, or `None` for clips under 400 ms.
    pub fn integrated_lufs(&self) -> Option<f32> {
        if self.block_powers.is_empty() {
            return None;
        }
        // Absolute gate: discard blocks below −70 LUFS.
        let abs_gate: Vec<f64> = self
            .block_powers
            .iter()
            .copied()
            .filter(|p| Self::block_lufs(*p) > -70.0)
            .collect();
        if abs_gate.is_empty() {
            return None;
        }
        // Relative gate: mean of surviving blocks, minus 10 LU.
        let mean = |ps: &[f64]| ps.iter().sum::<f64>() / ps.len() as f64;
        let rel_threshold = Self::block_lufs(mean(&abs_gate)) - 10.0;
        let gated: Vec<f64> = abs_gate
            .into_iter()
            .filter(|p| Self::block_lufs(*p) > rel_threshold)
            .collect();
        if gated.is_empty() {
            return None;
        }
        Some(Self::block_lufs(mean(&gated)))
    }
}

/// Decode + analyze a file at its native rate. Gain is the correction (dB)
/// toward [`TARGET_LUFS`], clamped to ±[`MAX_GAIN_DB`].
pub fn analyze_file(path: &Path) -> Result<LoudnessAnalysis> {
    let (samples, rate) = decode_to_stereo(path)?;
    let mut meter = LoudnessMeter::new(rate);
    for pair in samples.chunks(2) {
        meter.push(pair[0], *pair.get(1).unwrap_or(&0.0));
    }
    let integrated = meter
        .integrated_lufs()
        .ok_or_else(|| crate::error::CrabError::Audio("clip too short to measure".into()))?;
    Ok(LoudnessAnalysis {
        integrated_lufs: integrated,
        gain_db: (TARGET_LUFS - integrated).clamp(-MAX_GAIN_DB, MAX_GAIN_DB),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Sine generator: 1 kHz at `amp`, `secs` long, stereo-identical.
    fn sine(amp: f32, secs: f32, rate: u32) -> Vec<f32> {
        let n = (rate as f32 * secs) as usize;
        (0..n)
            .flat_map(|i| {
                let s = amp * (std::f32::consts::TAU * 1_000.0 * i as f32 / rate as f32).sin();
                [s, s]
            })
            .collect()
    }

    fn measure(samples: &[f32], rate: u32) -> Option<f32> {
        let mut m = LoudnessMeter::new(rate);
        for pair in samples.chunks(2) {
            m.push(pair[0], pair[1]);
        }
        m.integrated_lufs()
    }

    #[test]
    fn stereo_full_scale_sine_reads_zero_lufs() {
        // Stereo 1 kHz FS sine: K(1 kHz) ≈ +0.62 dB doubles per the two
        // channels → −0.691 + 10·log10(2·0.5·G) ≈ −0.07 LUFS.
        let lufs = measure(&sine(1.0, 3.0, 48_000), 48_000).unwrap();
        assert!((-0.45..=0.1).contains(&lufs), "got {lufs}");
    }

    #[test]
    fn mono_full_scale_sine_is_minus_3_lufs() {
        // ITU anchor: a mono 997 Hz FS sine measures −3.01 LUFS — this is
        // exactly what calibrates the −0.691 offset (K(997) ≈ +0.69 dB).
        let rate = 48_000u32;
        let n = rate as usize * 3;
        let mut samples = Vec::with_capacity(n * 2);
        for i in 0..n {
            let s = (std::f32::consts::TAU * 997.0 * i as f32 / rate as f32).sin();
            samples.push(s);
            samples.push(0.0); // silent right channel → mono-equivalent
        }
        let lufs = measure(&samples, rate).unwrap();
        assert!((-3.3..=-2.7).contains(&lufs), "got {lufs}");
    }

    #[test]
    fn quieter_sine_reads_lower() {
        let loud = measure(&sine(1.0, 3.0, 44_100), 44_100).unwrap();
        let quiet = measure(&sine(0.1, 3.0, 44_100), 44_100).unwrap();
        assert!((loud - quiet - 20.0).abs() < 0.5, "{loud} vs {quiet}");
    }

    #[test]
    fn gating_ignores_silence() {
        let rate = 48_000;
        let mut samples = sine(0.1, 4.0, rate);
        samples.extend(vec![0.0; rate as usize * 4]); // 4 s digital silence
        let gated = measure(&samples, rate).unwrap();
        let clean = measure(&sine(0.1, 4.0, rate), rate).unwrap();
        assert!(
            (gated - clean).abs() < 0.7,
            "silence must be gated: {gated} vs {clean}"
        );
    }

    #[test]
    fn gain_points_at_target() {
        let a = LoudnessAnalysis {
            integrated_lufs: -33.0,
            gain_db: 10.0,
        };
        assert!((a.gain_db - (TARGET_LUFS - a.integrated_lufs)).abs() < 1e-5);
        // Over-loud track gets negative correction, clamped at the bound.
        let hot = LoudnessAnalysis {
            integrated_lufs: -3.0,
            gain_db: (TARGET_LUFS - (-3.0)).clamp(-MAX_GAIN_DB, MAX_GAIN_DB),
        };
        assert!((hot.gain_db - (-20.0)).abs() < 1e-5);
    }

    #[test]
    fn too_short_clip_reports_none() {
        let m = LoudnessMeter::new(48_000);
        assert!(m.integrated_lufs().is_none());
    }
}
