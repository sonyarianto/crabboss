//! Scheduler: timed events with expiry badges, warnings, and editor.

use iced::{
    widget::{button, checkbox, column, row, scrollable, text, text_input},
    Element, Length,
};

use crate::app::{App, Message};
use crate::widgets::action_label;

pub(crate) fn view(state: &App) -> Element<'_, Message> {
    let mut list = column![].spacing(4);
    for (i, e) in state.sched_events.iter().enumerate() {
        let today = chrono::Local::now().format("%Y-%m-%d").to_string();
        let badge = match e.expiry_status(&today) {
            crabcore::scheduler::ExpiryStatus::Expired => "expired",
            crabcore::scheduler::ExpiryStatus::ExpiresToday => "last day",
            crabcore::scheduler::ExpiryStatus::Active(n) if n <= 7 => "expiring",
            _ => "",
        };
        list = list.push(
            column![
                text(format!(
                    "{} | {} | {} -> {} | {} {} {}",
                    e.name,
                    e.start_time,
                    e.action_type,
                    e.target,
                    e.days,
                    if e.enabled { "[on]" } else { "[off]" },
                    badge
                ))
                .size(12),
                row![
                    button(text(if e.enabled { "Disable" } else { "Enable" }).size(11))
                        .on_press(Message::SchedulerToggleEvent(i)),
                    button(text("Run").size(11)).on_press(Message::SchedulerRunEvent(i)),
                    button(text("Edit").size(11)).on_press(Message::SchedulerEdit(i)),
                    button(text("Del").size(11)).on_press(Message::SchedulerDeleteEvent(i)),
                ]
                .spacing(6),
            ]
            .spacing(2),
        );
    }
    let mut col = column![row![
        text("Scheduler").size(16),
        iced::widget::space::horizontal(),
        checkbox(state.sched_enabled)
            .label("Enabled")
            .on_toggle(Message::SchedulerMasterToggled),
        button(text("+ New").size(12)).on_press(Message::SchedulerNew),
    ]
    .spacing(8),]
    .spacing(8)
    .padding(12);
    if !state.sched_warnings.is_empty() {
        col = col.push(text(state.sched_warnings.join(" | ")).size(11));
    }
    col = col.push(scrollable(list).height(Length::Fill));
    if state.sched_editor_open {
        let day_names: [Element<'_, Message>; 7] = [
            checkbox(state.se_days[0])
                .label("Mon")
                .on_toggle(|b| Message::SchedDayChanged(0, b))
                .into(),
            checkbox(state.se_days[1])
                .label("Tue")
                .on_toggle(|b| Message::SchedDayChanged(1, b))
                .into(),
            checkbox(state.se_days[2])
                .label("Wed")
                .on_toggle(|b| Message::SchedDayChanged(2, b))
                .into(),
            checkbox(state.se_days[3])
                .label("Thu")
                .on_toggle(|b| Message::SchedDayChanged(3, b))
                .into(),
            checkbox(state.se_days[4])
                .label("Fri")
                .on_toggle(|b| Message::SchedDayChanged(4, b))
                .into(),
            checkbox(state.se_days[5])
                .label("Sat")
                .on_toggle(|b| Message::SchedDayChanged(5, b))
                .into(),
            checkbox(state.se_days[6])
                .label("Sun")
                .on_toggle(|b| Message::SchedDayChanged(6, b))
                .into(),
        ];
        let mut day_row = row![].spacing(8);
        for d in day_names {
            day_row = day_row.push(d);
        }
        col = col.push(
            column![
                text(if state.sched_edit_idx.is_none() {
                    "New event"
                } else {
                    "Edit event"
                })
                .size(14),
                text_input("Name", &state.se_name)
                    .on_input(Message::SchedName)
                    .padding(6),
                row![
                    text_input("HH:MM", &state.se_time)
                        .on_input(Message::SchedTime)
                        .padding(6),
                    button(text("<").size(12)).on_press(Message::SchedActionPrev),
                    text(format!("action: {}", action_label(state.se_action))).size(12),
                    button(text(">").size(12)).on_press(Message::SchedActionNext),
                ]
                .spacing(6),
                text_input("Target (file / playlist / preset)", &state.se_target)
                    .on_input(Message::SchedTarget)
                    .padding(6),
                text_input(
                    "Valid until YYYY-MM-DD (empty = forever)",
                    &state.se_expires
                )
                .on_input(Message::SchedExpires)
                .padding(6),
                day_row,
                text(&state.sched_error).size(11),
                row![
                    button(text("Save").size(12)).on_press(Message::SchedulerSave),
                    button(text("Cancel").size(12)).on_press(Message::SchedulerEditorClose),
                ]
                .spacing(8),
            ]
            .spacing(6)
            .padding(8),
        );
    }
    col.into()
}
