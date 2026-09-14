//! Pure display helpers shared by the screens (and a few `update` paths).
//! No state, no I/O — formatting and label logic only.

use std::path::PathBuf;

use iced::{
    widget::{button, row, text},
    Element,
};

use crabcore::audio::{MicState, EQ_CENTER_HZ};
use crabcore::library::{Track, TrackKind};

use crate::app::Message;

pub(crate) struct LoudnessDone {
    pub(crate) id: String,
    pub(crate) file_name: String,
    pub(crate) lufs: f32,
    pub(crate) gain_db: f32,
}

pub(crate) fn fmt_dur(d: Option<f64>) -> String {
    let total = d.unwrap_or(0.0).max(0.0) as u64;
    format!("{:02}:{:02}", total / 60, total % 60)
}

pub(crate) fn kind_label(k: TrackKind) -> &'static str {
    match k {
        TrackKind::Jingle => "Jingle",
        TrackKind::Ad => "Ad",
        TrackKind::Music => "Music",
    }
}

/// Display title: metadata title, else the file name without its container
/// extension ("Song.mp3" -> "Song"). Display-only; stored data is untouched.
pub(crate) fn track_label(t: &Track) -> String {
    t.title
        .clone()
        .unwrap_or_else(|| strip_audio_extension(&t.file_name))
}

/// Strip the trailing container extension for display. Keeps names without
/// a dot (or dotfiles) as-is.
pub(crate) fn strip_audio_extension(name: &str) -> String {
    match name.rfind('.') {
        Some(i) if i > 0 => name[..i].to_string(),
        _ => name.to_string(),
    }
}

/// "Title - Artist", omitting the separator when one side is missing so
/// untagged files never render a dangling " - ".
pub(crate) fn join_title_artist(title: &str, artist: &str) -> String {
    match (title.is_empty(), artist.is_empty()) {
        (false, false) => format!("{title} - {artist}"),
        (false, true) => title.to_string(),
        (true, false) => artist.to_string(),
        (true, true) => String::new(),
    }
}

/// On-air line with a source tag only when certain: the artist slot holds
/// either a real artist (manual play — no tag, nothing claimed) or one of
/// our own automation sentinels, which moves to a `· via X` suffix instead
/// of masquerading as the artist.
/// Track line with a source tag only when certain: the artist slot holds
/// either a real artist (manual play — no tag, nothing claimed) or one of
/// our own automation sentinels, which moves to a `· via X` suffix instead
/// of masquerading as the artist. Shared by the strip, footer, and Home.
pub(crate) fn track_source_label(title: &str, artist: &str) -> String {
    const SOURCES: [&str; 5] = [
        "Auto-DJ",
        "Cart",
        "Scheduler",
        "Ad break",
        "Silence detector",
    ];
    if SOURCES.contains(&artist) && !title.is_empty() {
        format!("{title} · via {artist}")
    } else {
        join_title_artist(title, artist)
    }
}

pub(crate) fn on_air_label(title: &str, artist: &str) -> String {
    format!("ON AIR: {}", track_source_label(title, artist))
}

/// `--engine cpal` (only backend; `--engine rodio` warns and uses cpal).
pub(crate) fn engine_choice() -> String {
    let mut args = std::env::args().skip(1);
    let mut choice = std::env::var("CRABBOSS_ENGINE").unwrap_or_else(|_| "cpal".into());
    while let Some(a) = args.next() {
        if a == "--engine" {
            if let Some(v) = args.next() {
                choice = v;
            }
        } else if let Some(v) = a.strip_prefix("--engine=") {
            choice = v.to_string();
        }
    }
    choice.to_lowercase()
}

pub(crate) fn lin_to_dbfs(lin: f32) -> f32 {
    20.0 * lin.max(0.001).log10()
}

pub(crate) fn stream_bitrate_step(current: u32, up: bool) -> u32 {
    const LADDER: [u32; 16] = [
        8, 16, 24, 32, 40, 48, 64, 80, 96, 112, 128, 160, 192, 224, 256, 320,
    ];
    let idx = LADDER
        .iter()
        .position(|&b| b >= current)
        .unwrap_or(LADDER.len() - 1);
    match up {
        true => LADDER[(idx + 1).min(LADDER.len() - 1)],
        false => LADDER[idx.saturating_sub(1)],
    }
}

pub(crate) fn duck_ms_step(ladder: &[f32], current: f32, up: bool) -> f32 {
    let idx = ladder
        .iter()
        .position(|&b| b >= current)
        .unwrap_or(ladder.len() - 1);
    match up {
        true => ladder[(idx + 1).min(ladder.len() - 1)],
        false => ladder[idx.saturating_sub(1)],
    }
}

pub(crate) const ATTACK_LADDER: [f32; 9] = [1.0, 2.0, 5.0, 10.0, 20.0, 50.0, 100.0, 200.0, 500.0];
pub(crate) const RELEASE_LADDER: [f32; 9] =
    [10.0, 25.0, 50.0, 100.0, 200.0, 400.0, 800.0, 1500.0, 3000.0];

pub(crate) fn action_name(idx: usize) -> &'static str {
    match idx {
        0 => "play",
        1 => "load",
        2 => "generate",
        4 => "queue",
        _ => "command",
    }
}

pub(crate) fn action_label(idx: usize) -> &'static str {
    match idx {
        0 => "play",
        1 => "load",
        2 => "generate",
        3 => "command",
        4 => "queue",
        _ => "command",
    }
}

pub(crate) fn report_range_bounds(idx: usize) -> (chrono::DateTime<chrono::Utc>, String) {
    use chrono::{Duration as CDur, Local};
    let now = Local::now();
    let label = match idx {
        0 => "Today",
        2 => "Last 30 days",
        3 => "All time",
        _ => "Last 7 days",
    }
    .to_string();
    let from = match idx {
        0 => now
            .date_naive()
            .and_hms_opt(0, 0, 0)
            .unwrap()
            .and_local_timezone(Local)
            .unwrap(),
        2 => now - CDur::days(30),
        3 => now - CDur::days(365 * 20),
        _ => now - CDur::days(7),
    };
    (from.with_timezone(&chrono::Utc), label)
}

pub(crate) fn eq_band_label(band: usize) -> String {
    let hz = EQ_CENTER_HZ.get(band).copied().unwrap_or(0.0);
    if hz >= 1000.0 {
        format!("{:.1}k", hz / 1000.0)
    } else {
        format!("{:.0}", hz)
    }
}

pub(crate) fn short_name(path: &str) -> String {
    PathBuf::from(path)
        .file_name()
        .map(|n| n.to_string_lossy().to_string())
        .unwrap_or_default()
}

pub(crate) fn stepper(label: String, dec: Message, inc: Message) -> Element<'static, Message> {
    row![
        button(text("-").size(12)).on_press(dec),
        text(label).size(12),
        button(text("+").size(12)).on_press(inc),
    ]
    .spacing(8)
    .into()
}

pub(crate) fn mic_state_label(st: &MicState) -> String {
    format!("{:?}", st)
}

pub(crate) fn mic_state_is_live(st: &MicState) -> bool {
    matches!(st, MicState::Live)
}
