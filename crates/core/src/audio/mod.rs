//! Audio playback engine
//!
//! Legacy `Player` (rodio) + new `CpalEngine` (cpal) behind `Engine` trait.
//! See ROADMAP: rodio → cpal migration.

mod cpal_engine;
mod engine;
mod mixer;
mod player;

pub use cpal_engine::CpalEngine;
pub use engine::Engine;
pub use mixer::{Frame, Mixer};
pub use player::{Player, PlayerState, TrackInfo};
