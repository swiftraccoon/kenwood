// SPDX-FileCopyrightText: 2026 Swift Raccoon
// SPDX-License-Identifier: GPL-2.0-or-later OR GPL-3.0-or-later

//! Debug page, the engineering surface: event log and tools.

pub(crate) mod log;
pub(crate) mod tools;

use eframe::egui;

use crate::app::App;

/// Render the debug page: tools on the right, log filling the rest.
pub(crate) fn show(app: &mut App, ctx: &egui::Context) {
    egui::SidePanel::right("debug_tools")
        .resizable(true)
        .default_width(300.0)
        .show(ctx, |ui| {
            egui::ScrollArea::vertical().show(ui, |ui| {
                tools::show(app, ui);
            });
        });
    egui::CentralPanel::default().show(ctx, |ui| {
        log::show(app, ui);
    });
}
