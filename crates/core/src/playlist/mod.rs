//! Playlist management
//!
//! Create, edit, and manage playlists.

mod generator;
mod manager;

pub use generator::{generate, GenConfig, PlaycountPriority};
pub use manager::{Playlist, PlaylistItem, PlaylistManager};
