//! CrabBoss Core Engine
//!
//! Audio playback, library management, playlist scheduling, and more.

pub mod ads;
pub mod audio;
pub mod cart;
pub mod error;
pub mod library;
pub mod license;
pub mod playlist;
pub mod report;
pub mod scheduler;
pub mod settings;

pub use error::{CrabError, Result};
