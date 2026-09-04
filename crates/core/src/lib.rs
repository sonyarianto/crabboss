//! CrabBoss Core Engine
//!
//! Audio playback, library management, playlist scheduling, and more.

pub mod audio;
pub mod error;
pub mod library;
pub mod license;
pub mod playlist;

pub use error::{CrabError, Result};
