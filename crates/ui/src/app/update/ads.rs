//! Ad blocks: dated intro/spot/outro chains, the block editor, and
//! firing (manual Run + auto-tick share `fire_ad_block`).

use std::path::PathBuf;

use super::super::App;

pub(crate) fn toggle(state: &mut App, i: usize) {
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

pub(crate) fn delete(state: &mut App, i: usize) {
    if let Some(b) = state.ad_blocks.get(i) {
        let id = b.id.clone();
        if let Err(e) = state.ads.delete(&id) {
            tracing::error!("Ad delete failed: {}", e);
        }
    }
    state.refresh_ads();
}

pub(crate) fn run(state: &mut App, i: usize) {
    state.fire_ad_block(i);
}

pub(crate) fn editor_new(state: &mut App) {
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

pub(crate) fn editor_edit(state: &mut App, i: usize) {
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

pub(crate) fn editor_close(state: &mut App) {
    state.ads_editor_open = false;
    state.ads_error.clear();
}

pub(crate) fn save(state: &mut App) {
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

impl App {
    pub(crate) fn refresh_ads(&mut self) {
        match self.ads.list_all() {
            Ok(blocks) => self.ad_blocks = blocks,
            Err(e) => {
                let msg = format!("Ads read failed: {e}");
                tracing::warn!("{msg}");
                self.ads_error = msg;
            }
        }
    }

    pub(crate) fn fire_ad_block(&mut self, idx: usize) {
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
        super::voice::clear_voice_labels(self);
    }
}
