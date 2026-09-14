//! Settings: category list plus one detail page (Windows-Settings style).

use iced::{
    widget::{button, checkbox, column, container, row, scrollable, text, text_input},
    Element, Length,
};

use crabcore::audio::EQ_BAND_COUNT;

use crate::app::{App, Message, SettingsSection};
use crate::widgets::{eq_band_label, lin_to_dbfs, mic_state_is_live, mic_state_label, stepper};

pub(crate) fn view(state: &App) -> Element<'_, Message> {
    let s = &state.settings;
    let stream_cfg = state.player.stream_config();
    let stream_state = state.player.stream_state();
    let stream_stats = state.player.stream_stats();
    let mic_cfg = state.player.mic_config();
    let mic_state = state.player.mic_state();

    let mut devices = column![text("Output devices:").size(12)].spacing(4);
    // Highlight what is actually sounding: the saved choice, or the live
    // device when running on the system default (sel_device is empty then).
    let active_output = if state.sel_device.is_empty() {
        state.player.device_name()
    } else {
        state.sel_device.clone()
    };
    for d in &state.output_devices {
        let name = d.clone();
        let entry = button(text(d).size(12)).on_press(Message::SettingsSelectDevice(name));
        devices = devices.push(if *d == active_output {
            entry.style(iced::widget::button::primary)
        } else {
            entry
        });
    }

    let mut inputs = column![text("Input devices:").size(12)].spacing(4);
    let cur_mic = mic_cfg.device.clone().unwrap_or_default();
    for d in &state.input_devices {
        let name = d.clone();
        let entry = button(text(d).size(12)).on_press(Message::MicSelectDevice(name));
        inputs = inputs.push(if *d == cur_mic {
            entry.style(iced::widget::button::primary)
        } else {
            entry
        });
    }

    let mut eq = column![text("12-band EQ:").size(12)].spacing(2);
    for band in 0..EQ_BAND_COUNT {
        eq = eq.push(
            row![
                text(format!(
                    "{}: {:+.0} dB",
                    eq_band_label(band),
                    s.eq_gains_db[band]
                ))
                .size(12)
                .width(Length::Fixed(160.0)),
                button(text("-").size(11)).on_press(Message::EqDec(band)),
                button(text("+").size(11)).on_press(Message::EqInc(band)),
            ]
            .spacing(6),
        );
    }

    // Category list + one detail page (Windows-Settings style). The cards
    // are gone: each section gets the full content width instead.
    let mut nav = column![].spacing(4);
    for sec in [
        SettingsSection::Station,
        SettingsSection::AudioDevice,
        SettingsSection::Playout,
        SettingsSection::Equalizer,
        SettingsSection::Loudness,
        SettingsSection::Streaming,
        SettingsSection::Microphone,
        SettingsSection::License,
    ] {
        let entry = button(text(sec.label()).size(13))
            .width(Length::Fill)
            .on_press(Message::SettingsNav(sec));
        nav = nav.push(if sec == state.settings_section {
            entry.style(iced::widget::button::primary)
        } else {
            entry.style(iced::widget::button::text)
        });
    }

    let sec = state.settings_section;
    let content: Element<'_, Message> = match sec {
        SettingsSection::Station => column![
            text(sec.label()).size(16),
            text(sec.description()).size(11),
            text_input("Station name", &state.settings.station_name)
                .on_input(Message::StationName)
                .padding(6),
        ]
        .spacing(8)
        .into(),
        SettingsSection::AudioDevice => column![
            text(sec.label()).size(16),
            text(sec.description()).size(11),
            text(format!(
                "Engine: {} | Device: {}",
                state.audio_engine,
                state.player.device_name()
            ))
            .size(12),
            devices,
            text(&state.device_note).size(11),
            button(text("Refresh devices").size(12)).on_press(Message::SettingsRefreshDevices),
        ]
        .spacing(8)
        .into(),
        SettingsSection::Playout => column![
            text(sec.label()).size(16),
            text(sec.description()).size(11),
            stepper(
                format!("Crossfade: {:.1} s", s.crossfade_secs),
                Message::XfadeDec,
                Message::XfadeInc
            ),
            stepper(
                format!("Silence alarm: {:.0} s", s.silence_threshold_secs),
                Message::SilenceDec,
                Message::SilenceInc
            ),
        ]
        .spacing(8)
        .into(),
        SettingsSection::Equalizer => column![
            text(sec.label()).size(16),
            text(sec.description()).size(11),
            row![
                checkbox(s.eq_enabled)
                    .label("EQ enabled")
                    .on_toggle(|_| Message::EqToggle),
                button(text("Reset EQ").size(11)).on_press(Message::EqReset),
            ]
            .spacing(8),
            eq,
            stepper(
                format!("Limiter: {:.1} dBFS", lin_to_dbfs(s.limiter_ceiling)),
                Message::LimiterDec,
                Message::LimiterInc
            ),
        ]
        .spacing(8)
        .into(),
        SettingsSection::Loudness => column![
            text(sec.label()).size(16),
            text(sec.description()).size(11),
            row![checkbox(s.loudness_norm)
                .label("Loudness normalize")
                .on_toggle(|_| Message::LoudnessToggle),]
            .spacing(8),
            stepper(
                format!("Target: {:.0} LUFS", s.loudness_target_lufs),
                Message::LoudnessTargetDec,
                Message::LoudnessTargetInc
            ),
        ]
        .spacing(8)
        .into(),
        SettingsSection::Streaming => column![
            text(sec.label()).size(16),
            text(sec.description()).size(11),
            row![
                checkbox(stream_cfg.enabled)
                    .label("Stream enabled")
                    .on_toggle(|_| Message::StreamToggle),
                checkbox(stream_cfg.tls)
                    .label("TLS (https)")
                    .on_toggle(|_| Message::StreamTlsToggle),
                text(stream_state.label()).size(12),
                text(if stream_state.is_live() {
                    format!(
                        "{} kbps - {:.1} MB - {}s",
                        stream_cfg.bitrate_kbps,
                        stream_stats.bytes_sent as f64 / 1_048_576.0,
                        stream_stats.stream_secs
                    )
                } else {
                    String::new()
                })
                .size(11),
            ]
            .spacing(8),
            text_input("Host", &stream_cfg.host)
                .on_input(Message::StreamHost)
                .padding(6),
            text_input("Port", &stream_cfg.port.to_string())
                .on_input(Message::StreamPort)
                .padding(6),
            text_input("Mount", &stream_cfg.mount)
                .on_input(Message::StreamMount)
                .padding(6),
            text_input("Username", &stream_cfg.username)
                .on_input(Message::StreamUsername)
                .padding(6),
            text_input("Password", &stream_cfg.password)
                .on_input(Message::StreamPassword)
                .padding(6),
            stepper(
                format!("Bitrate: {} kbps", stream_cfg.bitrate_kbps),
                Message::StreamBitrateDec,
                Message::StreamBitrateInc
            ),
        ]
        .spacing(8)
        .into(),
        SettingsSection::Microphone => column![
            text(sec.label()).size(16),
            text(sec.description()).size(11),
            row![
                checkbox(mic_cfg.enabled)
                    .label("Mic enabled")
                    .on_toggle(|_| Message::MicToggle),
                text(mic_state_label(&mic_state)).size(12),
                text(if mic_state_is_live(&mic_state) {
                    format!(
                        "{:.1} dBFS{}",
                        state.player.mic_level_db(),
                        if state.player.mic_ducking() {
                            " - ducking"
                        } else {
                            ""
                        }
                    )
                } else {
                    String::new()
                })
                .size(11),
            ]
            .spacing(8),
            inputs,
            text(&state.mic_note).size(11),
            button(text("Refresh inputs").size(12)).on_press(Message::MicRefreshDevices),
            stepper(
                format!("Mic level: {:.0}%", mic_cfg.level * 100.0),
                Message::MicLevelDec,
                Message::MicLevelInc
            ),
            row![checkbox(mic_cfg.duck_enabled)
                .label("Ducking")
                .on_toggle(|_| Message::MicDuckToggle),]
            .spacing(8),
            stepper(
                format!("Threshold: {:+.0} dB", mic_cfg.duck_threshold_db),
                Message::MicThresholdDec,
                Message::MicThresholdInc
            ),
            stepper(
                format!("Depth: -{:.0} dB", mic_cfg.duck_depth_db),
                Message::MicDepthDec,
                Message::MicDepthInc
            ),
            stepper(
                format!("Attack: {:.0} ms", mic_cfg.attack_ms),
                Message::MicAttackDec,
                Message::MicAttackInc
            ),
            stepper(
                format!("Release: {:.0} ms", mic_cfg.release_ms),
                Message::MicReleaseDec,
                Message::MicReleaseInc
            ),
        ]
        .spacing(8)
        .into(),
        SettingsSection::License => column![
            text(sec.label()).size(16),
            text(sec.description()).size(11),
            text(format!("License: {}", state.license_status)).size(12),
            text(&state.license_error).size(11),
            text_input("License key CB-XXXX-XXXX-XXXX", &state.license_key)
                .on_input(Message::LicenseKeyInput)
                .padding(6),
            row![
                button(text("Activate").size(12))
                    .width(Length::Fill)
                    .on_press(Message::ActivateLicense),
                button(text("Clear").size(12))
                    .width(Length::Fill)
                    .on_press(Message::ClearLicense),
            ]
            .spacing(6),
        ]
        .spacing(8)
        .into(),
    };

    row![
        container(nav.spacing(6).padding(10))
            .width(Length::Fixed(168.0))
            .height(Length::Fill),
        iced::widget::rule::vertical(1),
        container(
            scrollable(column![content].padding(iced::Padding {
                top: 12.0,
                right: 26.0,
                bottom: 12.0,
                left: 12.0,
            }))
            .height(Length::Fill)
        )
        .width(Length::Fill)
        .height(Length::Fill),
    ]
    .into()
}
