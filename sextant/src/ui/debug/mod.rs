// SPDX-FileCopyrightText: 2026 Swift Raccoon
// SPDX-License-Identifier: GPL-2.0-or-later OR GPL-3.0-or-later

//! Debug page, the engineering surface: event log and tools.

use eframe::egui;

use crate::app::App;

pub(crate) mod log;
pub(crate) mod tools;

/// Render the debug page: tools on the right, log filling the rest.
pub(crate) fn show(app: &mut App, ui: &mut egui::Ui) {
    egui::Panel::right("debug_tools")
        .resizable(true)
        .default_size(300.0)
        .show(ui, |ui| {
            egui::ScrollArea::vertical().show(ui, |ui| {
                tools::show(app, ui);
            });
        });
    egui::CentralPanel::default().show(ui, |ui| {
        log::show(app, ui);
    });
}
