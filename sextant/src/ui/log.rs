// SPDX-FileCopyrightText: 2026 Swift Raccoon
// SPDX-License-Identifier: GPL-2.0-or-later OR GPL-3.0-or-later

//! Log panel — last-error banner and the scrolling event log.

use eframe::egui;

use crate::app::{App, LogLevel};

/// Render the last-error banner and the event-log scroll area.
pub(crate) fn show(app: &App, ui: &mut egui::Ui) {
    if let Some(err) = &app.last_error {
        ui.colored_label(egui::Color32::LIGHT_RED, format!("Last error: {err}"));
    }

    ui.separator();

    ui.label("Event log");
    egui::ScrollArea::vertical()
        .auto_shrink([false, false])
        .stick_to_bottom(true)
        .max_height(220.0)
        .show(ui, |ui| {
            for line in &app.log {
                let colour = match line.level {
                    LogLevel::Info => egui::Color32::from_gray(200),
                    LogLevel::Event => egui::Color32::LIGHT_BLUE,
                    LogLevel::Error => egui::Color32::LIGHT_RED,
                };
                ui.colored_label(colour, &line.text);
            }
        });
}
