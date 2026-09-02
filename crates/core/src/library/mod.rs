//! Music library management
//!
//! SQLite-backed database for storing audio file metadata, tags, and usage stats.

mod db;

pub use db::{Library, Track, TrackId};
