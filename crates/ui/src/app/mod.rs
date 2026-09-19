//! Application root: module declarations, the public facade (so
//! `crate::app::{App, Message, Screen, SettingsSection}` keeps working
//! for screens, backup, and widgets), and the Iced wiring
//! (subscription + cart hotkeys).
//!
//! Layout:
//!
//! ```text
//! app/
//!   mod.rs      # this file: facade + Iced wiring
//!   message.rs  # Message enum
//!   state.rs    # App store + navigation types + settings persistence
//!   boot.rs     # startup sequence
//!   tick.rs     # 200 ms periodic processing
//!   update/     # central dispatcher + one domain module each
//! ```

pub(crate) mod boot;
pub(crate) mod message;
pub(crate) mod state;
pub(crate) mod tick;
pub(crate) mod update;

pub(crate) use boot::boot;
pub(crate) use message::Message;
pub(crate) use state::{App, Screen, SettingsSection};
pub(crate) use update::update;

use std::time::Duration;

use iced::Subscription;

fn cart_hotkey(key: iced::keyboard::Key, _: iced::keyboard::Modifiers) -> Option<Message> {
    use iced::keyboard::Key;
    match key {
        Key::Character(c) => match c.as_str() {
            "1" => Some(Message::CartHotkey(0)),
            "2" => Some(Message::CartHotkey(1)),
            "3" => Some(Message::CartHotkey(2)),
            "4" => Some(Message::CartHotkey(3)),
            "5" => Some(Message::CartHotkey(4)),
            "6" => Some(Message::CartHotkey(5)),
            "7" => Some(Message::CartHotkey(6)),
            "8" => Some(Message::CartHotkey(7)),
            _ => None,
        },
        _ => None,
    }
}

pub(crate) fn subscription(_: &App) -> Subscription<Message> {
    Subscription::batch(vec![
        iced::time::every(Duration::from_millis(200)).map(|_| Message::Tick),
        iced::window::close_requests().map(Message::WindowCloseRequested),
        iced::keyboard::listen().filter_map(|event| match event {
            iced::keyboard::Event::KeyPressed { key, modifiers, .. } => cart_hotkey(key, modifiers),
            _ => None,
        }),
    ])
}

#[cfg(test)]
mod app_tests {
    use super::*;
    use iced::keyboard::{Key, Modifiers};

    fn hotkey(c: &str) -> Option<Message> {
        cart_hotkey(Key::Character(c.into()), Modifiers::empty())
    }

    #[test]
    fn cart_hotkeys_map_1_to_8_onto_pads_0_to_7() {
        for (key, pad) in ["1", "2", "3", "4", "5", "6", "7", "8"].iter().zip(0..8) {
            assert!(
                matches!(hotkey(key), Some(Message::CartHotkey(i)) if i == pad),
                "{key} must fire pad {pad}"
            );
        }
    }

    #[test]
    fn cart_hotkeys_ignore_other_characters() {
        for key in ["0", "9", "a", "!", " "] {
            assert!(hotkey(key).is_none(), "{key} must not fire a pad");
        }
    }
}
