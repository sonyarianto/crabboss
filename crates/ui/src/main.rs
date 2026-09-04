//! CrabBoss — Radio Automation Software
//!
//! Desktop UI entry point using Slint.

use std::cell::RefCell;
use std::collections::HashMap;
use std::path::PathBuf;
use std::rc::Rc;

use tracing_subscriber::{fmt, EnvFilter};

slint::include_modules!();

use crabcore::audio::Engine;
use crabcore::library::TrackKind;

/// Application state shared between UI callbacks
struct AppState {
    player: Box<dyn Engine>,
    library: crabcore::library::Library,
    #[allow(dead_code)]
    playlist_manager: crabcore::playlist::PlaylistManager,
    #[allow(dead_code)]
    current_track_index: usize,
}

fn fmt_dur(d: Option<f64>) -> String {
    let total = d.unwrap_or(0.0).max(0.0) as u64;
    format!("{:02}:{:02}", total / 60, total % 60)
}

fn kind_label(k: TrackKind) -> &'static str {
    match k {
        TrackKind::Jingle => "Jingle",
        TrackKind::Ad => "Ad",
        TrackKind::Music => "Music",
    }
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

    // Persisted prefs (settings.json next to the db).
    let settings_path = std::env::current_dir()
        .unwrap_or_default()
        .join("settings.json");
    let settings = Rc::new(RefCell::new(crabcore::settings::AppSettings::load(
        &settings_path,
    )));

    // Engine A/B: rodio default, `--engine cpal` opts into new backend.
    let engine_name = engine_choice();
    let player: Box<dyn Engine> = match engine_name.as_str() {
        "cpal" => {
            tracing::info!("Audio engine: cpal");
            match settings.borrow().output_device.clone() {
                Some(dev) => Box::new(crabcore::audio::CpalEngine::open_named(&dev)),
                None => Box::new(crabcore::audio::CpalEngine::new()),
            }
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
    // Apply persisted DSP prefs live (no-ops on backends without support).
    player.set_crossfade_secs(settings.borrow().crossfade_secs);
    player.set_silence_threshold_secs(settings.borrow().silence_threshold_secs);

    // Initialize library (create db in current dir)
    let db_path = std::env::current_dir()
        .unwrap_or_default()
        .join("crabboss.db");
    let library =
        crabcore::library::Library::open(&db_path).expect("Failed to open library database");

    tracing::info!("Library loaded from: {}", db_path.display());

    // Scheduler store (shares the same crabboss.db file, separate connection)
    let scheduler = Rc::new(RefCell::new(
        crabcore::scheduler::SchedulerManager::open(&db_path)
            .expect("Failed to open scheduler store"),
    ));

    // Seed a starter flow for a new station (only when empty)
    if scheduler
        .borrow()
        .list_all()
        .map(|v| v.is_empty())
        .unwrap_or(false)
    {
        let _ = scheduler
            .borrow()
            .create("Midnight generate", "generate", "Day", "00:00", "Daily");
        let _ = scheduler
            .borrow()
            .create("Morning show", "load", "Morning.m3u", "08:00", "Daily");
        let _ =
            scheduler
                .borrow()
                .create("Top-of-hour jingle", "play", "toth.mp3", "09:00", "Daily");
        tracing::info!("Seeded starter scheduler events");
    }

    // Count existing tracks
    let track_count = library.get_all_tracks().unwrap_or_default().len();
    tracing::info!("Library contains {} tracks", track_count);

    // Playlist store (same db file, separate connection)
    let playlist_manager =
        crabcore::playlist::PlaylistManager::open(&db_path).expect("Failed to open playlists");
    let playlist_count = playlist_manager.list_all().unwrap_or_default().len();

    // Cart Wall store (same db file, separate connection)
    let carts = Rc::new(RefCell::new(
        crabcore::cart::CartManager::open(&db_path).expect("Failed to open cart store"),
    ));

    // Seed carts from jingles first, then music (only when empty)
    if carts
        .borrow()
        .list_all()
        .map(|v| v.is_empty())
        .unwrap_or(false)
    {
        let jingles = library.list_by_kind(TrackKind::Jingle).unwrap_or_default();
        let music = library.list_by_kind(TrackKind::Music).unwrap_or_default();
        for t in jingles.iter().chain(music.iter()).take(4) {
            let label = t.title.clone().unwrap_or_else(|| t.file_name.clone());
            let _ = carts.borrow().create(&label, &t.file_path);
        }
        if track_count > 0 {
            tracing::info!("Seeded cart wall from library");
        }
    }

    // Ads store (same db file, separate connection)
    let ads = Rc::new(RefCell::new(
        crabcore::ads::AdsManager::open(&db_path).expect("Failed to open ads store"),
    ));

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
    ui.set_audio_device(player.device_name().into());
    ui.set_track_count(track_count as i32);
    ui.set_playlist_count(playlist_count as i32);
    ui.set_upcoming_count(0);

    // Shared state
    let state = Rc::new(RefCell::new(AppState {
        player,
        library,
        playlist_manager,
        current_track_index: 0,
    }));

    // Tracks currently shown in the library list (all or search-filtered);
    // row indices from Slint resolve against this.
    let last_shown: Rc<RefCell<Vec<crabcore::library::Track>>> = Rc::new(RefCell::new(Vec::new()));

    // -- Library: push tracks to UI (missing files get a ⚠ prefix) --
    fn refresh_library(
        ui: &MainWindow,
        tracks: Vec<crabcore::library::Track>,
        last_shown: &Rc<RefCell<Vec<crabcore::library::Track>>>,
    ) {
        let rows: Vec<LibTrack> = tracks
            .iter()
            .map(|t| {
                let missing = !PathBuf::from(&t.file_path).is_file();
                let base = t.title.clone().unwrap_or_else(|| t.file_name.clone());
                LibTrack {
                    title: if missing {
                        format!("⚠ {}", base)
                    } else {
                        base
                    }
                    .into(),
                    artist: t.artist.clone().unwrap_or_default().into(),
                    duration: fmt_dur(t.duration_secs).into(),
                    kind: kind_label(t.kind).into(),
                }
            })
            .collect();
        *last_shown.borrow_mut() = tracks;
        let model = Rc::new(slint::VecModel::from(rows));
        ui.set_library_tracks(model.into());
    }
    refresh_library(
        &ui,
        state.borrow().library.get_all_tracks().unwrap_or_default(),
        &last_shown,
    );
    ui.set_library_status("".into());

    // -- Library health: startup scan + on-demand --
    fn run_health_check(
        ui: &MainWindow,
        state: &Rc<RefCell<AppState>>,
        last_shown: &Rc<RefCell<Vec<crabcore::library::Track>>>,
    ) {
        let s = state.borrow();
        let missing = s.library.missing_files().unwrap_or_default();
        if missing.is_empty() {
            ui.set_library_status("✓ All files OK".into());
        } else {
            for t in &missing {
                tracing::warn!("Missing file: {}", t.file_path);
            }
            ui.set_library_status(format!("⚠ {} files missing (see log)", missing.len()).into());
        }
        let tracks = s.library.get_all_tracks().unwrap_or_default();
        ui.set_track_count(tracks.len() as i32);
        drop(s);
        refresh_library(ui, tracks, last_shown);
    }
    {
        let n_missing = state
            .borrow()
            .library
            .missing_files()
            .unwrap_or_default()
            .len();
        if n_missing > 0 {
            tracing::warn!("Startup health scan: {} missing files", n_missing);
            ui.set_library_status(format!("⚠ {} files missing (see log)", n_missing).into());
        }
    }
    {
        let state = state.clone();
        let ui_weak = ui.as_weak();
        let last_shown = last_shown.clone();
        ui.on_library_health_check(move || {
            if let Some(ui) = ui_weak.upgrade() {
                tracing::info!("Manual library health scan");
                run_health_check(&ui, &state, &last_shown);
            }
        });
    }

    // -- Scheduler: push events to UI --
    fn refresh_scheduler(ui: &MainWindow, scheduler: &crabcore::scheduler::SchedulerManager) {
        use crabcore::scheduler::mask_from_days;
        let events = scheduler.list_all().unwrap_or_default();
        let rows: Vec<SchedRow> = events
            .iter()
            .map(|e| {
                let mask = mask_from_days(&e.days);
                SchedRow {
                    name: e.name.clone().into(),
                    time: e.start_time.clone().into(),
                    action: e.action_type.clone().into(),
                    target: e.target.clone().into(),
                    days: e.days.clone().into(),
                    enabled: e.enabled,
                    mon: mask & 1 != 0,
                    tue: mask & 2 != 0,
                    wed: mask & 4 != 0,
                    thu: mask & 8 != 0,
                    fri: mask & 16 != 0,
                    sat: mask & 32 != 0,
                    sun: mask & 64 != 0,
                }
            })
            .collect();
        let model = Rc::new(slint::VecModel::from(rows));
        ui.set_scheduler_events(model.into());
        ui.set_upcoming_count(events.iter().filter(|e| e.enabled).count() as i32);
    }
    ui.set_scheduler_enabled(true);
    ui.set_scheduler_show_editor(false);
    ui.set_scheduler_error("".into());
    refresh_scheduler(&ui, &scheduler.borrow());

    // Shared firing logic: used by manual Run and the auto-tick timer.
    fn fire_scheduled_event(
        state: &Rc<RefCell<AppState>>,
        ui_weak: &slint::Weak<MainWindow>,
        event: &crabcore::scheduler::ScheduledEvent,
    ) {
        tracing::info!(
            "Scheduler firing: {} [{} {}]",
            event.name,
            event.action_type,
            event.target
        );
        let s = state.borrow();
        match event.action_type.as_str() {
            // generate <preset>: real rotation (rules + daypart + jingles),
            // persisted as a playlist for the playout queue.
            "generate" => {
                let now = chrono::Local::now();
                let cfg = crabcore::playlist::GenConfig {
                    target_tracks: 10,
                    hour: now.format("%H").to_string().parse().unwrap_or(12),
                    weekday: now.format("%a").to_string(),
                    ..Default::default()
                };
                let rotation = crabcore::playlist::generate(&s.library, &cfg).unwrap_or_default();
                let n_music = rotation
                    .iter()
                    .filter(|t| t.kind == TrackKind::Music)
                    .count();
                let n_jingles = rotation
                    .iter()
                    .filter(|t| t.kind == TrackKind::Jingle)
                    .count();
                let pl_name = format!("{} {}", event.target, now.format("%H:%M"));
                match s
                    .playlist_manager
                    .create(&pl_name, Some("Auto-generated rotation"))
                {
                    Ok(pl) => {
                        for t in &rotation {
                            let _ = s.playlist_manager.add_track(
                                &pl.id,
                                &t.id,
                                t.kind == TrackKind::Jingle,
                                t.kind == TrackKind::Ad,
                            );
                        }
                        tracing::info!(
                            "Generated playlist '{}' ({} music + {} jingles)",
                            pl_name,
                            n_music,
                            n_jingles
                        );
                    }
                    Err(e) => tracing::error!("Failed to persist rotation: {}", e),
                }
                drop(s);
                if let Some(ui) = ui_weak.upgrade() {
                    ui.set_playlist_count(
                        state
                            .borrow()
                            .playlist_manager
                            .list_all()
                            .unwrap_or_default()
                            .len() as i32,
                    );
                    ui.set_now_playing_title(
                        format!(
                            "Generated '{}': {} music + {} jingles",
                            event.target, n_music, n_jingles
                        )
                        .into(),
                    );
                }
            }
            // queue <path>: insert after current track (blends at boundary;
            // rodio backend degrades to immediate play). Not logged until heard.
            "queue" => {
                let path = PathBuf::from(&event.target);
                if path.is_file() {
                    match s.player.queue(&path) {
                        Ok(()) => {
                            drop(s);
                            if let Some(ui) = ui_weak.upgrade() {
                                ui.set_now_playing_title(
                                    format!("Queued after current: {}", event.target).into(),
                                );
                            }
                        }
                        Err(e) => tracing::error!("Scheduler queue failed: {}", e),
                    }
                } else {
                    tracing::warn!("Scheduler target not found on disk: {}", event.target);
                    drop(s);
                    if let Some(ui) = ui_weak.upgrade() {
                        ui.set_now_playing_title(
                            format!("Scheduled: {} (file missing)", event.target).into(),
                        );
                    }
                }
            }
            "load" | "play" => {
                let path = PathBuf::from(&event.target);
                if path.is_file() {
                    let logged = s
                        .library
                        .find_by_path(&event.target)
                        .ok()
                        .flatten()
                        .map(|t| (t.id, t.duration_secs));
                    match s.player.play(&path) {
                        Ok(()) => {
                            if let Some((id, dur)) = logged {
                                let _ = s.library.record_play(&id, dur);
                            }
                            drop(s);
                            if let Some(ui) = ui_weak.upgrade() {
                                ui.set_is_playing(true);
                                ui.set_now_playing_title(event.target.clone().into());
                                ui.set_now_playing_artist("Scheduler".into());
                            }
                        }
                        Err(e) => tracing::error!("Scheduler play failed: {}", e),
                    }
                } else {
                    tracing::warn!("Scheduler target not found on disk: {}", event.target);
                    drop(s);
                    if let Some(ui) = ui_weak.upgrade() {
                        ui.set_now_playing_title(
                            format!("Scheduled: {} (file missing)", event.target).into(),
                        );
                    }
                }
            }
            other => {
                tracing::info!("Scheduler command '{}' (no-op in MVP)", other);
            }
        }
    }

    // -- Navigate --
    {
        let ui_weak = ui.as_weak();
        ui.on_navigate(move |screen: i32| {
            if let Some(ui) = ui_weak.upgrade() {
                ui.set_current_screen(screen);
            }
        });
    }

    // -- Scheduler toggled (master on/off) --
    {
        ui.on_scheduler_toggled(move |enabled| {
            tracing::info!("Scheduler master {}", if enabled { "ON" } else { "OFF" });
        });
    }

    // -- Scheduler save (Add/Edit dialog) --
    {
        let ui_weak = ui.as_weak();
        let scheduler = scheduler.clone();
        ui.on_scheduler_save_event(
            move |idx, name, time, action_idx, target, mon, tue, wed, thu, fri, sat, sun| {
                use crabcore::scheduler::days_from_mask;
                let action = match action_idx {
                    0 => "play",
                    1 => "load",
                    2 => "generate",
                    4 => "queue",
                    _ => "command",
                };
                let mut mask = 0u8;
                if mon {
                    mask |= 1;
                }
                if tue {
                    mask |= 2;
                }
                if wed {
                    mask |= 4;
                }
                if thu {
                    mask |= 8;
                }
                if fri {
                    mask |= 16;
                }
                if sat {
                    mask |= 32;
                }
                if sun {
                    mask |= 64;
                }
                let days = days_from_mask(mask);
                let res = if idx < 0 {
                    scheduler
                        .borrow()
                        .create(name.trim(), action, target.trim(), time.trim(), &days)
                        .map(|_| ())
                } else {
                    let ids: Vec<String> = scheduler
                        .borrow()
                        .list_all()
                        .unwrap_or_default()
                        .into_iter()
                        .map(|e| e.id)
                        .collect();
                    match ids.get(idx as usize) {
                        Some(id) => scheduler.borrow().update(
                            id,
                            name.trim(),
                            action,
                            target.trim(),
                            time.trim(),
                            &days,
                        ),
                        None => Err(crabcore::CrabError::Scheduler("event gone".into())),
                    }
                };
                match res {
                    Ok(()) => {
                        tracing::info!("Scheduler saved: {} at {}", name, time);
                        if let Some(ui) = ui_weak.upgrade() {
                            ui.set_scheduler_error("".into());
                            ui.set_scheduler_show_editor(false);
                            refresh_scheduler(&ui, &scheduler.borrow());
                        }
                    }
                    Err(e) => {
                        tracing::warn!("Scheduler save failed: {}", e);
                        if let Some(ui) = ui_weak.upgrade() {
                            ui.set_scheduler_error(format!("{}", e).into());
                        }
                    }
                }
            },
        );
    }

    // -- Scheduler toggle event --
    {
        let ui_weak = ui.as_weak();
        let scheduler = scheduler.clone();
        ui.on_scheduler_toggle_event(move |idx| {
            let ids: Vec<(String, bool)> = scheduler
                .borrow()
                .list_all()
                .unwrap_or_default()
                .into_iter()
                .map(|e| (e.id, e.enabled))
                .collect();
            if let Some((id, enabled)) = ids.get(idx as usize) {
                if let Err(e) = scheduler.borrow().set_enabled(id, !enabled) {
                    tracing::error!("Scheduler toggle failed: {}", e);
                }
            }
            if let Some(ui) = ui_weak.upgrade() {
                refresh_scheduler(&ui, &scheduler.borrow());
            }
        });
    }

    // -- Scheduler delete event --
    {
        let ui_weak = ui.as_weak();
        let scheduler = scheduler.clone();
        ui.on_scheduler_delete_event(move |idx| {
            let ids: Vec<String> = scheduler
                .borrow()
                .list_all()
                .unwrap_or_default()
                .into_iter()
                .map(|e| e.id)
                .collect();
            if let Some(id) = ids.get(idx as usize) {
                if let Err(e) = scheduler.borrow().delete(id) {
                    tracing::error!("Scheduler delete failed: {}", e);
                }
            }
            if let Some(ui) = ui_weak.upgrade() {
                refresh_scheduler(&ui, &scheduler.borrow());
            }
        });
    }

    // -- Scheduler run event (wire generate / load to engine) --
    {
        let state = state.clone();
        let ui_weak = ui.as_weak();
        let scheduler = scheduler.clone();
        ui.on_scheduler_run_event(move |idx| {
            let event = scheduler
                .borrow()
                .list_all()
                .unwrap_or_default()
                .into_iter()
                .nth(idx as usize);
            let Some(event) = event else { return };
            fire_scheduled_event(&state, &ui_weak, &event);
        });
    }

    // -- Ads: push blocks to UI --
    fn short_name(path: &str) -> String {
        PathBuf::from(path)
            .file_name()
            .map(|n| n.to_string_lossy().to_string())
            .unwrap_or_default()
    }

    fn refresh_ads(ui: &MainWindow, ads: &crabcore::ads::AdsManager) {
        use crabcore::scheduler::mask_from_days;
        let blocks = ads.list_all().unwrap_or_default();
        let rows: Vec<AdRow> = blocks
            .iter()
            .map(|b| {
                let mask = mask_from_days(&b.days);
                AdRow {
                    name: b.name.clone().into(),
                    time: b.play_time.clone().into(),
                    dates: format!("{} → {}", b.start_date, b.end_date).into(),
                    spot: short_name(&b.spot_path).into(),
                    days: b.days.clone().into(),
                    enabled: b.enabled,
                    mon: mask & 1 != 0,
                    tue: mask & 2 != 0,
                    wed: mask & 4 != 0,
                    thu: mask & 8 != 0,
                    fri: mask & 16 != 0,
                    sat: mask & 32 != 0,
                    sun: mask & 64 != 0,
                    spot_full: b.spot_path.clone().into(),
                    intro_full: b.intro_path.clone().unwrap_or_default().into(),
                    outro_full: b.outro_path.clone().unwrap_or_default().into(),
                    start_full: b.start_date.to_string().into(),
                    end_full: b.end_date.to_string().into(),
                }
            })
            .collect();
        let model = Rc::new(slint::VecModel::from(rows));
        ui.set_ads_items(model.into());
    }
    ui.set_ads_show_editor(false);
    ui.set_ads_error("".into());
    refresh_ads(&ui, &ads.borrow());

    // Shared break firing: intro now, spot + outro chained on the queue.
    fn fire_ad_block(
        state: &Rc<RefCell<AppState>>,
        ui_weak: &slint::Weak<MainWindow>,
        block: &crabcore::ads::AdBlock,
    ) {
        if !PathBuf::from(&block.spot_path).is_file() {
            tracing::warn!(
                "Ad block '{}' skipped, spot missing: {}",
                block.name,
                block.spot_path
            );
            if let Some(ui) = ui_weak.upgrade() {
                ui.set_now_playing_title(format!("Ad '{}': spot missing", block.name).into());
            }
            return;
        }
        tracing::info!("Ad break firing: {}", block.name);
        let s = state.borrow();
        let mut first = true;
        for clip in block.chain() {
            let r = if first {
                s.player.play(&PathBuf::from(&clip))
            } else {
                s.player.queue(&PathBuf::from(&clip))
            };
            if let Err(e) = r {
                tracing::error!("Ad clip failed ({}): {}", clip, e);
                return;
            }
            first = false;
        }
        if let Some(t) = s.library.find_by_path(&block.spot_path).ok().flatten() {
            let _ = s.library.record_play(&t.id, t.duration_secs);
        }
        drop(s);
        if let Some(ui) = ui_weak.upgrade() {
            ui.set_is_playing(true);
            ui.set_now_playing_title(format!("AD: {}", block.name).into());
            ui.set_now_playing_artist("Ad break".into());
        }
    }

    // -- Ads save (Add/Edit dialog) --
    {
        let ui_weak = ui.as_weak();
        let ads = ads.clone();
        ui.on_ads_save_block(
            move |idx,
                  name,
                  spot,
                  intro,
                  outro,
                  start,
                  end,
                  time,
                  mon,
                  tue,
                  wed,
                  thu,
                  fri,
                  sat,
                  sun| {
                use crabcore::scheduler::days_from_mask;
                let mut mask = 0u8;
                if mon {
                    mask |= 1;
                }
                if tue {
                    mask |= 2;
                }
                if wed {
                    mask |= 4;
                }
                if thu {
                    mask |= 8;
                }
                if fri {
                    mask |= 16;
                }
                if sat {
                    mask |= 32;
                }
                if sun {
                    mask |= 64;
                }
                let days = days_from_mask(mask);
                let res = if idx < 0 {
                    ads.borrow()
                        .create(
                            name.trim(),
                            spot.trim(),
                            intro.trim(),
                            outro.trim(),
                            start.trim(),
                            end.trim(),
                            time.trim(),
                            &days,
                        )
                        .map(|_| ())
                } else {
                    let ids: Vec<String> = ads
                        .borrow()
                        .list_all()
                        .unwrap_or_default()
                        .into_iter()
                        .map(|b| b.id)
                        .collect();
                    match ids.get(idx as usize) {
                        Some(id) => ads.borrow().update(
                            id,
                            name.trim(),
                            spot.trim(),
                            intro.trim(),
                            outro.trim(),
                            start.trim(),
                            end.trim(),
                            time.trim(),
                            &days,
                        ),
                        None => Err(crabcore::CrabError::Scheduler("block gone".into())),
                    }
                };
                match res {
                    Ok(()) => {
                        tracing::info!("Ad block saved: {} at {}", name, time);
                        if let Some(ui) = ui_weak.upgrade() {
                            ui.set_ads_error("".into());
                            ui.set_ads_show_editor(false);
                            refresh_ads(&ui, &ads.borrow());
                        }
                    }
                    Err(e) => {
                        tracing::warn!("Ad block save failed: {}", e);
                        if let Some(ui) = ui_weak.upgrade() {
                            ui.set_ads_error(format!("{}", e).into());
                        }
                    }
                }
            },
        );
    }

    // -- Ads toggle / delete / run --
    {
        let ui_weak = ui.as_weak();
        let ads = ads.clone();
        ui.on_ads_toggle_block(move |idx| {
            let ids: Vec<(String, bool)> = ads
                .borrow()
                .list_all()
                .unwrap_or_default()
                .into_iter()
                .map(|b| (b.id, b.enabled))
                .collect();
            if let Some((id, enabled)) = ids.get(idx as usize) {
                if let Err(e) = ads.borrow().set_enabled(id, !enabled) {
                    tracing::error!("Ad toggle failed: {}", e);
                }
            }
            if let Some(ui) = ui_weak.upgrade() {
                refresh_ads(&ui, &ads.borrow());
            }
        });
    }
    {
        let ui_weak = ui.as_weak();
        let ads = ads.clone();
        ui.on_ads_delete_block(move |idx| {
            let ids: Vec<String> = ads
                .borrow()
                .list_all()
                .unwrap_or_default()
                .into_iter()
                .map(|b| b.id)
                .collect();
            if let Some(id) = ids.get(idx as usize) {
                if let Err(e) = ads.borrow().delete(id) {
                    tracing::error!("Ad delete failed: {}", e);
                }
            }
            if let Some(ui) = ui_weak.upgrade() {
                refresh_ads(&ui, &ads.borrow());
            }
        });
    }
    {
        let state = state.clone();
        let ui_weak = ui.as_weak();
        let ads = ads.clone();
        ui.on_ads_run_block(move |idx| {
            let block = ads
                .borrow()
                .list_all()
                .unwrap_or_default()
                .into_iter()
                .nth(idx as usize);
            let Some(block) = block else { return };
            fire_ad_block(&state, &ui_weak, &block);
        });
    }

    // -- Scheduler auto-tick: fire due events while master is ON --
    // Runs on the UI thread (Slint Timer), so Engine/Library Rc access is safe.
    // Dedupes per (event, minute) so a 15s tick fires each event once.
    {
        let state = state.clone();
        let ui_weak = ui.as_weak();
        let scheduler = scheduler.clone();
        let ads = ads.clone();
        let fired: Rc<RefCell<HashMap<String, String>>> = Rc::new(RefCell::new(HashMap::new()));
        let fired_ads: Rc<RefCell<HashMap<String, String>>> = Rc::new(RefCell::new(HashMap::new()));
        let tick = slint::Timer::default();
        tick.start(
            slint::TimerMode::Repeated,
            std::time::Duration::from_secs(15),
            move || {
                let Some(ui) = ui_weak.upgrade() else { return };
                if !ui.get_scheduler_enabled() {
                    return;
                }
                let now = chrono::Local::now();
                let hhmm = now.format("%H:%M").to_string();
                let weekday = now.format("%a").to_string();
                let minute_key = now.format("%Y-%m-%d %H:%M").to_string();
                let due = scheduler
                    .borrow()
                    .due_events(&hhmm, &weekday)
                    .unwrap_or_default();
                let mut fired = fired.borrow_mut();
                for event in due {
                    let already = fired
                        .get(&event.id)
                        .map(|m| m == &minute_key)
                        .unwrap_or(false);
                    if already {
                        continue;
                    }
                    fired.insert(event.id.clone(), minute_key.clone());
                    fire_scheduled_event(&state, &ui_weak, &event);
                }
                drop(fired);
                // Ad blocks (validity window + weekday aware), same dedupe.
                let due_ads = ads
                    .borrow()
                    .due_blocks(now.date_naive(), &hhmm, &weekday)
                    .unwrap_or_default();
                if !due_ads.is_empty() {
                    let mut fired_ads = fired_ads.borrow_mut();
                    for block in due_ads {
                        let already = fired_ads
                            .get(&block.id)
                            .map(|m| m == &minute_key)
                            .unwrap_or(false);
                        if already {
                            continue;
                        }
                        fired_ads.insert(block.id.clone(), minute_key.clone());
                        fire_ad_block(&state, &ui_weak, &block);
                    }
                }
            },
        );
        // App-lifetime timer: intentionally never stopped.
        std::mem::forget(tick);
    }

    // -- Silence monitor: every 5s, recover dead air with a filler track --
    // Metering lives in CpalEngine; on rodio (default) the alarm never trips.
    // Recovery is rate-limited to once per minute to avoid storms.
    {
        let state = state.clone();
        let ui_weak = ui.as_weak();
        let last_recovery: Rc<RefCell<Option<String>>> = Rc::new(RefCell::new(None));
        let tick = slint::Timer::default();
        tick.start(
            slint::TimerMode::Repeated,
            std::time::Duration::from_secs(5),
            move || {
                let s = state.borrow();
                if !s.player.silence_alarm() {
                    return;
                }
                let minute = chrono::Local::now().format("%Y-%m-%d %H:%M").to_string();
                if last_recovery.borrow().as_ref() == Some(&minute) {
                    return;
                }
                *last_recovery.borrow_mut() = Some(minute);
                let current = s.player.current_track().map(|t| t.path);
                let filler = s
                    .library
                    .list_by_kind(TrackKind::Music)
                    .unwrap_or_default()
                    .into_iter()
                    .find(|t| {
                        Some(PathBuf::from(&t.file_path)) != current
                            && PathBuf::from(&t.file_path).is_file()
                    });
                match filler {
                    Some(t) => {
                        tracing::error!(
                            "SILENCE DETECTED — auto-recovering with filler: {}",
                            t.file_path
                        );
                        let path = PathBuf::from(&t.file_path);
                        let label = t.title.clone().unwrap_or_else(|| t.file_name.clone());
                        match s.player.play(&path) {
                            Ok(()) => {
                                let _ = s.library.record_play(&t.id, t.duration_secs);
                                drop(s);
                                if let Some(ui) = ui_weak.upgrade() {
                                    ui.set_is_playing(true);
                                    ui.set_now_playing_title(
                                        format!("⚠ Recovered: {}", label).into(),
                                    );
                                    ui.set_now_playing_artist("Silence detector".into());
                                }
                            }
                            Err(e) => tracing::error!("Filler play failed: {}", e),
                        }
                    }
                    None => {
                        tracing::error!("SILENCE DETECTED — no playable filler in library");
                        drop(s);
                        if let Some(ui) = ui_weak.upgrade() {
                            ui.set_now_playing_title("⚠ SILENCE — no filler available".into());
                        }
                    }
                }
            },
        );
        std::mem::forget(tick);
    }

    // -- Cart Wall: push pads to UI --
    fn refresh_carts(
        ui: &MainWindow,
        carts: &crabcore::cart::CartManager,
        library: &crabcore::library::Library,
    ) {
        let kinds: HashMap<String, TrackKind> = library
            .get_all_tracks()
            .unwrap_or_default()
            .into_iter()
            .map(|t| (t.file_path, t.kind))
            .collect();
        let all = carts.list_all().unwrap_or_default();
        let rows: Vec<CartRow> = all
            .iter()
            .map(|c| {
                let file_name = PathBuf::from(&c.file_path)
                    .file_name()
                    .map(|n| n.to_string_lossy().to_string())
                    .unwrap_or_default();
                let kind_label = match kinds.get(&c.file_path) {
                    Some(TrackKind::Jingle) => "Jingle",
                    Some(TrackKind::Ad) => "Ad",
                    _ => "Music",
                };
                CartRow {
                    label: c.label.clone().into(),
                    sub: format!("{} • {}", kind_label, file_name).into(),
                    has_file: PathBuf::from(&c.file_path).is_file(),
                }
            })
            .collect();
        let model = Rc::new(slint::VecModel::from(rows));
        ui.set_cart_items(model.into());
    }
    ui.set_cart_status("".into());
    refresh_carts(&ui, &carts.borrow(), &state.borrow().library);

    // -- Cart play (instant) --
    {
        let state = state.clone();
        let ui_weak = ui.as_weak();
        let carts = carts.clone();
        ui.on_cart_play(move |idx| {
            let cart = carts
                .borrow()
                .list_all()
                .unwrap_or_default()
                .into_iter()
                .nth(idx as usize);
            let Some(cart) = cart else { return };
            let path = PathBuf::from(&cart.file_path);
            if !path.is_file() {
                tracing::warn!("Cart '{}' file missing: {}", cart.label, cart.file_path);
                if let Some(ui) = ui_weak.upgrade() {
                    ui.set_cart_status(format!("⚠ '{}' file missing", cart.label).into());
                }
                return;
            }
            let s = state.borrow();
            let logged = s
                .library
                .find_by_path(&cart.file_path)
                .ok()
                .flatten()
                .map(|t| (t.id, t.duration_secs));
            match s.player.play(&path) {
                Ok(()) => {
                    tracing::info!("Cart fired: {}", cart.label);
                    if let Some((id, dur)) = logged {
                        let _ = s.library.record_play(&id, dur);
                    }
                    drop(s);
                    if let Some(ui) = ui_weak.upgrade() {
                        ui.set_is_playing(true);
                        ui.set_now_playing_title(cart.label.clone().into());
                        ui.set_now_playing_artist("Cart".into());
                        ui.set_cart_status(format!("▶ {}", cart.label).into());
                    }
                }
                Err(e) => tracing::error!("Cart play failed: {}", e),
            }
        });
    }

    // -- Cart delete --
    {
        let state = state.clone();
        let ui_weak = ui.as_weak();
        let carts = carts.clone();
        ui.on_cart_delete(move |idx| {
            let ids: Vec<String> = carts
                .borrow()
                .list_all()
                .unwrap_or_default()
                .into_iter()
                .map(|c| c.id)
                .collect();
            if let Some(id) = ids.get(idx as usize) {
                if let Err(e) = carts.borrow().delete(id) {
                    tracing::error!("Cart delete failed: {}", e);
                }
            }
            if let Some(ui) = ui_weak.upgrade() {
                let s = state.borrow();
                refresh_carts(&ui, &carts.borrow(), &s.library);
            }
        });
    }

    // -- Cart add: next jingle first, then any track not on a pad (max 8) --
    {
        let state = state.clone();
        let ui_weak = ui.as_weak();
        let carts = carts.clone();
        ui.on_cart_add(move || {
            let existing: Vec<String> = carts
                .borrow()
                .list_all()
                .unwrap_or_default()
                .into_iter()
                .map(|c| c.file_path)
                .collect();
            if existing.len() >= 8 {
                if let Some(ui) = ui_weak.upgrade() {
                    ui.set_cart_status("Cart wall is full (8)".into());
                }
                return;
            }
            let s = state.borrow();
            let tracks = s.library.get_all_tracks().unwrap_or_default();
            let next = tracks
                .iter()
                .find(|t| t.kind == TrackKind::Jingle && !existing.contains(&t.file_path))
                .or_else(|| tracks.iter().find(|t| !existing.contains(&t.file_path)));
            match next {
                Some(t) => {
                    let label = t.title.clone().unwrap_or_else(|| t.file_name.clone());
                    if let Err(e) = carts.borrow().create(&label, &t.file_path) {
                        tracing::error!("Cart add failed: {}", e);
                    }
                    if let Some(ui) = ui_weak.upgrade() {
                        ui.set_cart_status(format!("Loaded '{}'", label).into());
                        refresh_carts(&ui, &carts.borrow(), &s.library);
                    }
                }
                None => {
                    if let Some(ui) = ui_weak.upgrade() {
                        ui.set_cart_status("Import tracks first".into());
                    }
                }
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

    // -- Settings: device list, selection, live DSP prefs --
    fn push_devices(ui: &MainWindow) {
        let devs = crabcore::audio::CpalEngine::list_output_devices();
        let model = Rc::new(slint::VecModel::from(
            devs.into_iter()
                .map(|d| d.into())
                .collect::<Vec<slint::SharedString>>(),
        ));
        ui.set_settings_devices(model.into());
    }
    fn settings_labels(ui: &MainWindow, settings: &crabcore::settings::AppSettings) {
        ui.set_settings_xfade(format!("{:.1} s", settings.crossfade_secs).into());
        ui.set_settings_silence(format!("{:.0} s", settings.silence_threshold_secs).into());
    }
    push_devices(&ui);
    ui.set_settings_device(
        settings
            .borrow()
            .output_device
            .clone()
            .unwrap_or_default()
            .into(),
    );
    ui.set_settings_device_note("".into());
    settings_labels(&ui, &settings.borrow());
    {
        let ui_weak = ui.as_weak();
        ui.on_settings_refresh_devices(move || {
            if let Some(ui) = ui_weak.upgrade() {
                push_devices(&ui);
            }
        });
    }
    {
        let ui_weak = ui.as_weak();
        let settings = settings.clone();
        let settings_path = settings_path.clone();
        ui.on_settings_select_device(move |name| {
            let name = name.to_string();
            settings.borrow_mut().output_device = Some(name.clone());
            if settings.borrow().save(&settings_path).is_ok() {
                tracing::info!("Output device set to '{}' (restart to apply)", name);
            }
            if let Some(ui) = ui_weak.upgrade() {
                ui.set_settings_device(name.into());
                ui.set_settings_device_note("Restart CrabBoss to apply the new device".into());
            }
        });
    }
    // Stepper helper: adjust, clamp, persist, apply live, relabel.
    {
        let state = state.clone();
        let ui_weak = ui.as_weak();
        let settings = settings.clone();
        let settings_path = settings_path.clone();
        ui.on_settings_xfade_inc(move || {
            let mut s = settings.borrow_mut();
            s.crossfade_secs = (s.crossfade_secs + 0.5).clamp(0.0, 30.0);
            let _ = s.save(&settings_path);
            state.borrow().player.set_crossfade_secs(s.crossfade_secs);
            if let Some(ui) = ui_weak.upgrade() {
                settings_labels(&ui, &s);
            }
        });
    }
    {
        let state = state.clone();
        let ui_weak = ui.as_weak();
        let settings = settings.clone();
        let settings_path = settings_path.clone();
        ui.on_settings_xfade_dec(move || {
            let mut s = settings.borrow_mut();
            s.crossfade_secs = (s.crossfade_secs - 0.5).clamp(0.0, 30.0);
            let _ = s.save(&settings_path);
            state.borrow().player.set_crossfade_secs(s.crossfade_secs);
            if let Some(ui) = ui_weak.upgrade() {
                settings_labels(&ui, &s);
            }
        });
    }
    {
        let state = state.clone();
        let ui_weak = ui.as_weak();
        let settings = settings.clone();
        let settings_path = settings_path.clone();
        ui.on_settings_silence_inc(move || {
            let mut s = settings.borrow_mut();
            s.silence_threshold_secs = (s.silence_threshold_secs + 1.0).clamp(1.0, 120.0);
            let _ = s.save(&settings_path);
            state
                .borrow()
                .player
                .set_silence_threshold_secs(s.silence_threshold_secs);
            if let Some(ui) = ui_weak.upgrade() {
                settings_labels(&ui, &s);
            }
        });
    }
    {
        let state = state.clone();
        let ui_weak = ui.as_weak();
        let settings = settings.clone();
        let settings_path = settings_path.clone();
        ui.on_settings_silence_dec(move || {
            let mut s = settings.borrow_mut();
            s.silence_threshold_secs = (s.silence_threshold_secs - 1.0).clamp(1.0, 120.0);
            let _ = s.save(&settings_path);
            state
                .borrow()
                .player
                .set_silence_threshold_secs(s.silence_threshold_secs);
            if let Some(ui) = ui_weak.upgrade() {
                settings_labels(&ui, &s);
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

    // -- Import Files (native dialog, multi-select audio) --
    {
        let state = state.clone();
        let ui_weak = ui.as_weak();
        let last_shown = last_shown.clone();
        ui.on_import_files(move || {
            let files = rfd::FileDialog::new()
                .set_title("Import audio files")
                .add_filter(
                    "Audio",
                    &[
                        "mp3", "flac", "wav", "ogg", "oga", "aac", "m4a", "opus", "aiff", "wv",
                    ],
                )
                .pick_files();
            let Some(files) = files else { return };
            let s = state.borrow();
            let mut added = 0;
            let mut skipped = 0;
            for f in &files {
                match s.library.add_track(f) {
                    Ok(t) => {
                        tracing::info!("Imported {} as {:?}", f.display(), t.kind);
                        added += 1;
                    }
                    Err(e) => {
                        tracing::warn!("Skipping {}: {}", f.display(), e);
                        skipped += 1;
                    }
                }
            }
            let tracks = s.library.get_all_tracks().unwrap_or_default();
            drop(s);
            if let Some(ui) = ui_weak.upgrade() {
                ui.set_track_count(tracks.len() as i32);
                ui.set_library_status(
                    if skipped > 0 {
                        format!("Imported {}, skipped {}", added, skipped)
                    } else {
                        format!("Imported {}", added)
                    }
                    .into(),
                );
                refresh_library(&ui, tracks, &last_shown);
            }
        });
    }

    // -- Library Search (live filter) --
    {
        let state = state.clone();
        let last_shown = last_shown.clone();
        let ui_weak = ui.as_weak();
        ui.on_library_search_changed(move |query| {
            let s = state.borrow();
            let tracks = if query.trim().is_empty() {
                s.library.get_all_tracks().unwrap_or_default()
            } else {
                s.library.search(query.trim()).unwrap_or_default()
            };
            drop(s);
            if let Some(ui) = ui_weak.upgrade() {
                refresh_library(&ui, tracks, &last_shown);
            }
        });
    }

    // -- Library Track Double-Click (Play) --
    {
        let state = state.clone();
        let ui_weak = ui.as_weak();
        let last_shown = last_shown.clone();
        ui.on_library_track_double_clicked(move |index: i32| {
            let shown = last_shown.borrow();
            let track = shown.get(index as usize).cloned();
            drop(shown);
            let Some(track) = track else { return };
            let s = state.borrow();
            let path = PathBuf::from(&track.file_path);
            tracing::info!("Playing track: {:?}", path);

            match s.player.play(&path) {
                Ok(()) => {
                    let _ = s.library.record_play(&track.id, track.duration_secs);
                    if let Some(ui) = ui_weak.upgrade() {
                        let title = track
                            .title
                            .clone()
                            .unwrap_or_else(|| track.file_name.clone());
                        let artist = track.artist.clone().unwrap_or_default();
                        let dur = fmt_dur(track.duration_secs);

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
        });
    }

    // -- Reports: range query (jingles/ads excluded), newest first --
    fn report_range_bounds(idx: i32) -> (chrono::DateTime<chrono::Utc>, String) {
        use chrono::{Duration, Local};
        let now = Local::now();
        let label = match idx {
            0 => "Today",
            2 => "Last 30 days",
            3 => "All time",
            _ => "Last 7 days",
        }
        .to_string();
        let from = match idx {
            0 => now
                .date_naive()
                .and_hms_opt(0, 0, 0)
                .unwrap()
                .and_local_timezone(Local)
                .unwrap(),
            2 => now - Duration::days(30),
            3 => now - Duration::days(365 * 20),
            _ => now - Duration::days(7),
        };
        (from.with_timezone(&chrono::Utc), label)
    }

    fn refresh_report(ui: &MainWindow, state: &Rc<RefCell<AppState>>, range_idx: i32) {
        use crabcore::library::TrackKind;
        let (from, label) = report_range_bounds(range_idx);
        let to = chrono::Utc::now();
        let s = state.borrow();
        let entries = crabcore::report::play_report(
            &s.library,
            from,
            to,
            &[TrackKind::Jingle, TrackKind::Ad],
        )
        .unwrap_or_default();
        drop(s);
        let airtime: f64 = entries.iter().filter_map(|e| e.duration_secs).sum();
        let rows: Vec<ReportRow> = entries
            .iter()
            .take(100)
            .map(|e| ReportRow {
                time: e.played_at.format("%d/%m %H:%M").to_string().into(),
                title: e.title.clone().into(),
                artist: e.artist.clone().into(),
                kind: e.kind.as_str().into(),
            })
            .collect();
        let model = Rc::new(slint::VecModel::from(rows));
        ui.set_report_entries(model.into());
        ui.set_report_summary(
            format!(
                "{}: {} plays • {:.0} min music airtime (jingles/ads excluded{})",
                label,
                entries.len(),
                airtime / 60.0,
                if entries.len() > 100 {
                    "; showing newest 100"
                } else {
                    ""
                }
            )
            .into(),
        );
    }
    refresh_report(&ui, &state, 1);
    {
        let state = state.clone();
        let ui_weak = ui.as_weak();
        ui.on_report_range_changed(move |idx| {
            if let Some(ui) = ui_weak.upgrade() {
                ui.set_report_range(idx);
                refresh_report(&ui, &state, idx);
            }
        });
    }
    {
        let state = state.clone();
        let ui_weak = ui.as_weak();
        ui.on_report_export(move || {
            let path = rfd::FileDialog::new()
                .set_title("Export play report (CSV)")
                .set_file_name("crabboss-report.csv")
                .add_filter("CSV", &["csv"])
                .save_file();
            let Some(path) = path else { return };
            let (from, _) = report_range_bounds(
                ui_weak
                    .upgrade()
                    .map(|ui| ui.get_report_range())
                    .unwrap_or(1),
            );
            let s = state.borrow();
            let entries = crabcore::report::play_report(
                &s.library,
                from,
                chrono::Utc::now(),
                &[
                    crabcore::library::TrackKind::Jingle,
                    crabcore::library::TrackKind::Ad,
                ],
            )
            .unwrap_or_default();
            drop(s);
            match std::fs::write(&path, crabcore::report::to_csv(&entries)) {
                Ok(()) => {
                    tracing::info!(
                        "Report exported: {} ({} rows)",
                        path.display(),
                        entries.len()
                    );
                    if let Some(ui) = ui_weak.upgrade() {
                        ui.set_report_summary(
                            format!("Exported {} rows to {}", entries.len(), path.display()).into(),
                        );
                    }
                }
                Err(e) => tracing::error!("Report export failed: {}", e),
            }
        });
    }

    tracing::info!("🦀 CrabBoss UI ready — launching window");
    ui.run()?;

    Ok(())
}
