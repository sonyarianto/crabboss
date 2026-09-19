//! Settings: every tuner on the Settings screen (device, playout,
//! EQ, loudness, streaming, mic, station, backup).

use crabcore::audio::{EQ_BAND_COUNT, TARGET_MAX_LUFS, TARGET_MIN_LUFS};
use crabcore::stream::{StreamFormat, StreamProtocol};

use std::sync::mpsc;

use super::super::App;
use crate::widgets::{
    duck_ms_step, opus_bitrate_step, opus_snap_bitrate, stream_bitrate_step, ATTACK_LADDER,
    RELEASE_LADDER,
};

pub(crate) fn refresh_devices(state: &mut App) {
    state.output_devices = crabcore::audio::CpalEngine::list_output_devices();
}

pub(crate) fn select_device(state: &mut App, name: String) {
    state.settings.output_device = Some(name.clone());
    state.save_settings();
    tracing::info!("Output device set to '{}' (restart to apply)", name);
    state.sel_device = name;
    state.device_note = "Restart CrabBoss to apply the new device".into();
}

pub(crate) fn xfade_inc(state: &mut App) {
    state.settings.crossfade_secs = (state.settings.crossfade_secs + 0.5).clamp(0.0, 30.0);
    state
        .player
        .set_crossfade_secs(state.settings.crossfade_secs);
    state.save_settings();
}

pub(crate) fn xfade_dec(state: &mut App) {
    state.settings.crossfade_secs = (state.settings.crossfade_secs - 0.5).clamp(0.0, 30.0);
    state
        .player
        .set_crossfade_secs(state.settings.crossfade_secs);
    state.save_settings();
}

pub(crate) fn silence_inc(state: &mut App) {
    state.settings.silence_threshold_secs =
        (state.settings.silence_threshold_secs + 1.0).clamp(1.0, 120.0);
    state
        .player
        .set_silence_threshold_secs(state.settings.silence_threshold_secs);
    state.save_settings();
}

pub(crate) fn silence_dec(state: &mut App) {
    state.settings.silence_threshold_secs =
        (state.settings.silence_threshold_secs - 1.0).clamp(1.0, 120.0);
    state
        .player
        .set_silence_threshold_secs(state.settings.silence_threshold_secs);
    state.save_settings();
}

pub(crate) fn eq_toggle(state: &mut App) {
    state.settings.eq_enabled = !state.settings.eq_enabled;
    state.player.set_eq_enabled(state.settings.eq_enabled);
    state.save_settings();
}

pub(crate) fn eq_set(state: &mut App, band: usize, gain_db: f32) {
    // Drag path (no save): the fader fires per pixel, and every save is
    // an atomic file write — persisting happens once on release.
    if band < EQ_BAND_COUNT {
        state.settings.eq_gains_db[band] = gain_db.clamp(-12.0, 12.0);
        state
            .player
            .set_eq_band(band, state.settings.eq_gains_db[band]);
    }
}

pub(crate) fn eq_save(state: &mut App) {
    state.save_settings();
}

pub(crate) fn eq_reset(state: &mut App) {
    state.settings.eq_gains_db = [0.0; EQ_BAND_COUNT];
    for (band, gain) in state.settings.eq_gains_db.iter().enumerate() {
        state.player.set_eq_band(band, *gain);
    }
    state.save_settings();
}

pub(crate) fn limiter_inc(state: &mut App) {
    state.settings.limiter_ceiling = (state.settings.limiter_ceiling * 1.122).clamp(0.1, 1.0);
    state
        .player
        .set_limiter_ceiling(state.settings.limiter_ceiling);
    state.save_settings();
}

pub(crate) fn limiter_dec(state: &mut App) {
    state.settings.limiter_ceiling = (state.settings.limiter_ceiling / 1.122).clamp(0.1, 1.0);
    state
        .player
        .set_limiter_ceiling(state.settings.limiter_ceiling);
    state.save_settings();
}

pub(crate) fn loudness_toggle(state: &mut App) {
    state.settings.loudness_norm = !state.settings.loudness_norm;
    state
        .player
        .set_loudness_enabled(state.settings.loudness_norm);
    state.save_settings();
}

pub(crate) fn loudness_target_inc(state: &mut App) {
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

pub(crate) fn loudness_target_dec(state: &mut App) {
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

pub(crate) fn stream_toggle(state: &mut App) {
    state.settings.stream.enabled = !state.settings.stream.enabled;
    state.save_settings();
    state
        .player
        .set_stream_config(state.settings.stream.clone());
    if state.settings.stream.enabled {
        if let Err(e) = state.player.stream_start() {
            tracing::warn!("Stream start failed: {e}");
        }
        state.mark_stream_live();
    } else {
        state.player.stream_stop();
        state.clear_stream_live();
    }
}

pub(crate) fn stream_tls_toggle(state: &mut App) {
    // Takes effect on the next start (TLS wraps the fresh
    // connection); restart the stream to apply it live.
    state.settings.stream.tls = !state.settings.stream.tls;
    state.save_settings();
    state
        .player
        .set_stream_config(state.settings.stream.clone());
}

pub(crate) fn stream_host(state: &mut App, v: String) {
    state.settings.stream.host = v;
    state.save_settings();
    state
        .player
        .set_stream_config(state.settings.stream.clone());
}

pub(crate) fn stream_username(state: &mut App, v: String) {
    state.settings.stream.username = v;
    state.save_settings();
    state
        .player
        .set_stream_config(state.settings.stream.clone());
}

pub(crate) fn stream_port(state: &mut App, v: String) {
    if let Ok(p) = v.trim().parse::<u16>() {
        state.settings.stream.port = p;
        state.save_settings();
        state
            .player
            .set_stream_config(state.settings.stream.clone());
    }
}

pub(crate) fn stream_mount(state: &mut App, v: String) {
    state.settings.stream.mount = v;
    state.save_settings();
    state
        .player
        .set_stream_config(state.settings.stream.clone());
}

pub(crate) fn stream_password(state: &mut App, v: String) {
    // An emptied field clears the password for real: there is no
    // implicit "keep the old secret" — wiping is explicit by
    // deleting the text or pressing Clear.
    state.settings.stream.password = v;
    state.save_settings();
    state
        .player
        .set_stream_config(state.settings.stream.clone());
}

pub(crate) fn stream_password_clear(state: &mut App) {
    if !state.settings.stream.password.is_empty() {
        tracing::info!("Stream password cleared by operator");
    }
    state.settings.stream.password.clear();
    state.save_settings();
    state
        .player
        .set_stream_config(state.settings.stream.clone());
}

pub(crate) fn stream_bitrate_inc(state: &mut App) {
    state.settings.stream.bitrate_kbps = match state.settings.stream.format {
        StreamFormat::Opus => opus_bitrate_step(state.settings.stream.bitrate_kbps, true),
        StreamFormat::Mp3 => stream_bitrate_step(state.settings.stream.bitrate_kbps, true),
    };
    state.save_settings();
    state
        .player
        .set_stream_config(state.settings.stream.clone());
}

pub(crate) fn stream_bitrate_dec(state: &mut App) {
    state.settings.stream.bitrate_kbps = match state.settings.stream.format {
        StreamFormat::Opus => opus_bitrate_step(state.settings.stream.bitrate_kbps, false),
        StreamFormat::Mp3 => stream_bitrate_step(state.settings.stream.bitrate_kbps, false),
    };
    state.save_settings();
    state
        .player
        .set_stream_config(state.settings.stream.clone());
}

pub(crate) fn stream_format_changed(state: &mut App, format: StreamFormat) {
    // Takes effect on the next start (a new encoder + headers wrap
    // the fresh connection); restart the stream to apply it live.
    state.settings.stream.format = format;
    if format == StreamFormat::Opus {
        state.settings.stream.bitrate_kbps = opus_snap_bitrate(state.settings.stream.bitrate_kbps);
    }
    state.save_settings();
    state
        .player
        .set_stream_config(state.settings.stream.clone());
}

pub(crate) fn stream_protocol_changed(state: &mut App, protocol: StreamProtocol) {
    // Takes effect on the next start (a new handshake wraps the fresh
    // connection); restart the stream to apply it live. Shoutcast is
    // MP3-only, so move off Opus now instead of failing at connect.
    state.settings.stream.protocol = protocol;
    if protocol.is_shoutcast() && state.settings.stream.format == StreamFormat::Opus {
        state.settings.stream.format = StreamFormat::Mp3;
        tracing::info!("Stream protocol needs MP3; format switched from Opus");
    }
    state.save_settings();
    state
        .player
        .set_stream_config(state.settings.stream.clone());
}

pub(crate) fn stream_sid_inc(state: &mut App) {
    // DNAS stream IDs start at 1; cap high enough for big servers.
    state.settings.stream.sid = (state.settings.stream.sid + 1).clamp(1, 99);
    state.save_settings();
    state
        .player
        .set_stream_config(state.settings.stream.clone());
}

pub(crate) fn stream_sid_dec(state: &mut App) {
    state.settings.stream.sid = state.settings.stream.sid.saturating_sub(1).max(1);
    state.save_settings();
    state
        .player
        .set_stream_config(state.settings.stream.clone());
}

pub(crate) fn stream_restart(state: &mut App) {
    // One-click apply for pending changes while live: fresh connection
    // with the current selection. Listeners rebuffer — the button only
    // appears when the selection differs from what's on air.
    if !state.settings.stream.enabled {
        return;
    }
    state.player.stream_stop();
    state
        .player
        .set_stream_config(state.settings.stream.clone());
    if let Err(e) = state.player.stream_start() {
        tracing::warn!("Stream restart failed: {e}");
    }
    state.mark_stream_live();
    tracing::info!("Stream restarted with new settings");
}

pub(crate) fn mic_toggle(state: &mut App) {
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

pub(crate) fn mic_refresh_devices(state: &mut App) {
    state.input_devices = crabcore::audio::CpalEngine::list_input_devices();
}

pub(crate) fn mic_select_device(state: &mut App, name: String) {
    state.settings.mic.device = Some(name);
    state.save_settings();
    state.player.set_mic_config(state.settings.mic.clone());
    state.mic_note = "Input switched live - no restart needed".into();
}

pub(crate) fn cue_select_device(state: &mut App, name: String) {
    state.settings.cue.device = Some(name.clone());
    state.save_settings();
    state.player.set_cue_config(state.settings.cue.clone());
    state.cue_status = state.player.cue_state().label();
    tracing::info!("Cue device set to '{name}': {}", state.cue_status);
}

pub(crate) fn cue_volume_inc(state: &mut App) {
    let vol = (state.player.cue_volume() + 0.05).clamp(0.0, 1.5);
    state.settings.cue.volume = vol;
    state.save_settings();
    state.player.set_cue_volume(vol);
    state.cue_status = state.player.cue_state().label();
}

pub(crate) fn cue_volume_dec(state: &mut App) {
    let vol = (state.player.cue_volume() - 0.05).clamp(0.0, 1.5);
    state.settings.cue.volume = vol;
    state.save_settings();
    state.player.set_cue_volume(vol);
    state.cue_status = state.player.cue_state().label();
}

pub(crate) fn mic_level_inc(state: &mut App) {
    state.settings.mic.level = (state.settings.mic.level + 0.05).clamp(0.0, 1.5);
    state.save_settings();
    state.player.set_mic_config(state.settings.mic.clone());
}

pub(crate) fn mic_level_dec(state: &mut App) {
    state.settings.mic.level = (state.settings.mic.level - 0.05).clamp(0.0, 1.5);
    state.save_settings();
    state.player.set_mic_config(state.settings.mic.clone());
}

pub(crate) fn mic_duck_toggle(state: &mut App) {
    state.settings.mic.duck_enabled = !state.settings.mic.duck_enabled;
    state.save_settings();
    state.player.set_mic_config(state.settings.mic.clone());
}

pub(crate) fn mic_threshold_inc(state: &mut App) {
    state.settings.mic.duck_threshold_db =
        (state.settings.mic.duck_threshold_db + 3.0).clamp(-60.0, 0.0);
    state.save_settings();
    state.player.set_mic_config(state.settings.mic.clone());
}

pub(crate) fn mic_threshold_dec(state: &mut App) {
    state.settings.mic.duck_threshold_db =
        (state.settings.mic.duck_threshold_db - 3.0).clamp(-60.0, 0.0);
    state.save_settings();
    state.player.set_mic_config(state.settings.mic.clone());
}

pub(crate) fn mic_depth_inc(state: &mut App) {
    state.settings.mic.duck_depth_db = (state.settings.mic.duck_depth_db + 3.0).clamp(0.0, 24.0);
    state.save_settings();
    state.player.set_mic_config(state.settings.mic.clone());
}

pub(crate) fn mic_depth_dec(state: &mut App) {
    state.settings.mic.duck_depth_db = (state.settings.mic.duck_depth_db - 3.0).clamp(0.0, 24.0);
    state.save_settings();
    state.player.set_mic_config(state.settings.mic.clone());
}

pub(crate) fn mic_attack_inc(state: &mut App) {
    state.settings.mic.attack_ms = duck_ms_step(&ATTACK_LADDER, state.settings.mic.attack_ms, true);
    state.save_settings();
    state.player.set_mic_config(state.settings.mic.clone());
}

pub(crate) fn mic_attack_dec(state: &mut App) {
    state.settings.mic.attack_ms =
        duck_ms_step(&ATTACK_LADDER, state.settings.mic.attack_ms, false);
    state.save_settings();
    state.player.set_mic_config(state.settings.mic.clone());
}

pub(crate) fn mic_release_inc(state: &mut App) {
    state.settings.mic.release_ms =
        duck_ms_step(&RELEASE_LADDER, state.settings.mic.release_ms, true);
    state.save_settings();
    state.player.set_mic_config(state.settings.mic.clone());
}

pub(crate) fn mic_release_dec(state: &mut App) {
    state.settings.mic.release_ms =
        duck_ms_step(&RELEASE_LADDER, state.settings.mic.release_ms, false);
    state.save_settings();
    state.player.set_mic_config(state.settings.mic.clone());
}

pub(crate) fn station_name(state: &mut App, v: String) {
    // Stored raw like the stream host field (trimmed + defaulted on
    // load); the dashboard header mirrors it live.
    state.settings.station_name = v.clone();
    state.station_name = v;
    state.save_settings();
}

pub(crate) fn backup_now(state: &mut App) {
    let path = rfd::FileDialog::new()
        .set_title("Backup station data (JSON)")
        .set_file_name(format!(
            "crabboss-backup-{}.json",
            chrono::Local::now().format("%Y%m%d-%H%M%S")
        ))
        .add_filter("JSON", &["json"])
        .save_file();
    let Some(path) = path else {
        return;
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

pub(crate) fn restore_now(state: &mut App) {
    let path = rfd::FileDialog::new()
        .set_title("Restore station data (JSON)")
        .add_filter("JSON", &["json"])
        .pick_file();
    let Some(path) = path else {
        return;
    };
    // Parse the pick first so a garbage file fails without littering a
    // safety snapshot; the snapshot then runs before any mutation and
    // aborts the restore when it fails (fail closed: no safety net, no
    // swap — the transaction alone can't undo a wrong-file pick).
    let backup = match crate::backup::read_backup(&path) {
        Ok(b) => b,
        Err(e) => {
            tracing::error!("Restore failed: {e}");
            state.backup_status = format!("Restore failed: {e}");
            return;
        }
    };
    let safety =
        match crate::backup::write_pre_restore_safety(&state.data_dir, &state.build_backup()) {
            Ok(p) => p,
            Err(e) => {
                tracing::error!("Restore aborted: {e}");
                state.backup_status = format!("Restore aborted: {e}");
                return;
            }
        };
    match state.apply_backup(backup) {
        Ok(status) => {
            tracing::info!(
                "Restore from {}: {status} (pre-restore backup: {})",
                path.display(),
                safety.display()
            );
            state.backup_status = format!("{status} (pre-restore backup: {})", safety.display());
        }
        Err(e) => {
            tracing::warn!(
                "Restore failed: {e} (previous state kept, safety copy at {})",
                safety.display()
            );
            state.backup_status = format!("Restore failed: {e}");
        }
    }
}

/// Timer fire (called from `on_tick` after the pumps): poll the Icecast
/// `status-json.xsl` for this mount's listener count while live. The
/// fetch runs on a worker thread; any failure degrades to `None`
/// ("—" in the UI), never an error state.
pub(crate) fn maybe_poll_listeners(state: &mut App) {
    let elapsed = state.last_listeners_poll.map(|t| t.elapsed().as_secs());
    if !crate::rules::listeners_due(
        state.player.stream_state().is_live(),
        state.listeners_polling,
        elapsed,
        crabcore::stream::LISTENER_POLL_SECS,
    ) {
        return;
    }
    let cfg = state.settings.stream.clone();
    let (tx, rx) = mpsc::channel();
    state.listeners_rx = Some(rx);
    state.listeners_polling = true;
    state.last_listeners_poll = Some(std::time::Instant::now());
    if std::thread::Builder::new()
        .name("listener-poll".into())
        .spawn(move || {
            let _ = tx.send(crabcore::stream::fetch_listener_count(&cfg));
        })
        .is_err()
    {
        tracing::error!("Listener poll: failed to spawn worker thread");
        state.listeners_polling = false;
        state.listeners_rx = None;
    }
}

/// Reap a finished listener poll into `stream_listeners`.
pub(crate) fn pump_listeners(state: &mut App) {
    if !state.listeners_polling {
        return;
    }
    match state.listeners_rx.as_mut() {
        Some(rx) => match rx.try_recv() {
            Ok(n) => {
                state.stream_listeners = n;
                state.listeners_polling = false;
                state.listeners_rx = None;
            }
            Err(std::sync::mpsc::TryRecvError::Empty) => {}
            Err(std::sync::mpsc::TryRecvError::Disconnected) => {
                state.listeners_polling = false;
                state.listeners_rx = None;
            }
        },
        None => {
            state.listeners_polling = false;
        }
    }
}
