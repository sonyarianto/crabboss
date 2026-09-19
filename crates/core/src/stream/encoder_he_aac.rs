//! HE-AAC (v1/v2) encoding of the program mix via Fraunhofer FDK AAC
//! (`fdk-aac-sys`), framed as ADTS for the Icecast source client.
//!
//! AOT selection is bitrate-driven, mirroring broadcast practice:
//! v2 (parametric stereo) at or below 48 kbps, v1 (SBR) above it.
//! Transport is `TT_MP4_ADTS`, so every `encode` batch that completes a
//! frame yields self-describing bytes (header + payload) the server
//! forwards verbatim — no out-of-band config needed, unlike raw/LOAS.
//! Constant bitrate everywhere, like the MP3/Opus paths.
//!
//! The instance lives on the sender thread only (created, used, and
//! closed there — the handle never crosses threads).

use fdk_aac_sys as fdk;

use crate::error::{CrabError, Result};
use crate::stream::encoder::StreamEncoder;
use crate::stream::StreamConfig;

/// Bitrate (kbps) at or below which parametric stereo (HE-AAC v2) wins
/// over plain SBR (HE-AAC v1) for stereo music.
const V2_MAX_KBPS: u32 = 48;

/// Clamp for the requested bitrate: below this SBR starves, above it
/// plain AAC-LC would be the more transparent choice.
const MIN_KBPS: u32 = 16;
const MAX_KBPS: u32 = 160;

/// Pick the audio object type for a target bitrate (pure, unit-tested).
fn select_aot(kbps: u32) -> (u32, &'static str) {
    if kbps <= V2_MAX_KBPS {
        (fdk::AUDIO_OBJECT_TYPE_AOT_PS as u32, "HE-AAC v2")
    } else {
        (fdk::AUDIO_OBJECT_TYPE_AOT_SBR as u32, "HE-AAC v1")
    }
}

fn set_param(handle: fdk::HANDLE_AACENCODER, param: u32, value: u32, what: &str) -> Result<()> {
    // SAFETY: live encoder handle from `aacEncOpen`, checked below.
    let err = unsafe { fdk::aacEncoder_SetParam(handle, param, value) };
    if err != fdk::AACENC_ERROR_AACENC_OK {
        return Err(CrabError::Audio(format!(
            "HE-AAC {what}: FDK rejected the setting (error {err})"
        )));
    }
    Ok(())
}

/// Frame-sized HE-AAC encoder: arbitrary f32 stereo chunks in, complete
/// ADTS bytes out.
#[derive(Debug)]
pub struct HeAacEncoder {
    handle: fdk::HANDLE_AACENCODER,
    /// Input samples consumed per channel per frame (queried from FDK:
    /// 2048 under SBR, not the 1024 of plain AAC-LC — never hardcode).
    frame_len: usize,
    /// Interleaved i16 staging (FDK eats `INT_PCM`, i.e. int16).
    staging: Vec<i16>,
    /// Scratch output buffer sized to `maxOutBufBytes`.
    scratch: Vec<u8>,
    out: Vec<u8>,
}

// Raw handle is `Send` (not `Sync`): the instance may live on the
// sender thread, but `&` sharing across threads is a compile error.
unsafe impl Send for HeAacEncoder {}

impl Drop for HeAacEncoder {
    fn drop(&mut self) {
        // SAFETY: handle opened in `new`, closed exactly once here.
        unsafe {
            fdk::aacEncClose(&mut self.handle);
        }
    }
}

impl HeAacEncoder {
    /// Build a CBR encoder; `sample_rate` is the device rate of the
    /// incoming mix (FDK resamples internally for SBR — no pre-resample
    /// staging like the Opus path). Fails loudly on bad rates instead
    /// of streaming mislabeled audio.
    pub fn new(config: &StreamConfig, sample_rate: u32) -> Result<Self> {
        if sample_rate == 0 {
            return Err(CrabError::Audio("HE-AAC: device rate is 0".into()));
        }
        let kbps = config.bitrate_kbps.clamp(MIN_KBPS, MAX_KBPS);
        let (aot, aot_label) = select_aot(kbps);

        let mut handle: fdk::HANDLE_AACENCODER = std::ptr::null_mut();
        // SAFETY: null handle in, valid instance out on success.
        let err = unsafe { fdk::aacEncOpen(&mut handle, 0, 2) };
        if err != fdk::AACENC_ERROR_AACENC_OK || handle.is_null() {
            return Err(CrabError::Audio(format!(
                "HE-AAC init: FDK refused to open an encoder (error {err})"
            )));
        }
        // From here every fallible step must close the handle: use a
        // local closure scope so early returns can't leak it.
        let build = || -> Result<(usize, usize)> {
            set_param(handle, fdk::AACENC_PARAM_AACENC_AOT, aot, "AOT")?;
            set_param(
                handle,
                fdk::AACENC_PARAM_AACENC_SAMPLERATE,
                sample_rate,
                "sample rate",
            )?;
            set_param(
                handle,
                fdk::AACENC_PARAM_AACENC_CHANNELMODE,
                fdk::CHANNEL_MODE_MODE_2 as u32,
                "stereo channel mode",
            )?;
            set_param(
                handle,
                fdk::AACENC_PARAM_AACENC_BITRATE,
                kbps * 1000,
                "bitrate",
            )?;
            // CBR like the MP3/Opus paths: predictable broadcast bandwidth.
            set_param(handle, fdk::AACENC_PARAM_AACENC_BITRATEMODE, 0, "CBR")?;
            set_param(
                handle,
                fdk::AACENC_PARAM_AACENC_TRANSMUX,
                fdk::TRANSPORT_TYPE_TT_MP4_ADTS as u32,
                "ADTS transport",
            )?;
            set_param(
                handle,
                fdk::AACENC_PARAM_AACENC_AFTERBURNER,
                1,
                "afterburner",
            )?;
            // Initialize the instance with the present parameter set
            // (mandatory before `aacEncInfo` / the first real encode).
            // SAFETY: NULL descriptors are the documented init signal.
            let err = unsafe {
                fdk::aacEncEncode(
                    handle,
                    std::ptr::null(),
                    std::ptr::null(),
                    std::ptr::null(),
                    std::ptr::null_mut(),
                )
            };
            if err != fdk::AACENC_ERROR_AACENC_OK {
                return Err(CrabError::Audio(format!(
                    "HE-AAC init: FDK rejected the configuration (error {err})"
                )));
            }
            // SAFETY: zeroed info struct filled by FDK on success.
            let mut info: fdk::AACENC_InfoStruct = unsafe { std::mem::zeroed() };
            let err = unsafe { fdk::aacEncInfo(handle, &mut info) };
            if err != fdk::AACENC_ERROR_AACENC_OK {
                return Err(CrabError::Audio(format!(
                    "HE-AAC init: FDK info query failed (error {err})"
                )));
            }
            if info.frameLength == 0 || info.maxOutBufBytes == 0 {
                return Err(CrabError::Audio(
                    "HE-AAC init: FDK reported an unusable frame layout".into(),
                ));
            }
            Ok((info.frameLength as usize, info.maxOutBufBytes as usize))
        };
        let (frame_len, out_cap) = match build() {
            Ok(v) => v,
            Err(e) => {
                // SAFETY: handle opened above, closed exactly once here.
                unsafe {
                    fdk::aacEncClose(&mut handle);
                }
                return Err(e);
            }
        };
        tracing::info!("HE-AAC encoder ready ({aot_label}, {kbps} kbps CBR, ADTS)");
        Ok(Self {
            handle,
            frame_len,
            staging: Vec::with_capacity(frame_len * 2),
            scratch: vec![0u8; out_cap],
            out: Vec::new(),
        })
    }

    /// Encode one full frame from the staging head (caller guarantees
    /// at least `frame_len` samples per channel are staged).
    fn encode_frame(&mut self) -> Result<()> {
        let total = (self.frame_len * 2) as i32;
        let in_ptr = self.staging.as_mut_ptr() as *mut std::ffi::c_void;
        let mut in_ptrs = [in_ptr];
        let mut in_id = [fdk::AACENC_BufferIdentifier_IN_AUDIO_DATA as i32];
        let mut in_size = [total * 2];
        let mut in_elsize = [2];
        let in_desc = fdk::AACENC_BufDesc {
            numBufs: 1,
            bufs: in_ptrs.as_mut_ptr(),
            bufferIdentifiers: in_id.as_mut_ptr(),
            bufSizes: in_size.as_mut_ptr(),
            bufElSizes: in_elsize.as_mut_ptr(),
        };
        let out_ptr = self.scratch.as_mut_ptr() as *mut std::ffi::c_void;
        let mut out_ptrs = [out_ptr];
        let mut out_id = [fdk::AACENC_BufferIdentifier_OUT_BITSTREAM_DATA as i32];
        let mut out_size = [self.scratch.len() as i32];
        let mut out_elsize = [1];
        let out_desc = fdk::AACENC_BufDesc {
            numBufs: 1,
            bufs: out_ptrs.as_mut_ptr(),
            bufferIdentifiers: out_id.as_mut_ptr(),
            bufSizes: out_size.as_mut_ptr(),
            bufElSizes: out_elsize.as_mut_ptr(),
        };
        let in_args = fdk::AACENC_InArgs {
            numInSamples: total,
            numAncBytes: 0,
        };
        // SAFETY: descriptors point at live buffers sized above;
        // `out_args` is written by FDK on return.
        let mut out_args: fdk::AACENC_OutArgs = unsafe { std::mem::zeroed() };
        let err =
            unsafe { fdk::aacEncEncode(self.handle, &in_desc, &out_desc, &in_args, &mut out_args) };
        if err != fdk::AACENC_ERROR_AACENC_OK {
            return Err(CrabError::Audio(format!(
                "HE-AAC encode failed (error {err})"
            )));
        }
        let n = out_args.numOutBytes.max(0) as usize;
        self.out.extend_from_slice(&self.scratch[..n]);
        self.staging.drain(..self.frame_len * 2);
        Ok(())
    }
}

impl StreamEncoder for HeAacEncoder {
    fn encode(&mut self, interleaved: &[f32]) -> Result<&[u8]> {
        self.out.clear();
        // FDK eats int16: clamp (post-limiter floats can touch the rails)
        // and convert. `as` saturates by construction.
        self.staging.extend(
            interleaved
                .iter()
                .map(|s| (s.clamp(-1.0, 1.0) * 32767.0) as i16),
        );
        while self.staging.len() >= self.frame_len * 2 {
            self.encode_frame()?;
        }
        Ok(&self.out)
    }

    fn flush(&mut self) -> Result<&[u8]> {
        // Drain the codec delay: an encode call with no input samples
        // flushes pending output. Best effort, bounded iterations.
        self.out.clear();
        for _ in 0..8 {
            let out_ptr = self.scratch.as_mut_ptr() as *mut std::ffi::c_void;
            let mut out_ptrs = [out_ptr];
            let mut out_id = [fdk::AACENC_BufferIdentifier_OUT_BITSTREAM_DATA as i32];
            let mut out_size = [self.scratch.len() as i32];
            let mut out_elsize = [1];
            let out_desc = fdk::AACENC_BufDesc {
                numBufs: 1,
                bufs: out_ptrs.as_mut_ptr(),
                bufferIdentifiers: out_id.as_mut_ptr(),
                bufSizes: out_size.as_mut_ptr(),
                bufElSizes: out_elsize.as_mut_ptr(),
            };
            // Empty input descriptor: flush signal, not audio.
            let mut no_bufs: [*mut std::ffi::c_void; 1] = [std::ptr::null_mut()];
            let mut no_id = [0];
            let mut no_size = [0];
            let mut no_elsize = [0];
            let in_desc = fdk::AACENC_BufDesc {
                numBufs: 0,
                bufs: no_bufs.as_mut_ptr(),
                bufferIdentifiers: no_id.as_mut_ptr(),
                bufSizes: no_size.as_mut_ptr(),
                bufElSizes: no_elsize.as_mut_ptr(),
            };
            let in_args = fdk::AACENC_InArgs {
                numInSamples: -1,
                numAncBytes: 0,
            };
            // SAFETY: same contract as `encode_frame`.
            let mut out_args: fdk::AACENC_OutArgs = unsafe { std::mem::zeroed() };
            let err = unsafe {
                fdk::aacEncEncode(self.handle, &in_desc, &out_desc, &in_args, &mut out_args)
            };
            if err == fdk::AACENC_ERROR_AACENC_ENCODE_EOF {
                break;
            }
            if err != fdk::AACENC_ERROR_AACENC_OK {
                return Err(CrabError::Audio(format!(
                    "HE-AAC flush failed (error {err})"
                )));
            }
            let n = out_args.numOutBytes.max(0) as usize;
            if n == 0 {
                break;
            }
            self.out.extend_from_slice(&self.scratch[..n]);
        }
        // Unencoded staging (< 1 frame) is dropped, like the Opus tail.
        Ok(&self.out)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::stream::StreamFormat;

    fn cfg_heaac(kbps: u32) -> StreamConfig {
        StreamConfig {
            enabled: true,
            bitrate_kbps: kbps,
            format: StreamFormat::HeAac,
            ..Default::default()
        }
    }

    /// Parse one 7-byte ADTS header (protection_absent = 1, as emitted
    /// by FDK's ADTS transmuxer). Returns (profile, rate_idx, channels,
    /// frame_len incl. header). Panics on malformed input — test-only.
    fn parse_adts(frame: &[u8]) -> (u8, u8, u8, usize) {
        assert!(frame.len() >= 7, "ADTS frame shorter than header");
        assert_eq!(frame[0], 0xFF, "missing ADTS syncword byte 0");
        assert_eq!(frame[1] & 0xF0, 0xF0, "missing ADTS syncword byte 1");
        assert_eq!(frame[1] & 0x01, 0x01, "expected no CRC (protection_absent)");
        let profile = (frame[2] >> 6) & 0x03;
        let rate_idx = (frame[2] >> 2) & 0x0F;
        let channels = ((frame[2] & 0x01) << 2) | ((frame[3] >> 6) & 0x03);
        let len = (((frame[3] & 0x03) as usize) << 11)
            | ((frame[4] as usize) << 3)
            | (((frame[5] >> 5) & 0x07) as usize);
        (profile, rate_idx, channels, len)
    }

    fn sine_frames(frames: usize, frame_len: usize, rate: u32) -> Vec<f32> {
        let mut pcm = Vec::with_capacity(frames * frame_len * 2);
        let mut phase = 0.0f32;
        for _ in 0..frames * frame_len {
            phase += 2.0 * std::f32::consts::PI * 440.0 / rate as f32;
            let s = phase.sin() * 0.5;
            pcm.push(s);
            pcm.push(s);
        }
        pcm
    }

    #[test]
    fn aot_selection_prefers_v2_at_low_rates() {
        assert_eq!(select_aot(16).1, "HE-AAC v2");
        assert_eq!(select_aot(32).1, "HE-AAC v2");
        assert_eq!(select_aot(48).1, "HE-AAC v2");
        assert_eq!(select_aot(56).1, "HE-AAC v1");
        assert_eq!(select_aot(64).1, "HE-AAC v1");
        assert_eq!(select_aot(128).1, "HE-AAC v1");
    }

    #[test]
    fn rejects_zero_device_rate() {
        let err = HeAacEncoder::new(&cfg_heaac(48), 0).unwrap_err();
        assert!(err.to_string().contains("rate is 0"), "{err}");
    }

    #[test]
    fn encodes_sine_to_valid_adts_stereo_frames() {
        // (rate, kbps, ADTS channel field): v2 carries a mono core +
        // parametric-stereo extension (field = 1) while v1 carries a
        // stereo core (field = 2) — both decode to stereo. This is
        // correct encoder behavior, not a channel bug.
        for (rate, kbps, channels) in [(44_100, 48, 1), (48_000, 64, 2)] {
            let mut enc = HeAacEncoder::new(&cfg_heaac(kbps), rate).expect("encoder builds");
            // Feed in odd-sized chunks (staging must reassemble frames).
            let pcm = sine_frames(4, enc.frame_len, rate);
            let mut stream = Vec::new();
            for chunk in pcm.chunks(1000) {
                stream.extend_from_slice(enc.encode(chunk).expect("encode ok"));
            }
            assert!(!stream.is_empty(), "no bytes out at {rate} Hz / {kbps}k");
            // Walk the byte stream frame by frame: every header must be
            // well-formed stereo ADTS and lengths must chain exactly.
            let mut pos = 0;
            let mut frames = 0;
            while pos < stream.len() {
                let (_, rate_idx, got_channels, len) = parse_adts(&stream[pos..]);
                assert_eq!(got_channels, channels, "channel field at {rate} Hz");
                assert!(rate_idx <= 12, "bogus rate index {rate_idx}");
                assert!(len >= 7 && pos + len <= stream.len(), "bad frame len {len}");
                pos += len;
                frames += 1;
            }
            assert!(frames >= 3, "expected >= 3 ADTS frames, got {frames}");
        }
    }

    #[test]
    fn small_chunks_buffer_until_a_frame_fills() {
        let mut enc = HeAacEncoder::new(&cfg_heaac(48), 48_000).expect("encoder builds");
        let tiny = vec![0.0f32; 64];
        assert!(enc.encode(&tiny).expect("encode ok").is_empty());
        // A burst big enough for several frames must produce bytes.
        let big = sine_frames(3, enc.frame_len, 48_000);
        assert!(!enc.encode(&big).expect("encode ok").is_empty());
    }

    #[test]
    fn flush_does_not_panic_and_returns_bytes_or_empty() {
        let mut enc = HeAacEncoder::new(&cfg_heaac(48), 48_000).expect("encoder builds");
        let pcm = sine_frames(2, enc.frame_len, 48_000);
        let _ = enc.encode(&pcm).expect("encode ok");
        // Must not panic; tail bytes (codec delay) or empty are both fine.
        let _ = enc.flush().expect("flush ok");
    }

    /// Decode exactly `frames` ADTS frames with the FDK decoder
    /// (test-only). Returns (output rate, channels, interleaved i16 PCM).
    /// The count is exact on purpose: driving `DecodeFrame` past the
    /// available data (concealment path) is left untested — production
    /// never decodes at all.
    fn decode_adts_all(adts: &[u8], frames: usize) -> (i32, i32, Vec<fdk::INT_PCM>) {
        unsafe {
            let dec = fdk::aacDecoder_Open(fdk::TRANSPORT_TYPE_TT_MP4_ADTS, 1);
            assert!(!dec.is_null(), "decoder open failed");
            // Feed everything first: the decoder buffers internally and
            // reports NOT_ENOUGH_BITS until a full access unit arrived.
            let mut buf_ptr = adts.as_ptr() as *mut u8;
            let buf_size = adts.len() as u32;
            let mut bytes_valid = buf_size;
            let err = fdk::aacDecoder_Fill(dec, &mut buf_ptr, &buf_size, &mut bytes_valid);
            assert_eq!(
                err,
                fdk::AAC_DECODER_ERROR_AAC_DEC_OK,
                "decoder fill failed"
            );
            assert_eq!(bytes_valid, 0, "decoder did not consume all input");
            let mut rate = 0;
            let mut channels = 0;
            let mut pcm_all = Vec::new();
            for _ in 0..frames {
                // 2048 samples/ch covers HE-AAC SBR output with margin.
                let mut pcm_out = vec![0 as fdk::INT_PCM; 2048 * 2 * 2];
                let err = fdk::aacDecoder_DecodeFrame(
                    dec,
                    pcm_out.as_mut_ptr(),
                    (pcm_out.len() * 2) as i32,
                    0,
                );
                assert_eq!(
                    err,
                    fdk::AAC_DECODER_ERROR_AAC_DEC_OK,
                    "decoder frame failed"
                );
                let info = &*fdk::aacDecoder_GetStreamInfo(dec);
                rate = info.sampleRate;
                channels = info.numChannels;
                let n = info.frameSize.max(0) as usize * channels.max(0) as usize;
                pcm_all.extend_from_slice(&pcm_out[..n.min(pcm_out.len())]);
            }
            fdk::aacDecoder_Close(dec);
            (rate, channels, pcm_all)
        }
    }

    #[test]
    fn builds_through_build_encoder_and_rejected_for_shoutcast() {
        use crate::stream::encoder::build_encoder;
        use crate::stream::StreamProtocol;

        let cfg = cfg_heaac(48);
        assert!(build_encoder(&cfg, 48_000).is_ok());
        let mut sc = cfg.clone();
        sc.protocol = StreamProtocol::ShoutcastV2;
        assert!(build_encoder(&sc, 48_000).is_err());
        let mut sc1 = cfg.clone();
        sc1.protocol = StreamProtocol::ShoutcastV1;
        assert!(build_encoder(&sc1, 48_000).is_err());
    }

    #[test]
    fn adts_roundtrips_to_stereo_sine_through_fdk_decoder() {
        // The v2 row is the strong claim: a mono core + PS extension on
        // the wire must still decode to stereo with real signal in it.
        for (rate, kbps) in [(44_100u32, 32u32), (48_000, 64)] {
            let mut enc = HeAacEncoder::new(&cfg_heaac(kbps), rate).expect("encoder builds");
            let pcm = sine_frames(8, enc.frame_len, rate);
            let mut stream = Vec::new();
            for chunk in pcm.chunks(4096) {
                stream.extend_from_slice(enc.encode(chunk).expect("encode ok"));
            }
            stream.extend_from_slice(enc.flush().expect("flush ok"));
            // Count the ADTS frames on the wire: that exact count is
            // what the decoder must reproduce (encode frames + flush).
            let mut frames = 0;
            let mut pos = 0;
            while pos < stream.len() {
                let (_, _, _, len) = parse_adts(&stream[pos..]);
                pos += len;
                frames += 1;
            }
            assert!(frames >= 8, "expected encode+flush frames, got {frames}");
            let (out_rate, channels, decoded) = decode_adts_all(&stream, frames);
            assert_eq!(channels, 2, "must decode to stereo at {rate} Hz");
            assert_eq!(out_rate, rate as i32, "output rate must match input");
            // Skip decoder-delay silence: measure over the second half.
            let half = decoded.len() / 2;
            let tail = &decoded[half..];
            let (mut sum_l, mut sum_r, mut sum_d, mut peak) = (0.0f64, 0.0, 0.0, 0i32);
            let mut n = 0u32;
            let (pairs, _) = tail.as_chunks::<2>();
            for s in pairs {
                let (l, r) = (s[0] as f64, s[1] as f64);
                sum_l += l * l;
                sum_r += r * r;
                sum_d += (l - r) * (l - r);
                peak = peak.max((s[0] as i32).abs()).max((s[1] as i32).abs());
                n += 1;
            }
            let rms_l = (sum_l / n as f64).sqrt();
            let rms_r = (sum_r / n as f64).sqrt();
            assert!(rms_l > 1000.0, "left channel silent (rms {rms_l})");
            assert!(rms_r > 1000.0, "right channel silent (rms {rms_r})");
            assert!(peak > 5000, "suspiciously weak output (peak {peak})");
            // Dual-mono in: PS must reconstruct near-identical channels.
            let rms_d = (sum_d / n as f64).sqrt();
            assert!(
                rms_d < 0.5 * rms_l,
                "channels diverged (diff rms {rms_d} vs left {rms_l})"
            );
        }
    }
}
