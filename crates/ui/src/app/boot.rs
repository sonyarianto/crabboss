//! Startup sequence: settings, engine, stores, seeds. Resolves the
//! data dir, migrates legacy files, boots the audio engine, opens every
//! store, seeds starter content on first run, and hands back a live
//! `App` plus the initial Iced task.

use std::collections::{HashMap, VecDeque};

use iced::Task;

use crabcore::audio::Engine;
use crabcore::library::{Library, TrackKind};
use crabcore::playlist::PlaylistManager;

use super::{App, Message, Screen, SettingsSection};
use crate::widgets::{engine_choice, track_label};

pub(crate) fn boot() -> (App, Task<Message>) {
    use tracing_subscriber::EnvFilter;
    let _ = tracing_subscriber::fmt()
        .with_env_filter(
            EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info")),
        )
        .try_init();

    tracing::info!("CrabBoss starting up...");

    // Stable data locations (P1.1): one root per install, not per
    // working directory. Legacy current-directory files migrate once.
    let legacy_dir = std::env::current_dir().unwrap_or_default();
    let args: Vec<String> = std::env::args().skip(1).collect();
    let paths = crabcore::paths::resolve_from(&args, &legacy_dir);
    tracing::info!("Data dir: {}", paths.root.display());
    if let Err(e) = paths.ensure_root() {
        tracing::error!("Cannot create data dir {}: {e}", paths.root.display());
    }
    let migration = paths.migrate_legacy(&legacy_dir);
    for (label, outcome) in [
        ("settings", &migration.settings),
        ("database", &migration.database),
    ] {
        match outcome {
            crabcore::paths::FileMigration::Copied { from, to } => {
                tracing::info!(
                    "Migrated legacy {label}: {} -> {}",
                    from.display(),
                    to.display()
                );
            }
            crabcore::paths::FileMigration::Failed { from, error } => {
                tracing::error!(
                    "Legacy {label} migration failed ({}): {error}",
                    from.display()
                );
            }
            crabcore::paths::FileMigration::SkippedExisting
            | crabcore::paths::FileMigration::SkippedNoLegacy => {}
        }
    }
    let data_dir = paths.root.clone();
    let settings_path = paths.settings.clone();
    let load = crabcore::settings::AppSettings::load(&settings_path);
    if let Some(w) = load.warning() {
        tracing::warn!("Settings load: {w}");
    }
    let settings_needs_quarantine = load.needs_quarantine();
    let settings_notice = load.warning();
    let mut settings = load.settings();

    let engine_name = {
        let choice = engine_choice();
        if choice != "cpal" {
            tracing::warn!("Unknown engine '{choice}', using cpal");
        }
        "cpal".to_string()
    };
    tracing::info!("Audio engine: cpal");
    let mut player: Box<dyn Engine> = match settings.output_device.clone() {
        Some(dev) => Box::new(crabcore::audio::CpalEngine::open_named(&dev)),
        None => Box::new(crabcore::audio::CpalEngine::new()),
    };
    if !player.has_audio_device() {
        tracing::warn!("Running without audio output (headless mode)");
    }
    player.set_crossfade_secs(settings.crossfade_secs);
    player.set_silence_threshold_secs(settings.silence_threshold_secs);
    player.set_eq_enabled(settings.eq_enabled);
    for (band, gain) in settings.eq_gains_db.iter().enumerate() {
        player.set_eq_band(band, *gain);
    }
    player.set_limiter_ceiling(settings.limiter_ceiling);
    player.set_loudness_enabled(settings.loudness_norm);
    player.set_stream_config(settings.stream.clone());
    if settings.stream.enabled {
        if let Err(e) = player.stream_start() {
            tracing::warn!("Stream auto-start failed: {e}");
        }
    }
    player.set_mic_config(settings.mic.clone());
    if settings.mic.enabled {
        if let Err(e) = player.mic_start() {
            tracing::warn!("Mic auto-start failed: {e}");
        }
    }
    // Cue (PFL) bus (B2 Phase 2): config-only at boot. `None` device =
    // unavailable, program boot identical to Phase 1. A stale/missing cue
    // device never fails boot — the engine reports Error/Unavailable.
    player.set_cue_config(settings.cue.clone());

    let db_path = paths.database.clone();
    // Central schema bootstrap first: numbered, transactional migrations.
    // A failure here is fatal (no store can open safely), but it must
    // read as an operator error, not a panic backtrace.
    if let Err(e) = crabcore::db::Database::initialize(&db_path) {
        tracing::error!("Database initialization failed: {e}");
        eprintln!("CrabBoss cannot start: database initialization failed: {e}");
        std::process::exit(1);
    }
    let library = Library::open(&db_path).expect("Failed to open library database");
    tracing::info!("Library loaded from: {}", db_path.display());

    match library.reclassify_all() {
        Ok(0) => {}
        Ok(n) => tracing::info!("Re-labeled {} tracks (kind repair)", n),
        Err(e) => tracing::warn!("Kind repair scan failed: {}", e),
    }
    match library.retarget_gains(settings.loudness_target_lufs) {
        Ok(0) => {}
        Ok(n) => tracing::info!("Re-targeted {n} loudness gains"),
        Err(e) => tracing::warn!("Gain retarget failed: {}", e),
    }
    {
        let loudness_lib = Library::open(&db_path).expect("Failed to open loudness lookup db");
        player.set_loudness_lookup(Some(Box::new(move |p| {
            loudness_lib
                .loudness_gain_by_path(&p.to_string_lossy())
                .unwrap_or(None)
        })));
    }

    let scheduler = crabcore::scheduler::SchedulerManager::open(&db_path)
        .expect("Failed to open scheduler store");
    if scheduler.list_all().map(|v| v.is_empty()).unwrap_or(false) {
        let _ = scheduler.create(
            "Midnight generate",
            "generate",
            "Day",
            "00:00",
            "Daily",
            None,
        );
        let _ = scheduler.create("Morning show", "load", "Morning", "08:00", "Daily", None);
        let _ = scheduler.create(
            "Top-of-hour jingle",
            "play",
            "toth.mp3",
            "09:00",
            "Daily",
            None,
        );
        tracing::info!("Seeded starter scheduler events");
    }

    let track_count = library.get_all_tracks().unwrap_or_default().len();
    tracing::info!("Library contains {} tracks", track_count);

    let playlist_manager = PlaylistManager::open(&db_path).expect("Failed to open playlists");
    let playlist_count = playlist_manager.list_all().unwrap_or_default().len();

    let carts = crabcore::cart::CartManager::open(&db_path).expect("Failed to open cart store");
    if carts.list_all().map(|v| v.is_empty()).unwrap_or(false) {
        let jingles = library.list_by_kind(TrackKind::Jingle).unwrap_or_default();
        let music = library.list_by_kind(TrackKind::Music).unwrap_or_default();
        for t in jingles.iter().chain(music.iter()).take(4) {
            let label = track_label(t);
            let _ = carts.create(&label, &t.file_path);
        }
        if track_count > 0 {
            tracing::info!("Seeded cart wall from library");
        }
    }

    let ads = crabcore::ads::AdsManager::open(&db_path).expect("Failed to open ads store");

    // License stays at the legacy location on purpose (out of scope
    // until a separate product decision moves it).
    let license_path = paths.license.clone();
    let license = crabcore::license::LicenseStore::open(&license_path);

    let volume = {
        let v = player.volume();
        if v <= 0.0 || v > 1.5 {
            0.8
        } else {
            v.clamp(0.0, 1.0)
        }
    };
    let autodj = settings.autodj;
    let sel_device = settings.output_device.clone().unwrap_or_default();
    let output_devices = crabcore::audio::CpalEngine::list_output_devices();
    let input_devices = crabcore::audio::CpalEngine::list_input_devices();
    // Silence unused-mut warning on settings: boot owns it, App takes it below.
    settings.autodj = autodj;
    let station_name = settings.station_name.clone();

    let mut app = App {
        player,
        library,
        playlist_manager,
        scheduler,
        carts,
        ads,
        settings,
        settings_path,
        data_dir,
        db_path,
        license,
        screen: Screen::Home,
        settings_section: SettingsSection::default(),
        station_name,
        audio_engine: engine_name,
        is_playing: false,
        now_title: "No track loaded".into(),
        now_artist: String::new(),
        now_art_path: None,
        now_art: None,
        volume,
        autodj,
        up_next: String::new(),
        up_next_list: Vec::new(),
        up_next_for: None,
        auto_continue: false,
        was_playing: false,
        autodj_history: crabcore::playlist::RuleHistory::default(),
        engine_track: None,
        pending_source: None,
        lib_tracks: Vec::new(),
        lib_total: 0,
        lib_search: String::new(),
        lib_kind: None,
        lib_missing_only: false,
        lib_dupes_only: false,
        lib_dupe_groups: 0,
        lib_selected: None,
        lib_status: String::new(),
        scanning: false,
        scan_rx: None,
        scan_done: 0,
        scan_total: 0,
        import_active: false,
        import_pending: VecDeque::new(),
        import_added: 0,
        import_skipped: 0,
        import_total: 0,
        last_import_dir: None,
        syncing: false,
        sync_rx: None,
        last_auto_sync: None,
        sched_enabled: true,
        sched_events: Vec::new(),
        sched_warnings: Vec::new(),
        sched_editor_open: false,
        sched_edit_idx: None,
        se_name: String::new(),
        se_time: "09:00".into(),
        se_action: 0,
        se_target: String::new(),
        se_expires: String::new(),
        se_days: [true; 7],
        sched_error: String::new(),
        fired: HashMap::new(),
        fired_ads: HashMap::new(),
        cart_list: Vec::new(),
        cart_status: String::new(),
        cart_assign: false,
        report_entries: Vec::new(),
        report_summary: String::new(),
        report_range: 1,
        recent_plays: Vec::new(),
        ad_blocks: Vec::new(),
        ads_editor_open: false,
        ads_edit_idx: None,
        ab_name: String::new(),
        ab_spot: String::new(),
        ab_intro: String::new(),
        ab_outro: String::new(),
        ab_start: chrono::Local::now().format("%Y-%m-%d").to_string(),
        ab_end: (chrono::Local::now() + chrono::Duration::days(30))
            .format("%Y-%m-%d")
            .to_string(),
        ab_time: "09:00".into(),
        ab_days: [true; 7],
        ads_error: String::new(),
        output_devices,
        sel_device,
        device_note: String::new(),
        input_devices,
        mic_note: String::new(),
        cue_status: String::new(),
        backup_status: String::new(),
        stream_listeners: None,
        stream_live_config: None,
        listeners_polling: false,
        listeners_rx: None,
        last_listeners_poll: None,
        settings_notice,
        settings_save_error: None,
        settings_needs_quarantine,
        license_status: String::new(),
        license_error: String::new(),
        license_key: String::new(),
        track_count,
        playlist_count,
        upcoming_count: 0,
        gen_hours: [
            crate::app::update::generator::DAYPARTS[0].hour,
            crate::app::update::generator::DAYPARTS[1].hour,
            crate::app::update::generator::DAYPARTS[2].hour,
            crate::app::update::generator::DAYPARTS[3].hour,
        ],
        gen_counts: [crate::app::update::generator::DEFAULT_GEN_COUNT; 4],
        gen_status: String::new(),
        saved_playlists: Vec::new(),
        last_recovery: None,
        tick_count: 0,
    };
    app.license_status = app.license.status().label().to_string();
    app.cue_status = app.player.cue_state().label();
    if app.settings.stream.enabled {
        app.mark_stream_live();
    }
    app.refresh_library();
    app.refresh_scheduler();
    app.refresh_carts();
    app.refresh_ads();
    app.refresh_report();
    app.refresh_counts();

    // Health hint on startup.
    {
        let n_missing = app.library.missing_files().unwrap_or_default().len();
        let pending = app.library.count_missing_loudness().unwrap_or(0);
        if n_missing > 0 || pending > 0 {
            let mut parts = Vec::new();
            if n_missing > 0 {
                parts.push(format!("{} files missing (see log)", n_missing));
            }
            if pending > 0 {
                parts.push(format!("{} to analyze", pending));
            }
            app.lib_status = parts.join(" - ");
        }
        if pending > 0 {
            tracing::info!("Auto-starting loudness scan ({pending} pending)");
            app.start_loudness_scan(false);
        }
    }

    tracing::info!("CrabBoss UI ready (Iced)");
    (app, Task::none())
}
