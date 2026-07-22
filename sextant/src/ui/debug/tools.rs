// SPDX-FileCopyrightText: 2026 Swift Raccoon
// SPDX-License-Identifier: GPL-2.0-or-later OR GPL-3.0-or-later

//! Debug tools column: stream stats, TX test tools, audio devices.

use eframe::egui;

use crate::app::App;
use crate::theme;

/// Render the tools column.
pub(crate) fn show(app: &mut App, ui: &mut egui::Ui) {
    stream_section(app, ui);
    ui.separator();
    tx_tools_section(app, ui);
    ui.separator();
    audio_section(app, ui);
}

/// Live stream/link counters in monospace.
fn stream_section(app: &App, ui: &mut egui::Ui) {
    ui.label(theme::section_label("stream"));
    let stats = app.last_rx_stats.map_or_else(
        || "no stream yet".to_owned(),
        |s| format!("rx {} · lost {} · late {}", s.received, s.lost, s.late),
    );
    let heard = app
        .link_last_heard_secs
        .map_or_else(String::new, |s| format!("\nlink heard {s:.0}s ago"));
    ui.label(
        egui::RichText::new(format!("{stats}{heard}"))
            .font(egui::FontId::new(11.0, egui::FontFamily::Monospace))
            .color(theme::TEXT_SECONDARY),
    );
}

/// TX silence test + WAV play/transmit.
fn tx_tools_section(app: &mut App, ui: &mut egui::Ui) {
    ui.label(theme::section_label("tx tools"));
    let connected = matches!(app.status, crate::session::ConnStatus::Connected { .. });
    if ui
        .add_enabled(
            connected && !app.active_tx,
            egui::Button::new("TX silence 2s"),
        )
        .clicked()
    {
        app.tx_silence_test();
    }
    ui.label(theme::section_label("wav file"));
    let _resp = ui.text_edit_singleline(&mut app.wav_path);
    ui.horizontal(|ui| {
        if ui.button("Play locally").clicked() {
            app.play_wav();
        }
        if ui
            .add_enabled(connected, egui::Button::new("Transmit"))
            .clicked()
        {
            app.transmit_wav();
        }
    });
}

/// Device pickers, RX meter, record toggle.
fn audio_section(app: &mut App, ui: &mut egui::Ui) {
    ui.label(theme::section_label("audio"));
    let inputs = app.audio_state.inputs.clone();
    let outputs = app.audio_state.outputs.clone();
    let mut devices_changed = false;
    device_combo(
        ui,
        "dbg_in",
        "in",
        &mut app.input_device,
        &inputs,
        &mut devices_changed,
    );
    device_combo(
        ui,
        "dbg_out",
        "out",
        &mut app.output_device,
        &outputs,
        &mut devices_changed,
    );
    if devices_changed {
        app.apply_audio_devices();
    }
    if ui.small_button("Refresh device list").clicked() {
        app.refresh_devices();
    }
    ui.add_space(4.0);
    ui.horizontal(|ui| {
        ui.label(theme::section_label("rx"));
        theme::level_meter(ui, app.audio_state.rx_level, theme::OK);
    });
    ui.add_space(4.0);
    if app.audio_state.recording {
        if ui.button("■ Stop recording").clicked() {
            app.stop_recording();
        }
    } else if ui.button("● Record RX to WAV").clicked() {
        app.start_recording();
    }
}

/// One labelled device combo ("Default" when empty).
pub(crate) fn device_combo(
    ui: &mut egui::Ui,
    id: &str,
    label: &str,
    selected: &mut String,
    devices: &[String],
    changed: &mut bool,
) {
    ui.horizontal(|ui| {
        ui.label(theme::section_label(label));
        let display = if selected.is_empty() {
            "Default"
        } else {
            selected.as_str()
        };
        egui::ComboBox::from_id_salt(id)
            .selected_text(display)
            .width(200.0)
            .show_ui(ui, |ui| {
                if ui
                    .selectable_label(selected.is_empty(), "Default")
                    .clicked()
                {
                    selected.clear();
                    *changed = true;
                }
                for dev in devices {
                    if ui.selectable_label(selected == dev, dev).clicked() {
                        selected.clone_from(dev);
                        *changed = true;
                    }
                }
            });
    });
}
