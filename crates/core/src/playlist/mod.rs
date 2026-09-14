//! Playlist management
//!
//! Create, edit, and manage playlists.

mod generator;
mod manager;

pub use generator::{
    forecast_up_next, generate, generate_next, GenConfig, PlaycountPriority, RuleHistory,
};
pub use manager::{Playlist, PlaylistItem, PlaylistManager};
