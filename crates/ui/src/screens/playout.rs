//! Playout desk: broadcast strip on top, Coming-Up forecast plus the lean
//! library list below.

use iced::{
    widget::{button, checkbox, column, container, progress_bar, row, slider, text},
    Element, Length,
};

use crate::app::{App, Message};
use crate::widgets::{fmt_dur, join_title_artist, track_label, track_source_label};

use super::library;

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

pub(crate) fn view(state: &App) -> Element<'_, Message> {
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
        container(library::panel(state, false))
            .width(Length::Fill)
            .height(Length::Fill),
    ]
    .spacing(8)
    .padding(8)
    .into()
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
