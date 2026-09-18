//! Settings: category list plus one detail page (Windows-Settings style).

use iced::{
    widget::{button, checkbox, column, container, row, scrollable, text, text_input},
    Element, Length,
};

use crabcore::audio::{CueState, EQ_BAND_COUNT};
use crabcore::stream::{StreamFormat, StreamState};

use crate::app::{App, Message, SettingsSection};
use crate::widgets::{
    eq_band_label, lin_to_dbfs, mic_state_is_live, mic_state_label, stepper, stream_password_status,
};

pub(crate) fn view(state: &App) -> Element<'_, Message> {
    let s = &state.settings;
    let stream_cfg = state.player.stream_config();
    let stream_state = state.player.stream_state();
    let stream_stats = state.player.stream_stats();
    let mic_cfg = state.player.mic_config();
    let mic_state = state.player.mic_state();
    let cue_cfg = state.player.cue_config();
    let cue_volume = state.player.cue_volume();

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

    // Cue (PFL) picker reuses the output list: private headphone bus.
    // Highlight follows the *opened* device while the cue stream is up
    // (like the program's active-output highlight); when off, the saved
    // choice stays highlighted and a failed open highlights nothing —
    // the error line explains instead of a lying highlight.
    let mut cue_devices = column![text("Cue (headphones) devices:").size(12)].spacing(4);
    let cur_cue = if state.player.cue_state() == CueState::Unavailable {
        cue_cfg.device.clone().unwrap_or_default()
    } else {
        state.player.cue_device_name()
    };
    for d in &state.output_devices {
        let name = d.clone();
        let entry = button(text(d).size(12)).on_press(Message::CueSelectDevice(name));
        cue_devices = cue_devices.push(if *d == cur_cue {
            entry.style(iced::widget::button::primary)
        } else {
            entry
        });
    }

    let mut format_row = row![text("Format:").size(12)].spacing(6);
    // `●` marks the encoder actually on air (fixed per connection);
    // the primary highlight marks the *selection* for the next start.
    // The two disagree while live after a change — the note under the
    // row says so explicitly instead of silently lying.
    let airing = matches!(
        stream_state,
        StreamState::Live | StreamState::Connecting
    );
    // Any selection differing from the live snapshot needs a restart
    // (encoder + host/port/mount/TLS are all per-connection). No button
    // when off: there is nothing on air to disrupt.
    let pending_restart = match &state.stream_live_config {
        Some(live) => *live != s.stream && !matches!(stream_state, StreamState::Off),
        None => false,
    };
    for format in [StreamFormat::Mp3, StreamFormat::Opus] {
        let live_here = airing
            && state
                .stream_live_config
                .as_ref()
                .is_some_and(|c| c.format == format);
        let label = if live_here {
            format!("● {}", format.label())
        } else {
            format.label().to_string()
        };
        let entry =
            button(text(label).size(12)).on_press(Message::StreamFormatChanged(format));
        format_row = format_row.push(if stream_cfg.format == format {
            entry.style(iced::widget::button::primary)
        } else {
            entry
        });
    }
    if pending_restart {
        format_row = format_row.push(
            button(text("Apply & restart").size(12))
                .style(iced::widget::button::danger)
                .on_press(Message::StreamRestart),
        );
    }

    // Status line names the live encoder; the note under the format
    // buttons calls out a pending selection while live.
    let stream_status = match &stream_state {
        StreamState::Live => match &state.stream_live_config {
            Some(live) => format!("🔴 Live — {} {} kbps", live.format.label(), live.bitrate_kbps),
            None => stream_state.label(),
        },
        StreamState::Connecting => match &state.stream_live_config {
            Some(live) => format!(
                "⏳ Connecting… — {} {} kbps",
                live.format.label(),
                live.bitrate_kbps
            ),
            None => stream_state.label(),
        },
        _ => stream_state.label(),
    };
    let format_note = match &state.stream_live_config {
        Some(live) if airing => {
            if live.format == stream_cfg.format && live.bitrate_kbps == stream_cfg.bitrate_kbps
            {
                format!(
                    "On air: {} {} kbps.",
                    live.format.label(),
                    live.bitrate_kbps
                )
            } else {
                format!(
                    "On air: {} {} kbps — selected {} {} kbps applies on restart.",
                    live.format.label(),
                    live.bitrate_kbps,
                    stream_cfg.format.label(),
                    stream_cfg.bitrate_kbps
                )
            }
        }
        Some(_) if pending_restart => {
            "Connection settings changed — restart stream to apply.".to_string()
        }
        _ => {
            "Opus sounds better per bit; mounts often end in .opus. Format applies on stream start."
                .to_string()
        }
    };

    // Graphic-EQ fader strip: 12 vertical faders in frequency order —
    // the fader positions ARE the curve, visible at a glance. Drag
    // applies live; the value persists once on release (see EqSave).
    let mut eq_strip = row![].spacing(6);
    for band in 0..EQ_BAND_COUNT {
        let gain = s.eq_gains_db[band];
        eq_strip = eq_strip.push(
            column![
                text(format!("{gain:+.0}")).size(10),
                iced::widget::vertical_slider(-12.0..=12.0, gain, move |v| {
                    Message::EqSet(band, v)
                })
                .step(0.5)
                .height(Length::Fixed(120.0))
                .width(28.0)
                .on_release(Message::EqSave),
                text(eq_band_label(band)).size(10),
            ]
            .spacing(4)
            .align_x(iced::Alignment::Center)
            .width(Length::Fixed(40.0)),
        );
    }
    let eq_flat = s.eq_gains_db.iter().all(|g| g.abs() < 0.05);

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
        SettingsSection::Station => {
            let mut station = column![
                text(sec.label()).size(16),
                text(sec.description()).size(11),
                row![
                    text("Station name:").size(12).width(Length::Fixed(110.0)),
                    text_input("CrabBoss FM", &state.settings.station_name)
                        .on_input(Message::StationName)
                        .padding(6)
                        .width(Length::Fill),
                ]
                .spacing(8)
                .align_y(iced::Alignment::Center),
            row![
                button(text("Backup...").size(12)).on_press(Message::BackupNow),
                button(text("Restore...").size(12)).on_press(Message::RestoreNow),
            ]
            .spacing(6),
            text(&state.backup_status).size(11),
            text(format!("Data: {}", state.data_dir.display())).size(11),
            ]
            .spacing(8);
            // Persistence health: boot-file warnings and save failures are
            // operator news, not log-only trivia.
            if let Some(notice) = &state.settings_notice {
                station = station.push(text(format!("Settings file: {notice}")).size(11));
            }
            if let Some(err) = &state.settings_save_error {
                station = station.push(text(err).size(11));
            }
            station.into()
        }
        SettingsSection::AudioDevice => {
            // Same-device guard (RadioBOSS manuals warn the same for
            // Main/Monitor: echo/doubling). Program + cue on one endpoint
            // still works (OS mixes), but preview loses all privacy.
            let prog_effective = s
                .output_device
                .clone()
                .unwrap_or_else(|| state.player.device_name());
            let cue_same = cue_cfg
                .device
                .as_ref()
                .is_some_and(|c| c == &prog_effective);
            let mut col = column![
                text(sec.label()).size(16),
                text(sec.description()).size(11),
                text(format!(
                    "Engine: {} | Device: {}",
                    state.audio_engine,
                    state.player.device_name()
                ))
                .size(12),
                text(format!("Cue: {}", state.player.cue_device_name())).size(12),
                devices,
                text(&state.device_note).size(11),
                button(text("Refresh devices").size(12))
                    .on_press(Message::SettingsRefreshDevices),
                cue_devices,
                stepper(
                    format!("Cue volume: {:.0}%", cue_volume * 100.0),
                    Message::CueVolumeDec,
                    Message::CueVolumeInc,
                ),
                text(&state.cue_status).size(11),
                text("Cue previews Library tracks on headphones without touching program/stream.")
                    .size(11),
                text("Cue device switches live — no restart needed.").size(11),
            ]
            .spacing(8);
            if cue_same {
                col = col.push(text("Warning: cue = program device — preview will be audible on the same speakers (pick headphones for private PFL).").size(11));
            }
            col.into()
        }
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
        SettingsSection::Equalizer => {
            let col = column![
                text(sec.label()).size(16),
                text(sec.description()).size(11),
                row![
                    checkbox(s.eq_enabled)
                        .label("EQ enabled")
                        .on_toggle(|_| Message::EqToggle),
                    text(if eq_flat {
                        "Flat — bypassed"
                    } else {
                        "Custom curve"
                    })
                    .size(12),
                    button(text("Reset EQ").size(11)).on_press(Message::EqReset),
                ]
                .spacing(8)
                .align_y(iced::Alignment::Center),
                eq_strip,
                text("Limiter").size(13),
                stepper(
                    format!("Limiter: {:.1} dBFS", lin_to_dbfs(s.limiter_ceiling)),
                    Message::LimiterDec,
                    Message::LimiterInc
                ),
            ]
            .spacing(8);
            container(col)
                .width(Length::Fill)
                .max_width(620.0)
                .into()
        }
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
        SettingsSection::Streaming => {
            // Grouped layout: status / server / credentials / encoder,
            // content capped so fields stop stretching across wide
            // windows. All Messages and state reads are unchanged — only
            // order, width, and grouping moved.
            let mut body = column![
                text(sec.label()).size(16),
                text(sec.description()).size(11),
                row![
                    checkbox(stream_cfg.enabled)
                        .label("Stream enabled")
                        .on_toggle(|_| Message::StreamToggle),
                    text(stream_status).size(12),
                ]
                .spacing(8)
                .align_y(iced::Alignment::Center),
            ]
            .spacing(6);
            if stream_state.is_live() {
                // Listeners come from the public status API on a slow
                // poll; "—" means unreachable/disabled, never an error.
                let listeners = state
                    .stream_listeners
                    .map(|n| format!("{n} listeners"))
                    .unwrap_or_else(|| "—".into());
                // Name the live encoder, not just the bitrate: the
                // selection may already point at the next connection.
                let (lf, lb) = state
                    .stream_live_config
                    .as_ref()
                    .map(|c| (c.format, c.bitrate_kbps))
                    .unwrap_or((stream_cfg.format, stream_cfg.bitrate_kbps));
                body = body.push(
                    text(format!(
                        "{} {} kbps - {:.1} MB - {}s - {}",
                        lf.label(),
                        lb,
                        stream_stats.bytes_sent as f64 / 1_048_576.0,
                        stream_stats.stream_secs,
                        listeners
                    ))
                    .size(11),
                );
            }
            body = body.push(text("Server").size(13));
            // Labels persist; placeholders carry example values
            // (placeholders vanish once filled, and these persist).
            body = body.push(
                row![
                    text("Host:").size(12).width(Length::Fixed(56.0)),
                    text_input("127.0.0.1", &stream_cfg.host)
                        .on_input(Message::StreamHost)
                        .padding(6)
                        .width(Length::Fill),
                    text("Port:").size(12).width(Length::Fixed(44.0)),
                    text_input("8000", &stream_cfg.port.to_string())
                        .on_input(Message::StreamPort)
                        .padding(6)
                        .width(Length::Fixed(110.0)),
                ]
                .spacing(8)
                .align_y(iced::Alignment::Center),
            );
            body = body.push(
                row![
                    text("Mount:").size(12).width(Length::Fixed(56.0)),
                    text_input("/stream", &stream_cfg.mount)
                        .on_input(Message::StreamMount)
                        .padding(6)
                        .width(Length::Fill),
                ]
                .spacing(8)
                .align_y(iced::Alignment::Center),
            );
            body = body.push(
                row![checkbox(stream_cfg.tls)
                    .label("TLS (https)")
                    .on_toggle(|_| Message::StreamTlsToggle),]
                .spacing(8),
            );
            body = body.push(text("Credentials").size(13));
            body = body.push(
                row![
                    text("Username:").size(12).width(Length::Fixed(76.0)),
                    text_input("source", &stream_cfg.username)
                        .on_input(Message::StreamUsername)
                        .padding(6)
                        .width(Length::Fill),
                ]
                .spacing(8)
                .align_y(iced::Alignment::Center),
            );
            body = body.push(
                row![
                    text("Password:").size(12).width(Length::Fixed(76.0)),
                    text_input("source password", &stream_cfg.password)
                        .on_input(Message::StreamPassword)
                        .padding(6)
                        .width(Length::Fill),
                    button(text("Clear").size(12))
                        .on_press(Message::StreamPasswordClear),
                ]
                .spacing(8)
                .align_y(iced::Alignment::Center),
            );
            body = body.push(
                text(stream_password_status(!stream_cfg.password.is_empty())).size(11),
            );
            body = body.push(text("Encoder").size(13));
            body = body.push(stepper(
                format!("Bitrate: {} kbps", stream_cfg.bitrate_kbps),
                Message::StreamBitrateDec,
                Message::StreamBitrateInc
            ));
            body = body.push(format_row.align_y(iced::Alignment::Center));
            body = body.push(text(format_note).size(11));
            container(body.spacing(14))
                .width(Length::Fill)
                .max_width(620.0)
                .into()
        }
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
            .spacing(8)
            .align_y(iced::Alignment::Center),
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
