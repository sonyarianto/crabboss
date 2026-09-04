//! Scheduler event management (RadioBoss-style Scheduler tab)
//!
//! MVP: named events with an action (`play` / `load` / `generate` / `command`),
//! a daily start time (`HH:MM`), repeat days, and an enabled flag.
//! A background tick can call [`SchedulerManager::due_events`] to find
//! events that should fire now, then dispatch via `generate` / `load`.

mod manager;

pub use manager::{
    days_from_mask, mask_from_days, validate_hhmm, ScheduledEvent, SchedulerManager,
};
