//! Periodic tick: progress tracking, Auto-DJ continuity, scheduler/ad
//! auto-fire, and the silence watchdog. Runs every 200 ms off the Iced
//! subscription; heavy work (loudness analysis, imports) stays on
//! background pumps so the tick itself never blocks.

use std::path::PathBuf;

use crabcore::audio::PlayerState;
use crabcore::library::TrackKind;

use super::App;
use crate::widgets::{strip_audio_extension, track_label};

impl App {
    // -- Periodic tick (progress, Auto-DJ, scheduler, silence) ---------------
    pub(crate) fn on_tick(&mut self) {
        self.tick_count += 1;
        self.pump_loudness();
        self.pump_import();
        self.pump_autosync();
        super::update::library::maybe_autosync(self);
        super::update::settings::pump_listeners(self);
        super::update::settings::maybe_poll_listeners(self);

        // Voice take bookkeeping: mirror recording state + elapsed,
        // auto-stop at the 10-minute budget with a truthful status.
        self.voice_recording = self.player.voice_recording();
        if self.voice_recording {
            self.voice_rec_elapsed = self.player.voice_record_secs();
            if self.voice_rec_elapsed >= crabcore::voice::VOICE_MAX_RECORD_SECS {
                super::update::voice::stop_take(self, true);
            }
        } else {
            self.voice_rec_elapsed = 0.0;
        }

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
            // A live voice take names the stream (take filename stems
            // are timestamps, not show titles).
            let label = match &self.voice_live {
                Some((vp, name)) if vp == &t.path => name.clone(),
                _ => label,
            };
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
                            // A music deck is never a voice take.
                            self.voice_live = None;
                        } else if let Some((vp, name)) =
                            self.voice_queued.clone().filter(|(vp, _)| vp == p)
                        {
                            // A queued voice take reached the air: label it
                            // (no `record_play` — takes stay out of music
                            // reports), then it behaves like program.
                            tracing::info!("Promoted queued voice take: {name}");
                            self.is_playing = true;
                            self.now_title = format!("🎙 {name}");
                            self.now_artist = "Voice track".into();
                            self.up_next.clear();
                            self.player.set_stream_title(&name);
                            self.voice_live = Some((vp, name));
                            self.voice_queued = None;
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

        // Cover art follows the installed track (cached handle per path;
        // the image decodes once per track, never per frame).
        if self.now_art_path != self.engine_track {
            self.now_art_path = self.engine_track.clone();
            self.now_art = self
                .player
                .current_artwork()
                .map(|a| iced::widget::image::Handle::from_bytes(a.data.clone()));
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
                                super::update::voice::clear_voice_labels(self);
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
