//! Audio playback engine
//!
//! Legacy `Player` (rodio) + new `CpalEngine` (cpal) behind `Engine` trait.
//! See ROADMAP: rodio → cpal migration.

mod cpal_engine;
mod engine;
mod mixer;
mod player;
mod silence;

pub use cpal_engine::CpalEngine;
pub use engine::{needs_prefetch, Engine};
pub use mixer::{CrossfadeCurve, Frame, Mixer};
pub use player::{Player, PlayerState, TrackInfo};
pub use silence::{SilenceMonitor, SILENCE_FLOOR};
