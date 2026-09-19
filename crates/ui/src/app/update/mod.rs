//! Central dispatcher: one match on `Message`, one domain function
//! per arm. Most arms are synchronous (`Task::none()` at the end);
//! arms that open a native dialog return their `Task` early so the
//! iced event loop never blocks.

use iced::Task;

use super::{App, Message};

pub(crate) mod ads;
pub(crate) mod carts;
pub(crate) mod generator;
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
        self.refresh_saved_playlists();
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
        Message::PlaySelected => transport::play_selected(state),
        Message::VolumeChanged(v) => transport::set_volume(state, v),
        Message::AutodjToggled(en) => transport::toggle_autodj(state, en),
        // -- Library ---------------------------------------------------------
        Message::LibrarySearchChanged(q) => library::search_changed(state, q),
        Message::LibraryKindChanged(kind) => library::kind_changed(state, kind),
        Message::LibraryMissingToggled(only) => library::missing_toggled(state, only),
        Message::LibraryDupesToggled(only) => library::dupes_toggled(state, only),
        Message::LibraryTrackSelected(i) => library::track_selected(state, i),
        Message::LibraryCuePlay(i) => library::cue_play(state, i),
        Message::LibraryCueStop() => library::cue_stop(state),
        Message::ImportFiles => return library::import_files(state),
        Message::ImportFilesPicked(files) => library::import_files_picked(state, files),
        Message::HealthCheck => library::health_check(state),
        Message::LoudnessScan => library::loudness_scan(state),
        Message::AutoSyncToggled(on) => library::autosync_toggled(state, on),
        Message::AutoSyncIntervalInc => library::autosync_interval_step(state, true),
        Message::AutoSyncIntervalDec => library::autosync_interval_step(state, false),
        Message::WatchFolderAdd => library::watch_folder_add(state),
        Message::WatchFolderRemove(i) => library::watch_folder_remove(state, i),
        // -- Rotation generator --------------------------------------------------
        Message::GenHourInc(i) => generator::hour_step(state, i, true),
        Message::GenHourDec(i) => generator::hour_step(state, i, false),
        Message::GenCountInc(i) => generator::count_step(state, i, true),
        Message::GenCountDec(i) => generator::count_step(state, i, false),
        Message::GenFire(i) => generator::fire_one(state, i),
        Message::GenFireAll => generator::fire_all(state),
        Message::PlaylistToAir(id) => generator::playlist_to_air(state, id),
        Message::PlaylistNewName(v) => state.playlist_new_name = v,
        Message::PlaylistCreate => generator::playlist_create(state),
        Message::PlaylistSelect(id) => generator::playlist_select(state, id),
        Message::PlaylistDelete(id) => generator::playlist_delete(state, id),
        Message::PlaylistAddSelected(id) => generator::playlist_add_selected(state, id),
        Message::PlaylistRemoveItem(id, idx) => generator::playlist_remove_item(state, id, idx),
        Message::PlaylistMoveUp(id, idx) => generator::playlist_move(state, id, idx, true),
        Message::PlaylistMoveDown(id, idx) => generator::playlist_move(state, id, idx, false),
        Message::PlaylistRenameInput(v) => state.playlist_rename = v,
        Message::PlaylistRename(id) => generator::playlist_rename(state, id),
        Message::PlaylistExport(id) => generator::playlist_export(state, id),
        Message::PlaylistImport => generator::playlist_import(state),
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
        Message::CueSelectDevice(name) => settings::cue_select_device(state, name),
        Message::CueVolumeInc => settings::cue_volume_inc(state),
        Message::CueVolumeDec => settings::cue_volume_dec(state),
        Message::XfadeInc => settings::xfade_inc(state),
        Message::XfadeDec => settings::xfade_dec(state),
        Message::SilenceInc => settings::silence_inc(state),
        Message::SilenceDec => settings::silence_dec(state),
        Message::EqToggle => settings::eq_toggle(state),
        Message::EqSet(band, v) => settings::eq_set(state, band, v),
        Message::EqSave => settings::eq_save(state),
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
        Message::StreamProtocolChanged(protocol) => {
            settings::stream_protocol_changed(state, protocol)
        }
        Message::StreamSidInc => settings::stream_sid_inc(state),
        Message::StreamSidDec => settings::stream_sid_dec(state),
        Message::StreamRestart => settings::stream_restart(state),
        Message::StToggle => settings::st_toggle(state),
        Message::StBypassToggle => settings::st_bypass_toggle(state),
        Message::StLibPath(v) => settings::st_lib_path(state, v),
        Message::StLicenseKey(v) => settings::st_license_key(state, v),
        Message::StPresetPath(v) => settings::st_preset_path(state, v),
        Message::StPickLibrary => return settings::st_pick_library(state),
        Message::StLibraryPicked(p) => settings::st_library_picked(state, p),
        Message::StPickPreset => return settings::st_pick_preset(state),
        Message::StPresetPicked(p) => settings::st_preset_picked(state, p),
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
    }
    Task::none()
}
