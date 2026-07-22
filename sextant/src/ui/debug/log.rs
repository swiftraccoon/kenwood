// SPDX-FileCopyrightText: 2026 Swift Raccoon
// SPDX-License-Identifier: GPL-2.0-or-later OR GPL-3.0-or-later

//! Event log: timestamped, level-coloured, filterable, copyable.

use std::fmt::Write as _;

use eframe::egui;

use crate::app::{App, LogFilter, LogLevel};
use crate::theme;

/// Render the log header (filter chips + copy) and the scroll body.
pub(crate) fn show(app: &mut App, ui: &mut egui::Ui) {
    ui.horizontal(|ui| {
        ui.label(theme::section_label("event log"));
        for (filter, label) in [
            (LogFilter::All, "all"),
            (LogFilter::Events, "events"),
            (LogFilter::Errors, "errors"),
        ] {
            let selected = app.log_filter == filter;
            if ui.selectable_label(selected, label).clicked() {
                app.log_filter = filter;
            }
        }
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            if ui
                .small_button("Copy")
                .on_hover_text("Copy visible lines")
                .clicked()
            {
                let body = app
                    .log
                    .iter()
                    .filter(|l| app.log_filter.admits(l.level))
                    .fold(String::new(), |mut acc, l| {
                        let _line = writeln!(acc, "{} {}", l.stamp, l.text);
                        acc
                    });
                ui.ctx().copy_text(body);
            }
        });
    });
    egui::ScrollArea::vertical()
        .auto_shrink([false, false])
        .stick_to_bottom(true)
        .show(ui, |ui| {
            for line in app.log.iter().filter(|l| app.log_filter.admits(l.level)) {
                let colour = match line.level {
                    LogLevel::Info => theme::TEXT_SECONDARY,
                    LogLevel::Event => theme::INFO,
                    LogLevel::Error => theme::ERR,
                };
                ui.horizontal(|ui| {
                    ui.label(
                        egui::RichText::new(&line.stamp)
                            .font(egui::FontId::new(10.0, egui::FontFamily::Monospace))
                            .color(theme::TEXT_TERTIARY),
                    );
                    ui.label(
                        egui::RichText::new(&line.text)
                            .font(egui::FontId::new(11.0, egui::FontFamily::Monospace))
                            .color(colour),
                    );
                });
            }
        });
}
