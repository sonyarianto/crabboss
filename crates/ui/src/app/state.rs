//! Application state: navigation, the `App` store, and the settings
//! persistence primitive. Lives on the UI thread (the cpal engine is
//! `!Send` by design). Behavior methods live with their domains under
//! `update/`; the tick lives in `tick.rs`.

use std::collections::{HashMap, VecDeque};
use std::path::PathBuf;
use std::sync::mpsc::Receiver;

use crabcore::audio::Engine;
use crabcore::library::{Library, Track, TrackKind};
use crabcore::playlist::{PlaylistManager, RuleHistory};
use crabcore::stream::StreamConfig;
use crabcore::voice::{VoiceManager, VoiceTrack};

use super::update::generator::{PlaylistDetailItem, SavedPlaylist};
use crate::widgets::{LoudnessDone, SyncFound};

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
    Processor,
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
            SettingsSection::Processor => "Stereo Tool",
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
                "Source client (Icecast / Shoutcast v1 / v2): server, encoder, and live status."
            }
            SettingsSection::Microphone => {
                "Live input with voice-activated ducking of the music bed."
            }
            SettingsSection::Processor => {
                "Optional Thimeo on-air DSP on the stream (bring your own library + key + preset)."
            }
        }
    }
}

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
    /// Database file, for whole-list operations (e.g. backup restore)
    /// that need a single transaction across managers.
    pub(crate) db_path: PathBuf,

    pub(crate) screen: Screen,
    pub(crate) settings_section: SettingsSection,
    pub(crate) station_name: String,
    pub(crate) audio_engine: String,

    // Player UI
    pub(crate) is_playing: bool,
    pub(crate) now_title: String,
    pub(crate) now_artist: String,
    /// Installed track the cached cover belongs to (`None` = nothing
    /// cached). Compared against `engine_track` each tick; the image
    /// decodes once per track, never per frame.
    pub(crate) now_art_path: Option<PathBuf>,
    pub(crate) now_art: Option<iced::widget::image::Handle>,
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
    pub(crate) autodj_history: RuleHistory,
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
    pub(crate) lib_dupes_only: bool,
    /// Possible-duplicate group count from the last refresh (0 when the
    /// filter is off or the read failed).
    pub(crate) lib_dupe_groups: usize,
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
    /// Last directory the import dialog successfully picked from. Used
    /// as the next dialog's starting folder so Windows doesn't fall
    /// back to Quick Access / This PC (slow "Working on it..."
    /// enumeration on network / OneDrive machines).
    pub(crate) last_import_dir: Option<PathBuf>,

    // Folder auto-sync (timer walk; results queue via import above)
    pub(crate) syncing: bool,
    pub(crate) sync_rx: Option<Receiver<SyncFound>>,
    pub(crate) last_auto_sync: Option<std::time::Instant>,

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

    // Voice tracking (§1.4): recorded takes live outside the library
    // (never music reports/rotations). `voice_live`/`voice_queued`
    // are (path, name) pairs the tick uses to label voice decks.
    pub(crate) voice_manager: VoiceManager,
    pub(crate) voice_dir: PathBuf,
    pub(crate) voice_list: Vec<VoiceTrack>,
    pub(crate) voice_status: String,
    pub(crate) voice_recording: bool,
    pub(crate) voice_rec_elapsed: f64,
    pub(crate) voice_live: Option<(PathBuf, String)>,
    pub(crate) voice_queued: Option<(PathBuf, String)>,
    /// Rolling take path (set at record start, consumed at stop).
    pub(crate) voice_take_path: Option<PathBuf>,

    // Settings UI caches
    pub(crate) output_devices: Vec<String>,
    pub(crate) sel_device: String,
    pub(crate) device_note: String,
    pub(crate) input_devices: Vec<String>,
    pub(crate) mic_note: String,
    /// Cue (PFL) status line for Settings > Audio Device + Library hints
    /// (e.g. "Cue live: Realtek" / "Cue: no device"). Program status
    /// stays in `lib_status` / footer; cue never writes there except for
    /// per-pick errors shown inline in the Library list.
    pub(crate) cue_status: String,
    pub(crate) backup_status: String,
    /// Last polled listener count (`None` = never polled or last poll
    /// failed — the UI shows "—", never an error state).
    pub(crate) stream_listeners: Option<u64>,
    pub(crate) listeners_polling: bool,
    pub(crate) listeners_rx: Option<Receiver<Option<u64>>>,
    pub(crate) last_listeners_poll: Option<std::time::Instant>,
    /// Full config snapshot taken when the stream (re)starts. The encoder
    /// and connection are built once per start from that snapshot, while
    /// every field on the screen edits the next connection, so without
    /// this the UI would show the selection as if it were on air. A
    /// missing value means the stream never started (or stopped).
    pub(crate) stream_live_config: Option<StreamConfig>,
    /// Boot-time settings file warning (invalid/unreadable file). `None`
    /// on first run and on clean loads: no news is good news.
    pub(crate) settings_notice: Option<String>,
    /// Last settings save failure, cleared on the next successful save.
    pub(crate) settings_save_error: Option<String>,
    /// Boot file was untrusted: preserve it aside on the first save
    /// instead of replacing it blindly.
    pub(crate) settings_needs_quarantine: bool,

    // Home counts
    pub(crate) track_count: usize,
    pub(crate) playlist_count: usize,
    pub(crate) upcoming_count: usize,

    // Rotation generator panel (Home): per-daypart hour + track count
    // (session knobs, not persisted) plus the last fire status.
    pub(crate) gen_hours: [u8; 4],
    pub(crate) gen_counts: [usize; 4],
    pub(crate) gen_status: String,
    /// Saved playlists for the Home list (A1 fire-to-air). Rebuilt by
    /// `refresh_saved_playlists` on boot/restore/every generator fire.
    pub(crate) saved_playlists: Vec<SavedPlaylist>,
    // Manual builder (B): create/select/detail for the Home list.
    pub(crate) playlist_new_name: String,
    pub(crate) playlist_selected: Option<String>,
    pub(crate) playlist_detail: Vec<PlaylistDetailItem>,
    pub(crate) playlist_detail_missing: usize,
    /// Rename input for the expanded playlist (P0: core `rename`
    /// existed with no UI — this buffer backs it).
    pub(crate) playlist_rename: String,

    pub(crate) last_recovery: Option<String>,
    pub(crate) tick_count: u64,
}

impl App {
    /// Snapshot the connection actually put on air (encoder + server
    /// parameters are fixed per `stream_start`).
    pub(crate) fn mark_stream_live(&mut self) {
        self.stream_live_config = Some(self.settings.stream.clone());
    }

    pub(crate) fn clear_stream_live(&mut self) {
        self.stream_live_config = None;
    }
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
}
