// SPDX-FileCopyrightText: 2026 Swift Raccoon
// SPDX-License-Identifier: GPL-2.0-or-later OR GPL-3.0-or-later

//! Transmit panel — TX silence test, PTT hint, and the operator's
//! slow-data text + GPS-beacon inputs.
//!
//! The primary PTT toggle lives in the persistent top bar; this panel
//! holds the secondary transmit controls.

use eframe::egui;

use crate::app::App;
use crate::session::ConnStatus;

/// Render the secondary transmit controls and slow-data inputs.
pub(crate) fn show(app: &mut App, ui: &mut egui::Ui) {
    let connected = matches!(app.status, ConnStatus::Connected { .. });
    if ui
        .add_enabled(
            connected && !app.active_tx,
            egui::Button::new("TX silence (2 s)"),
        )
        .clicked()
    {
        app.tx_silence_test();
    }
    ui.label(
        egui::RichText::new("(hold Space to PTT)")
            .small()
            .color(egui::Color32::from_gray(140)),
    );

    ui.separator();
    ui.label("Slow-data text (≤20 chars)");
    if ui.text_edit_singleline(&mut app.tx_slow_text).changed() {
        app.push_slow_data();
    }

    ui.separator();
    let mut gps_changed = ui
        .checkbox(&mut app.tx_gps.enabled, "Beacon GPS position")
        .changed();
    ui.add_enabled_ui(app.tx_gps.enabled, |ui| {
        egui::Grid::new("tx_gps_form")
            .num_columns(2)
            .show(ui, |ui| {
                ui.label("Latitude");
                gps_changed |= ui.text_edit_singleline(&mut app.tx_gps.lat).changed();
                ui.end_row();
                ui.label("Longitude");
                gps_changed |= ui.text_edit_singleline(&mut app.tx_gps.lon).changed();
                ui.end_row();
                ui.label("Symbol");
                gps_changed |= ui.text_edit_singleline(&mut app.tx_gps.symbol).changed();
                ui.end_row();
                ui.label("Comment");
                gps_changed |= ui.text_edit_singleline(&mut app.tx_gps.comment).changed();
                ui.end_row();
            });
    });
    if gps_changed {
        app.push_slow_data();
    }
}
