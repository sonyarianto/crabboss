//! Scheduler: master switch, event list ops, the add/edit dialog,
//! and firing (manual Run + auto-tick share `fire_scheduled_event`).

use super::super::App;
use crate::widgets::action_name;

pub(crate) fn master_toggled(state: &mut App, en: bool) {
    state.sched_enabled = en;
    tracing::info!("Scheduler master {}", if en { "ON" } else { "OFF" });
}

pub(crate) fn toggle_event(state: &mut App, i: usize) {
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

pub(crate) fn delete_event(state: &mut App, i: usize) {
    if let Some(e) = state.sched_events.get(i) {
        let id = e.id.clone();
        if let Err(e) = state.scheduler.delete(&id) {
            tracing::error!("Scheduler delete failed: {}", e);
        }
    }
    state.refresh_scheduler();
}

pub(crate) fn run_event(state: &mut App, i: usize) {
    state.fire_scheduled_event(i);
}

pub(crate) fn editor_new(state: &mut App) {
    state.sched_edit_idx = None;
    state.se_name.clear();
    state.se_time = "09:00".into();
    state.se_target.clear();
    state.se_expires.clear();
    state.se_days = [true; 7];
    state.sched_error.clear();
    state.sched_editor_open = true;
}

pub(crate) fn editor_edit(state: &mut App, i: usize) {
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

pub(crate) fn editor_close(state: &mut App) {
    state.sched_editor_open = false;
    state.sched_error.clear();
}

pub(crate) fn save(state: &mut App) {
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

impl App {
    pub(crate) fn refresh_scheduler(&mut self) {
        match self.scheduler.list_all() {
            Ok(events) => self.sched_events = events,
            Err(e) => {
                let msg = format!("Scheduler read failed: {e}");
                tracing::warn!("{msg}");
                self.sched_warnings = vec![msg];
                return;
            }
        }
        let today = chrono::Local::now().format("%Y-%m-%d").to_string();
        match self.scheduler.expiry_warnings(&today, 3) {
            Ok(warnings) => self.sched_warnings = warnings,
            Err(e) => {
                let msg = format!("Scheduler warnings failed: {e}");
                tracing::warn!("{msg}");
                self.sched_warnings = vec![msg];
            }
        }
        self.upcoming_count = self.sched_events.iter().filter(|e| e.enabled).count();
    }

    // -- Scheduler firing (shared by manual Run + auto-tick) ------------------
    /// Play one audio file target straight to program (the legacy
    /// `play` action and the `load` fallback for non-playlist targets).
    pub(crate) fn play_single_file(&mut self, target: &str) {
        use std::path::PathBuf;

        let path = PathBuf::from(target);
        if path.is_file() {
            let logged = self
                .library
                .find_by_path(target)
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
                    self.now_title = target.to_string();
                    self.now_artist = "Scheduler".into();
                    self.engine_track = Some(path);
                    self.up_next.clear();
                    self.pending_source = None;
                }
                Err(e) => tracing::error!("Scheduler play failed: {}", e),
            }
        } else {
            tracing::warn!("Scheduler target not found on disk: {target}");
            self.now_title = format!("Scheduled: {target} (file missing)");
        }
    }

    pub(crate) fn fire_scheduled_event(&mut self, idx: usize) {
        use std::path::PathBuf;

        use crabcore::library::TrackKind;

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
            "load" => {
                // A2: a saved playlist name first (the Home list shows
                // exactly these) — fired in stored order via the shared
                // engine. Legacy single-file targets fall through to the
                // `play` path below, so existing events don't break.
                if let Some(id) =
                    super::generator::find_playlist_id(&self.playlist_manager, &event.target)
                {
                    match super::generator::fire_playlist_to_air(self, &id) {
                        Ok(f) => {
                            self.now_title = format!("Scheduled '{}': {}", event.name, f.detail());
                            self.now_artist = "Scheduler".into();
                            self.pending_source = Some("Scheduler".into());
                        }
                        Err(msg) => {
                            self.now_title = format!("Scheduled '{}': {msg}", event.name);
                        }
                    }
                } else {
                    self.play_single_file(&event.target);
                }
            }
            "play" => {
                let target = event.target.clone();
                self.play_single_file(&target);
            }
            other => {
                tracing::info!("Scheduler command '{}' (no-op in MVP)", other);
            }
        }
    }
}
