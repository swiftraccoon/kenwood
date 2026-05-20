// SPDX-FileCopyrightText: 2026 Swift Raccoon
// SPDX-License-Identifier: GPL-2.0-or-later OR GPL-3.0-or-later

//! Receive panel — incoming-stream status, slow-data, GPS, heard-list.

use eframe::egui;

use crate::app::App;

/// Render the receive-side display.
pub(crate) fn show(app: &App, ui: &mut egui::Ui) {
    ui.horizontal(|ui| {
        ui.label("Last slow-data message:");
        let text = app.last_slow_data.as_deref().unwrap_or("—");
        ui.colored_label(egui::Color32::LIGHT_GREEN, text);
    });

    ui.horizontal(|ui| {
        ui.label("Last GPS position:");
        let text = app.last_gps.map_or_else(
            || "—".to_owned(),
            |(lat, lon)| format!("{lat:.4}, {lon:.4}"),
        );
        ui.colored_label(egui::Color32::LIGHT_GREEN, text);
    });

    ui.separator();
    ui.label("Heard stations");
    egui::ScrollArea::vertical()
        .id_salt("heard_scroll")
        .auto_shrink([false, false])
        .max_height(160.0)
        .show(ui, |ui| {
            let recent = app.heard.recent();
            if recent.is_empty() {
                ui.label("—");
            }
            for station in recent {
                let gps = station
                    .last_gps
                    .map_or_else(String::new, |(la, lo)| format!("  @ {la:.3},{lo:.3}"));
                let msg = station
                    .last_message
                    .as_deref()
                    .map_or_else(String::new, |m| format!("  {m:?}"));
                ui.label(format!(
                    "{}  ({}×){}{}",
                    station.callsign, station.stream_count, gps, msg
                ));
            }
        });
}
