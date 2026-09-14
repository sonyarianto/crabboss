use std::path::PathBuf;

/// Core error type for CrabBoss
#[derive(Debug, thiserror::Error)]
pub enum CrabError {
    #[error("Audio playback error: {0}")]
    Audio(String),

    #[error("Database error: {0}")]
    Database(#[from] rusqlite::Error),

    #[error("File not found: {path}")]
    FileNotFound { path: PathBuf },

    #[error("Unsupported format: {format}")]
    UnsupportedFormat { format: String },

    #[error("Metadata error: {0}")]
    Metadata(String),

    #[error("I/O error: {0}")]
    Io(#[from] std::io::Error),

    #[error("Playlist error: {0}")]
    Playlist(String),

    #[error("Scheduler error: {0}")]
    Scheduler(String),

    #[error("Library error: {0}")]
    Library(String),

    #[error("Migration v{version} ({name}) failed: {source}")]
    Migration {
        version: u32,
        name: &'static str,
        #[source]
        source: rusqlite::Error,
    },

    #[error("Database schema v{found} is newer than supported v{supported}")]
    SchemaTooNew { found: u32, supported: u32 },

    #[error("Station lists replace failed: {0}")]
    BulkReplace(String),
}

pub type Result<T> = std::result::Result<T, CrabError>;
