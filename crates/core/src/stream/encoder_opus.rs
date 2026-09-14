//! Opus-in-Ogg encoding of the program mix (`opus` + hand-rolled Ogg
//! framing, following RFC 7845).
//!
//! Fixed 48 kHz stereo, 20 ms frames, CBR. Device rates other than
//! 48 kHz go through rubato first. Each Ogg page carries exactly one
//! packet (headers each on their own page too) — the standard shape
//! for low-delay live streams, which is also what Icecast expects.

use opus::{Application, Bitrate as OpusBitrate, Channels, Encoder as OpusEncoder};
use rubato::{FftFixedIn, Resampler};

use crate::error::{CrabError, Result};
use crate::stream::encoder::StreamEncoder;
use crate::stream::StreamConfig;

const OPUS_RATE: u32 = 48_000;
/// 20 ms frames: the latency/overhead sweet spot for live radio.
const FRAME_SAMPLES: usize = 960;
/// Resampler input quantum in device-rate frames.
const RES_CHUNK: usize = 1024;
/// Max Opus packet is 1275 B per spec; headroom for the stack scratch.
const PKT_CAP: usize = 4000;

/// Ogg page checksum: Xiph's MSB-first CRC-32 (poly 0x04C11DB7, init 0,
/// no reflection, no xor-out). This is deliberately NOT the zlib CRC —
/// it is the exact algorithm Ogg readers verify, so our pages parse.
const fn build_crc_table() -> [u32; 256] {
    let mut t = [0u32; 256];
    let mut i = 0;
    while i < 256 {
        let mut r = (i as u32) << 24;
        let mut j = 0;
        while j < 8 {
            r = if r & 0x8000_0000 != 0 {
                (r << 1) ^ 0x04C1_1DB7
            } else {
                r << 1
            };
            j += 1;
        }
        t[i] = r;
        i += 1;
    }
    t
}

const CRC_TABLE: [u32; 256] = build_crc_table();

fn ogg_crc32(data: &[u8]) -> u32 {
    let mut crc = 0u32;
    for &b in data {
        crc = (crc << 8) ^ CRC_TABLE[((crc >> 24) ^ b as u32) as usize];
    }
    crc
}

/// Append one Ogg page carrying a single packet. `header_type`: 0x02 =
/// beginning of stream, 0x04 = end of stream, 0x00 = continuation.
fn write_page(
    out: &mut Vec<u8>,
    serial: u32,
    seq: u32,
    header_type: u8,
    granule: u64,
    payload: &[u8],
) {
    let start = out.len();
    out.extend_from_slice(b"OggS");
    out.push(0); // stream structure version
    out.push(header_type);
    out.extend_from_slice(&granule.to_le_bytes());
    out.extend_from_slice(&serial.to_le_bytes());
    out.extend_from_slice(&seq.to_le_bytes());
    out.extend_from_slice(&[0, 0, 0, 0]); // checksum placeholder
    if payload.is_empty() {
        // EOS closer with no packet body.
        out.push(0);
    } else {
        // Lacing values: full 255s plus the remainder. A payload that
        // ends exactly on a 255 boundary terminates with an extra 0,
        // which this loop produces naturally.
        let mut rest = payload.len();
        let mut lacing = Vec::with_capacity(4);
        while rest >= 255 {
            lacing.push(255u8);
            rest -= 255;
        }
        lacing.push(rest as u8);
        out.push(lacing.len() as u8);
        out.extend_from_slice(&lacing);
        out.extend_from_slice(payload);
    }
    let sum = ogg_crc32(&out[start..]);
    out[start + 22..start + 26].copy_from_slice(&sum.to_le_bytes());
}

/// RFC 7845 §5.1 identification header (19 bytes, mapping family 0 =
/// plain stereo).
fn opus_head(pre_skip: u16, input_rate: u32) -> [u8; 19] {
    let mut h = [0u8; 19];
    h[0..8].copy_from_slice(b"OpusHead");
    h[8] = 1; // version
    h[9] = 2; // channel count
    h[10..12].copy_from_slice(&pre_skip.to_le_bytes());
    h[12..16].copy_from_slice(&input_rate.to_le_bytes());
    // output gain (0) and mapping family (0) stay zeroed.
    h
}

/// RFC 7845 §5.2 comment header with no user comments.
fn opus_tags() -> Vec<u8> {
    let vendor = b"CrabBoss";
    let mut t = Vec::with_capacity(8 + 4 + vendor.len() + 4);
    t.extend_from_slice(b"OpusTags");
    t.extend_from_slice(&(vendor.len() as u32).to_le_bytes());
    t.extend_from_slice(vendor);
    t.extend_from_slice(&0u32.to_le_bytes());
    t
}

/// Frame-sized Opus/Ogg encoder for the program bus.
pub struct OpusOggEncoder {
    enc: OpusEncoder,
    /// `None` when the device already runs at 48 kHz.
    resampler: Option<FftFixedIn<f32>>,
    res_out_cap: usize,
    /// Device-rate staging per channel (resample path only).
    res_in: [Vec<f32>; 2],
    /// 48 kHz staging per channel waiting to fill a 20 ms frame.
    pcm: [Vec<f32>; 2],
    out: Vec<u8>,
    serial: u32,
    seq: u32,
    /// Total 48 kHz samples emitted (Ogg granule position).
    granule: u64,
    started: bool,
    input_rate: u32,
    pre_skip: u16,
}

impl OpusOggEncoder {
    /// Build a CBR encoder; `sample_rate` is the device rate of the
    /// incoming mix (resampled to 48 kHz unless already there).
    pub fn new(config: &StreamConfig, sample_rate: u32) -> Result<Self> {
        if sample_rate == 0 {
            return Err(CrabError::Audio("Opus: device rate is 0".into()));
        }
        let mut enc = OpusEncoder::new(OPUS_RATE, Channels::Stereo, Application::Audio)
            .map_err(|e| CrabError::Audio(format!("Opus init: {e}")))?;
        let bps = (config.bitrate_kbps.clamp(16, 320) * 1000) as i32;
        enc.set_bitrate(OpusBitrate::Bits(bps))
            .map_err(|e| CrabError::Audio(format!("Opus bitrate: {e}")))?;
        // CBR like the MP3 path: predictable broadcast bandwidth.
        enc.set_vbr(false)
            .map_err(|e| CrabError::Audio(format!("Opus CBR: {e}")))?;
        let pre_skip = enc.get_lookahead().unwrap_or(0).max(0) as u16;
        let (resampler, res_out_cap) = if sample_rate == OPUS_RATE {
            (None, 0)
        } else {
            let r =
                FftFixedIn::<f32>::new(sample_rate as usize, OPUS_RATE as usize, RES_CHUNK, 2, 2)
                    .map_err(|e| CrabError::Audio(format!("Opus resampler: {e}")))?;
            let cap = r.output_frames_max();
            (Some(r), cap)
        };
        // Per-connection serial; listeners never compare across mounts.
        let serial = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| (d.as_nanos() & 0xFFFF_FFFF) as u32)
            .unwrap_or(0x0C8A_B055);
        Ok(Self {
            enc,
            resampler,
            res_out_cap,
            res_in: [Vec::new(), Vec::new()],
            pcm: [Vec::new(), Vec::new()],
            out: Vec::new(),
            serial,
            seq: 0,
            granule: 0,
            started: false,
            input_rate: sample_rate,
            pre_skip,
        })
    }

    fn push_page(&mut self, header_type: u8, granule: u64, payload: &[u8]) {
        write_page(
            &mut self.out,
            self.serial,
            self.seq,
            header_type,
            granule,
            payload,
        );
        self.seq += 1;
    }

    /// Move device-rate input into the 48 kHz staging FIFOs.
    fn stage(&mut self, interleaved: &[f32]) -> Result<()> {
        if self.resampler.is_none() {
            for (i, s) in interleaved.iter().enumerate() {
                self.pcm[i % 2].push(*s);
            }
            return Ok(());
        }
        for (i, s) in interleaved.iter().enumerate() {
            self.res_in[i % 2].push(*s);
        }
        let resampler = self.resampler.as_mut().expect("checked above");
        while self.res_in[0].len() >= RES_CHUNK {
            let wave_in = [
                self.res_in[0][..RES_CHUNK].to_vec(),
                self.res_in[1][..RES_CHUNK].to_vec(),
            ];
            self.res_in[0].drain(..RES_CHUNK);
            self.res_in[1].drain(..RES_CHUNK);
            let mut wave_out = [
                vec![0.0f32; self.res_out_cap],
                vec![0.0f32; self.res_out_cap],
            ];
            let (_, produced) = resampler
                .process_into_buffer(&wave_in, &mut wave_out, None)
                .map_err(|e| CrabError::Audio(format!("Opus resample: {e}")))?;
            self.pcm[0].extend_from_slice(&wave_out[0][..produced]);
            self.pcm[1].extend_from_slice(&wave_out[1][..produced]);
        }
        Ok(())
    }

    /// Encode every full 20 ms frame waiting in staging.
    fn drain_frames(&mut self) -> Result<()> {
        while self.pcm[0].len() >= FRAME_SAMPLES && self.pcm[1].len() >= FRAME_SAMPLES {
            let mut frame = [0.0f32; FRAME_SAMPLES * 2];
            for i in 0..FRAME_SAMPLES {
                frame[2 * i] = self.pcm[0][i];
                frame[2 * i + 1] = self.pcm[1][i];
            }
            self.pcm[0].drain(..FRAME_SAMPLES);
            self.pcm[1].drain(..FRAME_SAMPLES);
            let mut pkt = [0u8; PKT_CAP];
            let n = self
                .enc
                .encode_float(&frame, &mut pkt)
                .map_err(|e| CrabError::Audio(format!("Opus encode: {e}")))?;
            self.granule += FRAME_SAMPLES as u64;
            let granule = self.granule;
            self.push_page(0x00, granule, &pkt[..n]);
        }
        Ok(())
    }
}

impl StreamEncoder for OpusOggEncoder {
    fn encode(&mut self, interleaved: &[f32]) -> Result<&[u8]> {
        self.out.clear();
        if !self.started {
            self.started = true;
            let head = opus_head(self.pre_skip, self.input_rate);
            self.push_page(0x02, 0, &head);
            let tags = opus_tags();
            self.push_page(0x00, 0, &tags);
        }
        self.stage(interleaved)?;
        self.drain_frames()?;
        Ok(&self.out)
    }

    fn flush(&mut self) -> Result<&[u8]> {
        self.out.clear();
        // Opus frames are always complete, so there is no codec tail;
        // a sub-frame staging leftover (< 20 ms) is dropped. Just close
        // the Ogg stream so the reconnect starts clean.
        let granule = self.granule;
        self.push_page(0x04, granule, &[]);
        Ok(&self.out)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::stream::StreamFormat;
    use std::io::Cursor;

    fn cfg_opus(kbps: u32) -> StreamConfig {
        StreamConfig {
            bitrate_kbps: kbps,
            format: StreamFormat::Opus,
            ..Default::default()
        }
    }

    /// Feed `frames` device-rate frames in odd-sized chunks, collecting
    /// every byte produced.
    fn run_all(enc: &mut OpusOggEncoder, frames: usize, rate: u32) -> Vec<u8> {
        let mut all = Vec::new();
        let mut left = frames;
        let mut chunk_frames = 511;
        while left > 0 {
            let take = chunk_frames.min(left);
            // Quiet stereo sine so the encoder sees real signal, not just
            // digital silence (same byte-shape assertions either way).
            let mut chunk = Vec::with_capacity(take * 2);
            for i in 0..take {
                let t = i as f32 / rate as f32;
                let s = (t * 440.0 * std::f32::consts::TAU).sin() * 0.1;
                chunk.push(s);
                chunk.push(s);
            }
            all.extend_from_slice(enc.encode(&chunk).expect("encode"));
            left -= take;
            chunk_frames = if chunk_frames == 511 { 2048 } else { 511 };
        }
        all.extend_from_slice(enc.flush().expect("flush"));
        all
    }

    fn read_packets(bytes: &[u8]) -> Vec<ogg::Packet> {
        let mut reader = ogg::PacketReader::new(Cursor::new(bytes));
        let mut packets = Vec::new();
        while let Some(p) = reader.read_packet().expect("ogg parses") {
            packets.push(p);
        }
        packets
    }

    /// Walk our own pages (emitted back-to-back, no garbage): (type, granule).
    fn page_headers(bytes: &[u8]) -> Vec<(u8, u64)> {
        let mut out = Vec::new();
        let mut i = 0;
        while i + 27 <= bytes.len() {
            assert_eq!(&bytes[i..i + 4], b"OggS", "page sync at {i}");
            let header_type = bytes[i + 5];
            let granule = u64::from_le_bytes(bytes[i + 6..i + 14].try_into().unwrap());
            out.push((header_type, granule));
            let nseg = bytes[i + 26] as usize;
            let lace_end = i + 27 + nseg;
            let body: usize = bytes[i + 27..lace_end].iter().map(|&b| b as usize).sum();
            i = lace_end + body;
        }
        out
    }

    #[test]
    fn opus_stream_parses_with_headers_first() {
        let cfg = cfg_opus(96);
        let mut enc = OpusOggEncoder::new(&cfg, 48_000).expect("encoder builds");
        let bytes = run_all(&mut enc, 48_000, 48_000);
        assert!(bytes.starts_with(b"OggS"), "must start with an Ogg page");
        let packets = read_packets(&bytes);
        // 1 s @ 20 ms frames = 50 audio packets + 2 headers.
        assert_eq!(packets.len(), 52, "headers + 50 audio packets");
        assert!(
            packets[0].data.starts_with(b"OpusHead"),
            "first packet is OpusHead"
        );
        assert_eq!(packets[0].data.len(), 19);
        assert!(
            packets[1].data.starts_with(b"OpusTags"),
            "second packet is OpusTags"
        );
        for (i, p) in packets.iter().enumerate().skip(2) {
            assert!(!p.data.is_empty(), "audio packet {i} carries bytes");
        }
        // Granule counts 48 kHz samples: last audio page ends at 48000.
        let pages = page_headers(&bytes);
        assert_eq!(pages[0].0 & 0x02, 0x02, "first page has BOS");
        assert_eq!(pages[pages.len() - 1].0 & 0x04, 0x04, "last page has EOS");
        assert_eq!(pages[pages.len() - 2].1, 48_000);
        // CBR sanity: 1 s at 96 kbps ≈ 12 KB of audio payload.
        let audio_bytes: usize = packets.iter().skip(2).map(|p| p.data.len()).sum();
        assert!(
            (8_000..20_000).contains(&audio_bytes),
            "suspicious audio size: {audio_bytes}"
        );
    }

    #[test]
    fn resample_path_tracks_44k1() {
        let cfg = cfg_opus(64);
        let mut enc = OpusOggEncoder::new(&cfg, 44_100).expect("encoder builds");
        let bytes = run_all(&mut enc, 44_100, 44_100);
        let packets = read_packets(&bytes);
        let audio = packets.len() - 2;
        // 1 s in, 1 s out, ±1 frame for resampler priming.
        assert!(
            (49..=51).contains(&audio),
            "expected ~50 audio packets, got {audio}"
        );
        let pages = page_headers(&bytes);
        let mut last = 0u64;
        // Skip the 2 header pages and the EOS closer.
        for &(_, g) in pages.iter().skip(2).take(audio) {
            assert!(g > last, "granules increase");
            last = g;
        }
    }

    #[test]
    fn odd_chunks_never_lose_samples() {
        let cfg = cfg_opus(96);
        let mut enc = OpusOggEncoder::new(&cfg, 48_000).expect("encoder builds");
        let mut bytes = Vec::new();
        // Prime with a dribble of tiny writes, then exact frames. The
        // headers go out with the dribble; parse everything together.
        for _ in 0..7 {
            let b = enc.encode(&[0.1f32; 6]).expect("encode");
            assert!(b.is_empty() || b.starts_with(b"OggS"));
            bytes.extend_from_slice(b);
        }
        bytes.extend_from_slice(&run_all(&mut enc, 48_000, 48_000));
        let packets = read_packets(&bytes);
        // 7*3 stray frames + 48000 merge into full frames: exactly 50
        // audio packets plus the 2 headers.
        assert_eq!(packets.len(), 52, "got {} packets", packets.len());
    }
}
