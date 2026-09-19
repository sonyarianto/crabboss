//! CrabBoss
//!
//! Desktop UI entry point using Iced (Elm architecture).
//! The app lives in modules: `app` (Elm state/update/boot), `screens`
//! (one view per screen), `widgets` (shared display helpers).

mod app;
mod backup;
mod rules;
mod screens;
mod widgets;

use app::{boot, subscription, update};

fn main() -> iced::Result {
    iced::application(boot, update, screens::shell::view)
        .title("CrabBoss")
        .subscription(subscription)
        .theme(|_: &app::App| iced::Theme::Dark)
        // Off so the X button routes through `Message::WindowCloseRequested`
        // (graceful stream/Stereo Tool shutdown) instead of killing the
        // window while the sender thread still owns a live DSP instance.
        .exit_on_close_request(false)
        .run()
}
