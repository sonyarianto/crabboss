//! One screen per module; the shell (sidebar + footer + router) lives in
//! `shell`. All screens read `App` and send `Message` — the Elm update
//! logic stays central in `app.rs` on purpose.

pub(crate) mod ads;
pub(crate) mod carts;
pub(crate) mod home;
pub(crate) mod library;
pub(crate) mod playout;
pub(crate) mod reports;
pub(crate) mod scheduler;
pub(crate) mod settings;
pub(crate) mod shell;
