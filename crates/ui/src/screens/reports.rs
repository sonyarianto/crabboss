//! Play-log reports: ranged lists plus CSV export.

use iced::{
    widget::{button, column, row, scrollable, text},
    Element, Length,
};

use crate::app::{App, Message};
use crate::widgets::join_title_artist;

pub(crate) fn view(state: &App) -> Element<'_, Message> {
    const RANGES: [&str; 4] = ["Today", "Last 7 days", "Last 30 days", "All time"];
    let mut range_row = row![text("Range:").size(12)].spacing(6);
    for (i, name) in RANGES.iter().enumerate() {
        let label = if i == state.report_range {
            format!("[{}]", name)
        } else {
            name.to_string()
        };
        range_row =
            range_row.push(button(text(label).size(12)).on_press(Message::ReportRangeChanged(i)));
    }
    range_row = range_row.push(iced::widget::space::horizontal());
    range_row = range_row.push(button(text("Export CSV").size(12)).on_press(Message::ReportExport));
    let mut list = column![].spacing(2);
    for e in &state.report_entries {
        list = list.push(
            text(format!(
                "{} | {} [{}]",
                e.played_at.format("%d/%m %H:%M"),
                join_title_artist(&e.title, &e.artist),
                e.kind.as_str()
            ))
            .size(12),
        );
    }
    column![
        text("Reports").size(16),
        range_row,
        text(&state.report_summary).size(11),
        scrollable(list).height(Length::Fill),
    ]
    .spacing(8)
    .padding(12)
    .into()
}
