//! Ad blocks: dated intro/spot/outro chains with a full editor.

use iced::{
    widget::{button, checkbox, column, row, scrollable, text, text_input},
    Element, Length,
};

use crate::app::{App, Message};
use crate::widgets::short_name;

pub(crate) fn view(state: &App) -> Element<'_, Message> {
    let mut list = column![].spacing(4);
    for (i, b) in state.ad_blocks.iter().enumerate() {
        list = list.push(
            column![
                text(format!(
                    "{} | {} | {} -> {} | {} {}",
                    b.name,
                    b.play_time,
                    b.start_date,
                    b.end_date,
                    short_name(&b.spot_path),
                    if b.enabled { "[on]" } else { "[off]" }
                ))
                .size(12),
                text(format!("days: {}", b.days)).size(11),
                row![
                    button(text(if b.enabled { "Disable" } else { "Enable" }).size(11))
                        .on_press(Message::AdsToggle(i)),
                    button(text("Run").size(11)).on_press(Message::AdsRun(i)),
                    button(text("Edit").size(11)).on_press(Message::AdsEdit(i)),
                    button(text("Del").size(11)).on_press(Message::AdsDelete(i)),
                ]
                .spacing(6),
            ]
            .spacing(2),
        );
    }
    let mut col = column![row![
        text("Ads").size(16),
        iced::widget::space::horizontal(),
        button(text("+ New block").size(12)).on_press(Message::AdsNew),
    ]
    .spacing(8),]
    .spacing(8)
    .padding(12);
    // List-level errors (e.g. corrupt rows) surface here; the editor
    // shows `ads_error` inside itself while open, so don't double up.
    if !state.ads_editor_open && !state.ads_error.is_empty() {
        col = col.push(text(&state.ads_error).size(11));
    }
    col = col.push(scrollable(list).height(Length::Shrink));
    if state.ads_editor_open {
        let mut day_row = row![].spacing(8);
        day_row = day_row.push(
            checkbox(state.ab_days[0])
                .label("Mon")
                .on_toggle(|b| Message::AdDayChanged(0, b)),
        );
        day_row = day_row.push(
            checkbox(state.ab_days[1])
                .label("Tue")
                .on_toggle(|b| Message::AdDayChanged(1, b)),
        );
        day_row = day_row.push(
            checkbox(state.ab_days[2])
                .label("Wed")
                .on_toggle(|b| Message::AdDayChanged(2, b)),
        );
        day_row = day_row.push(
            checkbox(state.ab_days[3])
                .label("Thu")
                .on_toggle(|b| Message::AdDayChanged(3, b)),
        );
        day_row = day_row.push(
            checkbox(state.ab_days[4])
                .label("Fri")
                .on_toggle(|b| Message::AdDayChanged(4, b)),
        );
        day_row = day_row.push(
            checkbox(state.ab_days[5])
                .label("Sat")
                .on_toggle(|b| Message::AdDayChanged(5, b)),
        );
        day_row = day_row.push(
            checkbox(state.ab_days[6])
                .label("Sun")
                .on_toggle(|b| Message::AdDayChanged(6, b)),
        );
        col = col.push(
            column![
                text("Ad block").size(14),
                text_input("Name", &state.ab_name)
                    .on_input(Message::AdName)
                    .padding(6),
                text_input("Spot path (audio file)", &state.ab_spot)
                    .on_input(Message::AdSpot)
                    .padding(6),
                text_input("Intro path (optional)", &state.ab_intro)
                    .on_input(Message::AdIntro)
                    .padding(6),
                text_input("Outro path (optional)", &state.ab_outro)
                    .on_input(Message::AdOutro)
                    .padding(6),
                row![
                    text_input("Start YYYY-MM-DD", &state.ab_start)
                        .on_input(Message::AdStart)
                        .padding(6),
                    text_input("End YYYY-MM-DD", &state.ab_end)
                        .on_input(Message::AdEnd)
                        .padding(6),
                    text_input("HH:MM", &state.ab_time)
                        .on_input(Message::AdTime)
                        .padding(6),
                ]
                .spacing(6),
                day_row,
                text(&state.ads_error).size(11),
                row![
                    button(text("Save").size(12)).on_press(Message::AdsSave),
                    button(text("Cancel").size(12)).on_press(Message::AdsEditorClose),
                ]
                .spacing(8),
            ]
            .spacing(6),
        );
    }
    col.into()
}
