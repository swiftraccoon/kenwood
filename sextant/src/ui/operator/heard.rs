// SPDX-FileCopyrightText: 2026 Swift Raccoon
// SPDX-License-Identifier: GPL-2.0-or-later OR GPL-3.0-or-later

//! Heard list — two-line rows, newest first, active speaker pinned
//! with an amber "now" dot. Clicking a row copies the callsign.

use eframe::egui;

use crate::app::App;
use crate::theme;
use crate::ui::format::fmt_datetime;

/// Render the heard list (the operator page's only scroll region).
pub(crate) fn show(app: &App, ui: &mut egui::Ui) {
    ui.label(theme::section_label("heard"));
    egui::ScrollArea::vertical()
        .id_salt("heard_scroll")
        .auto_shrink([false, false])
        .show(ui, |ui| {
            let rx_active = app.rx_active_since.is_some();
            let speaker = app.current_rx_callsign.as_deref();
            let recent = app.heard.recent();
            if recent.is_empty() {
                ui.label(
                    egui::RichText::new("Nobody heard yet")
                        .text_style(egui::TextStyle::Small)
                        .color(theme::TEXT_TERTIARY),
                );
                return;
            }
            let offset = app.display_offset();
            for station in recent {
                let is_live = rx_active && speaker == Some(station.callsign.as_str());
                row(ui, station, is_live, offset);
            }
        });
}

/// One two-line station row.
fn row(
    ui: &mut egui::Ui,
    station: &crate::heard::HeardStation,
    is_live: bool,
    offset: time::UtcOffset,
) {
    let name_color = if is_live {
        theme::LIVE
    } else {
        theme::TEXT_PRIMARY
    };
    let resp = ui
        .horizontal(|ui| {
            ui.label(
                egui::RichText::new(&station.callsign)
                    .font(egui::FontId::new(13.0, egui::FontFamily::Monospace))
                    .color(name_color),
            );
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                // Wall-clock stamp in the operator's chosen display
                // offset — matches the event log's timezone. "—" for
                // legacy entries without one.
                let when = if is_live {
                    "● now".to_owned()
                } else {
                    station
                        .last_heard
                        .map_or_else(|| "—".to_owned(), |ts| fmt_datetime(ts, offset))
                };
                let when_color = if is_live {
                    theme::LIVE
                } else {
                    theme::TEXT_TERTIARY
                };
                ui.label(
                    egui::RichText::new(when)
                        .font(egui::FontId::new(10.0, egui::FontFamily::Monospace))
                        .color(when_color),
                );
            });
        })
        .response;
    let mut detail = Vec::new();
    if let Some(msg) = &station.last_message {
        detail.push(format!("“{msg}”"));
    }
    if let Some((lat, lon)) = station.last_gps {
        detail.push(format!("⌖ {lat:.4}° {lon:.4}°"));
    }
    detail.push(format!("{}×", station.stream_count));
    ui.label(
        egui::RichText::new(detail.join(" · "))
            .text_style(egui::TextStyle::Small)
            .color(theme::TEXT_TERTIARY),
    );
    ui.separator();
    let resp = resp.interact(egui::Sense::click());
    let resp = resp.on_hover_text("Click to copy · right-click for QRZ");
    if resp.clicked() {
        ui.ctx().copy_text(station.callsign.clone());
    }
    resp.context_menu(|ui| {
        if ui.button("Open on QRZ ↗").clicked() {
            ui.ctx()
                .open_url(egui::OpenUrl::new_tab(crate::ui::format::qrz_url(
                    &station.callsign,
                )));
            ui.close_menu();
        }
        if ui.button("Copy callsign").clicked() {
            ui.ctx().copy_text(station.callsign.clone());
            ui.close_menu();
        }
    });
}
