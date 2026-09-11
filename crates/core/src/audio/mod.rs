//! Audio playback engine
//!
//! Legacy `Player` (rodio) + new `CpalEngine` (cpal) behind `Engine` trait.
//! See ROADMAP: rodio → cpal migration.

mod cpal_engine;
mod engine;
mod loudness;
mod mixer;
mod player;
mod silence;

pub use cpal_engine::CpalEngine;
pub use engine::{needs_prefetch, Engine};
pub use loudness::{analyze_file, LoudnessAnalysis, LoudnessMeter, MAX_GAIN_DB, TARGET_LUFS};
pub use mixer::{CrossfadeCurve, EqBand, EqChain, Frame, Mixer, EQ_BAND_COUNT, EQ_CENTER_HZ};
pub use player::{Player, PlayerState, TrackInfo};
pub use silence::{SilenceMonitor, SILENCE_FLOOR};
