//! Dashboard: station status, counts, and quick actions.

use iced::{
    widget::{button, column, container, row, scrollable, text},
    Element, Length,
};

use crate::app::{App, Message, Screen};
use crate::widgets::on_air_label;

pub(crate) fn view(state: &App) -> Element<'_, Message> {
    let status = if state.is_playing {
        on_air_label(&state.now_title, &state.now_artist)
    } else {
        "Off air".to_string()
    };
    let stats = row![
        container(column![
            text(format!("{}", state.track_count)).size(22),
            text("Tracks").size(11),
        ])
        .padding(12)
        .width(Length::Fill),
        container(column![
            text(format!("{}", state.playlist_count)).size(22),
            text("Playlists").size(11),
        ])
        .padding(12)
        .width(Length::Fill),
        container(column![
            text(format!("{}", state.upcoming_count)).size(22),
            text("Scheduled").size(11),
        ])
        .padding(12)
        .width(Length::Fill),
    ]
    .spacing(12);
    let actions = row![
        button(text("Playout").size(14)).on_press(Message::Navigate(Screen::Playout)),
        button(text("Library").size(14)).on_press(Message::Navigate(Screen::Media)),
        button(text("Scheduler").size(14)).on_press(Message::Navigate(Screen::Scheduler)),
        button(text("Cart Wall").size(14)).on_press(Message::Navigate(Screen::Carts)),
    ]
    .spacing(12);
    scrollable(
        column![
            text(&state.station_name).size(20),
            text(status).size(12),
            stats,
            text("Quick Actions").size(14),
            actions,
            text(format!(
                "Engine: {} | Device: {}",
                state.audio_engine,
                state.player.device_name()
            ))
            .size(11),
            text(format!("License: {}", state.license_status)).size(11),
        ]
        .spacing(12)
        .padding(16),
    )
    .into()
}
