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

/// One finished folder-sync walk: candidate audio paths (unfiltered —
/// the UI thread drops the ones already in the library before queuing).
pub(crate) struct SyncFound {
    pub(crate) folders: usize,
    pub(crate) paths: Vec<PathBuf>,
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

/// Opus CBR ladder in kbps: speech-usable at the bottom, transparent
/// stereo music at the top. Narrower than the MP3 ladder on purpose —
/// Opus needs far fewer bits for the same quality.
pub(crate) const OPUS_LADDER: [u32; 8] = [24, 32, 48, 64, 80, 96, 128, 160];

pub(crate) fn opus_bitrate_step(current: u32, up: bool) -> u32 {
    let idx = OPUS_LADDER
        .iter()
        .position(|&b| b >= current)
        .unwrap_or(OPUS_LADDER.len() - 1);
    match up {
        true => OPUS_LADDER[(idx + 1).min(OPUS_LADDER.len() - 1)],
        false => OPUS_LADDER[idx.saturating_sub(1)],
    }
}

/// Snap an arbitrary kbps (e.g. carried over from the MP3 ladder when
/// switching formats) to the nearest Opus rung.
pub(crate) fn opus_snap_bitrate(kbps: u32) -> u32 {
    OPUS_LADDER
        .iter()
        .min_by_key(|&&b| b.abs_diff(kbps))
        .copied()
        .unwrap_or(96)
}

/// HE-AAC CBR ladder in kbps: v2 (parametric stereo) territory at the
/// bottom, v1 (SBR) above 48 kbps. Sits below the MP3 ladder on purpose —
/// HE-AAC needs far fewer bits for the same quality.
pub(crate) const HEAAC_LADDER: [u32; 9] = [24, 32, 40, 48, 56, 64, 80, 96, 128];

pub(crate) fn heaac_bitrate_step(current: u32, up: bool) -> u32 {
    let idx = HEAAC_LADDER
        .iter()
        .position(|&b| b >= current)
        .unwrap_or(HEAAC_LADDER.len() - 1);
    match up {
        true => HEAAC_LADDER[(idx + 1).min(HEAAC_LADDER.len() - 1)],
        false => HEAAC_LADDER[idx.saturating_sub(1)],
    }
}

/// Snap an arbitrary kbps (e.g. carried over from another format) to
/// the nearest HE-AAC rung.
pub(crate) fn heaac_snap_bitrate(kbps: u32) -> u32 {
    HEAAC_LADDER
        .iter()
        .min_by_key(|&&b| b.abs_diff(kbps))
        .copied()
        .unwrap_or(48)
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
    .align_y(iced::Alignment::Center)
    .into()
}

pub(crate) fn mic_state_label(st: &MicState) -> String {
    format!("{:?}", st)
}

/// Password saved-state line: tells whether a secret is stored without
/// ever showing its value.
pub(crate) fn stream_password_status(saved: bool) -> &'static str {
    if saved {
        "Password: saved"
    } else {
        "Password: not set (stream auth will fail)"
    }
}

pub(crate) fn mic_state_is_live(st: &MicState) -> bool {
    matches!(st, MicState::Live)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crabcore::library::TrackKind;

    #[test]
    fn fmt_dur_formats_mm_ss() {
        assert_eq!(fmt_dur(None), "00:00");
        assert_eq!(fmt_dur(Some(0.0)), "00:00");
        assert_eq!(fmt_dur(Some(65.0)), "01:05");
        assert_eq!(fmt_dur(Some(65.9)), "01:05");
        assert_eq!(fmt_dur(Some(-3.0)), "00:00");
    }

    #[test]
    fn kind_label_covers_all_kinds() {
        assert_eq!(kind_label(TrackKind::Music), "Music");
        assert_eq!(kind_label(TrackKind::Jingle), "Jingle");
        assert_eq!(kind_label(TrackKind::Ad), "Ad");
    }

    #[test]
    fn strip_audio_extension_keeps_bare_and_dotfiles() {
        assert_eq!(strip_audio_extension("Song.mp3"), "Song");
        assert_eq!(strip_audio_extension("archive.tar.gz"), "archive.tar");
        assert_eq!(strip_audio_extension("noext"), "noext");
        assert_eq!(strip_audio_extension(".hidden"), ".hidden");
    }

    #[test]
    fn join_title_artist_never_dangles_separator() {
        assert_eq!(join_title_artist("T", "A"), "T - A");
        assert_eq!(join_title_artist("T", ""), "T");
        assert_eq!(join_title_artist("", "A"), "A");
        assert_eq!(join_title_artist("", ""), "");
    }

    #[test]
    fn track_source_label_tags_automation_only() {
        assert_eq!(
            track_source_label("News", "Scheduler"),
            "News · via Scheduler"
        );
        assert_eq!(
            track_source_label("Song", "Real Artist"),
            "Song - Real Artist"
        );
        assert_eq!(track_source_label("", "Auto-DJ"), "Auto-DJ");
    }

    #[test]
    fn on_air_label_prefixes() {
        assert_eq!(on_air_label("Song", "Band"), "ON AIR: Song - Band");
    }

    #[test]
    fn lin_to_dbfs_unity_is_zero_and_silent_clamps() {
        assert!((lin_to_dbfs(1.0)).abs() < 1e-4);
        assert!((lin_to_dbfs(0.0) + 60.0).abs() < 1e-3);
    }

    #[test]
    fn stream_bitrate_step_walks_ladder_and_clamps() {
        assert_eq!(stream_bitrate_step(128, true), 160);
        assert_eq!(stream_bitrate_step(128, false), 112);
        assert_eq!(stream_bitrate_step(320, true), 320);
        assert_eq!(stream_bitrate_step(8, false), 8);
        assert_eq!(stream_bitrate_step(999, true), 320);
    }

    #[test]
    fn opus_bitrate_step_walks_narrower_ladder() {
        assert_eq!(opus_bitrate_step(96, true), 128);
        assert_eq!(opus_bitrate_step(96, false), 80);
        assert_eq!(opus_bitrate_step(160, true), 160);
        assert_eq!(opus_bitrate_step(24, false), 24);
        assert_eq!(opus_bitrate_step(320, true), 160);
        assert_eq!(opus_bitrate_step(8, false), 24);
    }

    #[test]
    fn opus_snap_bitrate_picks_nearest_rung() {
        assert_eq!(opus_snap_bitrate(128), 128);
        assert_eq!(opus_snap_bitrate(320), 160);
        assert_eq!(opus_snap_bitrate(100), 96);
        assert_eq!(opus_snap_bitrate(0), 24);
    }

    #[test]
    fn heaac_bitrate_step_walks_low_ladder() {
        assert_eq!(heaac_bitrate_step(48, true), 56);
        assert_eq!(heaac_bitrate_step(48, false), 40);
        assert_eq!(heaac_bitrate_step(128, true), 128);
        assert_eq!(heaac_bitrate_step(24, false), 24);
        assert_eq!(heaac_bitrate_step(320, true), 128);
        assert_eq!(heaac_bitrate_step(8, false), 24);
    }

    #[test]
    fn heaac_snap_bitrate_picks_nearest_rung() {
        assert_eq!(heaac_snap_bitrate(48), 48);
        assert_eq!(heaac_snap_bitrate(128), 128);
        assert_eq!(heaac_snap_bitrate(320), 128);
        assert_eq!(heaac_snap_bitrate(100), 96);
        assert_eq!(heaac_snap_bitrate(0), 24);
    }

    #[test]
    fn duck_ms_step_walks_given_ladder() {
        assert_eq!(duck_ms_step(&ATTACK_LADDER, 10.0, true), 20.0);
        assert_eq!(duck_ms_step(&ATTACK_LADDER, 10.0, false), 5.0);
        assert_eq!(duck_ms_step(&RELEASE_LADDER, 3000.0, true), 3000.0);
        assert_eq!(duck_ms_step(&RELEASE_LADDER, 10.0, false), 10.0);
    }

    #[test]
    fn action_names_cover_scheduler_indices() {
        assert_eq!(action_name(0), "play");
        assert_eq!(action_name(4), "queue");
        assert_eq!(action_name(3), "command");
        assert_eq!(action_label(3), "command");
        assert_eq!(action_label(4), "queue");
    }

    #[test]
    fn report_range_bounds_labels() {
        assert_eq!(report_range_bounds(0).1, "Today");
        assert_eq!(report_range_bounds(1).1, "Last 7 days");
        assert_eq!(report_range_bounds(2).1, "Last 30 days");
        assert_eq!(report_range_bounds(3).1, "All time");
        assert_eq!(report_range_bounds(99).1, "Last 7 days");
    }

    #[test]
    fn eq_band_label_formats() {
        assert!(!eq_band_label(0).is_empty());
        assert_eq!(eq_band_label(999), "0");
    }

    #[test]
    fn short_name_takes_file_name() {
        assert_eq!(short_name("/a/b/song.mp3"), "song.mp3");
        assert_eq!(short_name(""), "");
    }

    #[test]
    fn stream_password_status_never_shows_value() {
        assert_eq!(stream_password_status(true), "Password: saved");
        let empty = stream_password_status(false);
        assert!(empty.contains("not set"), "{empty}");
    }

    #[test]
    fn mic_state_helpers() {
        assert!(mic_state_is_live(&MicState::Live));
        assert!(!mic_state_is_live(&MicState::Off));
        assert!(!mic_state_is_live(&MicState::Error("x".into())));
        assert!(!mic_state_label(&MicState::Live).is_empty());
    }
}
