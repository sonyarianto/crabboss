//! Music library management
//!
//! SQLite-backed database for storing audio file metadata, tags, and usage stats.

mod artwork;
mod db;
mod dupes;

pub use artwork::{artwork_for, Artwork, MAX_ARTWORK_BYTES};

pub use db::{collect_audio_files, is_audio_path, AUDIO_EXTENSIONS};
pub use db::{Library, Track, TrackId, TrackKind};
pub use dupes::{duplicate_ids, find_duplicate_groups, DuplicateGroup, DURATION_TOLERANCE_SECS};
