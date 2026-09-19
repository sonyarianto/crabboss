//! CrabBoss Core Engine
//!
//! Audio playback, library management, playlist scheduling, and more.

pub mod ads;
pub mod audio;
pub mod cart;
pub mod db;
pub mod error;
pub mod library;
pub mod paths;
pub mod playlist;
pub mod report;
pub mod scheduler;
pub mod settings;
pub mod stream;

pub use error::{CrabError, Result};
