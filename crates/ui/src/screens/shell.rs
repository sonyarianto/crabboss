//! App shell: router + sidebar + status footer.

use iced::{
    widget::{button, column, container, row, scrollable, text},
    Element, Length,
};

use crate::app::{App, Message, Screen};
use crate::widgets::on_air_label;

use super::{ads, carts, home, library, playout, reports, scheduler, settings};

pub(crate) fn view(state: &App) -> Element<'_, Message> {
    let body: Element<'_, Message> = match state.screen {
        Screen::Home => home::view(state),
        Screen::Playout => playout::view(state),
        Screen::Media => library::page(state),
        Screen::Scheduler => scheduler::view(state),
        Screen::Carts => carts::view(state),
        Screen::Reports => reports::view(state),
        Screen::Ads => ads::view(state),
        Screen::Settings => settings::view(state),
    };

    column![
        row![
            view_sidebar(state),
            iced::widget::rule::vertical(1),
            container(body).width(Length::Fill).height(Length::Fill),
        ]
        .height(Length::Fill),
        iced::widget::rule::horizontal(1),
        view_footer(state),
    ]
    .into()
}

/// Full-width status footer: the on-air line gets the whole window width
/// (long titles no longer wrap inside the narrow sidebar) with room to
/// grow stream/mic indicators later. v1 carries only the on-air status.
fn view_footer(state: &App) -> Element<'_, Message> {
    let status = if state.is_playing {
        on_air_label(&state.now_title, &state.now_artist)
    } else {
        "OFF AIR".to_string()
    };
    container(
        row![
            text(status).size(12),
            iced::widget::space::horizontal(),
            text(format!("Version {}", env!("CARGO_PKG_VERSION"))).size(11),
        ]
        .align_y(iced::Alignment::Center),
    )
    .padding([6, 12])
    .width(Length::Fill)
    .into()
}

/// Halloy-style left sidebar (v1: fixed position, no collapse, no badges):
/// station name plus a scrollable entry list (one per screen, active
/// highlighted). Global status lives in the full-width footer.
fn view_sidebar(state: &App) -> Element<'_, Message> {
    let mut list = column![].spacing(4);
    for s in [
        Screen::Home,
        Screen::Playout,
        Screen::Media,
        Screen::Scheduler,
        Screen::Carts,
        Screen::Reports,
        Screen::Ads,
        Screen::Settings,
    ] {
        // Active screen gets the theme's accent button; the rest stay
        // transparent text buttons — no bracket hacks needed.
        let entry = button(text(s.label()).size(13))
            .width(Length::Fill)
            .on_press(Message::Navigate(s));
        list = list.push(if s == state.screen {
            entry.style(iced::widget::button::primary)
        } else {
            entry.style(iced::widget::button::text)
        });
    }

    container(
        column![
            text(&state.station_name).size(15),
            scrollable(list).height(Length::Fill),
        ]
        .spacing(6)
        .padding(10),
    )
    .width(Length::Fixed(172.0))
    .height(Length::Fill)
    .into()
}
