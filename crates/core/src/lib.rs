//! CrabBoss Core Engine
//!
//! Audio playback, library management, playlist scheduling, and more.

pub mod audio;
pub mod cart;
pub mod error;
pub mod library;
pub mod license;
pub mod playlist;
pub mod report;
pub mod scheduler;

pub use error::{CrabError, Result};
