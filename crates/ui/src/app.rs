//! Application state, messages, update logic, and boot.
//! The Elm core: every screen in `screens/` reads this state and sends
//! these messages; `update` is still one central match on purpose.

use std::collections::{HashMap, VecDeque};
use std::path::PathBuf;
use std::sync::mpsc::{self, Receiver, TryRecvError};
use std::time::Duration;

use iced::{Subscription, Task};

use crabcore::audio::{
    Engine, PlayerState, EQ_BAND_COUNT, MAX_GAIN_DB, TARGET_MAX_LUFS, TARGET_MIN_LUFS,
};
use crabcore::library::{Library, Track, TrackKind};
use crabcore::playlist::PlaylistManager;
use crabcore::stream::StreamFormat;

use crate::widgets::{
    action_name, duck_ms_step, engine_choice, opus_bitrate_step, opus_snap_bitrate,
    report_range_bounds, stream_bitrate_step, strip_audio_extension, track_label, LoudnessDone,
    ATTACK_LADDER, RELEASE_LADDER,
};

// ---------------------------------------------------------------------------
// Navigation + messages
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub(crate) enum Screen {
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
    pub(crate) fn label(self) -> &'static str {
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
pub(crate) enum SettingsSection {
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
    pub(crate) fn label(self) -> &'static str {
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
    pub(crate) fn description(self) -> &'static str {
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
pub(crate) enum Message {
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
    LibraryKindChanged(Option<TrackKind>),
    LibraryMissingToggled(bool),
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
    StreamPasswordClear,
    StreamBitrateInc,
    StreamBitrateDec,
    StreamFormatChanged(StreamFormat),
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
    BackupNow,
    RestoreNow,
    // License
    LicenseKeyInput(String),
    ActivateLicense,
    ClearLicense,
}

// ---------------------------------------------------------------------------
// App state (lives on the UI thread; the cpal engine is `!Send` by design)
// ---------------------------------------------------------------------------

pub(crate) struct App {
    pub(crate) player: Box<dyn Engine>,
    pub(crate) library: Library,
    pub(crate) playlist_manager: PlaylistManager,
    pub(crate) scheduler: crabcore::scheduler::SchedulerManager,
    pub(crate) carts: crabcore::cart::CartManager,
    pub(crate) ads: crabcore::ads::AdsManager,
    pub(crate) settings: crabcore::settings::AppSettings,
    pub(crate) settings_path: PathBuf,
    /// Stable data root (settings + database live under it).
    pub(crate) data_dir: PathBuf,
    pub(crate) license: crabcore::license::LicenseStore,

    pub(crate) screen: Screen,
    pub(crate) settings_section: SettingsSection,
    pub(crate) station_name: String,
    pub(crate) audio_engine: String,

    // Player UI
    pub(crate) is_playing: bool,
    pub(crate) now_title: String,
    pub(crate) now_artist: String,
    pub(crate) volume: f32,
    pub(crate) autodj: bool,
    pub(crate) up_next: String,
    /// Forecast display: next tracks Auto-DJ would pick (recomputed when
    /// the live track changes; a prediction, not a commitment).
    pub(crate) up_next_list: Vec<Track>,
    /// Engine track the forecast was built for (`None` = stale/never).
    pub(crate) up_next_for: Option<PathBuf>,
    pub(crate) auto_continue: bool,
    pub(crate) was_playing: bool,
    /// No-repeat windows across Auto-DJ picks (plus the queued pick): every
    /// track handed to the engine is pushed here so separation bites.
    pub(crate) autodj_history: crabcore::playlist::RuleHistory,
    /// Engine-side current track path as last seen. Direct play paths set
    /// it synchronously; the tick reconciler adopts anything else (promoted
    /// queued decks) with proper logging + labels.
    pub(crate) engine_track: Option<PathBuf>,
    /// Who queued the currently pending deck ("Auto-DJ", "Scheduler").
    /// Set on successful queue(), consumed by the reconcile adopter,
    /// cleared by any direct play or stop. Lets promoted decks keep the
    /// right `· via X` tag instead of guessing.
    pub(crate) pending_source: Option<String>,

    // Library
    pub(crate) lib_tracks: Vec<Track>,
    pub(crate) lib_total: usize,
    pub(crate) lib_search: String,
    pub(crate) lib_kind: Option<TrackKind>,
    pub(crate) lib_missing_only: bool,
    pub(crate) lib_selected: Option<usize>,
    pub(crate) lib_status: String,

    // Loudness background scan
    pub(crate) scanning: bool,
    pub(crate) scan_rx: Option<Receiver<LoudnessDone>>,
    pub(crate) scan_done: usize,
    pub(crate) scan_total: usize,

    // Chunked import
    pub(crate) import_active: bool,
    pub(crate) import_pending: VecDeque<PathBuf>,
    pub(crate) import_added: usize,
    pub(crate) import_skipped: usize,
    pub(crate) import_total: usize,

    // Scheduler
    pub(crate) sched_enabled: bool,
    pub(crate) sched_events: Vec<crabcore::scheduler::ScheduledEvent>,
    pub(crate) sched_warnings: Vec<String>,
    pub(crate) sched_editor_open: bool,
    pub(crate) sched_edit_idx: Option<usize>,
    pub(crate) se_name: String,
    pub(crate) se_time: String,
    pub(crate) se_action: usize,
    pub(crate) se_target: String,
    pub(crate) se_expires: String,
    pub(crate) se_days: [bool; 7],
    pub(crate) sched_error: String,
    pub(crate) fired: HashMap<String, String>,
    pub(crate) fired_ads: HashMap<String, String>,

    // Carts
    pub(crate) cart_list: Vec<crabcore::cart::Cart>,
    pub(crate) cart_status: String,
    pub(crate) cart_assign: bool,

    // Reports
    pub(crate) report_entries: Vec<crabcore::report::PlayLogEntry>,
    pub(crate) report_summary: String,
    pub(crate) report_range: usize,
    /// Newest-first play log of the last 24h (all kinds) for the
    /// "Recently played" strip. Refilled by `refresh_report`.
    pub(crate) recent_plays: Vec<crabcore::report::PlayLogEntry>,

    // Ads
    pub(crate) ad_blocks: Vec<crabcore::ads::AdBlock>,
    pub(crate) ads_editor_open: bool,
    pub(crate) ads_edit_idx: Option<usize>,
    pub(crate) ab_name: String,
    pub(crate) ab_spot: String,
    pub(crate) ab_intro: String,
    pub(crate) ab_outro: String,
    pub(crate) ab_start: String,
    pub(crate) ab_end: String,
    pub(crate) ab_time: String,
    pub(crate) ab_days: [bool; 7],
    pub(crate) ads_error: String,

    // Settings UI caches
    pub(crate) output_devices: Vec<String>,
    pub(crate) sel_device: String,
    pub(crate) device_note: String,
    pub(crate) input_devices: Vec<String>,
    pub(crate) mic_note: String,
    pub(crate) backup_status: String,
    /// Boot-time settings file warning (invalid/unreadable file). `None`
    /// on first run and on clean loads: no news is good news.
    pub(crate) settings_notice: Option<String>,
    /// Last settings save failure, cleared on the next successful save.
    pub(crate) settings_save_error: Option<String>,
    /// Boot file was untrusted: preserve it aside on the first save
    /// instead of replacing it blindly.
    pub(crate) settings_needs_quarantine: bool,

    // License UI
    pub(crate) license_status: String,
    pub(crate) license_error: String,
    pub(crate) license_key: String,

    // Home counts
    pub(crate) track_count: usize,
    pub(crate) playlist_count: usize,
    pub(crate) upcoming_count: usize,

    pub(crate) last_recovery: Option<String>,
    pub(crate) tick_count: u64,
}

impl App {
    // -- persistence -------------------------------------------------------
    pub(crate) fn save_settings(&mut self) {
        // The boot file was untrusted (invalid/unreadable): move it aside
        // first so this save cannot silently destroy evidence. Refuse to
        // save at all when the preserve step itself fails.
        if self.settings_needs_quarantine {
            self.settings_needs_quarantine = false;
            match crabcore::settings::quarantine_existing(&self.settings_path) {
                Ok(Some(backup)) => {
                    let msg = format!("Previous settings kept at {}", backup.display());
                    tracing::warn!("{msg}");
                    self.settings_notice = Some(msg);
                }
                Ok(None) => {}
                Err(e) => {
                    let msg = format!("Settings NOT saved: cannot preserve existing file: {e}");
                    tracing::error!("{msg}");
                    self.settings_save_error = Some(msg);
                    return;
                }
            }
        }
        match self.settings.save(&self.settings_path) {
            Ok(()) => self.settings_save_error = None,
            Err(e) => {
                let msg = format!("Settings save failed: {e}");
                tracing::warn!("{msg}");
                self.settings_save_error = Some(msg);
            }
        }
    }

    fn refresh_library(&mut self) {
        let mut tracks = if self.lib_search.trim().is_empty() {
            self.library.get_all_tracks().unwrap_or_default()
        } else {
            self.library
                .search(self.lib_search.trim())
                .unwrap_or_default()
        };
        if let Some(kind) = self.lib_kind {
            tracks.retain(|t| t.kind == kind);
        }
        if self.lib_missing_only {
            tracks.retain(|t| !PathBuf::from(&t.file_path).is_file());
        }
        self.lib_total = self.library.get_all_tracks().unwrap_or_default().len();
        self.lib_tracks = tracks;
        self.track_count = self.lib_total;
        if let Some(sel) = self.lib_selected {
            if sel >= self.lib_tracks.len() {
                self.lib_selected = None;
            }
        }
    }

    pub(crate) fn refresh_scheduler(&mut self) {
        self.sched_events = self.scheduler.list_all().unwrap_or_default();
        let today = chrono::Local::now().format("%Y-%m-%d").to_string();
        self.sched_warnings = self
            .scheduler
            .expiry_warnings(&today, 3)
            .unwrap_or_default();
        self.upcoming_count = self.sched_events.iter().filter(|e| e.enabled).count();
    }

    pub(crate) fn refresh_carts(&mut self) {
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

    pub(crate) fn refresh_ads(&mut self) {
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
        // Recently played: same log, last 24h, all kinds (jingles/ads get
        // their kind tag in the view instead of being hidden).
        let day_ago = to - chrono::Duration::hours(24);
        self.recent_plays = crabcore::report::play_report(&self.library, day_ago, to, &[])
            .unwrap_or_default()
            .into_iter()
            .take(15)
            .collect();
    }

    pub(crate) fn refresh_counts(&mut self) {
        self.track_count = self.library.get_all_tracks().unwrap_or_default().len();
        self.playlist_count = self.playlist_manager.list_all().unwrap_or_default().len();
        self.upcoming_count = self.sched_events.iter().filter(|e| e.enabled).count();
    }

    // -- Auto-DJ ------------------------------------------------------------
    fn autodj_pick(&mut self) -> Option<Track> {
        crabcore::playlist::generate_next(
            &self.library,
            &self.autodj_cfg(),
            &mut self.autodj_history,
        )
        .ok()
        .flatten()
    }

    /// One-pick config for Auto-DJ rotation (current hour/weekday).
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
                self.engine_track = Some(path);
                // Direct play discards any pending queue (and its source).
                // (History already advanced inside generate_next.)
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
}

// ---------------------------------------------------------------------------
// Boot (startup sequence: settings, engine, stores, seeds)
// ---------------------------------------------------------------------------

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

    let db_path = paths.database.clone();
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
        lib_kind: None,
        lib_missing_only: false,
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
        backup_status: String::new(),
        settings_notice,
        settings_save_error: None,
        settings_needs_quarantine,
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

pub(crate) fn update(state: &mut App, message: Message) -> Task<Message> {
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
        Message::LibraryKindChanged(kind) => {
            state.lib_kind = kind;
            state.lib_selected = None;
            state.refresh_library();
        }
        Message::LibraryMissingToggled(only) => {
            state.lib_missing_only = only;
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
                state.se_days = crate::rules::days_from_bits(mask);
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
            let days = days_from_mask(crate::rules::days_to_mask(state.se_days));
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
                state.ab_days = crate::rules::days_from_bits(mask);
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
            let days = days_from_mask(crate::rules::days_to_mask(state.ab_days));
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
            // An emptied field clears the password for real: there is no
            // implicit "keep the old secret" — wiping is explicit by
            // deleting the text or pressing Clear.
            state.settings.stream.password = v;
            state.save_settings();
            state
                .player
                .set_stream_config(state.settings.stream.clone());
        }
        Message::StreamPasswordClear => {
            if !state.settings.stream.password.is_empty() {
                tracing::info!("Stream password cleared by operator");
            }
            state.settings.stream.password.clear();
            state.save_settings();
            state
                .player
                .set_stream_config(state.settings.stream.clone());
        }
        Message::StreamBitrateInc => {
            state.settings.stream.bitrate_kbps = match state.settings.stream.format {
                StreamFormat::Opus => opus_bitrate_step(state.settings.stream.bitrate_kbps, true),
                StreamFormat::Mp3 => stream_bitrate_step(state.settings.stream.bitrate_kbps, true),
            };
            state.save_settings();
            state
                .player
                .set_stream_config(state.settings.stream.clone());
        }
        Message::StreamBitrateDec => {
            state.settings.stream.bitrate_kbps = match state.settings.stream.format {
                StreamFormat::Opus => opus_bitrate_step(state.settings.stream.bitrate_kbps, false),
                StreamFormat::Mp3 => stream_bitrate_step(state.settings.stream.bitrate_kbps, false),
            };
            state.save_settings();
            state
                .player
                .set_stream_config(state.settings.stream.clone());
        }
        Message::StreamFormatChanged(format) => {
            // Takes effect on the next start (a new encoder + headers wrap
            // the fresh connection); restart the stream to apply it live.
            state.settings.stream.format = format;
            if format == StreamFormat::Opus {
                state.settings.stream.bitrate_kbps =
                    opus_snap_bitrate(state.settings.stream.bitrate_kbps);
            }
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
        Message::BackupNow => {
            let path = rfd::FileDialog::new()
                .set_title("Backup station data (JSON)")
                .set_file_name(format!(
                    "crabboss-backup-{}.json",
                    chrono::Local::now().format("%Y%m%d-%H%M%S")
                ))
                .add_filter("JSON", &["json"])
                .save_file();
            let Some(path) = path else {
                return Task::none();
            };
            match crate::backup::write_backup(&path, &state.build_backup()) {
                Ok(()) => {
                    tracing::info!("Backup saved to {}", path.display());
                    state.backup_status = format!("Backup saved to {}", path.display());
                }
                Err(e) => {
                    tracing::error!("Backup failed: {e}");
                    state.backup_status = format!("Backup failed: {e}");
                }
            }
        }
        Message::RestoreNow => {
            let path = rfd::FileDialog::new()
                .set_title("Restore station data (JSON)")
                .add_filter("JSON", &["json"])
                .pick_file();
            let Some(path) = path else {
                return Task::none();
            };
            match crate::backup::read_backup(&path).and_then(|b| state.apply_backup(b)) {
                Ok(status) => {
                    tracing::info!("Restore from {}: {status}", path.display());
                    state.backup_status = status;
                }
                Err(e) => {
                    tracing::error!("Restore failed: {e}");
                    state.backup_status = format!("Restore failed: {e}");
                }
            }
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

pub(crate) fn subscription(_: &App) -> Subscription<Message> {
    Subscription::batch(vec![
        iced::time::every(Duration::from_millis(200)).map(|_| Message::Tick),
        iced::keyboard::listen().filter_map(|event| match event {
            iced::keyboard::Event::KeyPressed { key, modifiers, .. } => cart_hotkey(key, modifiers),
            _ => None,
        }),
    ])
}

impl App {
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
                        let source = crate::rules::take_pending_source(&mut self.pending_source);
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
            let minute_key = crate::rules::minute_key(&now.naive_local());
            let today = now.format("%Y-%m-%d").to_string();
            let due: Vec<(String, usize)> = self
                .scheduler
                .due_events(&today, &hhmm, &weekday)
                .unwrap_or_default()
                .into_iter()
                .filter_map(|e| {
                    if crate::rules::fired_this_minute(&self.fired, &e.id, &minute_key) {
                        return None;
                    }
                    self.sched_events
                        .iter()
                        .position(|s| s.id == e.id)
                        .map(|idx| (e.id.clone(), idx))
                })
                .collect();
            for (id, idx) in due {
                if crate::rules::claim_fire_slot(&mut self.fired, &id, &minute_key) {
                    self.fire_scheduled_event(idx);
                }
            }
            if let Ok(date) = chrono::NaiveDate::parse_from_str(&today, "%Y-%m-%d") {
                let due_ads: Vec<(String, usize)> = self
                    .ads
                    .due_blocks(date, &hhmm, &weekday)
                    .unwrap_or_default()
                    .into_iter()
                    .filter_map(|b| {
                        if crate::rules::fired_this_minute(&self.fired_ads, &b.id, &minute_key) {
                            return None;
                        }
                        self.ad_blocks
                            .iter()
                            .position(|a| a.id == b.id)
                            .map(|idx| (b.id.clone(), idx))
                    })
                    .collect();
                for (id, idx) in due_ads {
                    if crate::rules::claim_fire_slot(&mut self.fired_ads, &id, &minute_key) {
                        self.fire_ad_block(idx);
                    }
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

        // Auto-DJ continuity + prefetch. The decision table lives in
        // `rules.rs` and is pinned by tests; this block only acts on it.
        let dur_opt = if has_dur { Some(dur) } else { None };
        let tick = crate::rules::AutodjTick {
            autodj: self.autodj,
            auto_continue: self.auto_continue,
            playing,
            finished,
            was_playing: self.was_playing,
            pos,
            dur: dur_opt,
            pending: self.player.pending_count() + self.player.load_inflight(),
            has_queue: self.player.has_queue(),
        };
        let (action, next_was) = crate::rules::autodj_tick_action(&tick);
        self.was_playing = next_was;
        match action {
            crate::rules::AutodjAction::PlayNow => {
                self.autodj_play_now();
                return;
            }
            crate::rules::AutodjAction::Idle => return,
            crate::rules::AutodjAction::Prefetch => {}
        }
        let pick = self.autodj_pick();
        if let Some(pick) = pick {
            let Some(path) = crate::rules::pick_engine_path(&pick.file_path) else {
                return;
            };
            match self.player.queue(&path) {
                Ok(()) => {
                    let label = track_label(&pick);
                    tracing::info!("Auto-DJ queued: {}", label);
                    self.up_next = label;
                    // History advanced inside generate_next; a supersede
                    // discarding this deck just leaves a harmless ghost in
                    // soft windows.
                    self.pending_source = Some("Auto-DJ".into());
                }
                Err(e) => tracing::warn!("Auto-DJ queue failed: {}", e),
            }
        }
    }
}

#[cfg(test)]
mod app_tests {
    use super::*;
    use iced::keyboard::{Key, Modifiers};

    fn hotkey(c: &str) -> Option<Message> {
        cart_hotkey(Key::Character(c.into()), Modifiers::empty())
    }

    #[test]
    fn cart_hotkeys_map_1_to_8_onto_pads_0_to_7() {
        for (key, pad) in ["1", "2", "3", "4", "5", "6", "7", "8"].iter().zip(0..8) {
            assert!(
                matches!(hotkey(key), Some(Message::CartHotkey(i)) if i == pad),
                "{key} must fire pad {pad}"
            );
        }
    }

    #[test]
    fn cart_hotkeys_ignore_other_characters() {
        for key in ["0", "9", "a", "!", " "] {
            assert!(hotkey(key).is_none(), "{key} must not fire a pad");
        }
    }
}
