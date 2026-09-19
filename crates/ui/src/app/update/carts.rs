//! Cart wall: instant-fire pads plus the assign flow.

use std::path::PathBuf;

use crabcore::library::TrackKind;

use super::super::App;
use crate::widgets::track_label;

pub(crate) fn play(state: &mut App, i: usize) {
    state.play_cart(i);
}

pub(crate) fn hotkey(state: &mut App, i: usize) {
    // Scoped like the old FocusScope hotkeys: only on the Carts
    // screen, and never while an editor dialog is open (typing
    // "1".."8" into a field must not fire pads).
    use super::super::Screen;
    if state.screen != Screen::Carts || state.sched_editor_open || state.ads_editor_open {
        return;
    }
    state.play_cart(i);
}

pub(crate) fn delete(state: &mut App, i: usize) {
    if let Some(c) = state.cart_list.get(i) {
        let id = c.id.clone();
        if let Err(e) = state.carts.delete(&id) {
            tracing::error!("Cart delete failed: {}", e);
        }
    }
    state.refresh_carts();
}

pub(crate) fn add(state: &mut App) {
    let existing: Vec<String> = state
        .cart_list
        .iter()
        .map(|c| c.file_path.clone())
        .collect();
    if existing.len() >= 8 {
        state.cart_status = "Cart wall is full (8)".into();
        return;
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

pub(crate) fn place(state: &mut App, slot: usize) {
    let track = state
        .lib_selected
        .and_then(|i| state.lib_tracks.get(i).cloned());
    let Some(track) = track else {
        state.cart_status = "No library track armed - tap one first".into();
        return;
    };
    let label = track_label(&track);
    if let Err(e) = state.carts.assign_at(slot as i32, &label, &track.file_path) {
        tracing::error!("Cart place failed: {}", e);
        state.cart_status = format!("Place failed: {e}");
        return;
    }
    state.cart_assign = false;
    state.cart_status = format!("'{}' -> pad {}", label, slot + 1);
    state.refresh_carts();
}

pub(crate) fn toggle_assign(state: &mut App) {
    state.cart_assign = !state.cart_assign;
    state.cart_status = if state.cart_assign {
        "Assign: select a track in Media, then tap a pad".into()
    } else {
        String::new()
    };
}

impl App {
    pub(crate) fn refresh_carts(&mut self) {
        match self.carts.list_all() {
            Ok(list) => self.cart_list = list,
            Err(e) => {
                let msg = format!("Carts read failed: {e}");
                tracing::warn!("{msg}");
                self.cart_status = msg;
            }
        }
    }

    pub(crate) fn play_cart(&mut self, i: usize) {
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
                super::voice::clear_voice_labels(self);
            }
            Err(e) => tracing::error!("Cart play failed: {}", e),
        }
    }
}
