//! CrabBoss — Radio Automation Software
//!
//! Desktop UI entry point using Slint.

use std::cell::RefCell;
use std::path::PathBuf;
use std::rc::Rc;

use tracing_subscriber::{fmt, EnvFilter};

slint::include_modules!();

/// Application state shared between UI callbacks
struct AppState {
    player: crabcore::audio::Player,
    library: crabcore::library::Library,
    #[allow(dead_code)]
    playlist_manager: Option<crabcore::playlist::PlaylistManager>,
    #[allow(dead_code)]
    current_track_index: usize,
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    // Initialize logging
    fmt()
        .with_env_filter(
            EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info")),
        )
        .init();

    tracing::info!("🦀 CrabBoss starting up...");

    // Initialize audio player (falls back to headless if no audio device)
    let player = crabcore::audio::Player::new();
    if !player.has_audio_device() {
        tracing::warn!("⚠ Running without audio output (headless mode)");
    }

    // Initialize library (create db in current dir)
    let db_path = std::env::current_dir()
        .unwrap_or_default()
        .join("crabboss.db");
    let library =
        crabcore::library::Library::open(&db_path).expect("Failed to open library database");

    tracing::info!("Library loaded from: {}", db_path.display());

    // Count existing tracks
    let track_count = library.get_all_tracks().unwrap_or_default().len();
    tracing::info!("Library contains {} tracks", track_count);

    // Build Slint UI
    let ui = MainWindow::new().expect("Failed to create main window");

    // Shared state
    let state = Rc::new(RefCell::new(AppState {
        player,
        library,
        playlist_manager: None,
        current_track_index: 0,
    }));

    // -- Play --
    {
        let state = state.clone();
        let ui_weak = ui.as_weak();
        ui.on_play(move || {
            let s = state.borrow();
            if s.player.state() == crabcore::audio::PlayerState::Paused {
                s.player.resume();
            }
            drop(s);

            if let Some(ui) = ui_weak.upgrade() {
                ui.set_is_playing(true);
            }
        });
    }

    // -- Pause --
    {
        let state = state.clone();
        let ui_weak = ui.as_weak();
        ui.on_pause(move || {
            let s = state.borrow();
            s.player.pause();
            drop(s);

            if let Some(ui) = ui_weak.upgrade() {
                ui.set_is_playing(false);
            }
        });
    }

    // -- Stop --
    {
        let state = state.clone();
        let ui_weak = ui.as_weak();
        ui.on_stop(move || {
            let s = state.borrow();
            s.player.stop();
            drop(s);

            if let Some(ui) = ui_weak.upgrade() {
                ui.set_is_playing(false);
                ui.set_now_playing_title("No track loaded".into());
                ui.set_now_playing_artist("".into());
                ui.set_current_time("00:00".into());
                ui.set_total_time("00:00".into());
                ui.set_player_progress(0.0);
            }
        });
    }

    // -- Set Volume --
    {
        let state = state.clone();
        ui.on_set_volume(move |vol| {
            let s = state.borrow();
            s.player.set_volume(vol);
        });
    }

    // -- Import Files --
    {
        let _state = state.clone();
        let ui_weak = ui.as_weak();
        ui.on_import_files(move || {
            // For now, import from a specific directory or prompt
            // In a full app, this would use a file dialog
            tracing::info!("Import files requested");

            if let Some(ui) = ui_weak.upgrade() {
                ui.set_now_playing_title("Import: Use file dialog (coming soon)".into());
            }
        });
    }

    // -- Library Search --
    {
        let state = state.clone();
        ui.on_library_search_changed(move |query| {
            let s = state.borrow();
            let results = s
                .library
                .search(&query)
                .unwrap_or_default();
            tracing::debug!(
                "Search '{}': {} results",
                query,
                results.len()
            );
        });
    }

    // -- Library Track Double-Click (Play) --
    {
        let state = state.clone();
        let ui_weak = ui.as_weak();
        ui.on_library_track_double_clicked(move |index: i32| {
            let s = state.borrow();
            let tracks = s.library.get_all_tracks().unwrap_or_default();

            if let Some(track) = tracks.get(index as usize) {
                let path = PathBuf::from(&track.file_path);
                tracing::info!("Playing track: {:?}", path);

                match s.player.play(&path) {
                    Ok(()) => {
                        if let Some(ui) = ui_weak.upgrade() {
                            let title = track
                                .title
                                .clone()
                                .unwrap_or_else(|| track.file_name.clone());
                            let artist = track.artist.clone().unwrap_or_default();
                            let dur = track
                                .duration_secs
                                .map(|d| {
                                    let mins = (d as u64) / 60;
                                    let secs = (d as u64) % 60;
                                    format!("{:02}:{:02}", mins, secs)
                                })
                                .unwrap_or_default();

                            ui.set_is_playing(true);
                            ui.set_now_playing_title(title.into());
                            ui.set_now_playing_artist(artist.into());
                            ui.set_total_time(dur.into());
                        }
                    }
                    Err(e) => {
                        tracing::error!("Failed to play: {}", e);
                    }
                }
            }
        });
    }

    tracing::info!("🦀 CrabBoss UI ready — launching window");
    ui.run()?;

    Ok(())
}
