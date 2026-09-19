//! Program-bus encoders for the stream source clients.
//!
//! Every encoder takes interleaved stereo f32 at the device rate in
//! arbitrary chunk sizes and returns complete container bytes ready to
//! send. Constant bitrate everywhere: predictable broadcast bandwidth.
//! One `encode` call may legitimately return empty (a partial frame is
//! still buffering); the manager sends whatever comes back.

use crate::error::{CrabError, Result};
use crate::stream::{StreamConfig, StreamFormat};

/// Frame-sized program-bus encoder: arbitrary f32 stereo chunks in,
/// complete container bytes out.
pub trait StreamEncoder {
    /// Encode one chunk of interleaved stereo f32; returns the bytes to
    /// send (possibly empty while a frame is still filling up).
    fn encode(&mut self, interleaved: &[f32]) -> Result<&[u8]>;

    /// Flush any tail (call when stopping the stream).
    fn flush(&mut self) -> Result<&[u8]>;
}

/// Build the encoder selected by the stream config.
pub fn build_encoder(config: &StreamConfig, sample_rate: u32) -> Result<Box<dyn StreamEncoder>> {
    // Shoutcast DNAS is MP3-only: fail here (before any connection)
    // with an actionable message rather than a silent dead stream.
    if config.protocol.is_shoutcast() && config.format != StreamFormat::Mp3 {
        return Err(CrabError::Audio(
            "Shoutcast output supports MP3 only — switch Format to MP3 (or Protocol to Icecast for Opus)"
                .into(),
        ));
    }
    match config.format {
        StreamFormat::Mp3 => Ok(Box::new(crate::stream::encoder_mp3::Mp3Encoder::new(
            config,
            sample_rate,
        )?)),
        StreamFormat::Opus => Ok(Box::new(crate::stream::encoder_opus::OpusOggEncoder::new(
            config,
            sample_rate,
        )?)),
    }
}
