//! Core audio player using rodio

use std::fs::File;
use std::io::BufReader;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

use rodio::{Decoder, OutputStream, Sink};

use crate::error::{CrabError, Result};

/// Information about the currently loaded track.
#[derive(Debug, Clone)]
pub struct TrackInfo {
    pub path: PathBuf,
    pub title: Option<String>,
    pub artist: Option<String>,
    pub duration_secs: Option<f64>,
}

/// Player state
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PlayerState {
    Stopped,
    Playing,
    Paused,
}

enum AudioBackend {
    Live {
        _stream: OutputStream,
        sink: Arc<Mutex<Option<Sink>>>,
    },
    Headless,
}

/// The core audio player.
///
/// Wraps a rodio `Sink` behind a `Mutex` so it can be shared across threads.
/// Falls back to headless mode when no audio device is available (e.g. WSL, CI).
pub struct Player {
    backend: AudioBackend,
    state: Arc<Mutex<PlayerState>>,
    current_track: Arc<Mutex<Option<TrackInfo>>>,
    volume: Arc<Mutex<f32>>,
    progress_secs: Arc<Mutex<f64>>,
    running: Arc<AtomicBool>,
}

impl Player {
    /// Create a new player instance.
    ///
    /// Opens the default audio output device.
    /// Falls back to headless mode if no device is available.
    pub fn new() -> Self {
        let backend = match OutputStream::try_default() {
            Ok((stream, stream_handle)) => {
                match Sink::try_new(&stream_handle) {
                    Ok(sink) => {
                        tracing::info!("Audio output initialized (WASAPI/ALSA)");
                        AudioBackend::Live {
                            _stream: stream,
                            sink: Arc::new(Mutex::new(Some(sink))),
                        }
                    }
                    Err(e) => {
                        tracing::warn!("Failed to create audio sink: {}. Running headless.", e);
                        AudioBackend::Headless
                    }
                }
            }
            Err(e) => {
                tracing::warn!(
                    "No audio device available: {}. Running in headless mode.",
                    e
                );
                AudioBackend::Headless
            }
        };

        Self {
            backend,
            state: Arc::new(Mutex::new(PlayerState::Stopped)),
            current_track: Arc::new(Mutex::new(None)),
            volume: Arc::new(Mutex::new(1.0)),
            progress_secs: Arc::new(Mutex::new(0.0)),
            running: Arc::new(AtomicBool::new(true)),
        }
    }

    /// Check if the player has a real audio device
    pub fn has_audio_device(&self) -> bool {
        matches!(self.backend, AudioBackend::Live { .. })
    }

    fn with_sink(&self, f: impl FnOnce(&Sink)) {
        if let AudioBackend::Live { ref sink, .. } = self.backend {
            if let Some(s) = sink.lock().unwrap().as_ref() {
                f(s);
            }
        }
    }

    /// Load and play an audio file.
    pub fn play(&self, path: &Path) -> Result<()> {
        let file = File::open(path).map_err(|_| CrabError::FileNotFound {
            path: path.to_path_buf(),
        })?;
        let decoder = Decoder::new(BufReader::new(file))
            .map_err(|e| CrabError::Audio(e.to_string()))?;

        // Get total duration from metadata before playing
        let total_duration = self.read_duration(path);

        // Stop any current playback
        self.stop();

        if let AudioBackend::Live { ref sink, .. } = self.backend {
            let guard = sink.lock().unwrap();
            if let Some(ref s) = *guard {
                s.append(decoder);
            }

            // Set volume
            let vol = *self.volume.lock().unwrap();
            if let Some(ref s) = *guard {
                s.set_volume(vol);
            }
        }

        // Update state
        *self.state.lock().unwrap() = PlayerState::Playing;
        *self.current_track.lock().unwrap() = Some(TrackInfo {
            path: path.to_path_buf(),
            title: None,
            artist: None,
            duration_secs: total_duration,
        });
        *self.progress_secs.lock().unwrap() = 0.0;

        tracing::info!("Playing: {}", path.display());
        Ok(())
    }

    /// Pause playback
    pub fn pause(&self) {
        self.with_sink(|s| s.pause());
        *self.state.lock().unwrap() = PlayerState::Paused;
    }

    /// Resume playback
    pub fn resume(&self) {
        self.with_sink(|s| s.play());
        *self.state.lock().unwrap() = PlayerState::Playing;
    }

    /// Stop playback
    pub fn stop(&self) {
        if let AudioBackend::Live { ref sink, .. } = self.backend {
            let mut guard = sink.lock().unwrap();
            if let Some(s) = guard.take() {
                drop(s);
            }
        }

        *self.state.lock().unwrap() = PlayerState::Stopped;
        *self.current_track.lock().unwrap() = None;
        *self.progress_secs.lock().unwrap() = 0.0;
    }

    /// Toggle play/pause
    pub fn toggle_play_pause(&self) {
        let current = *self.state.lock().unwrap();
        match current {
            PlayerState::Playing => self.pause(),
            PlayerState::Paused => self.resume(),
            PlayerState::Stopped => {}
        }
    }

    /// Set volume (0.0 to 1.0)
    pub fn set_volume(&self, vol: f32) {
        let clamped = vol.clamp(0.0, 1.0);
        *self.volume.lock().unwrap() = clamped;
        self.with_sink(|s| s.set_volume(clamped));
    }

    /// Get current volume
    pub fn volume(&self) -> f32 {
        *self.volume.lock().unwrap()
    }

    /// Get current player state
    pub fn state(&self) -> PlayerState {
        *self.state.lock().unwrap()
    }

    /// Get current track info
    pub fn current_track(&self) -> Option<TrackInfo> {
        self.current_track.lock().unwrap().clone()
    }

    /// Check if playback has finished
    pub fn is_finished(&self) -> bool {
        if let AudioBackend::Live { ref sink, .. } = self.backend {
            if let Some(s) = sink.lock().unwrap().as_ref() {
                return s.empty();
            }
        }
        true
    }

    /// Read duration from file metadata
    fn read_duration(&self, path: &Path) -> Option<f64> {
        lofty::read_from_path(path).ok().and_then(|tagged_file| {
            use lofty::file::AudioFile;
            tagged_file
                .properties()
                .duration()
                .as_secs_f64()
                .into()
        })
    }
}

impl Drop for Player {
    fn drop(&mut self) {
        self.running.store(false, Ordering::SeqCst);
        self.stop();
    }
}
