//! CrabBoss — Radio Automation Software
//!
//! Desktop UI entry point using Slint.

use std::cell::RefCell;
use std::path::PathBuf;
use std::rc::Rc;

use tracing_subscriber::{fmt, EnvFilter};

slint::include_modules!();

use crabcore::audio::Engine;

/// Application state shared between UI callbacks
struct AppState {
    player: Box<dyn Engine>,
    library: crabcore::library::Library,
    #[allow(dead_code)]
    playlist_manager: Option<crabcore::playlist::PlaylistManager>,
    #[allow(dead_code)]
    current_track_index: usize,
}

/// `--engine rodio|cpal` (default rodio until CpalEngine reaches parity).
fn engine_choice() -> String {
    let mut args = std::env::args().skip(1);
    let mut choice = std::env::var("CRABBOSS_ENGINE").unwrap_or_else(|_| "rodio".into());
    while let Some(a) = args.next() {
        if a == "--engine" {
            if let Some(v) = args.next() {
                choice = v;
            }
        } else if let Some(v) = a.strip_prefix("--engine=") {
            choice = v.to_string();
        }
    }
    choice.to_lowercase()
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    // Initialize logging
    fmt()
        .with_env_filter(
            EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info")),
        )
        .init();

    tracing::info!("🦀 CrabBoss starting up...");

    // Engine A/B: rodio default, `--engine cpal` opts into new backend.
    let engine_name = engine_choice();
    let player: Box<dyn Engine> = match engine_name.as_str() {
        "cpal" => {
            tracing::info!("Audio engine: cpal (experimental)");
            Box::new(crabcore::audio::CpalEngine::new())
        }
        _ => {
            if engine_name != "rodio" {
                tracing::warn!("Unknown engine '{}', falling back to rodio", engine_name);
            }
            tracing::info!("Audio engine: rodio (stable)");
            Box::new(crabcore::audio::Player::new())
        }
    };
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

    // License store (offline key, license.json next to db)
    let license_path = std::env::current_dir()
        .unwrap_or_default()
        .join("license.json");
    let license_store = Rc::new(RefCell::new(crabcore::license::LicenseStore::open(
        &license_path,
    )));

    // Router initial state: Home, no login (license-key model instead).
    ui.set_current_screen(1);
    ui.set_station_name("CrabBoss FM".into());
    ui.set_license_status(license_store.borrow().status().label().into());
    ui.set_license_error("".into());
    ui.set_audio_engine(engine_name.clone().into());
    ui.set_audio_device("Default".into());
    ui.set_track_count(track_count as i32);
    ui.set_playlist_count(0);
    ui.set_upcoming_count(0);

    // Shared state
    let state = Rc::new(RefCell::new(AppState {
        player,
        library,
        playlist_manager: None,
        current_track_index: 0,
    }));

    // -- Navigate --
    {
        let ui_weak = ui.as_weak();
        ui.on_navigate(move |screen: i32| {
            if let Some(ui) = ui_weak.upgrade() {
                ui.set_current_screen(screen);
            }
        });
    }

    // -- Activate license --
    {
        let ui_weak = ui.as_weak();
        let license_store = license_store.clone();
        ui.on_activate_license(move |key| {
            let mut store = license_store.borrow_mut();
            match store.activate(&key, "Station") {
                Ok(info) => {
                    tracing::info!("License activated: {}", info.key);
                    if let Some(ui) = ui_weak.upgrade() {
                        ui.set_license_status(store.status().label().into());
                        ui.set_license_error("".into());
                    }
                }
                Err(e) => {
                    tracing::warn!("Invalid license '{}': {}", key, e);
                    if let Some(ui) = ui_weak.upgrade() {
                        ui.set_license_error(format!("Invalid key: {}", e).into());
                    }
                }
            }
        });
    }

    // -- Clear license --
    {
        let ui_weak = ui.as_weak();
        let license_store = license_store.clone();
        ui.on_clear_license(move || {
            let mut store = license_store.borrow_mut();
            store.clear().ok();
            tracing::info!("License cleared");
            if let Some(ui) = ui_weak.upgrade() {
                ui.set_license_status(store.status().label().into());
                ui.set_license_error("".into());
            }
        });
    }

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
