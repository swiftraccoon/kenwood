use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, List, ListItem, ListState, Paragraph};

use crate::app::{App, McpState, Pane, SettingRow, cat_settings, mcp_settings};

fn bool_span(b: bool) -> (String, Color) {
    if b {
        ("On".into(), Color::Green)
    } else {
        ("Off".into(), Color::DarkGray)
    }
}

fn num_span(v: u8) -> (String, Color) {
    (format!("{v}"), Color::Yellow)
}

/// Render the CAT settings list (instant writes, no disconnect).
pub fn render_cat(app: &App, frame: &mut Frame, list_area: Rect, detail_area: Rect) {
    let rows = cat_settings();
    render_settings_list(
        app,
        frame,
        list_area,
        detail_area,
        &rows,
        app.settings_cat_index,
        " Settings (CAT — instant) [Enter: toggle, +/-: adjust] ",
    );
}

/// Render the MCP settings list (~3s per change, brief disconnect).
pub fn render_mcp(app: &App, frame: &mut Frame, list_area: Rect, detail_area: Rect) {
    let rows = mcp_settings();
    render_settings_list(
        app,
        frame,
        list_area,
        detail_area,
        &rows,
        app.settings_mcp_index,
        " Settings (MCP — ~3s per change) [Enter: toggle, +/-: adjust] ",
    );
}

fn render_settings_list(
    app: &App,
    frame: &mut Frame,
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
    lines.push(kv(" Backlight", &s.backlight.to_string()));
    lines.push(kv(" AF Gain", &s.af_gain.to_string()));
    lines.push(kv(" Beep", &on_off(s.beep)));
    lines.push(kv(" Lock", &on_off(!s.lock))); // CAT inverted on D75
    lines.push(kv(" Dual Band", &on_off(!s.dual_band))); // CAT inverted on D75
    lines.push(kv(" Bluetooth", &on_off(s.bluetooth)));
    lines.push(kv(" VOX", &on_off(s.vox)));
    lines.push(kv(" GPS", &on_off(s.gps_enabled)));
    lines.push(kv(" Beacon", &beacon_label(s.beacon_type)));
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
            .map_or("N/A".into(), |st| format!("{st:?}")),
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
            .map_or("N/A".into(), |st| format!("{st:?}")),
    ));
    lines.push(kv(" Attenuator", &on_off(s.band_b.attenuator)));
    lines.push(kv(" Squelch", &s.band_b.squelch.to_string()));

    frame.render_widget(Paragraph::new(lines).block(detail_block), detail_area);
}

/// Get the display value and color for a settings row.
fn get_row_value(app: &App, row: SettingRow) -> (String, Color) {
    match row {
        // --- RX (live CAT for squelch, MCP for filters) ---
        SettingRow::SquelchA => num_span(app.state.band_a.squelch),
        SettingRow::SquelchB => num_span(app.state.band_b.squelch),
        SettingRow::FmNarrow => mcp_num(app, |s| s.settings().fm_narrow()),
        SettingRow::SsbHighCut => mcp_num(app, |s| s.settings().ssb_high_cut()),
        SettingRow::CwHighCut => mcp_num(app, |s| s.settings().cw_high_cut()),
        SettingRow::AmHighCut => mcp_num(app, |s| s.settings().am_high_cut()),
        SettingRow::AutoFilter => mcp_num(app, |s| s.settings().auto_filter()),

        // --- Scan ---
        SettingRow::ScanResume => mcp_num(app, |s| s.settings().scan_resume()),
        SettingRow::DigitalScanResume => mcp_num(app, |s| s.settings().digital_scan_resume()),
        SettingRow::ScanRestartTime => mcp_num(app, |s| s.settings().scan_restart_time()),
        SettingRow::ScanRestartCarrier => mcp_num(app, |s| s.settings().scan_restart_carrier()),

        // --- TX ---
        SettingRow::TimeoutTimer => mcp_num(app, |s| s.settings().timeout_timer()),
        SettingRow::TxInhibit => mcp_bool(app, |s| s.settings().tx_inhibit()),
        SettingRow::BeatShift => mcp_bool(app, |s| s.settings().beat_shift()),

        // --- VOX (gain/delay: live CAT; rest: MCP) ---
        SettingRow::VoxEnabled => bool_span(app.state.vox),
        SettingRow::VoxGain => num_span(app.state.vox_gain),
        SettingRow::VoxDelay => (format!("{} (×100ms)", app.state.vox_delay), Color::Yellow),
        SettingRow::VoxTxOnBusy => mcp_bool(app, |s| s.settings().vox_tx_on_busy()),

        // --- CW ---
        SettingRow::CwBreakIn => mcp_bool(app, |s| s.settings().cw_break_in()),
        SettingRow::CwDelayTime => mcp_num(app, |s| s.settings().cw_delay_time()),
        SettingRow::CwPitch => mcp_num(app, |s| s.settings().cw_pitch()),

        // --- DTMF ---
        SettingRow::DtmfSpeed => mcp_num(app, |s| s.settings().dtmf_speed()),
        SettingRow::DtmfPauseTime => mcp_num(app, |s| s.settings().dtmf_pause_time()),
        SettingRow::DtmfTxHold => mcp_bool(app, |s| s.settings().dtmf_tx_hold()),

        // --- Repeater ---
        SettingRow::RepeaterAutoOffset => mcp_bool(app, |s| s.settings().repeater_auto_offset()),
        SettingRow::RepeaterCallKey => mcp_num(app, |s| s.settings().repeater_call_key()),

        // --- Auxiliary ---
        SettingRow::MicSensitivity => mcp_num(app, |s| s.settings().mic_sensitivity()),
        SettingRow::PfKey1 => mcp_num(app, |s| s.settings().pf_key1()),
        SettingRow::PfKey2 => mcp_num(app, |s| s.settings().pf_key2()),

        // --- Lock (Lock: live CAT inverted; rest: MCP) ---
        SettingRow::Lock => bool_span(!app.state.lock),
        SettingRow::KeyLockType => mcp_str(app, |s| match s.settings().key_lock_type_raw() {
            0 => "Key Only".into(),
            1 => "Key+PTT".into(),
            2 => "Key+PTT+Dial".into(),
            v => format!("{v}"),
        }),
        SettingRow::LockKeyA => mcp_bool(app, |s| s.settings().lock_key_a()),
        SettingRow::LockKeyB => mcp_bool(app, |s| s.settings().lock_key_b()),
        SettingRow::LockKeyC => mcp_bool(app, |s| s.settings().lock_key_c()),
        SettingRow::LockPtt => mcp_bool(app, |s| s.settings().lock_key_ptt()),
        SettingRow::AprsLock => mcp_bool(app, |s| s.settings().aprs_lock()),

        // --- Display (DualBand: live CAT inverted; rest: MCP) ---
        SettingRow::DualDisplaySize => mcp_num(app, |s| s.settings().dual_display_size()),
        SettingRow::DisplayArea => mcp_num(app, |s| s.settings().display_area()),
        SettingRow::InfoLine => mcp_num(app, |s| s.settings().info_line()),
        SettingRow::BacklightControl => mcp_num(app, |s| s.settings().backlight_control()),
        SettingRow::BacklightTimer => mcp_num(app, |s| s.settings().backlight_timer()),
        SettingRow::DisplayHoldTime => mcp_num(app, |s| s.settings().display_hold_time()),
        SettingRow::DisplayMethod => mcp_num(app, |s| s.settings().display_method()),
        SettingRow::PowerOnDisplay => mcp_num(app, |s| s.settings().power_on_display()),
        SettingRow::DualBand => bool_span(!app.state.dual_band),

        // --- Audio ---
        SettingRow::EmrVolumeLevel => mcp_num(app, |s| s.settings().emr_volume_level()),
        SettingRow::AutoMuteReturnTime => mcp_num(app, |s| s.settings().auto_mute_return_time()),
        SettingRow::Announce => mcp_bool(app, |s| s.settings().announce()),
        SettingRow::KeyBeep => mcp_bool(app, |s| s.settings().key_beep()),
        SettingRow::BeepVolume => mcp_num(app, |s| s.settings().beep_volume()),
        SettingRow::VoiceLanguage => mcp_num(app, |s| s.settings().voice_language()),
        SettingRow::VoiceVolume => mcp_num(app, |s| s.settings().voice_volume()),
        SettingRow::VoiceSpeed => mcp_num(app, |s| s.settings().voice_speed()),
        SettingRow::VolumeLock => mcp_bool(app, |s| s.settings().volume_lock()),

        // --- Units ---
        SettingRow::SpeedDistanceUnit => {
            mcp_str(app, |s| match s.settings().speed_distance_unit_raw() {
                0 => "mph".into(),
                1 => "km/h".into(),
                2 => "knots".into(),
                v => format!("{v}"),
            })
        }
        SettingRow::AltitudeRainUnit => mcp_str(app, |s| {
            if s.settings().altitude_rain_unit_raw() == 0 {
                "ft/in".into()
            } else {
                "m/mm".into()
            }
        }),
        SettingRow::TemperatureUnit => mcp_str(app, |s| {
            if s.settings().temperature_unit_raw() == 0 {
                "°F".into()
            } else {
                "°C".into()
            }
        }),

        // --- Bluetooth (Bluetooth: live CAT; BtAutoConnect: MCP) ---
        SettingRow::Bluetooth => bool_span(app.state.bluetooth),
        SettingRow::BtAutoConnect => mcp_bool(app, |s| s.settings().bt_auto_connect()),

        // --- Interface ---
        SettingRow::GpsBtInterface => mcp_num(app, |s| s.settings().gps_bt_interface()),
        SettingRow::PcOutputMode => mcp_num(app, |s| s.settings().pc_output_mode()),
        SettingRow::AprsUsbMode => mcp_num(app, |s| s.settings().aprs_usb_mode()),
        SettingRow::UsbAudioOutput => mcp_bool(app, |s| s.settings().usb_audio_output()),
        SettingRow::InternetLink => mcp_bool(app, |s| s.settings().internet_link()),

        // --- System ---
        SettingRow::Language => mcp_str(app, |s| {
            use kenwood_thd75::types::settings::Language;
            match s.settings().language() {
                Language::English => "English".into(),
                Language::Japanese => "Japanese".into(),
            }
        }),
        SettingRow::PowerOnMessageFlag => mcp_bool(app, |s| s.settings().power_on_message_flag()),

        // --- Battery ---
        SettingRow::BatterySaver => mcp_bool(app, |s| s.settings().battery_saver()),
        SettingRow::AutoPowerOff => mcp_str(app, |s| match s.settings().auto_power_off_raw() {
            0 => "Off".into(),
            1 => "30 min".into(),
            2 => "60 min".into(),
            3 => "90 min".into(),
            4 => "120 min".into(),
            v => format!("{v}"),
        }),

        // --- CAT Radio Controls ---
        SettingRow::PowerA => (format!("{}", app.state.band_a.power_level), Color::Yellow),
        SettingRow::PowerB => (format!("{}", app.state.band_b.power_level), Color::Yellow),
        SettingRow::AttenuatorA => bool_span(app.state.band_a.attenuator),
        SettingRow::AttenuatorB => bool_span(app.state.band_b.attenuator),
        SettingRow::ModeA => (format!("{}", app.state.band_a.mode), Color::Cyan),
        SettingRow::ModeB => (format!("{}", app.state.band_b.mode), Color::Cyan),
        SettingRow::ActiveBand => ("?".into(), Color::DarkGray),
        SettingRow::VfoMemModeA => ("?".into(), Color::DarkGray),
        SettingRow::VfoMemModeB => ("?".into(), Color::DarkGray),
        SettingRow::FmRadio => ("?".into(), Color::DarkGray),
        SettingRow::TncBaud => ("?".into(), Color::DarkGray),
        SettingRow::BeaconType => (beacon_label(app.state.beacon_type), Color::Yellow),
        SettingRow::GpsEnabled => bool_span(app.state.gps_enabled),
        SettingRow::GpsPcOutput => ("?".into(), Color::DarkGray),
        SettingRow::AutoInfo => ("?".into(), Color::DarkGray),
        SettingRow::CallsignSlot => ("?".into(), Color::DarkGray),
        SettingRow::DstarSlot => ("?".into(), Color::DarkGray),
    }
}

/// Read a boolean from the MCP image; returns ("?", DarkGray) if not loaded.
fn mcp_bool(app: &App, f: impl Fn(&kenwood_thd75::memory::MemoryImage) -> bool) -> (String, Color) {
    if let McpState::Loaded { ref image, .. } = app.mcp {
        bool_span(f(image))
    } else {
        ("?".into(), Color::DarkGray)
    }
}

/// Read a u8 from the MCP image; returns ("?", DarkGray) if not loaded.
fn mcp_num(app: &App, f: impl Fn(&kenwood_thd75::memory::MemoryImage) -> u8) -> (String, Color) {
    if let McpState::Loaded { ref image, .. } = app.mcp {
        num_span(f(image))
    } else {
        ("?".into(), Color::DarkGray)
    }
}

/// Read a string from the MCP image; returns ("?", DarkGray) if not loaded.
fn mcp_str(
    app: &App,
    f: impl Fn(&kenwood_thd75::memory::MemoryImage) -> String,
) -> (String, Color) {
    if let McpState::Loaded { ref image, .. } = app.mcp {
        (f(image), Color::Yellow)
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

fn beacon_label(b: u8) -> String {
    match b {
        0 => "Off".into(),
        1 => "Auto".into(),
        2 => "Manual".into(),
        _ => format!("{b}"),
    }
}
