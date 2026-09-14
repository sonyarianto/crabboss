//! Library: searchable track table (shared by the Library screen at full
//! management power and the Playout desk in lean mode).

use std::path::PathBuf;

use iced::{
    widget::{button, checkbox, column, container, row, scrollable, text, text_input},
    Element, Length, Theme,
};

use crabcore::library::TrackKind;

use crate::app::{App, Message};
use crate::widgets::{fmt_dur, kind_label, stepper, track_label};

pub(crate) fn panel(state: &App, tools: bool) -> Element<'_, Message> {
    let shown = state.lib_tracks.len();
    let count_label = if shown == state.lib_total {
        format!(
            "{} track{}",
            state.lib_total,
            if state.lib_total == 1 { "" } else { "s" }
        )
    } else {
        format!("{shown} of {} tracks", state.lib_total)
    };
    // Management tools live on the Library screen only; the Playout desk
    // keeps a lean list (Library = ngurus, Playout = nge-live).
    let mut header = row![
        text(format!("Library - {}", count_label)).size(14),
        iced::widget::space::horizontal(),
    ]
    .spacing(6);
    if tools {
        header = header
            .push(button(text("Health").size(12)).on_press(Message::HealthCheck))
            .push(button(text("Loudness").size(12)).on_press(Message::LoudnessScan))
            .push(button(text("Import").size(12)).on_press(Message::ImportFiles));
    }

    let search = text_input("Search tracks...", &state.lib_search)
        .on_input(Message::LibrarySearchChanged)
        .padding(8);

    // Kind filter + missing-only: narrows the same list, no new queries.
    let mut filters = row![text("Kind:").size(12)].spacing(6);
    for (label, kind) in [
        ("All", None),
        ("Music", Some(TrackKind::Music)),
        ("Jingles", Some(TrackKind::Jingle)),
        ("Ads", Some(TrackKind::Ad)),
    ] {
        let entry = button(text(label).size(12)).on_press(Message::LibraryKindChanged(kind));
        filters = filters.push(if state.lib_kind == kind {
            entry.style(iced::widget::button::primary)
        } else {
            entry
        });
    }
    filters = filters.push(
        checkbox(state.lib_missing_only)
            .label("Missing only")
            .on_toggle(Message::LibraryMissingToggled),
    );
    filters = filters.push(
        checkbox(state.lib_dupes_only)
            .label("Duplicates only")
            .on_toggle(Message::LibraryDupesToggled),
    );

    // Fixed column widths shared by the header and every row, so the list
    // reads as a table: only Title/Artist flex, everything else lines up.
    const PLAY_W: f32 = 64.0;
    const KIND_W: f32 = 70.0;
    const DUR_W: f32 = 68.0;
    const GAIN_W: f32 = 72.0;

    let mut list = column![
        row![
            text("").width(PLAY_W),
            text("Kind").width(KIND_W).size(11),
            text("Title").width(Length::Fill).size(11),
            text("Artist").width(Length::Fill).size(11),
            text("Duration")
                .width(DUR_W)
                .size(11)
                .align_x(iced::alignment::Horizontal::Right),
            text("Gain")
                .width(GAIN_W)
                .size(11)
                .align_x(iced::alignment::Horizontal::Right),
        ]
        .spacing(6),
        iced::widget::rule::horizontal(1),
    ]
    .spacing(4)
    // Keep text clear of the floating scrollbar, which would otherwise
    // cover the last pixels of the Gain column.
    .padding(iced::padding::right(14));
    if state.lib_tracks.is_empty() {
        list = list.push(text("Import audio files to get started").size(12));
    } else {
        for (i, t) in state.lib_tracks.iter().take(500).enumerate() {
            let missing = !PathBuf::from(&t.file_path).is_file();
            let base = track_label(t);
            let title = if missing { format!("! {}", base) } else { base };
            let selected = Some(i) == state.lib_selected;
            let artist = t.artist.clone().unwrap_or_default();
            let dur = fmt_dur(t.duration_secs);
            let gain = t
                .loudness_gain_db
                .map(|g| format!("{g:+.1} dB"))
                .unwrap_or_default();
            let cells: iced::widget::Row<'_, Message> = row![
                button(
                    text("Play")
                        .size(11)
                        .width(Length::Fill)
                        .align_x(iced::alignment::Horizontal::Center)
                )
                .width(PLAY_W)
                .on_press(Message::LibraryTrackPlay(i)),
                text(kind_label(t.kind)).size(12).width(KIND_W),
                button(text(title).size(12))
                    .style(iced::widget::button::text)
                    .padding(0)
                    .width(Length::Fill)
                    .on_press(Message::LibraryTrackSelected(i)),
                text(artist).size(12).width(Length::Fill),
                text(dur)
                    .size(12)
                    .width(DUR_W)
                    .align_x(iced::alignment::Horizontal::Right),
                text(gain)
                    .size(12)
                    .width(GAIN_W)
                    .align_x(iced::alignment::Horizontal::Right),
            ]
            .spacing(6)
            .align_y(iced::Alignment::Center);
            // Selected row: subtle theme-accent wash instead of a ">"
            // text prefix.
            let entry: Element<'_, Message> = if selected {
                container(cells)
                    .width(Length::Fill)
                    .style(|theme: &Theme| {
                        let mut bg = theme.palette().primary;
                        bg.a = 0.22;
                        iced::widget::container::Style::default().background(bg)
                    })
                    .into()
            } else {
                cells.into()
            };
            list = list.push(entry);
        }
        if shown > 500 {
            list = list.push(text(format!("... showing 500 of {shown} (refine search)")).size(11));
        }
    }

    // Auto-sync (Library screen only): watch folders + timer. New files
    // queue through the normal import pump with progress; nothing here
    // ever deletes anything.
    let mut body = column![header, search, filters];
    if tools {
        let sync_row = row![
            checkbox(state.settings.auto_sync_enabled)
                .label("Auto-sync")
                .on_toggle(Message::AutoSyncToggled),
            stepper(
                format!("Every {} min", state.settings.auto_sync_interval_mins),
                Message::AutoSyncIntervalDec,
                Message::AutoSyncIntervalInc,
            ),
            button(text("Watch folder").size(12)).on_press(Message::WatchFolderAdd),
        ]
        .spacing(6)
        .align_y(iced::Alignment::Center);
        body = body.push(sync_row);
        for (i, folder) in state.settings.watch_folders.iter().enumerate() {
            body = body.push(
                row![
                    text(folder.to_string_lossy()).size(11),
                    iced::widget::space::horizontal(),
                    button(text("Remove").size(11)).on_press(Message::WatchFolderRemove(i)),
                ]
                .spacing(6)
                .align_y(iced::Alignment::Center),
            );
        }
    }

    body = body.push(text(&state.lib_status).size(11));
    body = body.push(scrollable(list).height(Length::Fill));

    body.spacing(6).padding(8).into()
}

pub(crate) fn page(state: &App) -> Element<'_, Message> {
    panel(state, true)
}
