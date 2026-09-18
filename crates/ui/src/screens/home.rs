//! Dashboard: station status, counts, and quick actions.

use iced::{
    widget::{button, column, container, row, scrollable, text, text_input},
    Element, Length,
};

use crate::app::update::generator::DAYPARTS;
use crate::app::{App, Message, Screen};
use crate::widgets::{on_air_label, stepper, track_label};

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
    // Saved playlists (A1 fire-to-air + B manual builder with reorder).
    let mut pls = column![text("Saved Playlists").size(14)].spacing(4);
    pls = pls.push(
        row![
            text_input("New playlist name...", &state.playlist_new_name)
                .on_input(Message::PlaylistNewName)
                .padding(6)
                .width(Length::Fill),
            button(text("Create").size(12)).on_press(Message::PlaylistCreate),
            button(text("Import .m3u").size(12)).on_press(Message::PlaylistImport),
        ]
        .spacing(8)
        .align_y(iced::Alignment::Center),
    );
    if state.saved_playlists.is_empty() {
        pls = pls.push(text("Generate a rotation above or create one manually").size(11));
    } else {
        for p in &state.saved_playlists {
            let id = p.id.clone();
            let open = state.playlist_selected.as_deref() == Some(id.as_str());
            pls = pls.push(
                row![
                    text(&p.name).size(12).width(Length::Fill),
                    text(format!("{} tracks", p.tracks)).size(11),
                    button(text(if open { "Close" } else { "Edit" }).size(12))
                        .on_press(Message::PlaylistSelect(id.clone())),
                    button(text("Queue to Air").size(12))
                        .on_press(Message::PlaylistToAir(id.clone())),
                    button(text("Delete").size(12))
                        .style(iced::widget::button::danger)
                        .on_press(Message::PlaylistDelete(id.clone())),
                ]
                .spacing(8)
                .align_y(iced::Alignment::Center),
            );
            if open {
                pls = pls.push(detail_view(state, &id));
            }
        }
    }
    scrollable(
        column![
            text(&state.station_name).size(20),
            text(status).size(12),
            stats,
            text("Quick Actions").size(14),
            actions,
            gen,
            pls,
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

/// Expanded manual-builder detail for one playlist (B): stored-order
/// rows with Up/Down/Remove plus the Library add gate. Missing files
/// stay listed with a flag — the fire path skips them with a count.
fn detail_view<'a>(state: &'a App, playlist_id: &str) -> Element<'a, Message> {
    let mut col = column![].spacing(2).padding(iced::padding::left(12));
    // P0 rename: the core `rename` had no UI. Prefilled on expand
    // (`playlist_select`); blank saves are rejected with a status line.
    col = col.push(
        row![
            text_input("Rename playlist...", &state.playlist_rename)
                .on_input(Message::PlaylistRenameInput)
                .padding(6)
                .width(Length::Fill),
            button(text("Rename").size(11))
                .on_press(Message::PlaylistRename(playlist_id.to_string())),
            button(text("Export .m3u").size(11))
                .on_press(Message::PlaylistExport(playlist_id.to_string())),
        ]
        .spacing(6)
        .align_y(iced::Alignment::Center),
    );
    if state.playlist_detail.is_empty() {
        col = col.push(text("Empty — pick a track in Library, then Add here").size(11));
    } else {
        let n = state.playlist_detail.len();
        for (idx, item) in state.playlist_detail.iter().enumerate() {
            let title = if item.missing {
                format!("{}. {} (! missing)", idx + 1, item.label)
            } else {
                format!("{}. {}", idx + 1, item.label)
            };
            let up: Element<'_, Message> = if idx == 0 {
                button(text("↑").size(11)).into()
            } else {
                button(text("↑").size(11))
                    .on_press(Message::PlaylistMoveUp(playlist_id.to_string(), idx))
                    .into()
            };
            let down: Element<'_, Message> = if idx + 1 >= n {
                button(text("↓").size(11)).into()
            } else {
                button(text("↓").size(11))
                    .on_press(Message::PlaylistMoveDown(playlist_id.to_string(), idx))
                    .into()
            };
            col = col.push(
                row![
                    text(title).size(11).width(Length::Fill),
                    up,
                    down,
                    button(text("Remove").size(11))
                        .style(iced::widget::button::danger)
                        .on_press(Message::PlaylistRemoveItem(playlist_id.to_string(), idx)),
                ]
                .spacing(6)
                .align_y(iced::Alignment::Center),
            );
        }
        if state.playlist_detail_missing > 0 {
            col = col.push(
                text(format!(
                    "{} missing — fire skips them",
                    state.playlist_detail_missing
                ))
                .size(11),
            );
        }
    }
    // Add gate: the Library selection lives on another screen, so the
    // button phrases what would be added (or what to do first).
    match state.lib_selected.and_then(|i| state.lib_tracks.get(i)) {
        Some(t) => {
            col = col.push(
                row![
                    text(format!("Selected: {}", track_label(t))).size(11),
                    button(text("Add here").size(11))
                        .on_press(Message::PlaylistAddSelected(playlist_id.to_string())),
                ]
                .spacing(8)
                .align_y(iced::Alignment::Center),
            );
        }
        None => {
            col = col.push(text("Pick a track in Library to add").size(11));
        }
    }
    col.into()
}
