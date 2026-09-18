//! Audio playback engine: `CpalEngine` (cpal) behind the `Engine` trait.

mod cpal_engine;
mod cue;
mod engine;
mod loudness;
mod mic;
mod mixer;
mod silence;

pub use cpal_engine::CpalEngine;
pub use cue::{CueConfig, CueState};
pub use engine::{needs_prefetch, Engine, PlayerState, TrackInfo};
pub use loudness::{
    analyze_file, LoudnessAnalysis, LoudnessMeter, MAX_GAIN_DB, TARGET_LUFS, TARGET_MAX_LUFS,
    TARGET_MIN_LUFS,
};
pub use mic::{Ducker, MicConfig, MicResampler, MicState, MIC_RING_SAMPLES};
pub use mixer::{CrossfadeCurve, EqBand, EqChain, Frame, Mixer, EQ_BAND_COUNT, EQ_CENTER_HZ};
pub use silence::{SilenceMonitor, SILENCE_FLOOR};
