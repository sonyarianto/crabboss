//! Central dispatcher: one match on `Message`, one domain function
//! per arm, `Task::none()` at the end (every arm is synchronous).
//! Trivial field assigns stay inline; anything with logic lives in its
//! domain module. `refresh_counts` also lives here: it spans
//! library/playlists/scheduler, so no single domain owns it.

use iced::Task;

use super::{App, Message};

pub(crate) mod ads;
pub(crate) mod carts;
pub(crate) mod library;
pub(crate) mod reports;
pub(crate) mod scheduler;
pub(crate) mod settings;
pub(crate) mod transport;

impl App {
    /// Cross-domain home counts (library + playlists + scheduler), used
    /// by boot and backup restore. No single domain owns it, so it lives
    /// with the dispatcher.
    pub(crate) fn refresh_counts(&mut self) {
        self.track_count = self.library.get_all_tracks().unwrap_or_default().len();
        self.playlist_count = self.playlist_manager.list_all().unwrap_or_default().len();
        self.upcoming_count = self.sched_events.iter().filter(|e| e.enabled).count();
    }
}

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
        Message::Play => transport::play(state),
        Message::Pause => transport::pause(state),
        Message::Stop => transport::stop(state),
        Message::Next => transport::next(state),
        Message::Prev => transport::prev(state),
        Message::VolumeChanged(v) => transport::set_volume(state, v),
        Message::AutodjToggled(en) => transport::toggle_autodj(state, en),
        // -- Library ---------------------------------------------------------
        Message::LibrarySearchChanged(q) => library::search_changed(state, q),
        Message::LibraryKindChanged(kind) => library::kind_changed(state, kind),
        Message::LibraryMissingToggled(only) => library::missing_toggled(state, only),
        Message::LibraryDupesToggled(only) => library::dupes_toggled(state, only),
        Message::LibraryTrackSelected(i) => library::track_selected(state, i),
        Message::LibraryTrackPlay(i) => library::track_play(state, i),
        Message::ImportFiles => library::import_files(state),
        Message::HealthCheck => library::health_check(state),
        Message::LoudnessScan => library::loudness_scan(state),
        Message::AutoSyncToggled(on) => library::autosync_toggled(state, on),
        Message::AutoSyncIntervalInc => library::autosync_interval_step(state, true),
        Message::AutoSyncIntervalDec => library::autosync_interval_step(state, false),
        Message::WatchFolderAdd => library::watch_folder_add(state),
        Message::WatchFolderRemove(i) => library::watch_folder_remove(state, i),
        // -- Scheduler -------------------------------------------------------
        Message::SchedulerMasterToggled(en) => scheduler::master_toggled(state, en),
        Message::SchedulerToggleEvent(i) => scheduler::toggle_event(state, i),
        Message::SchedulerDeleteEvent(i) => scheduler::delete_event(state, i),
        Message::SchedulerRunEvent(i) => scheduler::run_event(state, i),
        Message::SchedulerNew => scheduler::editor_new(state),
        Message::SchedulerEdit(i) => scheduler::editor_edit(state, i),
        Message::SchedulerEditorClose => scheduler::editor_close(state),
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
        Message::SchedulerSave => scheduler::save(state),
        // -- Carts ------------------------------------------------------------
        Message::CartPlay(i) => carts::play(state, i),
        Message::CartHotkey(i) => carts::hotkey(state, i),
        Message::CartDelete(i) => carts::delete(state, i),
        Message::CartAdd => carts::add(state),
        Message::CartPlace(slot) => carts::place(state, slot),
        Message::CartToggleAssign => carts::toggle_assign(state),
        // -- Reports -----------------------------------------------------------
        Message::ReportRangeChanged(i) => reports::range_changed(state, i),
        Message::ReportExport => reports::export(state),
        // -- Ads ----------------------------------------------------------------
        Message::AdsToggle(i) => ads::toggle(state, i),
        Message::AdsDelete(i) => ads::delete(state, i),
        Message::AdsRun(i) => ads::run(state, i),
        Message::AdsNew => ads::editor_new(state),
        Message::AdsEdit(i) => ads::editor_edit(state, i),
        Message::AdsEditorClose => ads::editor_close(state),
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
        Message::AdsSave => ads::save(state),
        // -- Settings ------------------------------------------------------------
        Message::SettingsRefreshDevices => settings::refresh_devices(state),
        Message::SettingsSelectDevice(name) => settings::select_device(state, name),
        Message::XfadeInc => settings::xfade_inc(state),
        Message::XfadeDec => settings::xfade_dec(state),
        Message::SilenceInc => settings::silence_inc(state),
        Message::SilenceDec => settings::silence_dec(state),
        Message::EqToggle => settings::eq_toggle(state),
        Message::EqInc(band) => settings::eq_inc(state, band),
        Message::EqDec(band) => settings::eq_dec(state, band),
        Message::EqReset => settings::eq_reset(state),
        Message::LimiterInc => settings::limiter_inc(state),
        Message::LimiterDec => settings::limiter_dec(state),
        Message::LoudnessToggle => settings::loudness_toggle(state),
        Message::LoudnessTargetInc => settings::loudness_target_inc(state),
        Message::LoudnessTargetDec => settings::loudness_target_dec(state),
        Message::StreamToggle => settings::stream_toggle(state),
        Message::StreamTlsToggle => settings::stream_tls_toggle(state),
        Message::StreamHost(v) => settings::stream_host(state, v),
        Message::StreamUsername(v) => settings::stream_username(state, v),
        Message::StreamPort(v) => settings::stream_port(state, v),
        Message::StreamMount(v) => settings::stream_mount(state, v),
        Message::StreamPassword(v) => settings::stream_password(state, v),
        Message::StreamPasswordClear => settings::stream_password_clear(state),
        Message::StreamBitrateInc => settings::stream_bitrate_inc(state),
        Message::StreamBitrateDec => settings::stream_bitrate_dec(state),
        Message::StreamFormatChanged(format) => settings::stream_format_changed(state, format),
        Message::MicToggle => settings::mic_toggle(state),
        Message::MicRefreshDevices => settings::mic_refresh_devices(state),
        Message::MicSelectDevice(name) => settings::mic_select_device(state, name),
        Message::MicLevelInc => settings::mic_level_inc(state),
        Message::MicLevelDec => settings::mic_level_dec(state),
        Message::MicDuckToggle => settings::mic_duck_toggle(state),
        Message::MicThresholdInc => settings::mic_threshold_inc(state),
        Message::MicThresholdDec => settings::mic_threshold_dec(state),
        Message::MicDepthInc => settings::mic_depth_inc(state),
        Message::MicDepthDec => settings::mic_depth_dec(state),
        Message::MicAttackInc => settings::mic_attack_inc(state),
        Message::MicAttackDec => settings::mic_attack_dec(state),
        Message::MicReleaseInc => settings::mic_release_inc(state),
        Message::MicReleaseDec => settings::mic_release_dec(state),
        Message::StationName(v) => settings::station_name(state, v),
        Message::BackupNow => settings::backup_now(state),
        Message::RestoreNow => settings::restore_now(state),
        // -- License ----------------------------------------------------------
        Message::LicenseKeyInput(v) => settings::license_key_input(state, v),
        Message::ActivateLicense => settings::activate_license(state),
        Message::ClearLicense => settings::clear_license(state),
    }
    Task::none()
}
