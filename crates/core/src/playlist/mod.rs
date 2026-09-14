//! Playlist management
//!
//! Create, edit, and manage playlists.

mod generator;
mod manager;

pub use generator::{generate, generate_next, GenConfig, PlaycountPriority, RuleHistory};
pub use manager::{Playlist, PlaylistItem, PlaylistManager};
