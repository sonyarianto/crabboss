//! Stream manager: bridges the realtime audio callback to the network.
//!
//! The cpal callback pushes the post-DSP program mix into a lock-free
//! ring buffer ([`rtrb`]) — never blocks, never allocates on the audio
//! thread. A background thread drains it, encodes to MP3, and sends to
//! the Icecast server paced in real time (the source must behave like a
//! live feed). Drops and server restarts trigger bounded reconnects.

use std::sync::atomic::{AtomicU64, AtomicU8, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use rtrb::RingBuffer;

use crate::stream::encoder::Mp3Encoder;
use crate::stream::source::IcecastSource;
use crate::stream::{StreamConfig, StreamState, StreamStats};

/// Ring capacity in stereo f32 samples (~10.9 s at 48 kHz stereo).
/// Large enough to ride out a reconnect, small enough to stay bounded.
const RING_SAMPLES: usize = 1 << 21;

/// Encode granularity: one MPEG Layer III frame of audio per batch.
const DRAIN_FRAMES: usize = 1152;

/// Idle sleep when the ring is starved (callback hasn't produced yet).
const STARVE_SLEEP: Duration = Duration::from_millis(20);

/// State codes shared lock-free with the UI (`StreamState` semantics).
const STATE_OFF: u8 = 0;
const STATE_CONNECTING: u8 = 1;
const STATE_LIVE: u8 = 2;
const STATE_ERROR: u8 = 3;

/// Bounded reconnect policy after connect/send failures. The first delays
/// are deliberately roomy: a dropped attempt can leave a half-open
/// connection holding the mount server-side for a few seconds, and an
/// instant retry would just collect a 409 "in use" for our own zombie.
const MAX_RECONNECTS: u32 = 5;
const RETRY_DELAYS_SECS: [u64; 5] = [2, 5, 10, 20, 30];

/// Producer side of the tap: the audio callback calls [`StreamTap::push`]
/// with post-DSP interleaved stereo frames. Never blocks; on overflow the
/// newest samples are dropped (the stream stalls gracefully rather than
/// the audio glitching). Generation-checked so a tap from a stopped or
/// restarted stream silently no-ops.
#[derive(Clone)]
pub struct StreamTap {
    producer: Arc<Mutex<rtrb::Producer<f32>>>,
    generation: Arc<AtomicU64>,
    my_gen: u64,
}

impl StreamTap {
    /// Push interleaved stereo samples from the audio callback.
    pub fn push(&self, interleaved: &[f32]) {
        if self.generation.load(Ordering::Relaxed) != self.my_gen {
            return; // stale tap (stream stopped or restarted)
        }
        if let Ok(mut p) = self.producer.try_lock() {
            let _ = p.push_entire_slice(interleaved); // drops newest on full ring
        }
    }
}

/// Owns the streaming pipeline. Created once (inside the engine);
/// `start`/`stop` manage the sender thread.
pub struct StreamManager {
    config: Mutex<StreamConfig>,
    /// 0 = off, 1 = connecting, 2 = live, 3 = error.
    state: Arc<AtomicU8>,
    /// Human-readable last error (empty = none).
    last_error: Arc<Mutex<String>>,
    /// Now-playing title queued by the UI; the sender thread picks it
    /// up and emits an in-band metadata block.
    title: Arc<Mutex<String>>,
    stats: Arc<StreamStatsAtomic>,
    /// Monotonic generation: bumped by every `start`/`stop`; the tap
    /// only accepts pushes while its generation matches.
    generation: Arc<AtomicU64>,
    /// Live tap for the engine (cleared by `stop`).
    tap: Option<StreamTap>,
}

/// Atomic mirror of [`StreamStats`] so the UI can read counters without
/// touching the sender thread.
#[derive(Debug, Default)]
struct StreamStatsAtomic {
    bytes_sent: AtomicU64,
    stream_secs: AtomicU64,
    reconnects: AtomicU64,
}

impl StreamStatsAtomic {
    fn snapshot(&self) -> StreamStats {
        StreamStats {
            bytes_sent: self.bytes_sent.load(Ordering::Relaxed),
            stream_secs: self.stream_secs.load(Ordering::Relaxed),
            reconnects: self.reconnects.load(Ordering::Relaxed) as u32,
        }
    }
}

impl StreamManager {
    pub fn new(config: StreamConfig) -> Self {
        Self {
            config: Mutex::new(config.sanitized()),
            state: Arc::new(AtomicU8::new(STATE_OFF)),
            last_error: Arc::new(Mutex::new(String::new())),
            title: Arc::new(Mutex::new(String::new())),
            stats: Arc::new(StreamStatsAtomic::default()),
            generation: Arc::new(AtomicU64::new(0)),
            tap: None,
        }
    }

    pub fn config(&self) -> StreamConfig {
        self.config.lock().unwrap().clone()
    }

    /// Update the config. Applied on the next `start` (a live stream
    /// must be restarted to pick up server/bitrate changes).
    pub fn set_config(&self, config: StreamConfig) {
        *self.config.lock().unwrap() = config.sanitized();
    }

    pub fn state(&self) -> StreamState {
        match self.state.load(Ordering::Relaxed) {
            STATE_LIVE => StreamState::Live,
            STATE_CONNECTING => StreamState::Connecting,
            STATE_ERROR => StreamState::Error(self.last_error.lock().unwrap().clone()),
            _ => StreamState::Off,
        }
    }

    pub fn stats(&self) -> StreamStats {
        self.stats.snapshot()
    }

    /// Queue the now-playing title (in-band metadata update, best effort;
    /// a no-op when the server didn't negotiate metadata).
    pub fn set_title(&self, title: &str) {
        *self.title.lock().unwrap() = title.replace(['\r', '\n'], " ");
    }

    /// True while the manager wants audio (connecting or live).
    pub fn running(&self) -> bool {
        matches!(self.state(), StreamState::Connecting | StreamState::Live)
    }

    /// Start streaming. No-op (returns `None`) when the config's master
    /// switch is off or the stream is already running. Otherwise spawns
    /// the sender thread and returns the [`StreamTap`] for the audio
    /// callback plus the encoder input rate actually requested.
    ///
    /// `device_rate` must be MPEG-legal (44100/48000/…); an odd rate
    /// surfaces as `Error("LAME rate …")` in [`Self::state`].
    pub fn start(&mut self, device_rate: u32) -> Option<StreamTap> {
        if !self.config.lock().unwrap().enabled {
            return None;
        }
        if self.running() {
            return self.tap.clone();
        }

        let gen = self.generation.fetch_add(1, Ordering::SeqCst) + 1;
        self.state.store(STATE_CONNECTING, Ordering::Relaxed);
        *self.last_error.lock().unwrap() = String::new();
        self.stats = Arc::new(StreamStatsAtomic::default());
        *self.title.lock().unwrap() = String::new();

        let (producer, consumer) = RingBuffer::new(RING_SAMPLES);
        let tap = StreamTap {
            producer: Arc::new(Mutex::new(producer)),
            generation: self.generation.clone(),
            my_gen: gen,
        };
        self.tap = Some(tap.clone());

        let cfg = self.config();
        let state = self.state.clone();
        let last_error = self.last_error.clone();
        let title = self.title.clone();
        let stats = self.stats.clone();
        let generation = self.generation.clone();

        let spawned = std::thread::Builder::new()
            .name("icecast-sender".into())
            .spawn(move || {
                sender_loop(
                    consumer,
                    cfg,
                    device_rate,
                    state,
                    last_error,
                    title,
                    stats,
                    generation,
                    gen,
                );
            });
        if spawned.is_err() {
            // Thread spawn failed: roll back to Off.
            self.generation.fetch_add(1, Ordering::SeqCst);
            self.state.store(STATE_OFF, Ordering::Relaxed);
            self.tap = None;
            tracing::error!("Stream: failed to spawn sender thread");
            return None;
        }
        Some(tap)
    }

    /// Stop streaming and tear down the sender thread (the tap goes
    /// stale via generation bump; the thread exits on its next check).
    pub fn stop(&mut self) {
        self.generation.fetch_add(1, Ordering::SeqCst);
        self.state.store(STATE_OFF, Ordering::Relaxed);
        self.tap = None;
    }
}

/// Background loop: connect → drain ring → encode → send → repeat.
/// Reconnects with backoff (bounded) on connect or send failures.
#[allow(clippy::too_many_arguments)]
fn sender_loop(
    mut consumer: rtrb::Consumer<f32>,
    cfg: StreamConfig,
    device_rate: u32,
    state: Arc<AtomicU8>,
    last_error: Arc<Mutex<String>>,
    title: Arc<Mutex<String>>,
    stats: Arc<StreamStatsAtomic>,
    generation: Arc<AtomicU64>,
    gen: u64,
) {
    fn alive(generation: &AtomicU64, gen: u64) -> bool {
        generation.load(Ordering::Relaxed) == gen
    }
    fn fail(state: &AtomicU8, last_error: &Mutex<String>, msg: &str) {
        *last_error.lock().unwrap() = msg.to_string();
        state.store(STATE_ERROR, Ordering::Relaxed);
    }

    let mut reconnects = 0u32;
    let mut last_title = String::new();

    loop {
        if !alive(&generation, gen) {
            return;
        }

        // --- Connect ---
        state.store(STATE_CONNECTING, Ordering::Relaxed);
        let (mut source, _proto) = match IcecastSource::connect(&cfg) {
            Ok(pair) => pair,
            Err(e) => {
                if reconnects >= MAX_RECONNECTS {
                    fail(&state, &last_error, &e.to_string());
                    tracing::error!("Stream: giving up after {reconnects} retries: {e}");
                    return;
                }
                let delay = Duration::from_secs(RETRY_DELAYS_SECS[reconnects as usize]);
                tracing::warn!("Stream connect failed (retry {reconnects}, {delay:?}): {e}");
                *last_error.lock().unwrap() = e.to_string();
                state.store(STATE_ERROR, Ordering::Relaxed);
                sleep_interruptibly(delay, &generation, gen);
                reconnects += 1;
                stats.reconnects.store(reconnects as u64, Ordering::Relaxed);
                continue;
            }
        };

        // --- Encoder ---
        let mut encoder = match Mp3Encoder::new(&cfg, device_rate) {
            Ok(e) => e,
            Err(e) => {
                fail(&state, &last_error, &e.to_string());
                tracing::error!("Stream: {e} (pick a supported output device rate)");
                return;
            }
        };

        // --- Live ---
        state.store(STATE_LIVE, Ordering::Relaxed);
        reconnects = 0;
        stats.reconnects.store(0, Ordering::Relaxed);
        let started = Instant::now();
        // Drop anything buffered before go-live (pre-roll silence or a prior
        // run's tail): dumping it as one burst trips server flood protection
        // (RST right after handshake), and a live feed starts at the live
        // edge by definition — listeners join the now, not the backlog.
        while consumer.pop().is_ok() {}
        // Wall-clock pacing anchor: each batch below accounts its own air
        // time. The source must behave like a live feed, never a file dump.
        let mut deadline = Instant::now();
        let mut scratch: Vec<f32> = Vec::with_capacity(DRAIN_FRAMES * 2);

        loop {
            if !alive(&generation, gen) {
                return;
            }

            // Queue a metadata update when the title changed.
            {
                let want = title.lock().unwrap().clone();
                if !want.is_empty() && want != last_title {
                    if let Err(e) = source.set_metadata(&want) {
                        tracing::warn!("Stream metadata update failed: {e}");
                    }
                    last_title = want;
                }
            }

            let avail_frames = consumer.slots() / 2;
            if avail_frames == 0 {
                std::thread::sleep(STARVE_SLEEP);
                continue;
            }
            let frames = avail_frames.min(DRAIN_FRAMES);
            scratch.clear();
            for _ in 0..frames * 2 {
                // Single consumer: pops cannot fail while slots() > 0.
                if let Ok(s) = consumer.pop() {
                    scratch.push(s);
                }
            }

            let mp3 = match encoder.encode(&scratch) {
                Ok(b) => b,
                Err(e) => {
                    fail(&state, &last_error, &e.to_string());
                    tracing::error!("Stream encode failed: {e}");
                    return;
                }
            };

            if let Err(e) = source.send(mp3) {
                // Connection dropped mid-stream: fall through to reconnect.
                tracing::warn!("Stream send failed: {e}");
                *last_error.lock().unwrap() = e.to_string();
                state.store(STATE_ERROR, Ordering::Relaxed);
                // Best effort: keep the MP3 bitstream legal across the
                // reconnect by flushing any encoder tail.
                if let Ok(tail) = encoder.flush() {
                    let _ = source.send(tail);
                }
                break;
            }
            stats
                .bytes_sent
                .fetch_add(mp3.len() as u64, Ordering::Relaxed);
            stats
                .stream_secs
                .store(started.elapsed().as_secs(), Ordering::Relaxed);
            // Pace to wall clock: this batch is `frames` of audio at the
            // encoder rate. Sleep the remainder; if a stall put us behind,
            // re-anchor instead of burst-catching-up (same RST hazard as
            // the backlog dump above).
            deadline += Duration::from_secs_f64(frames as f64 / device_rate as f64);
            let now = Instant::now();
            if deadline > now {
                std::thread::sleep(deadline - now);
            } else if now.duration_since(deadline) > Duration::from_secs(1) {
                deadline = now;
            }
        }

        // --- Mid-stream drop: bounded retry like connect failures ---
        if !alive(&generation, gen) {
            return;
        }
        if reconnects >= MAX_RECONNECTS {
            tracing::error!("Stream: giving up after {reconnects} retries");
            return;
        }
        let delay = Duration::from_secs(RETRY_DELAYS_SECS[reconnects as usize]);
        tracing::warn!("Stream dropped; retry {reconnects} in {delay:?}");
        sleep_interruptibly(delay, &generation, gen);
        reconnects += 1;
        stats.reconnects.store(reconnects as u64, Ordering::Relaxed);
    }
}

/// Sleep in short slices so a stop/restart request cuts the wait short.
fn sleep_interruptibly(total: Duration, generation: &AtomicU64, gen: u64) {
    let step = Duration::from_millis(100);
    let mut waited = Duration::ZERO;
    while waited < total && generation.load(Ordering::Relaxed) == gen {
        std::thread::sleep(step);
        waited += step;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cfg() -> StreamConfig {
        StreamConfig {
            enabled: true,
            ..Default::default()
        }
    }

    #[test]
    fn start_returns_tap_and_stop_kills_it() {
        // Note: this test's sender thread will fail to connect to the
        // default host quickly (connection refused on 127.0.0.1:8000
        // unless occupied); the tap lifecycle is what we assert on.
        let mut mgr = StreamManager::new(cfg());
        let tap = mgr.start(48_000);
        assert!(tap.is_some(), "enabled config yields a tap");
        assert!(mgr.running());

        // A stale tap from a stopped stream must no-op.
        mgr.stop();
        assert!(!mgr.running());
        assert_eq!(mgr.state(), StreamState::Off);
        let stale = tap.unwrap();
        stale.push(&[0.0f32; 128]); // must not panic, must drop
    }

    #[test]
    fn disabled_config_yields_no_tap() {
        let mut mgr = StreamManager::new(StreamConfig {
            enabled: false,
            ..Default::default()
        });
        assert!(mgr.start(48_000).is_none());
        assert_eq!(mgr.state(), StreamState::Off);
    }

    #[test]
    fn restart_generates_fresh_tap() {
        let mut mgr = StreamManager::new(cfg());
        let t1 = mgr.start(48_000).unwrap();
        mgr.stop();
        let t2 = mgr.start(48_000).unwrap();
        // Different generation: t1 must be inert now.
        t1.push(&[0.0f32; 64]);
        t2.push(&[0.0f32; 64]);
        assert!(mgr.running());
        mgr.stop();
    }

    #[test]
    fn stats_start_zeroed() {
        let mut mgr = StreamManager::new(cfg());
        let _ = mgr.start(48_000);
        let s = mgr.stats();
        assert_eq!(s.bytes_sent, 0);
        assert_eq!(s.stream_secs, 0);
        mgr.stop();
    }

    #[test]
    fn set_title_sanitizes_newlines() {
        let mgr = StreamManager::new(cfg());
        mgr.set_title("Artist\r\nTitle");
        // No direct getter; the sender loop consumes it. Just ensure
        // no panic and state unaffected.
        assert_eq!(mgr.state(), StreamState::Off);
    }

    #[test]
    fn tap_overfill_does_not_block_or_panic() {
        let mut mgr = StreamManager::new(cfg());
        let tap = mgr.start(48_000).unwrap();
        // Push far more than the ring holds — must not block.
        let big = vec![0.1f32; RING_SAMPLES + 4096];
        tap.push(&big);
        mgr.stop();
    }
}
