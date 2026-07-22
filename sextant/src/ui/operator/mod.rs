// SPDX-FileCopyrightText: 2026 Swift Raccoon
// SPDX-License-Identifier: GPL-2.0-or-later OR GPL-3.0-or-later

//! Operator page: the clean everyday face of the app.

pub(crate) mod connect_sheet;
pub(crate) mod heard;
pub(crate) mod hero;
pub(crate) mod tx_strip;

use eframe::egui;

use crate::app::App;

/// The operator deck: hero display (left, 55 %) beside the heard
/// list, separated by a hairline.
///
/// Both columns are explicitly allocated at the full panel height.
/// `ui.horizontal` starts as a single interact-row and centers
/// children vertically as it grows, which starved the heard list's
/// scroll area of its real height (only a few rows were reachable
/// unless the window was very tall).
pub(crate) fn show_deck(app: &mut App, ui: &mut egui::Ui) {
    let hero_width = ui.available_width() * 0.55;
    let height = ui.available_height();
    ui.horizontal_top(|ui| {
        ui.allocate_ui_with_layout(
            egui::vec2(hero_width, height),
            egui::Layout::top_down(egui::Align::Center),
            |ui| hero::show(app, ui),
        );
        ui.separator();
        ui.allocate_ui_with_layout(
            egui::vec2(ui.available_width(), height),
            egui::Layout::top_down(egui::Align::Min),
            |ui| heard::show(app, ui),
        );
    });
}
