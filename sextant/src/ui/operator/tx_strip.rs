// SPDX-FileCopyrightText: 2026 Swift Raccoon
// SPDX-License-Identifier: GPL-2.0-or-later OR GPL-3.0-or-later

//! TX strip — TRANSMIT toggle, mic meter, slow-data message, GPS
//! beacon. The always-visible bottom edge of the operator page.

use eframe::egui;

use crate::app::App;
use crate::session::ConnStatus;
use crate::theme;
use crate::ui::Overlay;

/// D-STAR slow-data text messages carry at most 20 characters.
const MSG_MAX_CHARS: usize = 20;

/// Render the strip contents (the caller supplies the inset panel).
pub(crate) fn show(app: &mut App, ui: &mut egui::Ui) {
    ui.horizontal(|ui| {
        transmit_button(app, ui);
        ui.label(
            egui::RichText::new("hold\nSPACE")
                .text_style(egui::TextStyle::Small)
                .color(theme::TEXT_TERTIARY),
        );
        ui.add_space(8.0);
        ui.label(theme::section_label("mic"));
        // Meter takes the flexible middle; reserve the right side for
        // the MSG field and beacon toggle.
        let reserved = 340.0;
        let meter_width = (ui.available_width() - reserved).max(60.0);
        ui.scope(|ui| {
            ui.set_max_width(meter_width);
            theme::level_meter(ui, app.audio_state.tx_level, theme::OK);
        });
        msg_field(app, ui);
        beacon_toggle(app, ui);
    });
    beacon_popup(app, ui);
}

/// The primary TRANSMIT toggle: red fill while on air.
fn transmit_button(app: &mut App, ui: &mut egui::Ui) {
    let connected = matches!(app.status, ConnStatus::Connected { .. });
    let (label, fill) = if app.active_tx {
        ("ON AIR", theme::TX)
    } else {
        ("TRANSMIT", theme::BG_CONTROL)
    };
    let btn = egui::Button::new(
        egui::RichText::new(label)
            .font(egui::FontId::new(12.5, theme::family_medium()))
            .color(theme::TEXT_PRIMARY),
    )
    .fill(fill)
    .min_size(egui::vec2(150.0, 36.0));
    if ui.add_enabled(connected, btn).clicked() {
        app.toggle_ptt();
    }
}

/// The ≤20-char slow-data message field.
fn msg_field(app: &mut App, ui: &mut egui::Ui) {
    ui.label(theme::section_label("msg"));
    let resp = ui.add(
        egui::TextEdit::singleline(&mut app.tx_slow_text)
            .hint_text("slow-data text")
            .desired_width(160.0)
            .font(egui::TextStyle::Monospace),
    );
    if resp.changed() {
        if app.tx_slow_text.chars().count() > MSG_MAX_CHARS {
            app.tx_slow_text = app.tx_slow_text.chars().take(MSG_MAX_CHARS).collect();
        }
        app.push_slow_data();
    }
}

/// Beacon on/off indicator; opens the beacon form popup.
fn beacon_toggle(app: &mut App, ui: &mut egui::Ui) {
    let label = if app.tx_gps.enabled {
        "⌖ BEACON ON"
    } else {
        "⌖ BEACON OFF"
    };
    let color = if app.tx_gps.enabled {
        theme::TEXT_PRIMARY
    } else {
        theme::TEXT_TERTIARY
    };
    if ui
        .button(
            egui::RichText::new(label)
                .text_style(egui::TextStyle::Small)
                .color(color),
        )
        .clicked()
    {
        app.overlay = if app.overlay == Overlay::Beacon {
            Overlay::None
        } else {
            Overlay::Beacon
        };
    }
}

/// The GPS-beacon entry form, floated above the strip while open.
fn beacon_popup(app: &mut App, ui: &egui::Ui) {
    if app.overlay != Overlay::Beacon {
        return;
    }
    let mut changed = false;
    let _window = egui::Window::new("beacon_form")
        .title_bar(false)
        .resizable(false)
        .anchor(egui::Align2::RIGHT_BOTTOM, [-16.0, -56.0])
        .show(ui.ctx(), |ui| {
            ui.label(theme::section_label("gps beacon"));
            changed |= ui
                .checkbox(&mut app.tx_gps.enabled, "Beacon my position")
                .changed();
            egui::Grid::new("beacon_grid")
                .num_columns(2)
                .show(ui, |ui| {
                    ui.label("Latitude");
                    changed |= ui.text_edit_singleline(&mut app.tx_gps.lat).changed();
                    ui.end_row();
                    ui.label("Longitude");
                    changed |= ui.text_edit_singleline(&mut app.tx_gps.lon).changed();
                    ui.end_row();
                    ui.label("Symbol");
                    changed |= ui.text_edit_singleline(&mut app.tx_gps.symbol).changed();
                    ui.end_row();
                    ui.label("Comment");
                    changed |= ui.text_edit_singleline(&mut app.tx_gps.comment).changed();
                    ui.end_row();
                });
            if ui.button("Done").clicked() {
                app.overlay = Overlay::None;
            }
        });
    if changed {
        app.push_slow_data();
    }
}
