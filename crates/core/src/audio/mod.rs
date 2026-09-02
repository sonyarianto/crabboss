//! Audio playback engine
//!
//! Handles audio output via rodio, with support for play/pause/stop/seek.

mod player;

pub use player::{Player, PlayerState, TrackInfo};
