//! CrabBoss
//!
//! Desktop UI entry point using Iced (Elm architecture).

use std::collections::{HashMap, VecDeque};
use std::path::PathBuf;
use std::sync::mpsc::{self, Receiver, TryRecvError};
use std::time::Duration;

use iced::{
    widget::{
        button, checkbox, column, container, progress_bar, row, scrollable, slider, text,
        text_input,
    },
    Element, Length, Subscription, Task, Theme,
};

use crabcore::audio::{
    Engine, PlayerState, EQ_BAND_COUNT, EQ_CENTER_HZ, MAX_GAIN_DB, TARGET_MAX_LUFS, TARGET_MIN_LUFS,
};
use crabcore::library::{Library, Track, TrackKind};
use crabcore::playlist::PlaylistManager;

// ---------------------------------------------------------------------------
// Helpers (same behavior as before)
// ---------------------------------------------------------------------------

struct LoudnessDone {
    id: String,
    file_name: String,
    lufs: f32,
    gain_db: f32,
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

/// Display title: metadata title, else the file name without its container
/// extension ("Song.mp3" -> "Song"). Display-only; stored data is untouched.
fn track_label(t: &Track) -> String {
    t.title
        .clone()
        .unwrap_or_else(|| strip_audio_extension(&t.file_name))
}

/// Strip the trailing container extension for display. Keeps names without
/// a dot (or dotfiles) as-is.
fn strip_audio_extension(name: &str) -> String {
    match name.rfind('.') {
        Some(i) if i > 0 => name[..i].to_string(),
        _ => name.to_string(),
    }
}

/// "Title - Artist", omitting the separator when one side is missing so
/// untagged files never render a dangling " - ".
fn join_title_artist(title: &str, artist: &str) -> String {
    match (title.is_empty(), artist.is_empty()) {
        (false, false) => format!("{title} - {artist}"),
        (false, true) => title.to_string(),
        (true, false) => artist.to_string(),
        (true, true) => String::new(),
    }
}

/// On-air line with a source tag only when certain: the artist slot holds
/// either a real artist (manual play — no tag, nothing claimed) or one of
/// our own automation sentinels, which moves to a `· via X` suffix instead
/// of masquerading as the artist.
/// Track line with a source tag only when certain: the artist slot holds
/// either a real artist (manual play — no tag, nothing claimed) or one of
/// our own automation sentinels, which moves to a `· via X` suffix instead
/// of masquerading as the artist. Shared by the strip, footer, and Home.
fn track_source_label(title: &str, artist: &str) -> String {
    const SOURCES: [&str; 5] = [
        "Auto-DJ",
        "Cart",
        "Scheduler",
        "Ad break",
        "Silence detector",
    ];
    if SOURCES.contains(&artist) && !title.is_empty() {
        format!("{title} · via {artist}")
    } else {
        join_title_artist(title, artist)
    }
}

fn on_air_label(title: &str, artist: &str) -> String {
    format!("ON AIR: {}", track_source_label(title, artist))
}

/// `--engine cpal` (only backend; `--engine rodio` warns and uses cpal).
fn engine_choice() -> String {
    let mut args = std::env::args().skip(1);
    let mut choice = std::env::var("CRABBOSS_ENGINE").unwrap_or_else(|_| "cpal".into());
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

fn lin_to_dbfs(lin: f32) -> f32 {
    20.0 * lin.max(0.001).log10()
}

fn stream_bitrate_step(current: u32, up: bool) -> u32 {
    const LADDER: [u32; 16] = [
        8, 16, 24, 32, 40, 48, 64, 80, 96, 112, 128, 160, 192, 224, 256, 320,
    ];
    let idx = LADDER
        .iter()
        .position(|&b| b >= current)
        .unwrap_or(LADDER.len() - 1);
    match up {
        true => LADDER[(idx + 1).min(LADDER.len() - 1)],
        false => LADDER[idx.saturating_sub(1)],
    }
}

fn duck_ms_step(ladder: &[f32], current: f32, up: bool) -> f32 {
    let idx = ladder
        .iter()
        .position(|&b| b >= current)
        .unwrap_or(ladder.len() - 1);
    match up {
        true => ladder[(idx + 1).min(ladder.len() - 1)],
        false => ladder[idx.saturating_sub(1)],
    }
}

const ATTACK_LADDER: [f32; 9] = [1.0, 2.0, 5.0, 10.0, 20.0, 50.0, 100.0, 200.0, 500.0];
const RELEASE_LADDER: [f32; 9] = [10.0, 25.0, 50.0, 100.0, 200.0, 400.0, 800.0, 1500.0, 3000.0];

fn action_name(idx: usize) -> &'static str {
    match idx {
        0 => "play",
        1 => "load",
        2 => "generate",
        4 => "queue",
        _ => "command",
    }
}

fn action_label(idx: usize) -> &'static str {
    match idx {
        0 => "play",
        1 => "load",
        2 => "generate",
        3 => "command",
        4 => "queue",
        _ => "command",
    }
}

fn report_range_bounds(idx: usize) -> (chrono::DateTime<chrono::Utc>, String) {
    use chrono::{Duration as CDur, Local};
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
        2 => now - CDur::days(30),
        3 => now - CDur::days(365 * 20),
        _ => now - CDur::days(7),
    };
    (from.with_timezone(&chrono::Utc), label)
}

fn eq_band_label(band: usize) -> String {
    let hz = EQ_CENTER_HZ.get(band).copied().unwrap_or(0.0);
    if hz >= 1000.0 {
        format!("{:.1}k", hz / 1000.0)
    } else {
        format!("{:.0}", hz)
    }
}

fn short_name(path: &str) -> String {
    PathBuf::from(path)
        .file_name()
        .map(|n| n.to_string_lossy().to_string())
        .unwrap_or_default()
}

// ---------------------------------------------------------------------------
// Navigation + messages
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
enum Screen {
    #[default]
    Home,
    Playout,
    Media,
    Scheduler,
    Carts,
    Reports,
    Ads,
    Settings,
}

impl Screen {
    fn label(self) -> &'static str {
        match self {
            Screen::Home => "Home",
            Screen::Playout => "Playout",
            Screen::Media => "Library",
            Screen::Scheduler => "Scheduler",
            Screen::Carts => "Cart Wall",
            Screen::Reports => "Reports",
            Screen::Ads => "Ads",
            Screen::Settings => "Settings",
        }
    }
}

/// Settings sub-pages (Windows-Settings style: category list + detail).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
enum SettingsSection {
    #[default]
    Station,
    AudioDevice,
    Playout,
    Equalizer,
    Loudness,
    Streaming,
    Microphone,
    License,
}

impl SettingsSection {
    fn label(self) -> &'static str {
        match self {
            SettingsSection::Station => "Station Name",
            SettingsSection::AudioDevice => "Audio Device",
            SettingsSection::Playout => "Playout",
            SettingsSection::Equalizer => "Equalizer",
            SettingsSection::Loudness => "Loudness",
            SettingsSection::Streaming => "Streaming",
            SettingsSection::Microphone => "Microphone",
            SettingsSection::License => "License",
        }
    }

    /// One-line subtitle shown under the section title.
    fn description(self) -> &'static str {
        match self {
            SettingsSection::Station => "Station identity shown in the sidebar and dashboard.",
            SettingsSection::AudioDevice => {
                "Monitoring output. Applies on restart; falls back to the system default if unplugged."
            }
            SettingsSection::Playout => "Crossfade length and dead-air alarm threshold.",
            SettingsSection::Equalizer => {
                "12-band program EQ plus the brickwall limiter, applied live."
            }
            SettingsSection::Loudness => "ReplayGain-style normalization toward the target loudness.",
            SettingsSection::Streaming => {
                "Icecast source client: server, mount, encoder, and live status."
            }
            SettingsSection::Microphone => {
                "Live input with voice-activated ducking of the music bed."
            }
            SettingsSection::License => "Offline license key and status.",
        }
    }
}

#[derive(Debug, Clone)]
enum Message {
    Navigate(Screen),
    SettingsNav(SettingsSection),
    Tick,
    // Transport
    Play,
    Pause,
    Stop,
    Next,
    Prev,
    VolumeChanged(f32),
    AutodjToggled(bool),
    // Library
    LibrarySearchChanged(String),
    LibraryTrackSelected(usize),
    LibraryTrackPlay(usize),
    ImportFiles,
    HealthCheck,
    LoudnessScan,
    // Scheduler
    SchedulerMasterToggled(bool),
    SchedulerToggleEvent(usize),
    SchedulerDeleteEvent(usize),
    SchedulerRunEvent(usize),
    SchedulerNew,
    SchedulerEdit(usize),
    SchedulerEditorClose,
    SchedName(String),
    SchedTime(String),
    SchedTarget(String),
    SchedExpires(String),
    SchedActionPrev,
    SchedActionNext,
    SchedDayChanged(usize, bool),
    SchedulerSave,
    // Carts
    CartPlay(usize),
    CartHotkey(usize),
    CartDelete(usize),
    CartAdd,
    CartPlace(usize),
    CartToggleAssign,
    // Reports
    ReportRangeChanged(usize),
    ReportExport,
    // Ads
    AdsToggle(usize),
    AdsDelete(usize),
    AdsRun(usize),
    AdsNew,
    AdsEdit(usize),
    AdsEditorClose,
    AdName(String),
    AdSpot(String),
    AdIntro(String),
    AdOutro(String),
    AdStart(String),
    AdEnd(String),
    AdTime(String),
    AdDayChanged(usize, bool),
    AdsSave,
    // Settings
    SettingsRefreshDevices,
    SettingsSelectDevice(String),
    XfadeInc,
    XfadeDec,
    SilenceInc,
    SilenceDec,
    EqToggle,
    EqInc(usize),
    EqDec(usize),
    EqReset,
    LimiterInc,
    LimiterDec,
    LoudnessToggle,
    LoudnessTargetInc,
    LoudnessTargetDec,
    StreamToggle,
    StreamTlsToggle,
    StreamHost(String),
    StreamUsername(String),
    StreamPort(String),
    StreamMount(String),
    StreamPassword(String),
    StreamBitrateInc,
    StreamBitrateDec,
    MicToggle,
    MicRefreshDevices,
    MicSelectDevice(String),
    MicLevelInc,
    MicLevelDec,
    MicDuckToggle,
    MicThresholdInc,
    MicThresholdDec,
    MicDepthInc,
    MicDepthDec,
    MicAttackInc,
    MicAttackDec,
    MicReleaseInc,
    MicReleaseDec,
    StationName(String),
    // License
    LicenseKeyInput(String),
    ActivateLicense,
    ClearLicense,
}

// ---------------------------------------------------------------------------
// App state (lives on the UI thread; the cpal engine is `!Send` by design)
// ---------------------------------------------------------------------------

struct App {
    player: Box<dyn Engine>,
    library: Library,
    playlist_manager: PlaylistManager,
    scheduler: crabcore::scheduler::SchedulerManager,
    carts: crabcore::cart::CartManager,
    ads: crabcore::ads::AdsManager,
    settings: crabcore::settings::AppSettings,
    settings_path: PathBuf,
    license: crabcore::license::LicenseStore,

    screen: Screen,
    settings_section: SettingsSection,
    station_name: String,
    audio_engine: String,

    // Player UI
    is_playing: bool,
    now_title: String,
    now_artist: String,
    volume: f32,
    autodj: bool,
    up_next: String,
    /// Forecast display: next tracks Auto-DJ would pick (recomputed when
    /// the live track changes; a prediction, not a commitment).
    up_next_list: Vec<Track>,
    /// Engine track the forecast was built for (`None` = stale/never).
    up_next_for: Option<PathBuf>,
    auto_continue: bool,
    was_playing: bool,
    /// No-repeat windows across Auto-DJ picks (plus the queued pick): every
    /// track handed to the engine is pushed here so separation bites.
    autodj_history: crabcore::playlist::RuleHistory,
    /// Engine-side current track path as last seen. Direct play paths set
    /// it synchronously; the tick reconciler adopts anything else (promoted
    /// queued decks) with proper logging + labels.
    engine_track: Option<PathBuf>,
    /// Who queued the currently pending deck ("Auto-DJ", "Scheduler").
    /// Set on successful queue(), consumed by the reconcile adopter,
    /// cleared by any direct play or stop. Lets promoted decks keep the
    /// right `· via X` tag instead of guessing.
    pending_source: Option<String>,

    // Library
    lib_tracks: Vec<Track>,
    lib_total: usize,
    lib_search: String,
    lib_selected: Option<usize>,
    lib_status: String,

    // Loudness background scan
    scanning: bool,
    scan_rx: Option<Receiver<LoudnessDone>>,
    scan_done: usize,
    scan_total: usize,

    // Chunked import
    import_active: bool,
    import_pending: VecDeque<PathBuf>,
    import_added: usize,
    import_skipped: usize,
    import_total: usize,

    // Scheduler
    sched_enabled: bool,
    sched_events: Vec<crabcore::scheduler::ScheduledEvent>,
    sched_warnings: Vec<String>,
    sched_editor_open: bool,
    sched_edit_idx: Option<usize>,
    se_name: String,
    se_time: String,
    se_action: usize,
    se_target: String,
    se_expires: String,
    se_days: [bool; 7],
    sched_error: String,
    fired: HashMap<String, String>,
    fired_ads: HashMap<String, String>,

    // Carts
    cart_list: Vec<crabcore::cart::Cart>,
    cart_status: String,
    cart_assign: bool,

    // Reports
    report_entries: Vec<crabcore::report::PlayLogEntry>,
    report_summary: String,
    report_range: usize,

    // Ads
    ad_blocks: Vec<crabcore::ads::AdBlock>,
    ads_editor_open: bool,
    ads_edit_idx: Option<usize>,
    ab_name: String,
    ab_spot: String,
    ab_intro: String,
    ab_outro: String,
    ab_start: String,
    ab_end: String,
    ab_time: String,
    ab_days: [bool; 7],
    ads_error: String,

    // Settings UI caches
    output_devices: Vec<String>,
    sel_device: String,
    device_note: String,
    input_devices: Vec<String>,
    mic_note: String,

    // License UI
    license_status: String,
    license_error: String,
    license_key: String,

    // Home counts
    track_count: usize,
    playlist_count: usize,
    upcoming_count: usize,

    last_recovery: Option<String>,
    tick_count: u64,
}

impl App {
    // -- persistence -------------------------------------------------------
    fn save_settings(&mut self) {
        if let Err(e) = self.settings.save(&self.settings_path) {
            tracing::warn!("Settings save failed: {e}");
        }
    }

    fn refresh_library(&mut self) {
        let tracks = if self.lib_search.trim().is_empty() {
            self.library.get_all_tracks().unwrap_or_default()
        } else {
            self.library
                .search(self.lib_search.trim())
                .unwrap_or_default()
        };
        self.lib_total = self.library.get_all_tracks().unwrap_or_default().len();
        self.lib_tracks = tracks;
        self.track_count = self.lib_total;
        if let Some(sel) = self.lib_selected {
            if sel >= self.lib_tracks.len() {
                self.lib_selected = None;
            }
        }
    }

    fn refresh_scheduler(&mut self) {
        self.sched_events = self.scheduler.list_all().unwrap_or_default();
        let today = chrono::Local::now().format("%Y-%m-%d").to_string();
        self.sched_warnings = self
            .scheduler
            .expiry_warnings(&today, 3)
            .unwrap_or_default();
        self.upcoming_count = self.sched_events.iter().filter(|e| e.enabled).count();
    }

    fn refresh_carts(&mut self) {
        self.cart_list = self.carts.list_all().unwrap_or_default();
    }

    fn play_cart(&mut self, i: usize) {
        let cart = self.cart_list.get(i).cloned();
        let Some(cart) = cart else { return };
        let path = PathBuf::from(&cart.file_path);
        if !path.is_file() {
            tracing::warn!("Cart '{}' file missing: {}", cart.label, cart.file_path);
            self.cart_status = format!("'{}' file missing", cart.label);
            return;
        }
        let logged = self
            .library
            .find_by_path(&cart.file_path)
            .ok()
            .flatten()
            .map(|t| (t.id, t.duration_secs));
        match self.player.play(&path) {
            Ok(()) => {
                tracing::info!("Cart fired: {}", cart.label);
                if let Some((id, dur)) = logged {
                    let _ = self.library.record_play(&id, dur);
                }
                self.auto_continue = true;
                self.is_playing = true;
                self.now_title = cart.label.clone();
                self.now_artist = "Cart".into();
                self.cart_status = format!("Playing {}", cart.label);
                self.engine_track = Some(path);
                self.up_next.clear();
                self.pending_source = None;
            }
            Err(e) => tracing::error!("Cart play failed: {}", e),
        }
    }

    fn refresh_ads(&mut self) {
        self.ad_blocks = self.ads.list_all().unwrap_or_default();
    }

    fn refresh_report(&mut self) {
        let (from, label) = report_range_bounds(self.report_range);
        let to = chrono::Utc::now();
        let entries = crabcore::report::play_report(
            &self.library,
            from,
            to,
            &[TrackKind::Jingle, TrackKind::Ad],
        )
        .unwrap_or_default();
        let airtime: f64 = entries.iter().filter_map(|e| e.duration_secs).sum();
        self.report_summary = format!(
            "{}: {} plays - {:.0} min music airtime (jingles/ads excluded{})",
            label,
            entries.len(),
            airtime / 60.0,
            if entries.len() > 100 {
                "; showing newest 100"
            } else {
                ""
            }
        );
        self.report_entries = entries.into_iter().take(100).collect();
    }

    fn refresh_counts(&mut self) {
        self.track_count = self.library.get_all_tracks().unwrap_or_default().len();
        self.playlist_count = self.playlist_manager.list_all().unwrap_or_default().len();
        self.upcoming_count = self.sched_events.iter().filter(|e| e.enabled).count();
    }

    // -- Auto-DJ ------------------------------------------------------------
    fn autodj_pick(&self) -> Option<Track> {
        crabcore::playlist::generate_next(&self.library, &self.autodj_cfg(), &self.autodj_history)
            .ok()
            .flatten()
    }

    /// One-pick config shared by the pick and the history push sites, so
    /// the rule windows recorded always match the rules picked with.
    fn autodj_cfg(&self) -> crabcore::playlist::GenConfig {
        let now = chrono::Local::now();
        crabcore::playlist::GenConfig {
            target_tracks: 1,
            hour: now.format("%H").to_string().parse().unwrap_or(12),
            weekday: now.format("%a").to_string(),
            ..Default::default()
        }
    }

    fn autodj_play_now(&mut self) {
        let pick = self.autodj_pick();
        let Some(pick) = pick else {
            tracing::warn!("Auto-DJ: library is empty");
            return;
        };
        let path = PathBuf::from(&pick.file_path);
        if !path.is_file() {
            tracing::warn!("Auto-DJ: file missing: {}", pick.file_path);
            return;
        }
        match self.player.play(&path) {
            Ok(()) => {
                let _ = self.library.record_play(&pick.id, pick.duration_secs);
                let label = track_label(&pick);
                tracing::info!("Auto-DJ playing: {}", label);
                self.is_playing = true;
                self.now_title = label;
                self.now_artist = pick
                    .artist
                    .clone()
                    .filter(|a| !a.trim().is_empty())
                    .unwrap_or_else(|| "Auto-DJ".into());
                self.up_next.clear();
                self.autodj_history.push_track(&pick, &self.autodj_cfg());
                self.engine_track = Some(path);
                // Direct play discards any pending queue (and its source).
                self.pending_source = None;
            }
            Err(e) => tracing::error!("Auto-DJ play failed: {}", e),
        }
    }

    // -- Scheduler / ads firing (shared by manual Run + auto-tick) ----------
    fn fire_scheduled_event(&mut self, idx: usize) {
        let event = match self.sched_events.get(idx).cloned() {
            Some(e) => e,
            None => return,
        };
        tracing::info!(
            "Scheduler firing: {} [{} {}]",
            event.name,
            event.action_type,
            event.target
        );
        match event.action_type.as_str() {
            "generate" => {
                let now = chrono::Local::now();
                let cfg = crabcore::playlist::GenConfig {
                    target_tracks: 10,
                    hour: now.format("%H").to_string().parse().unwrap_or(12),
                    weekday: now.format("%a").to_string(),
                    ..Default::default()
                };
                let rotation =
                    crabcore::playlist::generate(&self.library, &cfg).unwrap_or_default();
                let n_music = rotation
                    .iter()
                    .filter(|t| t.kind == TrackKind::Music)
                    .count();
                let n_jingles = rotation
                    .iter()
                    .filter(|t| t.kind == TrackKind::Jingle)
                    .count();
                let pl_name = format!("{} {}", event.target, now.format("%H:%M"));
                match self
                    .playlist_manager
                    .create(&pl_name, Some("Auto-generated rotation"))
                {
                    Ok(pl) => {
                        for t in &rotation {
                            let _ = self.playlist_manager.add_track(
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
                self.playlist_count = self.playlist_manager.list_all().unwrap_or_default().len();
                self.now_title = format!(
                    "Generated '{}': {} music + {} jingles",
                    event.target, n_music, n_jingles
                );
            }
            "queue" => {
                let path = PathBuf::from(&event.target);
                if path.is_file() {
                    match self.player.queue(&path) {
                        Ok(()) => {
                            self.auto_continue = true;
                            self.now_title = format!("Queued after current: {}", event.target);
                            self.pending_source = Some("Scheduler".into());
                        }
                        Err(e) => tracing::error!("Scheduler queue failed: {}", e),
                    }
                } else {
                    tracing::warn!("Scheduler target not found on disk: {}", event.target);
                    self.now_title = format!("Scheduled: {} (file missing)", event.target);
                }
            }
            "load" | "play" => {
                let path = PathBuf::from(&event.target);
                if path.is_file() {
                    let logged = self
                        .library
                        .find_by_path(&event.target)
                        .ok()
                        .flatten()
                        .map(|t| (t.id, t.duration_secs));
                    match self.player.play(&path) {
                        Ok(()) => {
                            if let Some((id, dur)) = logged {
                                let _ = self.library.record_play(&id, dur);
                            }
                            self.auto_continue = true;
                            self.is_playing = true;
                            self.now_title = event.target.clone();
                            self.now_artist = "Scheduler".into();
                            self.engine_track = Some(path);
                            self.up_next.clear();
                            self.pending_source = None;
                        }
                        Err(e) => tracing::error!("Scheduler play failed: {}", e),
                    }
                } else {
                    tracing::warn!("Scheduler target not found on disk: {}", event.target);
                    self.now_title = format!("Scheduled: {} (file missing)", event.target);
                }
            }
            other => {
                tracing::info!("Scheduler command '{}' (no-op in MVP)", other);
            }
        }
    }

    fn fire_ad_block(&mut self, idx: usize) {
        let block = match self.ad_blocks.get(idx).cloned() {
            Some(b) => b,
            None => return,
        };
        if !PathBuf::from(&block.spot_path).is_file() {
            tracing::warn!(
                "Ad block '{}' skipped, spot missing: {}",
                block.name,
                block.spot_path
            );
            self.now_title = format!("Ad '{}': spot missing", block.name);
            return;
        }
        tracing::info!("Ad break firing: {}", block.name);
        let mut first = true;
        for clip in block.chain() {
            let r = if first {
                self.player.play(&PathBuf::from(&clip))
            } else {
                self.player.queue(&PathBuf::from(&clip))
            };
            if let Err(e) = r {
                tracing::error!("Ad clip failed ({}): {}", clip, e);
                return;
            }
            first = false;
        }
        if let Some(t) = self.library.find_by_path(&block.spot_path).ok().flatten() {
            let _ = self.library.record_play(&t.id, t.duration_secs);
        }
        self.auto_continue = true;
        self.is_playing = true;
        self.now_title = format!("AD: {}", block.name);
        self.now_artist = "Ad break".into();
    }

    // -- Loudness scan -------------------------------------------------------
    fn start_loudness_scan(&mut self, announce_empty: bool) {
        if self.scanning {
            return;
        }
        let jobs: Vec<(String, String, String)> = self
            .library
            .tracks_missing_loudness(usize::MAX)
            .unwrap_or_default()
            .into_iter()
            .map(|t| (t.id, t.file_path, t.file_name))
            .collect();
        if jobs.is_empty() {
            if announce_empty {
                self.lib_status = "All tracks analyzed".into();
            }
            return;
        }
        let total = jobs.len();
        let target_lufs = self
            .settings
            .loudness_target_lufs
            .clamp(TARGET_MIN_LUFS, TARGET_MAX_LUFS);
        self.scanning = true;
        self.scan_done = 0;
        self.scan_total = total;
        let (tx, rx) = mpsc::channel();
        self.scan_rx = Some(rx);
        self.lib_status = format!("Loudness scan starting... ({total} to go)");
        tracing::info!("Loudness scan started ({total} tracks, background thread)");
        if std::thread::Builder::new()
            .name("loudness-scan".into())
            .spawn(move || {
                for (id, path, file_name) in jobs {
                    let p = PathBuf::from(&path);
                    let (lufs, gain_db) = if !p.is_file() {
                        (-70.0, 0.0)
                    } else {
                        match crabcore::audio::analyze_file(&p) {
                            Ok(a) => (
                                a.integrated_lufs,
                                (target_lufs - a.integrated_lufs).clamp(-MAX_GAIN_DB, MAX_GAIN_DB),
                            ),
                            Err(e) => {
                                tracing::warn!("Loudness failed for {file_name}: {e}");
                                (-70.0, 0.0)
                            }
                        }
                    };
                    if tx
                        .send(LoudnessDone {
                            id,
                            file_name,
                            lufs,
                            gain_db,
                        })
                        .is_err()
                    {
                        break;
                    }
                }
            })
            .is_err()
        {
            tracing::error!("Loudness scan: failed to spawn worker thread");
            self.scanning = false;
            self.scan_rx = None;
            self.lib_status = "Loudness scan failed to start".into();
        }
    }

    fn pump_loudness(&mut self) {
        if !self.scanning {
            return;
        }
        let (mut batch, mut worker_gone) = (Vec::new(), false);
        if let Some(rx) = self.scan_rx.as_mut() {
            loop {
                match rx.try_recv() {
                    Ok(m) => batch.push(m),
                    Err(TryRecvError::Empty) => break,
                    Err(TryRecvError::Disconnected) => {
                        worker_gone = true;
                        break;
                    }
                }
            }
        } else {
            return;
        }
        if batch.is_empty() && !worker_gone {
            return;
        }
        for m in &batch {
            let _ = self.library.set_loudness(&m.id, m.lufs, m.gain_db);
            tracing::info!(
                "Loudness {}: {:.1} LUFS -> {:+.1} dB",
                m.file_name,
                m.lufs,
                m.gain_db
            );
        }
        self.scan_done += batch.len();
        if self.scan_done >= self.scan_total || worker_gone {
            self.scanning = false;
            self.scan_rx = None;
            self.refresh_library();
            self.lib_status = format!("Loudness scan complete ({} analyzed)", self.scan_done);
            tracing::info!(
                "Loudness scan complete: {}/{} analyzed",
                self.scan_done,
                self.scan_total
            );
        } else {
            self.lib_status = format!(
                "Analyzing loudness... {}/{}",
                self.scan_done, self.scan_total
            );
        }
    }

    // -- Import pump (one file per tick so the UI never freezes) -------------
    fn pump_import(&mut self) {
        if !self.import_active {
            return;
        }
        let Some(f) = self.import_pending.pop_front() else {
            self.import_active = false;
            self.refresh_library();
            self.lib_status = if self.import_skipped > 0 {
                format!(
                    "Imported {}, skipped {}",
                    self.import_added, self.import_skipped
                )
            } else {
                format!("Imported {}", self.import_added)
            };
            tracing::info!(
                "Import complete: {} added, {} skipped",
                self.import_added,
                self.import_skipped
            );
            if self.import_added > 0 {
                self.start_loudness_scan(false);
            }
            return;
        };
        let done = self.import_total - self.import_pending.len();
        match self.library.add_track(&f) {
            Ok(t) => {
                tracing::info!("Imported {} as {:?}", f.display(), t.kind);
                self.import_added += 1;
            }
            Err(e) => {
                tracing::warn!("Skipping {}: {}", f.display(), e);
                self.import_skipped += 1;
            }
        }
        self.lib_status = format!("Importing {done}/{}...", self.import_total);
    }

    // -- Periodic tick (progress, Auto-DJ, scheduler, silence) ---------------
    fn on_tick(&mut self) {
        self.tick_count += 1;
        self.pump_loudness();
        self.pump_import();

        let pos = self.player.position_secs();
        let (dur, has_dur) = match self.player.current_track() {
            Some(t) => (
                t.duration_secs.unwrap_or(0.0),
                t.duration_secs.unwrap_or(0.0) > 0.0,
            ),
            None => (0.0, false),
        };
        let playing = self.player.state() == PlayerState::Playing;
        let finished = self.player.is_finished();
        self.is_playing =
            playing || self.player.state() == PlayerState::Buffering && self.auto_continue;

        // Stream now-playing metadata (cheap, lock-free).
        if let Some(t) = self.player.current_track() {
            let label = t.title.clone().unwrap_or_else(|| {
                t.path
                    .file_name()
                    .map(|f| strip_audio_extension(&f.to_string_lossy()))
                    .unwrap_or_default()
            });
            self.player.set_stream_title(&label);
        }
        let _ = (pos, dur, has_dur);

        // Adopt engine-side track changes the UI didn't make (promoted
        // queued decks): proper play logging + labels for tracks that
        // started without a direct play path. Direct play paths record
        // engine_track synchronously, so only promotions land here.
        // Gated on Playing with nothing decoding to avoid false hits
        // mid-handoff (old deck still sounding, new path already known).
        let engine_path = self.player.current_track().map(|t| t.path);
        match (&engine_path, &self.engine_track) {
            (Some(p), Some(known)) if p == known => {}
            (None, None) => {}
            (None, Some(_)) => {
                self.engine_track = None;
            }
            _ => {
                let settled =
                    self.player.state() == PlayerState::Playing && self.player.load_inflight() == 0;
                if settled {
                    if let Some(p) = &engine_path {
                        // The pending source (recorded at queue time) follows
                        // the deck onto the air, so the `· via X` tag stays
                        // truthful; consumed here either way.
                        let source = self
                            .pending_source
                            .take()
                            .unwrap_or_else(|| "Auto-DJ".into());
                        if let Ok(Some(t)) = self.library.find_by_path(&p.to_string_lossy()) {
                            tracing::info!("Promoted queued deck: {}", t.file_path);
                            let _ = self.library.record_play(&t.id, t.duration_secs);
                            self.is_playing = true;
                            self.now_title = track_label(&t);
                            self.now_artist = t
                                .artist
                                .clone()
                                .filter(|a| !a.trim().is_empty())
                                .unwrap_or(source);
                            self.up_next.clear();
                        }
                    }
                    self.engine_track = engine_path;
                }
            }
        }

        // Refresh the coming-up forecast when the live track changed (or
        // never built). Forecast only makes sense with Auto-DJ on; manual
        // mode has no predictable order, so the list stays empty there.
        if self.up_next_for != self.engine_track {
            self.up_next_for = self.engine_track.clone();
            self.up_next_list = if self.autodj {
                let cfg = self.autodj_cfg();
                crabcore::playlist::forecast_up_next(&self.library, &cfg, &self.autodj_history, 5)
            } else {
                Vec::new()
            };
        }

        // Scheduler + ads auto-fire (dedupe per event/minute).
        if self.sched_enabled {
            let now = chrono::Local::now();
            let hhmm = now.format("%H:%M").to_string();
            let weekday = now.format("%a").to_string();
            let minute_key = now.format("%Y-%m-%d %H:%M").to_string();
            let today = now.format("%Y-%m-%d").to_string();
            let due: Vec<(String, usize)> = self
                .scheduler
                .due_events(&today, &hhmm, &weekday)
                .unwrap_or_default()
                .into_iter()
                .filter_map(|e| {
                    let already = self
                        .fired
                        .get(&e.id)
                        .map(|m| m == &minute_key)
                        .unwrap_or(false);
                    if already {
                        return None;
                    }
                    self.sched_events
                        .iter()
                        .position(|s| s.id == e.id)
                        .map(|idx| (e.id.clone(), idx))
                })
                .collect();
            for (id, idx) in due {
                self.fired.insert(id, minute_key.clone());
                self.fire_scheduled_event(idx);
            }
            if let Ok(date) = chrono::NaiveDate::parse_from_str(&today, "%Y-%m-%d") {
                let due_ads: Vec<(String, usize)> = self
                    .ads
                    .due_blocks(date, &hhmm, &weekday)
                    .unwrap_or_default()
                    .into_iter()
                    .filter_map(|b| {
                        let already = self
                            .fired_ads
                            .get(&b.id)
                            .map(|m| m == &minute_key)
                            .unwrap_or(false);
                        if already {
                            return None;
                        }
                        self.ad_blocks
                            .iter()
                            .position(|a| a.id == b.id)
                            .map(|idx| (b.id.clone(), idx))
                    })
                    .collect();
                for (id, idx) in due_ads {
                    self.fired_ads.insert(id, minute_key.clone());
                    self.fire_ad_block(idx);
                }
            }
        }

        // Silence monitor: recover dead air with a filler track (max 1/min).
        if self.player.silence_alarm() {
            let minute = chrono::Local::now().format("%Y-%m-%d %H:%M").to_string();
            if self.last_recovery.as_ref() != Some(&minute) {
                self.last_recovery = Some(minute);
                let current = self.player.current_track().map(|t| t.path);
                let filler = self
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
                            "SILENCE DETECTED - auto-recovering with filler: {}",
                            t.file_path
                        );
                        let path = PathBuf::from(&t.file_path);
                        let label = track_label(&t);
                        match self.player.play(&path) {
                            Ok(()) => {
                                let _ = self.library.record_play(&t.id, t.duration_secs);
                                self.auto_continue = true;
                                self.is_playing = true;
                                self.now_title = format!("Recovered: {}", label);
                                self.now_artist = "Silence detector".into();
                                self.engine_track = Some(path);
                                self.up_next.clear();
                                self.pending_source = None;
                            }
                            Err(e) => tracing::error!("Filler play failed: {}", e),
                        }
                    }
                    None => {
                        tracing::error!("SILENCE DETECTED - no playable filler in library");
                        self.now_title = "SILENCE - no filler available".into();
                    }
                }
            }
        }

        // Auto-DJ continuity + prefetch.
        if !self.autodj || !self.auto_continue {
            self.was_playing = playing;
            return;
        }
        let eof_transition = finished && (playing || self.was_playing);
        self.was_playing = playing;
        if eof_transition {
            self.autodj_play_now();
            return;
        }
        if !playing {
            return;
        }
        // Installed decks plus decode jobs still in flight: `queue()` returns
        // the moment the job is submitted, so without the in-flight count the
        // 200 ms tick would re-queue the same pick every tick until the first
        // decode lands, stacking duplicate decks behind the live one.
        let pending = self.player.pending_count() + self.player.load_inflight();
        let has_queue = self.player.has_queue();
        let dur_opt = if has_dur { Some(dur) } else { None };
        if !crabcore::audio::needs_prefetch(pos, dur_opt, pending, has_queue, 8.0) {
            return;
        }
        let pick = self.autodj_pick();
        if let Some(pick) = pick {
            let path = PathBuf::from(&pick.file_path);
            if !path.is_file() {
                return;
            }
            match self.player.queue(&path) {
                Ok(()) => {
                    let label = track_label(&pick);
                    tracing::info!("Auto-DJ queued: {}", label);
                    self.up_next = label;
                    // Count it now: it will sound, and the next pick must
                    // already separate from it (a supersede discarding it
                    // just leaves a harmless ghost in soft windows).
                    let cfg = self.autodj_cfg();
                    self.autodj_history.push_track(&pick, &cfg);
                    self.pending_source = Some("Auto-DJ".into());
                }
                Err(e) => tracing::warn!("Auto-DJ queue failed: {}", e),
            }
        }
    }
}

// ---------------------------------------------------------------------------
// Boot (startup sequence: settings, engine, stores, seeds)
// ---------------------------------------------------------------------------

fn boot() -> (App, Task<Message>) {
    use tracing_subscriber::EnvFilter;
    let _ = tracing_subscriber::fmt()
        .with_env_filter(
            EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info")),
        )
        .try_init();

    tracing::info!("CrabBoss starting up...");

    let settings_path = std::env::current_dir()
        .unwrap_or_default()
        .join("settings.json");
    let mut settings = crabcore::settings::AppSettings::load(&settings_path);

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

    let db_path = std::env::current_dir()
        .unwrap_or_default()
        .join("crabboss.db");
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
        Err(e) => tracing::warn!("Gain retarget failed: {e}"),
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
        let _ = scheduler.create(
            "Morning show",
            "load",
            "Morning.m3u",
            "08:00",
            "Daily",
            None,
        );
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

    let license_path = std::env::current_dir()
        .unwrap_or_default()
        .join("license.json");
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
        license,
        screen: Screen::Home,
        settings_section: SettingsSection::default(),
        station_name,
        audio_engine: engine_name,
        is_playing: false,
        now_title: "No track loaded".into(),
        now_artist: String::new(),
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
        license_status: String::new(),
        license_error: String::new(),
        license_key: String::new(),
        track_count,
        playlist_count,
        upcoming_count: 0,
        last_recovery: None,
        tick_count: 0,
    };
    app.license_status = app.license.status().label().to_string();
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

// ---------------------------------------------------------------------------
// Update
// ---------------------------------------------------------------------------

fn update(state: &mut App, message: Message) -> Task<Message> {
    match message {
        Message::Navigate(s) => {
            state.screen = s;
        }
        Message::SettingsNav(s) => {
            state.settings_section = s;
        }
        Message::Tick => {
            state.on_tick();
        }
        // -- Transport ------------------------------------------------------
        Message::Play => {
            match state.player.state() {
                PlayerState::Paused => {
                    state.player.resume();
                    state.is_playing = true;
                }
                PlayerState::Playing | PlayerState::Buffering => {
                    state.is_playing = true;
                }
                PlayerState::Stopped => {
                    // Cold start: a bare Play with nothing loaded was a
                    // silent no-op (the tick overwrote is_playing right
                    // back). Play the selected track, else an Auto-DJ pick;
                    // continuity follows the toggle, not the button.
                    if let Some(track) = state
                        .lib_selected
                        .and_then(|i| state.lib_tracks.get(i))
                        .cloned()
                    {
                        let path = PathBuf::from(&track.file_path);
                        match state.player.play(&path) {
                            Ok(()) => {
                                let _ = state.library.record_play(&track.id, track.duration_secs);
                                state.auto_continue = state.autodj;
                                state.is_playing = true;
                                state.now_title = track_label(&track);
                                state.now_artist = track.artist.clone().unwrap_or_default();
                                state.engine_track = Some(path);
                            }
                            Err(e) => {
                                tracing::error!("Failed to play: {}", e);
                                state.lib_status = format!("Play failed: {}", e);
                            }
                        }
                    } else {
                        state.autodj_play_now();
                        state.auto_continue = state.autodj;
                    }
                }
            }
        }
        Message::Pause => {
            state.player.pause();
            state.is_playing = false;
        }
        Message::Stop => {
            state.player.stop();
            state.auto_continue = false;
            state.is_playing = false;
            state.now_title = "No track loaded".into();
            state.now_artist.clear();
            state.up_next.clear();
            state.engine_track = None;
            state.pending_source = None;
        }
        Message::Next => {
            state.auto_continue = true;
            state.autodj_play_now();
        }
        Message::Prev => {
            if let Some(cur) = state.player.current_track().map(|t| t.path) {
                match state.player.play(&cur) {
                    Ok(()) => {
                        state.auto_continue = true;
                        state.is_playing = true;
                        state.engine_track = Some(cur);
                        state.up_next.clear();
                        state.pending_source = None;
                    }
                    Err(e) => tracing::error!("Prev failed: {}", e),
                }
            }
        }
        Message::VolumeChanged(v) => {
            state.volume = v.clamp(0.0, 1.0);
            state.player.set_volume(state.volume);
        }
        Message::AutodjToggled(en) => {
            state.autodj = en;
            state.settings.autodj = en;
            state.save_settings();
            tracing::info!("Auto-DJ {}", if en { "ON" } else { "OFF" });
            if !en {
                state.up_next.clear();
            } else if state.player.state() == PlayerState::Stopped {
                // Kick off playback: without a first track there is never
                // an EOF transition for continuity to continue from.
                state.autodj_play_now();
                state.auto_continue = true;
            }
        }
        // -- Library ---------------------------------------------------------
        Message::LibrarySearchChanged(q) => {
            state.lib_search = q;
            state.lib_selected = None;
            state.refresh_library();
        }
        Message::LibraryTrackSelected(i) => {
            state.lib_selected = Some(i);
        }
        Message::LibraryTrackPlay(i) => {
            let track = state.lib_tracks.get(i).cloned();
            if let Some(track) = track {
                let path = PathBuf::from(&track.file_path);
                tracing::info!("Playing track: {:?}", path);
                match state.player.play(&path) {
                    Ok(()) => {
                        let _ = state.library.record_play(&track.id, track.duration_secs);
                        state.auto_continue = true;
                        state.lib_selected = Some(i);
                        state.is_playing = true;
                        state.now_title = track_label(&track);
                        state.now_artist = track.artist.clone().unwrap_or_default();
                        state.engine_track = Some(path);
                        // A manual play discards any prefetched deck, so its
                        // "Up next" label dies with it.
                        state.up_next.clear();
                        state.pending_source = None;
                    }
                    Err(e) => {
                        tracing::error!("Failed to play: {}", e);
                        state.lib_status = format!("Play failed: {}", e);
                    }
                }
            }
        }
        Message::ImportFiles => {
            if state.import_active {
                state.lib_status = "Import already running...".into();
                return Task::none();
            }
            let files = rfd::FileDialog::new()
                .set_title("Import audio files")
                .add_filter(
                    "Audio",
                    &[
                        "mp3", "flac", "wav", "ogg", "oga", "aac", "m4a", "opus", "aiff", "wv",
                    ],
                )
                .pick_files();
            let Some(files) = files else {
                return Task::none();
            };
            if files.is_empty() {
                return Task::none();
            }
            tracing::info!("Importing {} files...", files.len());
            state.import_active = true;
            state.import_total = files.len();
            state.import_added = 0;
            state.import_skipped = 0;
            state.import_pending = VecDeque::from(files);
            state.lib_status = format!("Importing 0/{}...", state.import_total);
        }
        Message::HealthCheck => {
            tracing::info!("Manual library health scan");
            let fixed = state.library.reclassify_all().unwrap_or(0);
            let missing = state.library.missing_files().unwrap_or_default();
            let pending = state.library.count_missing_loudness().unwrap_or(0);
            for t in &missing {
                tracing::warn!("Missing file: {}", t.file_path);
            }
            let mut parts = if missing.is_empty() {
                vec!["All files OK".to_string()]
            } else {
                vec![format!("{} files missing (see log)", missing.len())]
            };
            if fixed > 0 {
                parts.push(format!("re-labeled {fixed}"));
            }
            if pending > 0 {
                parts.push(format!("{} to analyze", pending));
            }
            state.lib_status = parts.join(" - ");
            state.refresh_library();
        }
        Message::LoudnessScan => {
            let target = state.settings.loudness_target_lufs;
            state.start_loudness_scan(true);
            let _ = target;
        }
        // -- Scheduler -------------------------------------------------------
        Message::SchedulerMasterToggled(en) => {
            state.sched_enabled = en;
            tracing::info!("Scheduler master {}", if en { "ON" } else { "OFF" });
        }
        Message::SchedulerToggleEvent(i) => {
            let ids: Vec<(String, bool)> = state
                .sched_events
                .iter()
                .map(|e| (e.id.clone(), e.enabled))
                .collect();
            if let Some((id, enabled)) = ids.get(i) {
                if let Err(e) = state.scheduler.set_enabled(id, !enabled) {
                    tracing::error!("Scheduler toggle failed: {}", e);
                }
            }
            state.refresh_scheduler();
        }
        Message::SchedulerDeleteEvent(i) => {
            if let Some(e) = state.sched_events.get(i) {
                let id = e.id.clone();
                if let Err(e) = state.scheduler.delete(&id) {
                    tracing::error!("Scheduler delete failed: {}", e);
                }
            }
            state.refresh_scheduler();
        }
        Message::SchedulerRunEvent(i) => {
            state.fire_scheduled_event(i);
        }
        Message::SchedulerNew => {
            state.sched_edit_idx = None;
            state.se_name.clear();
            state.se_time = "09:00".into();
            state.se_action = 0;
            state.se_target.clear();
            state.se_expires.clear();
            state.se_days = [true; 7];
            state.sched_error.clear();
            state.sched_editor_open = true;
        }
        Message::SchedulerEdit(i) => {
            if let Some(e) = state.sched_events.get(i).cloned() {
                state.sched_edit_idx = Some(i);
                state.se_name = e.name;
                state.se_time = e.start_time;
                state.se_action = match e.action_type.as_str() {
                    "play" => 0,
                    "load" => 1,
                    "generate" => 2,
                    "queue" => 4,
                    _ => 3,
                };
                state.se_target = e.target;
                state.se_expires = e.expires_on.unwrap_or_default();
                let mask = crabcore::scheduler::mask_from_days(&e.days);
                for b in 0..7 {
                    state.se_days[b] = mask & (1 << b) != 0;
                }
                if mask == 127 {
                    state.se_days = [true; 7];
                }
                state.sched_error.clear();
                state.sched_editor_open = true;
            }
        }
        Message::SchedulerEditorClose => {
            state.sched_editor_open = false;
            state.sched_error.clear();
        }
        Message::SchedName(v) => state.se_name = v,
        Message::SchedTime(v) => state.se_time = v,
        Message::SchedTarget(v) => state.se_target = v,
        Message::SchedExpires(v) => state.se_expires = v,
        Message::SchedActionPrev => {
            state.se_action = state.se_action.saturating_sub(1);
        }
        Message::SchedActionNext => {
            state.se_action = (state.se_action + 1).min(4);
        }
        Message::SchedDayChanged(i, v) => {
            if i < 7 {
                state.se_days[i] = v;
            }
        }
        Message::SchedulerSave => {
            use crabcore::scheduler::days_from_mask;
            let mut mask = 0u8;
            for (i, on) in state.se_days.iter().enumerate() {
                if *on {
                    mask |= 1 << i;
                }
            }
            let days = days_from_mask(mask);
            let action = action_name(state.se_action);
            let expires = state.se_expires.trim().to_string();
            let res = match state.sched_edit_idx {
                None => state
                    .scheduler
                    .create(
                        state.se_name.trim(),
                        action,
                        state.se_target.trim(),
                        state.se_time.trim(),
                        &days,
                        Some(expires.trim()),
                    )
                    .map(|_| ()),
                Some(idx) => {
                    let id = state.sched_events.get(idx).map(|e| e.id.clone());
                    match id {
                        Some(id) => state.scheduler.update(
                            &id,
                            state.se_name.trim(),
                            action,
                            state.se_target.trim(),
                            state.se_time.trim(),
                            &days,
                            Some(expires.trim()),
                        ),
                        None => Err(crabcore::CrabError::Scheduler("event gone".into())),
                    }
                }
            };
            match res {
                Ok(()) => {
                    tracing::info!("Scheduler saved: {}", state.se_name);
                    state.sched_error.clear();
                    state.sched_editor_open = false;
                    state.refresh_scheduler();
                }
                Err(e) => {
                    tracing::warn!("Scheduler save failed: {}", e);
                    state.sched_error = format!("{}", e);
                }
            }
        }
        // -- Carts ------------------------------------------------------------
        Message::CartPlay(i) => {
            state.play_cart(i);
        }
        Message::CartHotkey(i) => {
            // Scoped like the old FocusScope hotkeys: only on the Carts
            // screen, and never while an editor dialog is open (typing
            // "1".."8" into a field must not fire pads).
            if state.screen != Screen::Carts || state.sched_editor_open || state.ads_editor_open {
                return Task::none();
            }
            state.play_cart(i);
        }
        Message::CartDelete(i) => {
            if let Some(c) = state.cart_list.get(i) {
                let id = c.id.clone();
                if let Err(e) = state.carts.delete(&id) {
                    tracing::error!("Cart delete failed: {}", e);
                }
            }
            state.refresh_carts();
        }
        Message::CartAdd => {
            let existing: Vec<String> = state
                .cart_list
                .iter()
                .map(|c| c.file_path.clone())
                .collect();
            if existing.len() >= 8 {
                state.cart_status = "Cart wall is full (8)".into();
                return Task::none();
            }
            let tracks = state.library.get_all_tracks().unwrap_or_default();
            let next = tracks
                .iter()
                .find(|t| t.kind == TrackKind::Jingle && !existing.contains(&t.file_path))
                .or_else(|| tracks.iter().find(|t| !existing.contains(&t.file_path)));
            match next {
                Some(t) => {
                    let label = track_label(t);
                    if let Err(e) = state.carts.create(&label, &t.file_path) {
                        tracing::error!("Cart add failed: {}", e);
                    }
                    state.cart_status = format!("Loaded '{}'", label);
                    state.refresh_carts();
                }
                None => {
                    state.cart_status = "Import tracks first".into();
                }
            }
        }
        Message::CartPlace(slot) => {
            let track = state
                .lib_selected
                .and_then(|i| state.lib_tracks.get(i).cloned());
            let Some(track) = track else {
                state.cart_status = "No library track armed - tap one first".into();
                return Task::none();
            };
            let label = track_label(&track);
            if let Err(e) = state.carts.assign_at(slot as i32, &label, &track.file_path) {
                tracing::error!("Cart place failed: {}", e);
                return Task::none();
            }
            state.cart_assign = false;
            state.cart_status = format!("'{}' -> pad {}", label, slot + 1);
            state.refresh_carts();
        }
        Message::CartToggleAssign => {
            state.cart_assign = !state.cart_assign;
            state.cart_status = if state.cart_assign {
                "Assign: select a track in Media, then tap a pad".into()
            } else {
                String::new()
            };
        }
        // -- Reports -----------------------------------------------------------
        Message::ReportRangeChanged(i) => {
            state.report_range = i.min(3);
            state.refresh_report();
        }
        Message::ReportExport => {
            let path = rfd::FileDialog::new()
                .set_title("Export play report (CSV)")
                .set_file_name("crabboss-report.csv")
                .add_filter("CSV", &["csv"])
                .save_file();
            let Some(path) = path else {
                return Task::none();
            };
            let (from, _) = report_range_bounds(state.report_range);
            let entries = crabcore::report::play_report(
                &state.library,
                from,
                chrono::Utc::now(),
                &[TrackKind::Jingle, TrackKind::Ad],
            )
            .unwrap_or_default();
            match std::fs::write(&path, crabcore::report::to_csv(&entries)) {
                Ok(()) => {
                    tracing::info!(
                        "Report exported: {} ({} rows)",
                        path.display(),
                        entries.len()
                    );
                    state.report_summary =
                        format!("Exported {} rows to {}", entries.len(), path.display());
                }
                Err(e) => tracing::error!("Report export failed: {}", e),
            }
        }
        // -- Ads ----------------------------------------------------------------
        Message::AdsToggle(i) => {
            let ids: Vec<(String, bool)> = state
                .ad_blocks
                .iter()
                .map(|b| (b.id.clone(), b.enabled))
                .collect();
            if let Some((id, enabled)) = ids.get(i) {
                if let Err(e) = state.ads.set_enabled(id, !enabled) {
                    tracing::error!("Ad toggle failed: {}", e);
                }
            }
            state.refresh_ads();
        }
        Message::AdsDelete(i) => {
            if let Some(b) = state.ad_blocks.get(i) {
                let id = b.id.clone();
                if let Err(e) = state.ads.delete(&id) {
                    tracing::error!("Ad delete failed: {}", e);
                }
            }
            state.refresh_ads();
        }
        Message::AdsRun(i) => {
            state.fire_ad_block(i);
        }
        Message::AdsNew => {
            state.ads_edit_idx = None;
            state.ab_name.clear();
            state.ab_spot.clear();
            state.ab_intro.clear();
            state.ab_outro.clear();
            state.ab_start = chrono::Local::now().format("%Y-%m-%d").to_string();
            state.ab_end = (chrono::Local::now() + chrono::Duration::days(30))
                .format("%Y-%m-%d")
                .to_string();
            state.ab_time = "09:00".into();
            state.ab_days = [true; 7];
            state.ads_error.clear();
            state.ads_editor_open = true;
        }
        Message::AdsEdit(i) => {
            if let Some(b) = state.ad_blocks.get(i).cloned() {
                state.ads_edit_idx = Some(i);
                state.ab_name = b.name;
                state.ab_spot = b.spot_path;
                state.ab_intro = b.intro_path.unwrap_or_default();
                state.ab_outro = b.outro_path.unwrap_or_default();
                state.ab_start = b.start_date.to_string();
                state.ab_end = b.end_date.to_string();
                state.ab_time = b.play_time;
                let mask = crabcore::scheduler::mask_from_days(&b.days);
                for d in 0..7 {
                    state.ab_days[d] = mask & (1 << d) != 0;
                }
                if mask == 127 {
                    state.ab_days = [true; 7];
                }
                state.ads_error.clear();
                state.ads_editor_open = true;
            }
        }
        Message::AdsEditorClose => {
            state.ads_editor_open = false;
            state.ads_error.clear();
        }
        Message::AdName(v) => state.ab_name = v,
        Message::AdSpot(v) => state.ab_spot = v,
        Message::AdIntro(v) => state.ab_intro = v,
        Message::AdOutro(v) => state.ab_outro = v,
        Message::AdStart(v) => state.ab_start = v,
        Message::AdEnd(v) => state.ab_end = v,
        Message::AdTime(v) => state.ab_time = v,
        Message::AdDayChanged(i, v) => {
            if i < 7 {
                state.ab_days[i] = v;
            }
        }
        Message::AdsSave => {
            use crabcore::scheduler::days_from_mask;
            let mut mask = 0u8;
            for (i, on) in state.ab_days.iter().enumerate() {
                if *on {
                    mask |= 1 << i;
                }
            }
            let days = days_from_mask(mask);
            let res = match state.ads_edit_idx {
                None => state
                    .ads
                    .create(
                        state.ab_name.trim(),
                        state.ab_spot.trim(),
                        state.ab_intro.trim(),
                        state.ab_outro.trim(),
                        state.ab_start.trim(),
                        state.ab_end.trim(),
                        state.ab_time.trim(),
                        &days,
                    )
                    .map(|_| ()),
                Some(idx) => {
                    let id = state.ad_blocks.get(idx).map(|b| b.id.clone());
                    match id {
                        Some(id) => state.ads.update(
                            &id,
                            state.ab_name.trim(),
                            state.ab_spot.trim(),
                            state.ab_intro.trim(),
                            state.ab_outro.trim(),
                            state.ab_start.trim(),
                            state.ab_end.trim(),
                            state.ab_time.trim(),
                            &days,
                        ),
                        None => Err(crabcore::CrabError::Scheduler("block gone".into())),
                    }
                }
            };
            match res {
                Ok(()) => {
                    tracing::info!("Ad block saved: {}", state.ab_name);
                    state.ads_error.clear();
                    state.ads_editor_open = false;
                    state.refresh_ads();
                }
                Err(e) => {
                    tracing::warn!("Ad block save failed: {}", e);
                    state.ads_error = format!("{}", e);
                }
            }
        }
        // -- Settings ------------------------------------------------------------
        Message::SettingsRefreshDevices => {
            state.output_devices = crabcore::audio::CpalEngine::list_output_devices();
        }
        Message::SettingsSelectDevice(name) => {
            state.settings.output_device = Some(name.clone());
            state.save_settings();
            tracing::info!("Output device set to '{}' (restart to apply)", name);
            state.sel_device = name;
            state.device_note = "Restart CrabBoss to apply the new device".into();
        }
        Message::XfadeInc => {
            state.settings.crossfade_secs = (state.settings.crossfade_secs + 0.5).clamp(0.0, 30.0);
            state
                .player
                .set_crossfade_secs(state.settings.crossfade_secs);
            state.save_settings();
        }
        Message::XfadeDec => {
            state.settings.crossfade_secs = (state.settings.crossfade_secs - 0.5).clamp(0.0, 30.0);
            state
                .player
                .set_crossfade_secs(state.settings.crossfade_secs);
            state.save_settings();
        }
        Message::SilenceInc => {
            state.settings.silence_threshold_secs =
                (state.settings.silence_threshold_secs + 1.0).clamp(1.0, 120.0);
            state
                .player
                .set_silence_threshold_secs(state.settings.silence_threshold_secs);
            state.save_settings();
        }
        Message::SilenceDec => {
            state.settings.silence_threshold_secs =
                (state.settings.silence_threshold_secs - 1.0).clamp(1.0, 120.0);
            state
                .player
                .set_silence_threshold_secs(state.settings.silence_threshold_secs);
            state.save_settings();
        }
        Message::EqToggle => {
            state.settings.eq_enabled = !state.settings.eq_enabled;
            state.player.set_eq_enabled(state.settings.eq_enabled);
            state.save_settings();
        }
        Message::EqInc(band) => {
            if band < EQ_BAND_COUNT {
                state.settings.eq_gains_db[band] =
                    (state.settings.eq_gains_db[band] + 1.0).clamp(-12.0, 12.0);
                state
                    .player
                    .set_eq_band(band, state.settings.eq_gains_db[band]);
                state.save_settings();
            }
        }
        Message::EqDec(band) => {
            if band < EQ_BAND_COUNT {
                state.settings.eq_gains_db[band] =
                    (state.settings.eq_gains_db[band] - 1.0).clamp(-12.0, 12.0);
                state
                    .player
                    .set_eq_band(band, state.settings.eq_gains_db[band]);
                state.save_settings();
            }
        }
        Message::EqReset => {
            state.settings.eq_gains_db = [0.0; EQ_BAND_COUNT];
            for (band, gain) in state.settings.eq_gains_db.iter().enumerate() {
                state.player.set_eq_band(band, *gain);
            }
            state.save_settings();
        }
        Message::LimiterInc => {
            state.settings.limiter_ceiling =
                (state.settings.limiter_ceiling * 1.122).clamp(0.1, 1.0);
            state
                .player
                .set_limiter_ceiling(state.settings.limiter_ceiling);
            state.save_settings();
        }
        Message::LimiterDec => {
            state.settings.limiter_ceiling =
                (state.settings.limiter_ceiling / 1.122).clamp(0.1, 1.0);
            state
                .player
                .set_limiter_ceiling(state.settings.limiter_ceiling);
            state.save_settings();
        }
        Message::LoudnessToggle => {
            state.settings.loudness_norm = !state.settings.loudness_norm;
            state
                .player
                .set_loudness_enabled(state.settings.loudness_norm);
            state.save_settings();
        }
        Message::LoudnessTargetInc => {
            state.settings.loudness_target_lufs =
                (state.settings.loudness_target_lufs + 1.0).clamp(TARGET_MIN_LUFS, TARGET_MAX_LUFS);
            let target = state.settings.loudness_target_lufs;
            state.save_settings();
            match state.library.retarget_gains(target) {
                Ok(n) => tracing::info!("Re-targeted {n} loudness gains to {target:.0} LUFS"),
                Err(e) => tracing::warn!("Gain retarget failed: {e}"),
            }
            state.refresh_library();
        }
        Message::LoudnessTargetDec => {
            state.settings.loudness_target_lufs =
                (state.settings.loudness_target_lufs - 1.0).clamp(TARGET_MIN_LUFS, TARGET_MAX_LUFS);
            let target = state.settings.loudness_target_lufs;
            state.save_settings();
            match state.library.retarget_gains(target) {
                Ok(n) => tracing::info!("Re-targeted {n} loudness gains to {target:.0} LUFS"),
                Err(e) => tracing::warn!("Gain retarget failed: {e}"),
            }
            state.refresh_library();
        }
        Message::StreamToggle => {
            state.settings.stream.enabled = !state.settings.stream.enabled;
            state.save_settings();
            state
                .player
                .set_stream_config(state.settings.stream.clone());
            if state.settings.stream.enabled {
                if let Err(e) = state.player.stream_start() {
                    tracing::warn!("Stream start failed: {e}");
                }
            } else {
                state.player.stream_stop();
            }
        }
        Message::StreamTlsToggle => {
            // Takes effect on the next start (TLS wraps the fresh
            // connection); restart the stream to apply it live.
            state.settings.stream.tls = !state.settings.stream.tls;
            state.save_settings();
            state
                .player
                .set_stream_config(state.settings.stream.clone());
        }
        Message::StreamHost(v) => {
            state.settings.stream.host = v;
            state.save_settings();
            state
                .player
                .set_stream_config(state.settings.stream.clone());
        }
        Message::StreamUsername(v) => {
            state.settings.stream.username = v;
            state.save_settings();
            state
                .player
                .set_stream_config(state.settings.stream.clone());
        }
        Message::StreamPort(v) => {
            if let Ok(p) = v.trim().parse::<u16>() {
                state.settings.stream.port = p;
                state.save_settings();
                state
                    .player
                    .set_stream_config(state.settings.stream.clone());
            }
        }
        Message::StreamMount(v) => {
            state.settings.stream.mount = v;
            state.save_settings();
            state
                .player
                .set_stream_config(state.settings.stream.clone());
        }
        Message::StreamPassword(v) => {
            // Empty edits are ignored so the saved password cannot be
            // wiped by clearing the field; there is no length cap.
            if !v.is_empty() {
                state.settings.stream.password = v;
                state.save_settings();
                state
                    .player
                    .set_stream_config(state.settings.stream.clone());
            }
        }
        Message::StreamBitrateInc => {
            state.settings.stream.bitrate_kbps =
                stream_bitrate_step(state.settings.stream.bitrate_kbps, true);
            state.save_settings();
            state
                .player
                .set_stream_config(state.settings.stream.clone());
        }
        Message::StreamBitrateDec => {
            state.settings.stream.bitrate_kbps =
                stream_bitrate_step(state.settings.stream.bitrate_kbps, false);
            state.save_settings();
            state
                .player
                .set_stream_config(state.settings.stream.clone());
        }
        Message::MicToggle => {
            state.settings.mic.enabled = !state.settings.mic.enabled;
            state.save_settings();
            state.player.set_mic_config(state.settings.mic.clone());
            if state.settings.mic.enabled {
                if let Err(e) = state.player.mic_start() {
                    tracing::warn!("Mic start failed: {e}");
                }
            } else {
                state.player.mic_stop();
            }
        }
        Message::MicRefreshDevices => {
            state.input_devices = crabcore::audio::CpalEngine::list_input_devices();
        }
        Message::MicSelectDevice(name) => {
            state.settings.mic.device = Some(name);
            state.save_settings();
            state.player.set_mic_config(state.settings.mic.clone());
            state.mic_note = "Input switched live - no restart needed".into();
        }
        Message::MicLevelInc => {
            state.settings.mic.level = (state.settings.mic.level + 0.05).clamp(0.0, 1.5);
            state.save_settings();
            state.player.set_mic_config(state.settings.mic.clone());
        }
        Message::MicLevelDec => {
            state.settings.mic.level = (state.settings.mic.level - 0.05).clamp(0.0, 1.5);
            state.save_settings();
            state.player.set_mic_config(state.settings.mic.clone());
        }
        Message::MicDuckToggle => {
            state.settings.mic.duck_enabled = !state.settings.mic.duck_enabled;
            state.save_settings();
            state.player.set_mic_config(state.settings.mic.clone());
        }
        Message::MicThresholdInc => {
            state.settings.mic.duck_threshold_db =
                (state.settings.mic.duck_threshold_db + 3.0).clamp(-60.0, 0.0);
            state.save_settings();
            state.player.set_mic_config(state.settings.mic.clone());
        }
        Message::MicThresholdDec => {
            state.settings.mic.duck_threshold_db =
                (state.settings.mic.duck_threshold_db - 3.0).clamp(-60.0, 0.0);
            state.save_settings();
            state.player.set_mic_config(state.settings.mic.clone());
        }
        Message::MicDepthInc => {
            state.settings.mic.duck_depth_db =
                (state.settings.mic.duck_depth_db + 3.0).clamp(0.0, 24.0);
            state.save_settings();
            state.player.set_mic_config(state.settings.mic.clone());
        }
        Message::MicDepthDec => {
            state.settings.mic.duck_depth_db =
                (state.settings.mic.duck_depth_db - 3.0).clamp(0.0, 24.0);
            state.save_settings();
            state.player.set_mic_config(state.settings.mic.clone());
        }
        Message::MicAttackInc => {
            state.settings.mic.attack_ms =
                duck_ms_step(&ATTACK_LADDER, state.settings.mic.attack_ms, true);
            state.save_settings();
            state.player.set_mic_config(state.settings.mic.clone());
        }
        Message::MicAttackDec => {
            state.settings.mic.attack_ms =
                duck_ms_step(&ATTACK_LADDER, state.settings.mic.attack_ms, false);
            state.save_settings();
            state.player.set_mic_config(state.settings.mic.clone());
        }
        Message::MicReleaseInc => {
            state.settings.mic.release_ms =
                duck_ms_step(&RELEASE_LADDER, state.settings.mic.release_ms, true);
            state.save_settings();
            state.player.set_mic_config(state.settings.mic.clone());
        }
        Message::MicReleaseDec => {
            state.settings.mic.release_ms =
                duck_ms_step(&RELEASE_LADDER, state.settings.mic.release_ms, false);
            state.save_settings();
            state.player.set_mic_config(state.settings.mic.clone());
        }
        Message::StationName(v) => {
            // Stored raw like the stream host field (trimmed + defaulted on
            // load); the dashboard header mirrors it live.
            state.settings.station_name = v.clone();
            state.station_name = v;
            state.save_settings();
        }
        // -- License ----------------------------------------------------------
        Message::LicenseKeyInput(v) => {
            state.license_key = v;
        }
        Message::ActivateLicense => {
            let key = state.license_key.clone();
            match state.license.activate(&key, "Station") {
                Ok(info) => {
                    tracing::info!("License activated: {}", info.key);
                    state.license_status = state.license.status().label().to_string();
                    state.license_error.clear();
                }
                Err(e) => {
                    tracing::warn!("Invalid license '{}': {}", key, e);
                    state.license_error = format!("Invalid key: {}", e);
                }
            }
        }
        Message::ClearLicense => {
            state.license.clear().ok();
            tracing::info!("License cleared");
            state.license_status = state.license.status().label().to_string();
            state.license_error.clear();
        }
    }
    Task::none()
}

// ---------------------------------------------------------------------------
// View
// ---------------------------------------------------------------------------

fn cart_hotkey(key: iced::keyboard::Key, _: iced::keyboard::Modifiers) -> Option<Message> {
    use iced::keyboard::Key;
    match key {
        Key::Character(c) => match c.as_str() {
            "1" => Some(Message::CartHotkey(0)),
            "2" => Some(Message::CartHotkey(1)),
            "3" => Some(Message::CartHotkey(2)),
            "4" => Some(Message::CartHotkey(3)),
            "5" => Some(Message::CartHotkey(4)),
            "6" => Some(Message::CartHotkey(5)),
            "7" => Some(Message::CartHotkey(6)),
            "8" => Some(Message::CartHotkey(7)),
            _ => None,
        },
        _ => None,
    }
}

fn subscription(_: &App) -> Subscription<Message> {
    Subscription::batch(vec![
        iced::time::every(Duration::from_millis(200)).map(|_| Message::Tick),
        iced::keyboard::listen().filter_map(|event| match event {
            iced::keyboard::Event::KeyPressed { key, modifiers, .. } => cart_hotkey(key, modifiers),
            _ => None,
        }),
    ])
}

fn view(state: &App) -> Element<'_, Message> {
    let body: Element<'_, Message> = match state.screen {
        Screen::Home => view_home(state),
        Screen::Playout => view_playout(state),
        Screen::Media => view_library_page(state),
        Screen::Scheduler => view_scheduler(state),
        Screen::Carts => view_carts(state),
        Screen::Reports => view_reports(state),
        Screen::Ads => view_ads(state),
        Screen::Settings => view_settings(state),
    };

    column![
        row![
            view_sidebar(state),
            iced::widget::rule::vertical(1),
            container(body).width(Length::Fill).height(Length::Fill),
        ]
        .height(Length::Fill),
        iced::widget::rule::horizontal(1),
        view_footer(state),
    ]
    .into()
}

/// Full-width status footer: the on-air line gets the whole window width
/// (long titles no longer wrap inside the narrow sidebar) with room to
/// grow stream/mic indicators later. v1 carries only the on-air status.
fn view_footer(state: &App) -> Element<'_, Message> {
    let status = if state.is_playing {
        on_air_label(&state.now_title, &state.now_artist)
    } else {
        "OFF AIR".to_string()
    };
    container(
        row![
            text(status).size(12),
            iced::widget::space::horizontal(),
            text(format!("Version {}", env!("CARGO_PKG_VERSION"))).size(11),
        ]
        .align_y(iced::Alignment::Center),
    )
    .padding([6, 12])
    .width(Length::Fill)
    .into()
}

/// Halloy-style left sidebar (v1: fixed position, no collapse, no badges):
/// station name plus a scrollable entry list (one per screen, active
/// highlighted). Global status lives in the full-width footer.
fn view_sidebar(state: &App) -> Element<'_, Message> {
    let mut list = column![].spacing(4);
    for s in [
        Screen::Home,
        Screen::Playout,
        Screen::Media,
        Screen::Scheduler,
        Screen::Carts,
        Screen::Reports,
        Screen::Ads,
        Screen::Settings,
    ] {
        // Active screen gets the theme's accent button; the rest stay
        // transparent text buttons — no bracket hacks needed.
        let entry = button(text(s.label()).size(13))
            .width(Length::Fill)
            .on_press(Message::Navigate(s));
        list = list.push(if s == state.screen {
            entry.style(iced::widget::button::primary)
        } else {
            entry.style(iced::widget::button::text)
        });
    }

    container(
        column![
            text(&state.station_name).size(15),
            scrollable(list).height(Length::Fill),
        ]
        .spacing(6)
        .padding(10),
    )
    .width(Length::Fixed(172.0))
    .height(Length::Fill)
    .into()
}

fn view_home(state: &App) -> Element<'_, Message> {
    let status = if state.is_playing {
        on_air_label(&state.now_title, &state.now_artist)
    } else {
        "Off air".to_string()
    };
    let stats = row![
        container(column![
            text(format!("{}", state.track_count)).size(22),
            text("Tracks").size(11),
        ])
        .padding(12)
        .width(Length::Fill),
        container(column![
            text(format!("{}", state.playlist_count)).size(22),
            text("Playlists").size(11),
        ])
        .padding(12)
        .width(Length::Fill),
        container(column![
            text(format!("{}", state.upcoming_count)).size(22),
            text("Scheduled").size(11),
        ])
        .padding(12)
        .width(Length::Fill),
    ]
    .spacing(12);
    let actions = row![
        button(text("Playout").size(14)).on_press(Message::Navigate(Screen::Playout)),
        button(text("Library").size(14)).on_press(Message::Navigate(Screen::Media)),
        button(text("Scheduler").size(14)).on_press(Message::Navigate(Screen::Scheduler)),
        button(text("Cart Wall").size(14)).on_press(Message::Navigate(Screen::Carts)),
    ]
    .spacing(12);
    scrollable(
        column![
            text(&state.station_name).size(20),
            text(status).size(12),
            stats,
            text("Quick Actions").size(14),
            actions,
            text(format!(
                "Engine: {} | Device: {}",
                state.audio_engine,
                state.player.device_name()
            ))
            .size(11),
            text(format!("License: {}", state.license_status)).size(11),
        ]
        .spacing(12)
        .padding(16),
    )
    .into()
}

fn player_progress(state: &App) -> (String, String, f32) {
    let pos = state.player.position_secs();
    let (dur, has_dur) = match state.player.current_track() {
        Some(t) => (
            t.duration_secs.unwrap_or(0.0),
            t.duration_secs.unwrap_or(0.0) > 0.0,
        ),
        None => (0.0, false),
    };
    let cur = fmt_dur(Some(pos));
    let tot = if has_dur {
        fmt_dur(Some(dur))
    } else {
        "00:00".into()
    };
    let frac = if has_dur {
        (pos / dur).clamp(0.0, 1.0) as f32
    } else {
        0.0
    };
    (cur, tot, frac)
}

fn view_player_panel(state: &App) -> Element<'_, Message> {
    let (cur, tot, frac) = player_progress(state);
    let play_label = if state.is_playing { "Pause" } else { "Play" };
    let play_msg = if state.is_playing {
        Message::Pause
    } else {
        Message::Play
    };
    // Horizontal broadcast strip: track line, then transport + progress +
    // time, then Auto-DJ + up-next + monitor volume.
    column![
        text(track_source_label(&state.now_title, &state.now_artist)).size(14),
        row![
            button(text("Prev").size(13)).on_press(Message::Prev),
            button(text(play_label).size(13)).on_press(play_msg),
            button(text("Stop").size(13)).on_press(Message::Stop),
            button(text("Next").size(13)).on_press(Message::Next),
            progress_bar(0.0..=1.0, frac).length(Length::Fill),
            text(format!("{} / {}", cur, tot)).size(12),
        ]
        .spacing(8)
        .align_y(iced::Alignment::Center),
        row![
            checkbox(state.autodj)
                .label("Auto-DJ")
                .on_toggle(Message::AutodjToggled),
            text(if state.up_next.is_empty() {
                String::new()
            } else {
                format!("Up next: {}", state.up_next)
            })
            .size(11)
            .width(Length::Fill),
            text(format!("Vol {:.0}%", state.volume * 100.0)).size(12),
            slider(0.0..=1.0, state.volume, Message::VolumeChanged)
                .step(0.01_f32)
                .width(Length::Fixed(180.0)),
        ]
        .spacing(8)
        .align_y(iced::Alignment::Center),
    ]
    .spacing(8)
    .padding(12)
    .into()
}

fn view_library_panel(state: &App, tools: bool) -> Element<'_, Message> {
    let shown = state.lib_tracks.len();
    let count_label = if shown == state.lib_total {
        format!(
            "{} track{}",
            state.lib_total,
            if state.lib_total == 1 { "" } else { "s" }
        )
    } else {
        format!("{shown} of {} tracks", state.lib_total)
    };
    // Management tools live on the Library screen only; the Playout desk
    // keeps a lean list (Library = ngurus, Playout = nge-live).
    let mut header = row![
        text(format!("Library - {}", count_label)).size(14),
        iced::widget::space::horizontal(),
    ]
    .spacing(6);
    if tools {
        header = header
            .push(button(text("Health").size(12)).on_press(Message::HealthCheck))
            .push(button(text("Loudness").size(12)).on_press(Message::LoudnessScan))
            .push(button(text("Import").size(12)).on_press(Message::ImportFiles));
    }

    let search = text_input("Search tracks...", &state.lib_search)
        .on_input(Message::LibrarySearchChanged)
        .padding(8);

    // Fixed column widths shared by the header and every row, so the list
    // reads as a table: only Title/Artist flex, everything else lines up.
    const PLAY_W: f32 = 64.0;
    const KIND_W: f32 = 70.0;
    const DUR_W: f32 = 68.0;
    const GAIN_W: f32 = 72.0;

    let mut list = column![
        row![
            text("").width(PLAY_W),
            text("Kind").width(KIND_W).size(11),
            text("Title").width(Length::Fill).size(11),
            text("Artist").width(Length::Fill).size(11),
            text("Duration")
                .width(DUR_W)
                .size(11)
                .align_x(iced::alignment::Horizontal::Right),
            text("Gain")
                .width(GAIN_W)
                .size(11)
                .align_x(iced::alignment::Horizontal::Right),
        ]
        .spacing(6),
        iced::widget::rule::horizontal(1),
    ]
    .spacing(4)
    // Keep text clear of the floating scrollbar, which would otherwise
    // cover the last pixels of the Gain column.
    .padding(iced::padding::right(14));
    if state.lib_tracks.is_empty() {
        list = list.push(text("Import audio files to get started").size(12));
    } else {
        for (i, t) in state.lib_tracks.iter().take(500).enumerate() {
            let missing = !PathBuf::from(&t.file_path).is_file();
            let base = track_label(t);
            let title = if missing { format!("! {}", base) } else { base };
            let selected = Some(i) == state.lib_selected;
            let artist = t.artist.clone().unwrap_or_default();
            let dur = fmt_dur(t.duration_secs);
            let gain = t
                .loudness_gain_db
                .map(|g| format!("{g:+.1} dB"))
                .unwrap_or_default();
            let cells: iced::widget::Row<'_, Message> = row![
                button(
                    text("Play")
                        .size(11)
                        .width(Length::Fill)
                        .align_x(iced::alignment::Horizontal::Center)
                )
                .width(PLAY_W)
                .on_press(Message::LibraryTrackPlay(i)),
                text(kind_label(t.kind)).size(12).width(KIND_W),
                button(text(title).size(12))
                    .style(iced::widget::button::text)
                    .padding(0)
                    .width(Length::Fill)
                    .on_press(Message::LibraryTrackSelected(i)),
                text(artist).size(12).width(Length::Fill),
                text(dur)
                    .size(12)
                    .width(DUR_W)
                    .align_x(iced::alignment::Horizontal::Right),
                text(gain)
                    .size(12)
                    .width(GAIN_W)
                    .align_x(iced::alignment::Horizontal::Right),
            ]
            .spacing(6)
            .align_y(iced::Alignment::Center);
            // Selected row: subtle theme-accent wash instead of a ">"
            // text prefix.
            let entry: Element<'_, Message> = if selected {
                container(cells)
                    .width(Length::Fill)
                    .style(|theme: &Theme| {
                        let mut bg = theme.palette().primary;
                        bg.a = 0.22;
                        iced::widget::container::Style::default().background(bg)
                    })
                    .into()
            } else {
                cells.into()
            };
            list = list.push(entry);
        }
        if shown > 500 {
            list = list.push(text(format!("... showing 500 of {shown} (refine search)")).size(11));
        }
    }

    column![
        header,
        search,
        text(&state.lib_status).size(11),
        scrollable(list).height(Length::Fill),
    ]
    .spacing(6)
    .padding(8)
    .into()
}

fn view_library_page(state: &App) -> Element<'_, Message> {
    view_library_panel(state, true)
}

fn view_playout(state: &App) -> Element<'_, Message> {
    // Forecast display (Auto-DJ only): what the rotation would play next,
    // recomputed whenever the live track changes. The already-queued deck
    // (if any) keeps its own "Up next" label in the strip above.
    let coming_up: Element<'_, Message> = if state.up_next_list.is_empty() {
        column![].into()
    } else {
        let mut col = column![text("Coming Up").size(14)].spacing(4);
        for t in &state.up_next_list {
            let artist = t.artist.clone().unwrap_or_default();
            col = col.push(text(join_title_artist(&track_label(t), &artist)).size(12));
        }
        // Left padding matching the library header below (outer 8 + here 8),
        // so the section doesn't sit left of its neighbors.
        col.padding(iced::padding::left(8)).into()
    };
    column![
        text("Playout").size(16),
        view_player_panel(state),
        coming_up,
        container(view_library_panel(state, false))
            .width(Length::Fill)
            .height(Length::Fill),
    ]
    .spacing(8)
    .padding(8)
    .into()
}

fn view_scheduler(state: &App) -> Element<'_, Message> {
    let mut list = column![].spacing(4);
    for (i, e) in state.sched_events.iter().enumerate() {
        let today = chrono::Local::now().format("%Y-%m-%d").to_string();
        let badge = match e.expiry_status(&today) {
            crabcore::scheduler::ExpiryStatus::Expired => "expired",
            crabcore::scheduler::ExpiryStatus::ExpiresToday => "last day",
            crabcore::scheduler::ExpiryStatus::Active(n) if n <= 7 => "expiring",
            _ => "",
        };
        list = list.push(
            column![
                text(format!(
                    "{} | {} | {} -> {} | {} {} {}",
                    e.name,
                    e.start_time,
                    e.action_type,
                    e.target,
                    e.days,
                    if e.enabled { "[on]" } else { "[off]" },
                    badge
                ))
                .size(12),
                row![
                    button(text(if e.enabled { "Disable" } else { "Enable" }).size(11))
                        .on_press(Message::SchedulerToggleEvent(i)),
                    button(text("Run").size(11)).on_press(Message::SchedulerRunEvent(i)),
                    button(text("Edit").size(11)).on_press(Message::SchedulerEdit(i)),
                    button(text("Del").size(11)).on_press(Message::SchedulerDeleteEvent(i)),
                ]
                .spacing(6),
            ]
            .spacing(2),
        );
    }
    let mut col = column![row![
        text("Scheduler").size(16),
        iced::widget::space::horizontal(),
        checkbox(state.sched_enabled)
            .label("Enabled")
            .on_toggle(Message::SchedulerMasterToggled),
        button(text("+ New").size(12)).on_press(Message::SchedulerNew),
    ]
    .spacing(8),]
    .spacing(8)
    .padding(12);
    if !state.sched_warnings.is_empty() {
        col = col.push(text(state.sched_warnings.join(" | ")).size(11));
    }
    col = col.push(scrollable(list).height(Length::Fill));
    if state.sched_editor_open {
        let day_names: [Element<'_, Message>; 7] = [
            checkbox(state.se_days[0])
                .label("Mon")
                .on_toggle(|b| Message::SchedDayChanged(0, b))
                .into(),
            checkbox(state.se_days[1])
                .label("Tue")
                .on_toggle(|b| Message::SchedDayChanged(1, b))
                .into(),
            checkbox(state.se_days[2])
                .label("Wed")
                .on_toggle(|b| Message::SchedDayChanged(2, b))
                .into(),
            checkbox(state.se_days[3])
                .label("Thu")
                .on_toggle(|b| Message::SchedDayChanged(3, b))
                .into(),
            checkbox(state.se_days[4])
                .label("Fri")
                .on_toggle(|b| Message::SchedDayChanged(4, b))
                .into(),
            checkbox(state.se_days[5])
                .label("Sat")
                .on_toggle(|b| Message::SchedDayChanged(5, b))
                .into(),
            checkbox(state.se_days[6])
                .label("Sun")
                .on_toggle(|b| Message::SchedDayChanged(6, b))
                .into(),
        ];
        let mut day_row = row![].spacing(8);
        for d in day_names {
            day_row = day_row.push(d);
        }
        col = col.push(
            column![
                text(if state.sched_edit_idx.is_none() {
                    "New event"
                } else {
                    "Edit event"
                })
                .size(14),
                text_input("Name", &state.se_name)
                    .on_input(Message::SchedName)
                    .padding(6),
                row![
                    text_input("HH:MM", &state.se_time)
                        .on_input(Message::SchedTime)
                        .padding(6),
                    button(text("<").size(12)).on_press(Message::SchedActionPrev),
                    text(format!("action: {}", action_label(state.se_action))).size(12),
                    button(text(">").size(12)).on_press(Message::SchedActionNext),
                ]
                .spacing(6),
                text_input("Target (file / playlist / preset)", &state.se_target)
                    .on_input(Message::SchedTarget)
                    .padding(6),
                text_input(
                    "Valid until YYYY-MM-DD (empty = forever)",
                    &state.se_expires
                )
                .on_input(Message::SchedExpires)
                .padding(6),
                day_row,
                text(&state.sched_error).size(11),
                row![
                    button(text("Save").size(12)).on_press(Message::SchedulerSave),
                    button(text("Cancel").size(12)).on_press(Message::SchedulerEditorClose),
                ]
                .spacing(8),
            ]
            .spacing(6)
            .padding(8),
        );
    }
    col.into()
}

fn view_carts(state: &App) -> Element<'_, Message> {
    let kinds: HashMap<&str, TrackKind> = state
        .lib_tracks
        .iter()
        .map(|t| (t.file_path.as_str(), t.kind))
        .collect();
    let live_path = state
        .player
        .current_track()
        .map(|t| t.path.to_string_lossy().to_string());
    let mut grid = column![].spacing(6);
    for (i, c) in state.cart_list.iter().enumerate() {
        let kind = kinds
            .get(c.file_path.as_str())
            .map(|k| kind_label(*k))
            .unwrap_or("Music");
        let exists = PathBuf::from(&c.file_path).is_file();
        let playing = live_path.as_deref() == Some(c.file_path.as_str()) && state.is_playing;
        let (pos, frac) = if playing {
            let p = state.player.position_secs();
            let d = state
                .player
                .current_track()
                .and_then(|t| t.duration_secs)
                .unwrap_or(1.0)
                .max(0.01);
            (p, (p / d).clamp(0.0, 1.0) as f32)
        } else {
            (0.0, 0.0)
        };
        let _ = pos;
        grid = grid.push(
            column![
                row![
                    text(format!(
                        "Pad {}: {} [{}] {}{}",
                        i + 1,
                        c.label,
                        kind,
                        short_name(&c.file_path),
                        if exists { "" } else { " (missing)" }
                    ))
                    .size(12)
                    .width(Length::Fill),
                    button(text("Play").size(11)).on_press(Message::CartPlay(i)),
                    button(text("Del").size(11)).on_press(Message::CartDelete(i)),
                ]
                .spacing(6),
                progress_bar(0.0..=1.0, frac),
                row![button(text("Place here").size(11)).on_press(Message::CartPlace(i)),]
                    .spacing(6),
            ]
            .spacing(2),
        );
    }
    column![
        row![
            text("Cart Wall").size(16),
            iced::widget::space::horizontal(),
            button(text(if state.cart_assign {
                "Assign: ON"
            } else {
                "Assign"
            }))
            .on_press(Message::CartToggleAssign),
            button(text("+ Add").size(12)).on_press(Message::CartAdd),
        ]
        .spacing(8),
        text(&state.cart_status).size(11),
        text("Tip: select a track in Media, enable Assign, then Place on a pad.").size(11),
        scrollable(grid).height(Length::Fill),
    ]
    .spacing(8)
    .padding(12)
    .into()
}

fn view_reports(state: &App) -> Element<'_, Message> {
    const RANGES: [&str; 4] = ["Today", "Last 7 days", "Last 30 days", "All time"];
    let mut range_row = row![text("Range:").size(12)].spacing(6);
    for (i, name) in RANGES.iter().enumerate() {
        let label = if i == state.report_range {
            format!("[{}]", name)
        } else {
            name.to_string()
        };
        range_row =
            range_row.push(button(text(label).size(12)).on_press(Message::ReportRangeChanged(i)));
    }
    range_row = range_row.push(iced::widget::space::horizontal());
    range_row = range_row.push(button(text("Export CSV").size(12)).on_press(Message::ReportExport));
    let mut list = column![].spacing(2);
    for e in &state.report_entries {
        list = list.push(
            text(format!(
                "{} | {} [{}]",
                e.played_at.format("%d/%m %H:%M"),
                join_title_artist(&e.title, &e.artist),
                e.kind.as_str()
            ))
            .size(12),
        );
    }
    column![
        text("Reports").size(16),
        range_row,
        text(&state.report_summary).size(11),
        scrollable(list).height(Length::Fill),
    ]
    .spacing(8)
    .padding(12)
    .into()
}

fn view_ads(state: &App) -> Element<'_, Message> {
    let mut list = column![].spacing(4);
    for (i, b) in state.ad_blocks.iter().enumerate() {
        list = list.push(
            column![
                text(format!(
                    "{} | {} | {} -> {} | {} {}",
                    b.name,
                    b.play_time,
                    b.start_date,
                    b.end_date,
                    short_name(&b.spot_path),
                    if b.enabled { "[on]" } else { "[off]" }
                ))
                .size(12),
                text(format!("days: {}", b.days)).size(11),
                row![
                    button(text(if b.enabled { "Disable" } else { "Enable" }).size(11))
                        .on_press(Message::AdsToggle(i)),
                    button(text("Run").size(11)).on_press(Message::AdsRun(i)),
                    button(text("Edit").size(11)).on_press(Message::AdsEdit(i)),
                    button(text("Del").size(11)).on_press(Message::AdsDelete(i)),
                ]
                .spacing(6),
            ]
            .spacing(2),
        );
    }
    let mut col = column![row![
        text("Ads").size(16),
        iced::widget::space::horizontal(),
        button(text("+ New block").size(12)).on_press(Message::AdsNew),
    ]
    .spacing(8),]
    .spacing(8)
    .padding(12);
    col = col.push(scrollable(list).height(Length::Shrink));
    if state.ads_editor_open {
        let mut day_row = row![].spacing(8);
        day_row = day_row.push(
            checkbox(state.ab_days[0])
                .label("Mon")
                .on_toggle(|b| Message::AdDayChanged(0, b)),
        );
        day_row = day_row.push(
            checkbox(state.ab_days[1])
                .label("Tue")
                .on_toggle(|b| Message::AdDayChanged(1, b)),
        );
        day_row = day_row.push(
            checkbox(state.ab_days[2])
                .label("Wed")
                .on_toggle(|b| Message::AdDayChanged(2, b)),
        );
        day_row = day_row.push(
            checkbox(state.ab_days[3])
                .label("Thu")
                .on_toggle(|b| Message::AdDayChanged(3, b)),
        );
        day_row = day_row.push(
            checkbox(state.ab_days[4])
                .label("Fri")
                .on_toggle(|b| Message::AdDayChanged(4, b)),
        );
        day_row = day_row.push(
            checkbox(state.ab_days[5])
                .label("Sat")
                .on_toggle(|b| Message::AdDayChanged(5, b)),
        );
        day_row = day_row.push(
            checkbox(state.ab_days[6])
                .label("Sun")
                .on_toggle(|b| Message::AdDayChanged(6, b)),
        );
        col = col.push(
            column![
                text("Ad block").size(14),
                text_input("Name", &state.ab_name)
                    .on_input(Message::AdName)
                    .padding(6),
                text_input("Spot path (audio file)", &state.ab_spot)
                    .on_input(Message::AdSpot)
                    .padding(6),
                text_input("Intro path (optional)", &state.ab_intro)
                    .on_input(Message::AdIntro)
                    .padding(6),
                text_input("Outro path (optional)", &state.ab_outro)
                    .on_input(Message::AdOutro)
                    .padding(6),
                row![
                    text_input("Start YYYY-MM-DD", &state.ab_start)
                        .on_input(Message::AdStart)
                        .padding(6),
                    text_input("End YYYY-MM-DD", &state.ab_end)
                        .on_input(Message::AdEnd)
                        .padding(6),
                    text_input("HH:MM", &state.ab_time)
                        .on_input(Message::AdTime)
                        .padding(6),
                ]
                .spacing(6),
                day_row,
                text(&state.ads_error).size(11),
                row![
                    button(text("Save").size(12)).on_press(Message::AdsSave),
                    button(text("Cancel").size(12)).on_press(Message::AdsEditorClose),
                ]
                .spacing(8),
            ]
            .spacing(6),
        );
    }
    col.into()
}

fn stepper(label: String, dec: Message, inc: Message) -> Element<'static, Message> {
    row![
        button(text("-").size(12)).on_press(dec),
        text(label).size(12),
        button(text("+").size(12)).on_press(inc),
    ]
    .spacing(8)
    .into()
}

fn view_settings(state: &App) -> Element<'_, Message> {
    let s = &state.settings;
    let stream_cfg = state.player.stream_config();
    let stream_state = state.player.stream_state();
    let stream_stats = state.player.stream_stats();
    let mic_cfg = state.player.mic_config();
    let mic_state = state.player.mic_state();

    let mut devices = column![text("Output devices:").size(12)].spacing(4);
    // Highlight what is actually sounding: the saved choice, or the live
    // device when running on the system default (sel_device is empty then).
    let active_output = if state.sel_device.is_empty() {
        state.player.device_name()
    } else {
        state.sel_device.clone()
    };
    for d in &state.output_devices {
        let name = d.clone();
        let entry = button(text(d).size(12)).on_press(Message::SettingsSelectDevice(name));
        devices = devices.push(if *d == active_output {
            entry.style(iced::widget::button::primary)
        } else {
            entry
        });
    }

    let mut inputs = column![text("Input devices:").size(12)].spacing(4);
    let cur_mic = mic_cfg.device.clone().unwrap_or_default();
    for d in &state.input_devices {
        let name = d.clone();
        let entry = button(text(d).size(12)).on_press(Message::MicSelectDevice(name));
        inputs = inputs.push(if *d == cur_mic {
            entry.style(iced::widget::button::primary)
        } else {
            entry
        });
    }

    let mut eq = column![text("12-band EQ:").size(12)].spacing(2);
    for band in 0..EQ_BAND_COUNT {
        eq = eq.push(
            row![
                text(format!(
                    "{}: {:+.0} dB",
                    eq_band_label(band),
                    s.eq_gains_db[band]
                ))
                .size(12)
                .width(Length::Fixed(160.0)),
                button(text("-").size(11)).on_press(Message::EqDec(band)),
                button(text("+").size(11)).on_press(Message::EqInc(band)),
            ]
            .spacing(6),
        );
    }

    // Category list + one detail page (Windows-Settings style). The cards
    // are gone: each section gets the full content width instead.
    let mut nav = column![].spacing(4);
    for sec in [
        SettingsSection::Station,
        SettingsSection::AudioDevice,
        SettingsSection::Playout,
        SettingsSection::Equalizer,
        SettingsSection::Loudness,
        SettingsSection::Streaming,
        SettingsSection::Microphone,
        SettingsSection::License,
    ] {
        let entry = button(text(sec.label()).size(13))
            .width(Length::Fill)
            .on_press(Message::SettingsNav(sec));
        nav = nav.push(if sec == state.settings_section {
            entry.style(iced::widget::button::primary)
        } else {
            entry.style(iced::widget::button::text)
        });
    }

    let sec = state.settings_section;
    let content: Element<'_, Message> = match sec {
        SettingsSection::Station => column![
            text(sec.label()).size(16),
            text(sec.description()).size(11),
            text_input("Station name", &state.settings.station_name)
                .on_input(Message::StationName)
                .padding(6),
        ]
        .spacing(8)
        .into(),
        SettingsSection::AudioDevice => column![
            text(sec.label()).size(16),
            text(sec.description()).size(11),
            text(format!(
                "Engine: {} | Device: {}",
                state.audio_engine,
                state.player.device_name()
            ))
            .size(12),
            devices,
            text(&state.device_note).size(11),
            button(text("Refresh devices").size(12)).on_press(Message::SettingsRefreshDevices),
        ]
        .spacing(8)
        .into(),
        SettingsSection::Playout => column![
            text(sec.label()).size(16),
            text(sec.description()).size(11),
            stepper(
                format!("Crossfade: {:.1} s", s.crossfade_secs),
                Message::XfadeDec,
                Message::XfadeInc
            ),
            stepper(
                format!("Silence alarm: {:.0} s", s.silence_threshold_secs),
                Message::SilenceDec,
                Message::SilenceInc
            ),
        ]
        .spacing(8)
        .into(),
        SettingsSection::Equalizer => column![
            text(sec.label()).size(16),
            text(sec.description()).size(11),
            row![
                checkbox(s.eq_enabled)
                    .label("EQ enabled")
                    .on_toggle(|_| Message::EqToggle),
                button(text("Reset EQ").size(11)).on_press(Message::EqReset),
            ]
            .spacing(8),
            eq,
            stepper(
                format!("Limiter: {:.1} dBFS", lin_to_dbfs(s.limiter_ceiling)),
                Message::LimiterDec,
                Message::LimiterInc
            ),
        ]
        .spacing(8)
        .into(),
        SettingsSection::Loudness => column![
            text(sec.label()).size(16),
            text(sec.description()).size(11),
            row![checkbox(s.loudness_norm)
                .label("Loudness normalize")
                .on_toggle(|_| Message::LoudnessToggle),]
            .spacing(8),
            stepper(
                format!("Target: {:.0} LUFS", s.loudness_target_lufs),
                Message::LoudnessTargetDec,
                Message::LoudnessTargetInc
            ),
        ]
        .spacing(8)
        .into(),
        SettingsSection::Streaming => column![
            text(sec.label()).size(16),
            text(sec.description()).size(11),
            row![
                checkbox(stream_cfg.enabled)
                    .label("Stream enabled")
                    .on_toggle(|_| Message::StreamToggle),
                checkbox(stream_cfg.tls)
                    .label("TLS (https)")
                    .on_toggle(|_| Message::StreamTlsToggle),
                text(stream_state.label()).size(12),
                text(if stream_state.is_live() {
                    format!(
                        "{} kbps - {:.1} MB - {}s",
                        stream_cfg.bitrate_kbps,
                        stream_stats.bytes_sent as f64 / 1_048_576.0,
                        stream_stats.stream_secs
                    )
                } else {
                    String::new()
                })
                .size(11),
            ]
            .spacing(8),
            text_input("Host", &stream_cfg.host)
                .on_input(Message::StreamHost)
                .padding(6),
            text_input("Port", &stream_cfg.port.to_string())
                .on_input(Message::StreamPort)
                .padding(6),
            text_input("Mount", &stream_cfg.mount)
                .on_input(Message::StreamMount)
                .padding(6),
            text_input("Username", &stream_cfg.username)
                .on_input(Message::StreamUsername)
                .padding(6),
            text_input("Password", &stream_cfg.password)
                .on_input(Message::StreamPassword)
                .padding(6),
            stepper(
                format!("Bitrate: {} kbps", stream_cfg.bitrate_kbps),
                Message::StreamBitrateDec,
                Message::StreamBitrateInc
            ),
        ]
        .spacing(8)
        .into(),
        SettingsSection::Microphone => column![
            text(sec.label()).size(16),
            text(sec.description()).size(11),
            row![
                checkbox(mic_cfg.enabled)
                    .label("Mic enabled")
                    .on_toggle(|_| Message::MicToggle),
                text(mic_state_label(&mic_state)).size(12),
                text(if mic_state_is_live(&mic_state) {
                    format!(
                        "{:.1} dBFS{}",
                        state.player.mic_level_db(),
                        if state.player.mic_ducking() {
                            " - ducking"
                        } else {
                            ""
                        }
                    )
                } else {
                    String::new()
                })
                .size(11),
            ]
            .spacing(8),
            inputs,
            text(&state.mic_note).size(11),
            button(text("Refresh inputs").size(12)).on_press(Message::MicRefreshDevices),
            stepper(
                format!("Mic level: {:.0}%", mic_cfg.level * 100.0),
                Message::MicLevelDec,
                Message::MicLevelInc
            ),
            row![checkbox(mic_cfg.duck_enabled)
                .label("Ducking")
                .on_toggle(|_| Message::MicDuckToggle),]
            .spacing(8),
            stepper(
                format!("Threshold: {:+.0} dB", mic_cfg.duck_threshold_db),
                Message::MicThresholdDec,
                Message::MicThresholdInc
            ),
            stepper(
                format!("Depth: -{:.0} dB", mic_cfg.duck_depth_db),
                Message::MicDepthDec,
                Message::MicDepthInc
            ),
            stepper(
                format!("Attack: {:.0} ms", mic_cfg.attack_ms),
                Message::MicAttackDec,
                Message::MicAttackInc
            ),
            stepper(
                format!("Release: {:.0} ms", mic_cfg.release_ms),
                Message::MicReleaseDec,
                Message::MicReleaseInc
            ),
        ]
        .spacing(8)
        .into(),
        SettingsSection::License => column![
            text(sec.label()).size(16),
            text(sec.description()).size(11),
            text(format!("License: {}", state.license_status)).size(12),
            text(&state.license_error).size(11),
            text_input("License key CB-XXXX-XXXX-XXXX", &state.license_key)
                .on_input(Message::LicenseKeyInput)
                .padding(6),
            row![
                button(text("Activate").size(12))
                    .width(Length::Fill)
                    .on_press(Message::ActivateLicense),
                button(text("Clear").size(12))
                    .width(Length::Fill)
                    .on_press(Message::ClearLicense),
            ]
            .spacing(6),
        ]
        .spacing(8)
        .into(),
    };

    row![
        container(nav.spacing(6).padding(10))
            .width(Length::Fixed(168.0))
            .height(Length::Fill),
        iced::widget::rule::vertical(1),
        container(
            scrollable(column![content].padding(iced::Padding {
                top: 12.0,
                right: 26.0,
                bottom: 12.0,
                left: 12.0,
            }))
            .height(Length::Fill)
        )
        .width(Length::Fill)
        .height(Length::Fill),
    ]
    .into()
}

fn mic_state_label(st: &crabcore::audio::MicState) -> String {
    format!("{:?}", st)
}

fn mic_state_is_live(st: &crabcore::audio::MicState) -> bool {
    matches!(st, crabcore::audio::MicState::Live)
}

// ---------------------------------------------------------------------------
// Entry
// ---------------------------------------------------------------------------

fn main() -> iced::Result {
    iced::application(boot, update, view)
        .title("CrabBoss")
        .subscription(subscription)
        .theme(|_: &App| Theme::Dark)
        .run()
}
