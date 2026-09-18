//! Playout desk: broadcast strip on top, Coming-Up forecast plus the lean
//! library list below.

use iced::{
    widget::{button, checkbox, column, container, progress_bar, row, slider, text},
    Element, Length, Theme,
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
    // Fixed cover box (layout never jumps when art appears). Untagged
    // audio gets a visibly intentional bordered placeholder — an
    // invisible reserved box reads as a broken indent (the whole strip
    // sits ~84px right of its neighbors).
    let cover: Element<'_, Message> = match &state.now_art {
        Some(handle) => container(
            iced::widget::Image::new(handle.clone())
                .width(Length::Fixed(64.0))
                .height(Length::Fixed(64.0)),
        )
        .width(Length::Fixed(64.0))
        .height(Length::Fixed(64.0))
        .into(),
        // NOTE: no `center_y(Fill)` here — it overrides the fixed
        // height and stretches the box down the whole strip. The two
        // fill spacers do the vertical centering instead.
        None => container(
            column![
                iced::widget::space::vertical(),
                text("No cover")
                    .size(10)
                    .width(Length::Fill)
                    .align_x(iced::alignment::Horizontal::Center),
                iced::widget::space::vertical(),
            ]
            .width(Length::Fill)
            .height(Length::Fill),
        )
        .width(Length::Fixed(64.0))
        .height(Length::Fixed(64.0))
        .style(|theme: &Theme| {
            iced::widget::container::Style::default().border(iced::Border {
                color: theme.palette().text.scale_alpha(0.3),
                width: 1.0,
                radius: 4.0.into(),
            })
        })
        .into(),
    };
    // Horizontal broadcast strip: cover, then track line, transport +
    // progress + time, then Auto-DJ + up-next + monitor volume.
    row![
        cover,
        column![
            text(track_source_label(&state.now_title, &state.now_artist)).size(14),
            row![
                button(text("Prev").size(13)).on_press(Message::Prev),
                button(text(play_label).size(13)).on_press(play_msg),
                button(text("Stop").size(13)).on_press(Message::Stop),
                button(text("Next").size(13)).on_press(Message::Next),
                // The one explicit on-air gate: fires the selected list
                // track to program now (list buttons are cue-only).
                button(text("On Air").size(13))
                    .style(iced::widget::button::primary)
                    .on_press(Message::PlaySelected),
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
        .width(Length::Fill),
    ]
    .spacing(8)
    .align_y(iced::Alignment::Center)
    .padding(12)
    .into()
}
