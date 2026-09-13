//! MP3 encoding of the program mix via LAME (`mp3lame-encoder`).
//!
//! Constant bitrate for predictable broadcast bandwidth. Input is f32
//! interleaved stereo at the device rate.

use mp3lame_encoder::{Bitrate, Builder, Encoder, InterleavedPcm};

use crate::error::{CrabError, Result};
use crate::stream::StreamConfig;

/// Nearest supported CBR bitrate for a requested kbps value.
fn nearest_bitrate(kbps: u32) -> Bitrate {
    const ALL: [(u32, Bitrate); 16] = [
        (8, Bitrate::Kbps8),
        (16, Bitrate::Kbps16),
        (24, Bitrate::Kbps24),
        (32, Bitrate::Kbps32),
        (40, Bitrate::Kbps40),
        (48, Bitrate::Kbps48),
        (64, Bitrate::Kbps64),
        (80, Bitrate::Kbps80),
        (96, Bitrate::Kbps96),
        (112, Bitrate::Kbps112),
        (128, Bitrate::Kbps128),
        (160, Bitrate::Kbps160),
        (192, Bitrate::Kbps192),
        (224, Bitrate::Kbps224),
        (256, Bitrate::Kbps256),
        (320, Bitrate::Kbps320),
    ];
    ALL.iter()
        .min_by_key(|(v, _)| v.abs_diff(kbps))
        .map(|(_, b)| *b)
        .unwrap_or(Bitrate::Kbps128)
}

/// Frame-sized MP3 encoder for the program bus.
pub struct Mp3Encoder {
    enc: Encoder,
    /// Scratch buffer reused across `encode` calls.
    out: Vec<u8>,
}

impl Mp3Encoder {
    /// Build a CBR encoder at the given input rate; output rate mirrors
    /// the input when it is MPEG-1/2 legal, else LAME picks.
    pub fn new(config: &StreamConfig, sample_rate: u32) -> Result<Self> {
        let mut builder = Builder::new()
            .ok_or_else(|| CrabError::Audio("LAME: failed to allocate encoder".into()))?;
        builder
            .set_num_channels(2)
            .map_err(|e| CrabError::Audio(format!("LAME channels: {e}")))?;
        builder
            .set_sample_rate(sample_rate)
            .map_err(|e| CrabError::Audio(format!("LAME rate {sample_rate}: {e}")))?;
        builder
            .set_brate(nearest_bitrate(config.bitrate_kbps))
            .map_err(|e| CrabError::Audio(format!("LAME bitrate: {e}")))?;
        builder
            .set_quality(mp3lame_encoder::Quality::Best)
            .map_err(|e| CrabError::Audio(format!("LAME quality: {e}")))?;
        let enc = builder
            .build()
            .map_err(|e| CrabError::Audio(format!("LAME init: {e}")))?;
        Ok(Self {
            enc,
            out: Vec::with_capacity(16_384),
        })
    }

    /// Encode one chunk of interleaved stereo f32; returns the MP3 bytes.
    pub fn encode(&mut self, interleaved: &[f32]) -> Result<&[u8]> {
        self.out.clear();
        let pcm = InterleavedPcm(interleaved);
        let cap = mp3lame_encoder::max_required_buffer_size(interleaved.len() / 2);
        self.out.reserve(cap);
        let written = self
            .enc
            .encode(pcm, self.out.spare_capacity_mut())
            .map_err(|e| CrabError::Audio(format!("LAME encode: {e}")))?;
        unsafe {
            self.out.set_len(self.out.len().wrapping_add(written));
        }
        Ok(&self.out)
    }

    /// Flush any tail (call when stopping the stream).
    pub fn flush(&mut self) -> Result<&[u8]> {
        self.out.clear();
        let written = self
            .enc
            .flush::<mp3lame_encoder::FlushNoGap>(self.out.spare_capacity_mut())
            .map_err(|e| CrabError::Audio(format!("LAME flush: {e}")))?;
        unsafe {
            self.out.set_len(self.out.len().wrapping_add(written));
        }
        Ok(&self.out)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::stream::StreamConfig;

    #[test]
    fn encodes_silence_to_mp3_frames() {
        let cfg = StreamConfig {
            bitrate_kbps: 128,
            ..Default::default()
        };
        let mut enc = Mp3Encoder::new(&cfg, 48_000).expect("encoder builds");
        let mut total = 0usize;
        // ~0.5 s of silence in 1024-frame chunks.
        let chunk = vec![0.0f32; 2048];
        for _ in 0..24 {
            let bytes = enc.encode(&chunk).expect("encode");
            total += bytes.len();
        }
        let tail = enc.flush().expect("flush");
        total += tail.len();
        // 128 kbps ≈ 16 KB/s; half a second should produce a few KB.
        assert!(
            total > 4_000 && total < 40_000,
            "suspicious output size: {total}"
        );
    }

    #[test]
    fn accepts_standard_rates() {
        let cfg = StreamConfig::default();
        for rate in [32_000, 44_100, 48_000] {
            let mut enc = Mp3Encoder::new(&cfg, rate)
                .unwrap_or_else(|e| panic!("LAME should accept {rate}: {e}"));
            let bytes = enc.encode(&[0.0f32; 2048]).expect("encode");
            assert!(!bytes.is_empty(), "rate {rate} should produce output");
        }
    }
}
