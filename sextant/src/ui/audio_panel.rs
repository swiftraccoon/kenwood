// SPDX-FileCopyrightText: 2026 Swift Raccoon
// SPDX-License-Identifier: GPL-2.0-or-later OR GPL-3.0-or-later

//! Audio panel — device selection, level meters, recording, and
//! WAV playback / transmit-from-file controls.

use eframe::egui;

use crate::app::App;

/// Render the audio panel.
pub(crate) fn show(app: &mut App, ui: &mut egui::Ui) {
    // Device pickers. The lists are cloned out first so the combo
    // closures don't hold a borrow on `app` while it's also mutated.
    let inputs = app.audio_state.inputs.clone();
    let outputs = app.audio_state.outputs.clone();
    let mut devices_changed = false;

    ui.label("Input device");
    egui::ComboBox::from_id_salt("audio_input")
        .selected_text(device_label(&app.input_device))
        .show_ui(ui, |ui| {
            if ui
                .selectable_label(app.input_device.is_empty(), "Default")
                .clicked()
            {
                app.input_device.clear();
                devices_changed = true;
            }
            for dev in &inputs {
                if ui.selectable_label(&app.input_device == dev, dev).clicked() {
                    app.input_device.clone_from(dev);
                    devices_changed = true;
                }
            }
        });

    ui.label("Output device");
    egui::ComboBox::from_id_salt("audio_output")
        .selected_text(device_label(&app.output_device))
        .show_ui(ui, |ui| {
            if ui
                .selectable_label(app.output_device.is_empty(), "Default")
                .clicked()
            {
                app.output_device.clear();
                devices_changed = true;
            }
            for dev in &outputs {
                if ui
                    .selectable_label(&app.output_device == dev, dev)
                    .clicked()
                {
                    app.output_device.clone_from(dev);
                    devices_changed = true;
                }
            }
        });
    if devices_changed {
        app.apply_audio_devices();
    }
    if ui.button("Refresh device list").clicked() {
        app.refresh_devices();
    }

    ui.separator();
    ui.label("Levels");
    ui.add(egui::ProgressBar::new(app.audio_state.tx_level).text("TX"));
    ui.add(egui::ProgressBar::new(app.audio_state.rx_level).text("RX"));

    ui.separator();
    if app.audio_state.recording {
        if ui.button("Stop recording").clicked() {
            app.stop_recording();
        }
    } else if ui.button("Record RX to WAV").clicked() {
        app.start_recording();
    }

    ui.separator();
    ui.label("WAV file");
    ui.text_edit_singleline(&mut app.wav_path);
    ui.horizontal(|ui| {
        if ui.button("Play locally").clicked() {
            app.play_wav();
        }
        if ui.button("Transmit").clicked() {
            app.transmit_wav();
        }
    });
}

/// Display label for a device choice — the device name, or "Default"
/// when no specific device is selected.
const fn device_label(name: &str) -> &str {
    if name.is_empty() { "Default" } else { name }
}
