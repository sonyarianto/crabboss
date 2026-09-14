//! Dashboard: station status, counts, and quick actions.

use iced::{
    widget::{button, column, container, row, scrollable, text},
    Element, Length,
};

use crate::app::update::generator::DAYPARTS;
use crate::app::{App, Message, Screen};
use crate::widgets::{on_air_label, stepper};

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
    // Rotation generator: one row per daypart plus a Generate-all.
    // Engine rules are fixed per session knobs (hour + track count);
    // each fire persists a timestamped playlist.
    let mut gen = column![text("Rotation Generator").size(14)].spacing(4);
    for (i, daypart) in DAYPARTS.iter().enumerate() {
        let hour = state.gen_hours.get(i).copied().unwrap_or(daypart.hour);
        let count = state.gen_counts.get(i).copied().unwrap_or(15);
        gen = gen.push(
            row![
                text(daypart.name).size(12).width(Length::Fixed(70.0)),
                stepper(
                    format!("{hour:02}:00"),
                    Message::GenHourDec(i),
                    Message::GenHourInc(i),
                ),
                stepper(
                    format!("{count} tracks"),
                    Message::GenCountDec(i),
                    Message::GenCountInc(i),
                ),
                button(text("Generate").size(12)).on_press(Message::GenFire(i)),
            ]
            .spacing(8)
            .align_y(iced::Alignment::Center),
        );
    }
    gen = gen.push(
        row![
            button(text("Generate all dayparts").size(12)).on_press(Message::GenFireAll),
            text(&state.gen_status).size(11),
        ]
        .spacing(8)
        .align_y(iced::Alignment::Center),
    );
    scrollable(
        column![
            text(&state.station_name).size(20),
            text(status).size(12),
            stats,
            text("Quick Actions").size(14),
            actions,
            gen,
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
