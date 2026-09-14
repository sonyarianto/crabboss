//! Cart wall: instant-fire pads with progress, assign flow, and hotkeys.

use std::collections::HashMap;
use std::path::PathBuf;

use iced::{
    widget::{button, column, progress_bar, row, scrollable, text},
    Element, Length,
};

use crabcore::library::TrackKind;

use crate::app::{App, Message};
use crate::widgets::{kind_label, short_name};

pub(crate) fn view(state: &App) -> Element<'_, Message> {
    let kinds: HashMap<&str, TrackKind> = state
        .lib_tracks
        .iter()
        .map(|t| (t.file_path.as_str(), t.kind))
        .collect();
    let live_path = state
        .player
        .current_track()
        .map(|t| t.path.to_string_lossy().to_string());
    let mut grid = column![].spacing(6);
    for (i, c) in state.cart_list.iter().enumerate() {
        let kind = kinds
            .get(c.file_path.as_str())
            .map(|k| kind_label(*k))
            .unwrap_or("Music");
        let exists = PathBuf::from(&c.file_path).is_file();
        let playing = live_path.as_deref() == Some(c.file_path.as_str()) && state.is_playing;
        let (pos, frac) = if playing {
            let p = state.player.position_secs();
            let d = state
                .player
                .current_track()
                .and_then(|t| t.duration_secs)
                .unwrap_or(1.0)
                .max(0.01);
            (p, (p / d).clamp(0.0, 1.0) as f32)
        } else {
            (0.0, 0.0)
        };
        let _ = pos;
        grid = grid.push(
            column![
                row![
                    text(format!(
                        "Pad {}: {} [{}] {}{}",
                        i + 1,
                        c.label,
                        kind,
                        short_name(&c.file_path),
                        if exists { "" } else { " (missing)" }
                    ))
                    .size(12)
                    .width(Length::Fill),
                    button(text("Play").size(11)).on_press(Message::CartPlay(i)),
                    button(text("Del").size(11)).on_press(Message::CartDelete(i)),
                ]
                .spacing(6),
                progress_bar(0.0..=1.0, frac),
                row![button(text("Place here").size(11)).on_press(Message::CartPlace(i)),]
                    .spacing(6),
            ]
            .spacing(2),
        );
    }
    column![
        row![
            text("Cart Wall").size(16),
            iced::widget::space::horizontal(),
            button(text(if state.cart_assign {
                "Assign: ON"
            } else {
                "Assign"
            }))
            .on_press(Message::CartToggleAssign),
            button(text("+ Add").size(12)).on_press(Message::CartAdd),
        ]
        .spacing(8),
        text(&state.cart_status).size(11),
        text("Tip: select a track in Media, enable Assign, then Place on a pad.").size(11),
        scrollable(grid).height(Length::Fill),
    ]
    .spacing(8)
    .padding(12)
    .into()
}
