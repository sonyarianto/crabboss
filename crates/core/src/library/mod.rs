//! Music library management
//!
//! SQLite-backed database for storing audio file metadata, tags, and usage stats.

mod db;
mod dupes;

pub use db::{Library, Track, TrackId, TrackKind};
pub use dupes::{duplicate_ids, find_duplicate_groups, DuplicateGroup, DURATION_TOLERANCE_SECS};
