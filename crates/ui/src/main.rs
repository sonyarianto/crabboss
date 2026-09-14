//! CrabBoss
//!
//! Desktop UI entry point using Iced (Elm architecture).
//! The app lives in modules: `app` (Elm state/update/boot), `screens`
//! (one view per screen), `widgets` (shared display helpers).

mod app;
mod backup;
mod screens;
mod widgets;

use app::{boot, subscription, update};

fn main() -> iced::Result {
    iced::application(boot, update, screens::shell::view)
        .title("CrabBoss")
        .subscription(subscription)
        .theme(|_: &app::App| iced::Theme::Dark)
        .run()
}
