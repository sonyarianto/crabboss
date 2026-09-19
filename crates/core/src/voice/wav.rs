//! Minimal PCM16 WAV writer for voice-track recording.
//!
//! No new dependencies: 44-byte header, placeholder sizes on create,
//! patched on [`WavWriter::finalize`]. The engine always records stereo
//! at the device rate; the channel count stays a parameter so tests can
//! cover mono too.

use std::fs::File;
use std::io::{Seek, SeekFrom, Write};
use std::path::Path;

use crate::error::{CrabError, Result};

/// Finalized take metadata.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct WavMeta {
    /// Samples per channel written.
    pub samples: u64,
    pub channels: u16,
    pub sample_rate: u32,
}

impl WavMeta {
    pub fn duration_secs(&self) -> f64 {
        if self.sample_rate == 0 {
            0.0
        } else {
            self.samples as f64 / self.sample_rate as f64
        }
    }
}

pub struct WavWriter {
    file: File,
    channels: u16,
    sample_rate: u32,
    samples: u64,
}

impl WavWriter {
    /// Create the file with a placeholder header. `channels` is 1 or 2.
    pub fn create(path: &Path, channels: u16, sample_rate: u32) -> Result<Self> {
        if channels == 0 || channels > 2 {
            return Err(CrabError::Audio(format!(
                "WAV writer supports mono/stereo, got {channels} channels"
            )));
        }
        if sample_rate == 0 {
            return Err(CrabError::Audio("WAV writer needs a nonzero rate".into()));
        }
        let mut file = File::create(path)
            .map_err(|e| CrabError::Audio(format!("cannot record to {}: {e}", path.display())))?;
        write_header(&mut file, channels, sample_rate, 0)?;
        Ok(Self {
            file,
            channels,
            sample_rate,
            samples: 0,
        })
    }

    /// Append interleaved i16 frames (`len` must be a multiple of the
    /// channel count; a trailing partial frame is dropped, never torn).
    pub fn write_frames(&mut self, interleaved: &[i16]) -> Result<()> {
        let frames = interleaved.len() / self.channels as usize;
        for s in &interleaved[..frames * self.channels as usize] {
            self.file
                .write_all(&s.to_le_bytes())
                .map_err(|e| CrabError::Audio(format!("voice record write failed: {e}")))?;
        }
        self.samples += frames as u64;
        Ok(())
    }

    /// Patch the size fields and close. Always call — otherwise the file
    /// keeps placeholder sizes and most players reject it.
    pub fn finalize(mut self) -> Result<WavMeta> {
        let data_bytes = self.samples * self.channels as u64 * 2;
        self.file
            .seek(SeekFrom::Start(4))
            .map_err(|e| CrabError::Audio(format!("voice record finalize failed: {e}")))?;
        self.file
            .write_all(&(36 + data_bytes as u32).to_le_bytes())
            .map_err(|e| CrabError::Audio(format!("voice record finalize failed: {e}")))?;
        self.file
            .seek(SeekFrom::Start(40))
            .map_err(|e| CrabError::Audio(format!("voice record finalize failed: {e}")))?;
        self.file
            .write_all(&(data_bytes as u32).to_le_bytes())
            .map_err(|e| CrabError::Audio(format!("voice record finalize failed: {e}")))?;
        self.file
            .flush()
            .map_err(|e| CrabError::Audio(format!("voice record finalize failed: {e}")))?;
        Ok(WavMeta {
            samples: self.samples,
            channels: self.channels,
            sample_rate: self.sample_rate,
        })
    }
}

fn write_header(file: &mut File, channels: u16, rate: u32, data_bytes: u32) -> Result<()> {
    let mut h = [0u8; 44];
    h[0..4].copy_from_slice(b"RIFF");
    h[4..8].copy_from_slice(&(36 + data_bytes).to_le_bytes());
    h[8..12].copy_from_slice(b"WAVE");
    h[12..16].copy_from_slice(b"fmt ");
    h[16..20].copy_from_slice(&16u32.to_le_bytes());
    h[20..22].copy_from_slice(&1u16.to_le_bytes()); // PCM
    h[22..24].copy_from_slice(&channels.to_le_bytes());
    h[24..28].copy_from_slice(&rate.to_le_bytes());
    let byte_rate = rate * channels as u32 * 2;
    h[28..32].copy_from_slice(&byte_rate.to_le_bytes());
    h[32..34].copy_from_slice(&(channels * 2).to_le_bytes()); // block align
    h[34..36].copy_from_slice(&16u16.to_le_bytes()); // bits
    h[36..40].copy_from_slice(b"data");
    h[40..44].copy_from_slice(&data_bytes.to_le_bytes());
    file.write_all(&h)
        .map_err(|e| CrabError::Audio(format!("voice record header failed: {e}")))?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Read;

    fn tmp(name: &str) -> std::path::PathBuf {
        let mut p = std::env::temp_dir();
        p.push(format!("crabboss-voice-test-{name}.wav"));
        let _ = std::fs::remove_file(&p);
        p
    }

    fn read_header(path: &Path) -> Vec<u8> {
        let mut f = File::open(path).unwrap();
        let mut h = vec![0u8; 44];
        f.read_exact(&mut h).unwrap();
        h
    }

    #[test]
    fn header_roundtrips_sizes() {
        let p = tmp("sizes");
        let mut w = WavWriter::create(&p, 2, 48_000).unwrap();
        // 1 s of stereo silence: 48000 frames.
        w.write_frames(&vec![0i16; 48_000 * 2]).unwrap();
        let meta = w.finalize().unwrap();
        assert_eq!(meta.samples, 48_000);
        assert!((meta.duration_secs() - 1.0).abs() < 1e-9);

        let h = read_header(&p);
        assert_eq!(&h[0..4], b"RIFF");
        assert_eq!(&h[8..12], b"WAVE");
        assert_eq!(u16::from_le_bytes([h[22], h[23]]), 2);
        assert_eq!(u32::from_le_bytes([h[24], h[25], h[26], h[27]]), 48_000);
        let data = u32::from_le_bytes([h[40], h[41], h[42], h[43]]);
        assert_eq!(data, 48_000 * 2 * 2);
        let riff = u32::from_le_bytes([h[4], h[5], h[6], h[7]]);
        assert_eq!(riff, 36 + data);
        let total = std::fs::metadata(&p).unwrap().len();
        assert_eq!(total, 44 + data as u64);
        let _ = std::fs::remove_file(&p);
    }

    #[test]
    fn empty_take_finalizes_to_valid_header() {
        let p = tmp("empty");
        let w = WavWriter::create(&p, 1, 44_100).unwrap();
        let meta = w.finalize().unwrap();
        assert_eq!(meta.samples, 0);
        assert_eq!(meta.duration_secs(), 0.0);
        let h = read_header(&p);
        assert_eq!(&h[0..4], b"RIFF");
        assert_eq!(u32::from_le_bytes([h[40], h[41], h[42], h[43]]), 0);
        let _ = std::fs::remove_file(&p);
    }

    #[test]
    fn rejects_bad_params() {
        let p = tmp("bad");
        assert!(WavWriter::create(&p, 0, 48_000).is_err());
        assert!(WavWriter::create(&p, 3, 48_000).is_err());
        assert!(WavWriter::create(&p, 2, 0).is_err());
        assert!(!p.exists(), "failed create must not leave a file");
    }
}
