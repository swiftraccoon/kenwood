use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, List, ListItem, ListState, Paragraph};

use kenwood_thd75::memory::{MemoryImage, SettingsValueError};
use kenwood_thd75::types::{
    AltitudeRainUnit, AutoPowerOff, BatterySaverInterval, DtmfPause, Language, MicSensitivity,
    PcOutputInterface, SpeedDistanceUnit, StoredFrontPanelPfAssignment, TemperatureUnit,
    VoiceAnnounceMode,
};

use crate::app::{App, McpState, Pane, SettingRow, cat_settings, mcp_settings};

fn bool_span(b: bool) -> (String, Color) {
    if b {
        ("On".into(), Color::Green)
    } else {
        ("Off".into(), Color::DarkGray)
    }
}

fn num_span(v: impl std::fmt::Display) -> (String, Color) {
    (format!("{v}"), Color::Yellow)
}

/// Render the CAT settings list (instant writes, no disconnect).
pub(crate) fn render_cat(app: &App, frame: &mut Frame<'_>, list_area: Rect, detail_area: Rect) {
    let rows = cat_settings();
    render_settings_list(
        app,
        frame,
        list_area,
        detail_area,
        &rows,
        app.settings_cat_index,
        " Settings (CAT, instant) [Enter: toggle, +/-: adjust] ",
    );
}

/// Render the MCP settings list (~3s per change, brief disconnect).
pub(crate) fn render_mcp(app: &App, frame: &mut Frame<'_>, list_area: Rect, detail_area: Rect) {
    let rows = mcp_settings();
    render_settings_list(
        app,
        frame,
        list_area,
        detail_area,
        &rows,
        app.settings_mcp_index,
        " Settings (MCP, ~3s per change) [Enter: toggle, +/-: adjust] ",
    );
}

#[expect(
    clippy::too_many_lines,
    reason = "Walks the CAT/MCP settings table and builds the list widget row-by-row, \
              where each row's rendering depends on the setting's type. Splitting \
              per-setting-type would thread a row+setting context through multiple \
              helpers for no reader benefit; the inline match stays aligned with the \
              settings-table order."
)]
fn render_settings_list(
    app: &App,
    frame: &mut Frame<'_>,
    list_area: Rect,
    detail_area: Rect,
    rows: &[SettingRow],
    selected_index: usize,
    title: &str,
) {
    let block = Block::default()
        .title(title.to_string())
        .borders(Borders::ALL)
        .border_style(super::border_style(app, Pane::Main));

    let detail_block = Block::default()
        .title(" Radio Info (live) ")
        .borders(Borders::ALL)
        .border_style(super::border_style(app, Pane::Detail));

    // Build list items: section headers interspersed with setting rows.
    // selected_index tracks the rows slice index; we map it to the ListItem index
    // (which is larger due to the interleaved section headers) for ListState scrolling.

    // Compute the ListItem index for the currently selected row.
    let selected_list_idx = {
        let mut list_item_idx = 0usize;
        let mut found = None;
        for (row_idx, &row) in rows.iter().enumerate() {
            if row.section_header().is_some() {
                list_item_idx += 1; // header item
            }
            if row_idx == selected_index {
                found = Some(list_item_idx);
                break;
            }
            list_item_idx += 1;
        }
        found
    };

    let list_items: Vec<ListItem<'_>> =
        if matches!(app.mcp, McpState::Loaded { .. }) || rows.iter().any(|r| r.is_cat()) {
            let mut result = Vec::new();
            for (idx, &row) in rows.iter().enumerate() {
                // Section header if this row starts a new group
                if let Some(header) = row.section_header() {
                    result.push(ListItem::new(Line::from(vec![Span::styled(
                        format!(" {header}"),
                        Style::default()
                            .fg(Color::Cyan)
                            .add_modifier(Modifier::BOLD),
                    )])));
                }

                let (val, color) = get_row_value(app, row);
                let hint = if row.is_numeric() { " [+/-]" } else { "" };
                let selected_marker = if idx == selected_index {
                    "\u{25b8} "
                } else {
                    "  "
                };
                result.push(ListItem::new(Line::from(vec![
                    Span::styled(
                        format!("{selected_marker}{:<22}", row.label()),
                        Style::default().fg(Color::White),
                    ),
                    Span::styled(val, Style::default().fg(color)),
                    Span::styled(hint.to_string(), Style::default().fg(Color::DarkGray)),
                ])));
            }
            result
        } else {
            vec![ListItem::new(" No MCP data loaded. Press [m] then [r].")]
        };

    // Use ListState to scroll the list so the selected item is visible.
    // The selected item in ListState is the ListItem index, not the row slice index.
    let mut list_state = ListState::default();
    list_state.select(selected_list_idx);

    let list = List::new(list_items).block(block).highlight_style(
        Style::default()
            .fg(Color::Black)
            .bg(Color::Cyan)
            .add_modifier(Modifier::BOLD),
    );

    frame.render_stateful_widget(list, list_area, &mut list_state);

    // Right pane: live radio state from CAT commands (read-only display)
    let s = &app.state;
    let mut lines: Vec<Line<'_>> = Vec::new();

    lines.push(Line::from(Span::styled(
        " Radio Identity",
        Style::default()
            .fg(Color::Yellow)
            .add_modifier(Modifier::BOLD),
    )));
    lines.push(kv(" Firmware", &s.firmware_version));
    lines.push(kv(" Type", &s.radio_type));
    lines.push(kv(" Port", &app.port_path));
    lines.push(Line::from(""));

    lines.push(Line::from(Span::styled(
        " Live CAT State",
        Style::default()
            .fg(Color::Yellow)
            .add_modifier(Modifier::BOLD),
    )));
    lines.push(kv(" Battery", &{
        use kenwood_thd75::types::BatteryLevel;
        match s.battery_level {
            BatteryLevel::Empty => "Empty (Red)".to_string(),
            BatteryLevel::OneThird => "1/3 (Yellow)".to_string(),
            BatteryLevel::TwoThirds => "2/3 (Green)".to_string(),
            BatteryLevel::Full => "Full (Green)".to_string(),
            BatteryLevel::Charging => "Charging".to_string(),
            BatteryLevel::Unidentified5 => "State 5 (unqualified)".to_string(),
        }
    }));
    lines.push(kv(" AF Gain", &format!("{}", s.af_gain)));
    lines.push(kv(" Beep", &on_off(s.beep)));
    lines.push(kv(" Band Mode", &s.band_mode.to_string()));
    lines.push(kv(" Bluetooth", &on_off(s.bluetooth)));
    lines.push(kv(" VOX", &on_off(s.vox)));
    lines.push(kv(
        " GPS",
        &s.gps_settings.map_or_else(
            || "Unavailable".to_owned(),
            |settings| on_off(settings.enabled()),
        ),
    ));
    lines.push(kv(" Beacon", &format!("{}", s.beacon_mode)));
    lines.push(kv(
        " Fine Step",
        &s.fine_step
            .map_or_else(|| "N/A".to_string(), |fs| format!("{fs}")),
    ));
    lines.push(kv(
        " Filter SSB",
        &s.filter_width_ssb
            .map_or_else(|| "N/A".to_string(), |w| format!("{w}")),
    ));
    lines.push(kv(
        " Filter CW",
        &s.filter_width_cw
            .map_or_else(|| "N/A".to_string(), |w| format!("{w}")),
    ));
    lines.push(kv(
        " Filter AM",
        &s.filter_width_am
            .map_or_else(|| "N/A".to_string(), |w| format!("{w}")),
    ));
    lines.push(Line::from(""));

    lines.push(Line::from(Span::styled(
        " Band A",
        Style::default()
            .fg(Color::Yellow)
            .add_modifier(Modifier::BOLD),
    )));
    lines.push(kv(
        " Step",
        &s.band_a
            .step_size
            .map_or_else(|| "N/A".into(), |st| format!("{st}")),
    ));
    lines.push(kv(" Attenuator", &on_off(s.band_a.attenuator)));
    lines.push(kv(" Squelch", &s.band_a.squelch.to_string()));
    lines.push(Line::from(""));

    lines.push(Line::from(Span::styled(
        " Band B",
        Style::default()
            .fg(Color::Yellow)
            .add_modifier(Modifier::BOLD),
    )));
    lines.push(kv(
        " Step",
        &s.band_b
            .step_size
            .map_or_else(|| "N/A".into(), |st| format!("{st}")),
    ));
    lines.push(kv(" Attenuator", &on_off(s.band_b.attenuator)));
    lines.push(kv(" Squelch", &s.band_b.squelch.to_string()));
    lines.push(Line::from(""));

    lines.push(Line::from(Span::styled(
        " Not Available via CAT/MCP",
        Style::default()
            .fg(Color::DarkGray)
            .add_modifier(Modifier::ITALIC),
    )));
    lines.push(Line::from(Span::styled(
        " Recording: radio-button only",
        Style::default().fg(Color::DarkGray),
    )));
    lines.push(Line::from(Span::styled(
        " (no known MCP offset for Menu 301/302)",
        Style::default().fg(Color::DarkGray),
    )));

    frame.render_widget(Paragraph::new(lines).block(detail_block), detail_area);
}

/// Get the display value and color for a settings row.
#[expect(
    clippy::too_many_lines,
    reason = "One match arm per settings-table row: each arm formats a different radio \
              parameter type. The flat match mirrors the settings-table order; splitting \
              per-type adds indirection without reducing real complexity."
)]
fn get_row_value(app: &App, row: SettingRow) -> (String, Color) {
    match row {
        // --- RX (live CAT for squelch, MCP for filters) ---
        SettingRow::SquelchA => num_span(app.state.band_a.squelch.as_raw()),
        SettingRow::SquelchB => num_span(app.state.band_b.squelch.as_raw()),
        SettingRow::StepSizeA => (
            app.state
                .band_a
                .step_size
                .map_or_else(|| "N/A".into(), |st| format!("{st}")),
            Color::Yellow,
        ),
        SettingRow::StepSizeB => (
            app.state
                .band_b
                .step_size
                .map_or_else(|| "N/A".into(), |st| format!("{st}")),
            Color::Yellow,
        ),
        SettingRow::FineStep => (
            app.state
                .fine_step
                .map_or_else(|| "N/A".into(), |fs| format!("{fs}")),
            Color::Yellow,
        ),
        SettingRow::FilterWidthSsb => (
            app.state
                .filter_width_ssb
                .map_or_else(|| "N/A".into(), |w| format!("{w}")),
            Color::Yellow,
        ),
        SettingRow::FilterWidthCw => (
            app.state
                .filter_width_cw
                .map_or_else(|| "N/A".into(), |w| format!("{w}")),
            Color::Yellow,
        ),
        SettingRow::FilterWidthAm => (
            app.state
                .filter_width_am
                .map_or_else(|| "N/A".into(), |w| format!("{w}")),
            Color::Yellow,
        ),
        SettingRow::SsbHighCut => mcp_str(app, |s| {
            s.settings()
                .ssb_high_cut()
                .map(|value| format!("{value:?}"))
        }),
        SettingRow::CwWidth => mcp_str(app, |s| {
            s.settings().cw_width().map(|value| format!("{value:?}"))
        }),
        SettingRow::AmHighCut => mcp_str(app, |s| {
            s.settings().am_high_cut().map(|value| format!("{value:?}"))
        }),

        // --- Scan ---
        SettingRow::ScanResume => mcp_str(app, |s| {
            s.settings().scan_resume().map(|value| format!("{value:?}"))
        }),
        SettingRow::DigitalScanResume => mcp_str(app, |s| {
            s.settings()
                .digital_scan_resume()
                .map(|value| format!("{value:?}"))
        }),
        SettingRow::ScanRestartTime => mcp_str(app, |s| {
            s.settings()
                .scan_restart_time()
                .map(|value| format!("{} s", value.as_seconds()))
        }),
        SettingRow::ScanRestartCarrier => mcp_str(app, |s| {
            s.settings()
                .scan_restart_carrier()
                .map(|value| format!("{} s", value.as_seconds()))
        }),

        // --- TX ---
        SettingRow::TimeoutTimer => mcp_str(app, |s| {
            s.settings()
                .timeout_timer()
                .map(|value| format!("{} s", value.as_seconds()))
        }),
        SettingRow::TxInhibit => mcp_bool(app, |s| s.settings().tx_inhibit()),
        SettingRow::BeatShift => mcp_str(app, |s| {
            s.settings()
                .beat_shift()
                .map(|value| format!("Type {}", u8::from(value) + 1))
        }),

        // --- VOX (gain/delay: live CAT; rest: MCP) ---
        SettingRow::VoxEnabled => bool_span(app.state.vox),
        SettingRow::VoxGain => num_span(app.state.vox_gain.as_raw()),
        SettingRow::VoxDelay => (format!("{}", app.state.vox_delay), Color::Yellow),
        SettingRow::VoxTxOnBusy => mcp_bool(app, |s| s.settings().vox_tx_on_busy()),

        // --- CW ---
        SettingRow::CwPitch => mcp_str(app, |s| {
            s.settings()
                .cw_pitch()
                .map(|value| format!("{} Hz", value.as_hz()))
        }),

        // --- DTMF ---
        SettingRow::DtmfSpeed => mcp_str(app, |s| {
            s.settings().dtmf_speed().map(|value| format!("{value:?}"))
        }),
        SettingRow::DtmfPauseTime => mcp_str(app, |s| {
            s.settings().dtmf_pause_time().map(|value| match value {
                DtmfPause::Ms100 => "100 ms".to_owned(),
                DtmfPause::Ms250 => "250 ms".to_owned(),
                DtmfPause::Ms500 => "500 ms".to_owned(),
                DtmfPause::Ms750 => "750 ms".to_owned(),
                DtmfPause::Ms1000 => "1000 ms".to_owned(),
                DtmfPause::Ms1500 => "1500 ms".to_owned(),
                DtmfPause::Ms2000 => "2000 ms".to_owned(),
            })
        }),
        SettingRow::DtmfTxHold => mcp_bool(app, |s| s.settings().dtmf_tx_hold()),

        // --- Repeater ---
        SettingRow::RepeaterAutoOffset => mcp_bool(app, |s| s.settings().repeater_auto_offset()),
        SettingRow::RepeaterCallKey => mcp_str(app, |s| {
            s.settings()
                .repeater_call_key()
                .map(|value| format!("{value:?}"))
        }),

        // --- Auxiliary ---
        SettingRow::MicSensitivity => mcp_str(app, |s| {
            s.settings().mic_sensitivity().map(|value| match value {
                MicSensitivity::High => "High".to_owned(),
                MicSensitivity::Medium => "Medium".to_owned(),
                MicSensitivity::Low => "Low".to_owned(),
            })
        }),
        SettingRow::PfKey1 => mcp_str(app, |s| s.settings().pf_key1().map(format_pf_assignment)),
        SettingRow::PfKey2 => mcp_str(app, |s| s.settings().pf_key2().map(format_pf_assignment)),

        // --- Lock (MCP) ---
        SettingRow::KeyLock => mcp_bool(app, |s| s.settings().key_lock()),
        SettingRow::FrequencyLock => mcp_bool(app, |s| s.settings().frequency_lock()),
        SettingRow::AprsLockFrequency => mcp_bool(app, |s| s.settings().aprs_lock_frequency()),
        SettingRow::AprsLockPtt => mcp_bool(app, |s| s.settings().aprs_lock_ptt()),
        SettingRow::AprsLockKey => mcp_bool(app, |s| s.settings().aprs_lock_key()),

        // --- Display (DualBand: live CAT; rest: MCP) ---
        SettingRow::BacklightControl => mcp_str(app, |s| {
            s.settings()
                .backlight_control()
                .map(|value| format!("{value:?}"))
        }),
        SettingRow::BacklightTimer => mcp_str(app, |s| {
            s.settings()
                .backlight_timer()
                .map(|value| format!("{} s", value.as_seconds()))
        }),
        SettingRow::DualBand => (app.state.band_mode.to_string(), Color::Yellow),

        // --- Audio ---
        SettingRow::EmrVolumeLevel => mcp_num(app, |s| {
            s.settings()
                .emr_volume_level()
                .map(kenwood_thd75::types::EmrVolume::as_raw)
        }),
        SettingRow::AutoMuteReturnTime => mcp_str(app, |s| {
            s.settings()
                .auto_mute_return_time()
                .map(|value| format!("{} s", value.as_seconds()))
        }),
        SettingRow::Announce => mcp_str(app, |s| {
            s.settings().announce().map(|value| match value {
                VoiceAnnounceMode::Off => "Off".to_owned(),
                VoiceAnnounceMode::Manual => "Manual".to_owned(),
                VoiceAnnounceMode::Auto1 => "Auto1".to_owned(),
                VoiceAnnounceMode::Auto2 => "Auto2".to_owned(),
            })
        }),
        SettingRow::KeyBeep => mcp_bool(app, |s| s.settings().key_beep()),
        SettingRow::BeepVolume => mcp_str(app, |s| {
            s.settings().beep_volume().map(|value| {
                value
                    .fixed_level()
                    .map_or_else(|| "VOL Link".to_owned(), |level| format!("Level {level}"))
            })
        }),
        SettingRow::VoiceVolume => mcp_str(app, |s| {
            s.settings().voice_volume().map(|value| {
                value
                    .fixed_level()
                    .map_or_else(|| "VOL Link".to_owned(), |level| format!("Level {level}"))
            })
        }),
        SettingRow::VoiceSpeed => mcp_str(app, |s| {
            s.settings()
                .voice_speed()
                .map(|value| format!("Speed {}", u8::from(value) + 1))
        }),
        SettingRow::VolumeLock => mcp_bool(app, |s| s.settings().volume_lock()),

        // --- Units ---
        SettingRow::SpeedDistanceUnit => mcp_str(app, |s| {
            s.settings()
                .display_units()
                .map(|units| match units.speed_distance {
                    SpeedDistanceUnit::MilesPerHour => "mph".to_owned(),
                    SpeedDistanceUnit::KilometersPerHour => "km/h".to_owned(),
                    SpeedDistanceUnit::Knots => "knots".to_owned(),
                })
        }),
        SettingRow::AltitudeRainUnit => mcp_str(app, |s| {
            s.settings()
                .display_units()
                .map(|units| match units.altitude_rain {
                    AltitudeRainUnit::FeetInch => "ft/in".to_owned(),
                    AltitudeRainUnit::MetersMm => "m/mm".to_owned(),
                })
        }),
        SettingRow::TemperatureUnit => mcp_str(app, |s| {
            s.settings()
                .display_units()
                .map(|units| match units.temperature {
                    TemperatureUnit::Fahrenheit => "°F".to_owned(),
                    TemperatureUnit::Celsius => "°C".to_owned(),
                })
        }),

        // --- Bluetooth (Bluetooth: live CAT; BtAutoConnect: MCP) ---
        SettingRow::Bluetooth => bool_span(app.state.bluetooth),
        SettingRow::BtAutoConnect => mcp_bool(app, |s| s.settings().bt_auto_connect()),

        // --- Interface ---
        SettingRow::GpsBtInterface => mcp_str(app, |s| {
            s.settings()
                .gps_pc_output_interface()
                .map(format_pc_interface)
        }),
        SettingRow::AprsUsbMode => mcp_str(app, |s| {
            s.settings()
                .aprs_pc_output_interface()
                .map(format_pc_interface)
        }),

        // --- System ---
        SettingRow::Language => mcp_str(app, |s| {
            s.settings().language().map(|value| match value {
                Language::English => "English".to_owned(),
                Language::Japanese => "Japanese".to_owned(),
            })
        }),

        // --- Battery ---
        SettingRow::BatterySaver => mcp_str(app, |s| {
            s.settings().battery_saver().map(|value| match value {
                BatterySaverInterval::Off => "Off".to_owned(),
                BatterySaverInterval::Seconds0_2 => "0.2 s".to_owned(),
                BatterySaverInterval::Seconds0_4 => "0.4 s".to_owned(),
                BatterySaverInterval::Seconds0_6 => "0.6 s".to_owned(),
                BatterySaverInterval::Seconds0_8 => "0.8 s".to_owned(),
                BatterySaverInterval::Seconds1 => "1.0 s".to_owned(),
                BatterySaverInterval::Seconds2 => "2.0 s".to_owned(),
                BatterySaverInterval::Seconds3 => "3.0 s".to_owned(),
                BatterySaverInterval::Seconds4 => "4.0 s".to_owned(),
                BatterySaverInterval::Seconds5 => "5.0 s".to_owned(),
            })
        }),
        SettingRow::AutoPowerOff => mcp_str(app, |s| {
            s.settings().auto_power_off().map(|value| match value {
                AutoPowerOff::Off => "Off".to_owned(),
                AutoPowerOff::Min15 => "15 min".to_owned(),
                AutoPowerOff::Min30 => "30 min".to_owned(),
                AutoPowerOff::Min60 => "60 min".to_owned(),
            })
        }),

        // --- CAT Radio Controls ---
        SettingRow::PowerA => (format!("{}", app.state.band_a.power_level), Color::Yellow),
        SettingRow::PowerB => (format!("{}", app.state.band_b.power_level), Color::Yellow),
        SettingRow::AttenuatorA => bool_span(app.state.band_a.attenuator),
        SettingRow::AttenuatorB => bool_span(app.state.band_b.attenuator),
        SettingRow::OperatingModeA => (format!("{}", app.state.band_a.mode), Color::Cyan),
        SettingRow::OperatingModeB => (format!("{}", app.state.band_b.mode), Color::Cyan),
        SettingRow::BeaconMode => (format!("{}", app.state.beacon_mode), Color::Yellow),
        SettingRow::GpsEnabled => app.state.gps_settings.map_or_else(
            || ("Unavailable".into(), Color::DarkGray),
            |settings| bool_span(settings.enabled()),
        ),
        SettingRow::GpsPcOutput => app.state.gps_settings.map_or_else(
            || ("Unavailable".into(), Color::DarkGray),
            |settings| bool_span(settings.pc_output()),
        ),
        SettingRow::ScanResumeCat => ("Quarantined".into(), Color::Red),
        SettingRow::ActiveBand
        | SettingRow::TuningModeA
        | SettingRow::TuningModeB
        | SettingRow::FmRadio
        | SettingRow::PacketDataRate
        | SettingRow::AutoInfo
        | SettingRow::DstarSlot => ("?".into(), Color::DarkGray),
        SettingRow::AprsCallsign => app.state.aprs_callsign.as_ref().map_or_else(
            || ("?".into(), Color::DarkGray),
            |callsign| (callsign.to_string(), Color::Yellow),
        ),
    }
}

fn format_pf_assignment(value: StoredFrontPanelPfAssignment) -> String {
    match value {
        StoredFrontPanelPfAssignment::Official(function) => format!("{function:?}"),
        StoredFrontPanelPfAssignment::OffMenu(_) => {
            format!("Off-menu byte 0x{:02X}", value.as_raw())
        }
    }
}

fn format_pc_interface(value: PcOutputInterface) -> String {
    match value {
        PcOutputInterface::Usb => "USB".to_owned(),
        PcOutputInterface::Bluetooth => "Bluetooth".to_owned(),
    }
}

/// Read a strict boolean from the MCP image. Invalid stored bytes are rendered
/// as errors rather than being interpreted as false.
fn mcp_bool(
    app: &App,
    f: impl Fn(&MemoryImage) -> Result<bool, SettingsValueError>,
) -> (String, Color) {
    if let McpState::Loaded { ref image, .. } = app.mcp {
        match f(image) {
            Ok(value) => bool_span(value),
            Err(error) => (format!("Error: {error}"), Color::Red),
        }
    } else {
        ("?".into(), Color::DarkGray)
    }
}

/// Read a strict numeric value from the MCP image.
fn mcp_num(
    app: &App,
    f: impl Fn(&MemoryImage) -> Result<u8, SettingsValueError>,
) -> (String, Color) {
    if let McpState::Loaded { ref image, .. } = app.mcp {
        match f(image) {
            Ok(value) => num_span(value),
            Err(error) => (format!("Error: {error}"), Color::Red),
        }
    } else {
        ("?".into(), Color::DarkGray)
    }
}

/// Read and format a strict typed value from the MCP image.
fn mcp_str(
    app: &App,
    f: impl Fn(&MemoryImage) -> Result<String, SettingsValueError>,
) -> (String, Color) {
    if let McpState::Loaded { ref image, .. } = app.mcp {
        match f(image) {
            Ok(value) => (value, Color::Yellow),
            Err(error) => (format!("Error: {error}"), Color::Red),
        }
    } else {
        ("?".into(), Color::DarkGray)
    }
}

fn kv<'a>(label: &'a str, value: &str) -> Line<'a> {
    Line::from(vec![
        Span::styled(format!("{label:<16}"), Style::default().fg(Color::DarkGray)),
        Span::styled(value.to_string(), Style::default().fg(Color::White)),
    ])
}

fn on_off(b: bool) -> String {
    if b { "On".into() } else { "Off".into() }
}
