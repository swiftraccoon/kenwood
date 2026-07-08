// SPDX-FileCopyrightText: 2026 Swift Raccoon
// SPDX-License-Identifier: GPL-2.0-or-later OR GPL-3.0-or-later

//! Connect sheet — the overlay for choosing / connecting to a
//! reflector. Favorites first, then recents, then the searchable
//! directory; the raw host form lives under a collapsed "Manual…".

use eframe::egui;

use crate::app::App;
use crate::hosts::ReflectorHost;
use crate::theme;
use crate::ui::{Overlay, module_picker};

/// Cap on directory rows rendered — narrow with search instead.
const MAX_ROWS: usize = 200;

/// Render the sheet when open. Esc or the close button dismisses it.
pub(crate) fn show(app: &mut App, ctx: &egui::Context) {
    let Overlay::ConnectSheet { focus_search } = app.overlay else {
        return;
    };
    // Dimmed backdrop that swallows clicks behind the sheet.
    let screen = ctx.screen_rect();
    let _area = egui::Area::new(egui::Id::new("sheet_backdrop"))
        .order(egui::Order::Middle)
        .fixed_pos(screen.min)
        .show(ctx, |ui| {
            let _resp = ui.allocate_rect(screen, egui::Sense::click());
            ui.painter()
                .rect_filled(screen, 0.0, egui::Color32::from_black_alpha(0xA0));
        });

    let _window = egui::Window::new("connect_sheet")
        .title_bar(false)
        .resizable(false)
        .collapsible(false)
        .order(egui::Order::Foreground)
        .anchor(egui::Align2::CENTER_CENTER, [0.0, 0.0])
        .fixed_size([480.0, 440.0])
        .show(ctx, |ui| {
            sheet_contents(app, ui, focus_search);
        });

    if ctx.input(|i| i.key_pressed(egui::Key::Escape)) {
        app.overlay = Overlay::None;
    }
}

/// The sheet body: search, modules, favorites, recents, directory,
/// manual form.
fn sheet_contents(app: &mut App, ui: &mut egui::Ui, focus_search: bool) {
    ui.horizontal(|ui| {
        ui.heading("Reflectors");
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            if ui.small_button("✕").clicked() {
                app.overlay = Overlay::None;
            }
            if ui
                .small_button("↻")
                .on_hover_text("Refresh directory")
                .clicked()
            {
                app.refresh_directory();
            }
        });
    });
    let search = ui.add(
        egui::TextEdit::singleline(&mut app.directory_query)
            .hint_text("Search callsign…")
            .desired_width(f32::INFINITY),
    );
    if focus_search {
        search.request_focus();
        app.overlay = Overlay::ConnectSheet {
            focus_search: false,
        };
    }

    ui.horizontal(|ui| {
        ui.label(theme::section_label("modules  local"));
        module_picker(ui, "sheet_local_mod", &mut app.local_module);
        ui.label(theme::section_label("reflector"));
        module_picker(ui, "sheet_remote_mod", &mut app.reflector_module);
    });
    ui.separator();

    egui::ScrollArea::vertical()
        .max_height(240.0)
        .auto_shrink([false, false])
        .show(ui, |ui| {
            saved_rows(app, ui);
            directory_rows(app, ui);
        });

    ui.separator();
    ui.label(
        egui::RichText::new(app.directory.status())
            .text_style(egui::TextStyle::Small)
            .color(theme::TEXT_TERTIARY),
    );
    manual_section(app, ui);
}

/// Favorites then recents, as rows above the directory. Skipped when
/// a search query is active (the query addresses the directory).
fn saved_rows(app: &mut App, ui: &mut egui::Ui) {
    if !app.directory_query.trim().is_empty() {
        return;
    }
    if !app.favorites.is_empty() {
        ui.label(theme::section_label("favorites"));
        for saved in app.favorites.clone() {
            saved_row(app, ui, &saved, true);
        }
    }
    if !app.recents.is_empty() {
        ui.label(theme::section_label("recent"));
        for saved in app.recents.clone() {
            saved_row(app, ui, &saved, false);
        }
    }
    ui.label(theme::section_label("directory"));
}

/// One favorite/recent row: click connects immediately.
fn saved_row(app: &mut App, ui: &mut egui::Ui, saved: &crate::settings::SavedHost, starred: bool) {
    ui.horizontal(|ui| {
        if starred {
            ui.colored_label(theme::LIVE, "★");
        }
        let label = format!(
            "{}  ·  {}  ·  {}",
            saved.callsign, saved.protocol, saved.host
        );
        if ui.selectable_label(false, label).clicked() {
            app.apply_saved_host(saved);
            app.try_connect();
            app.overlay = Overlay::None;
        }
    });
}

/// Directory rows matching the query, star toggles inline, click
/// fills the form and connects.
fn directory_rows(app: &mut App, ui: &mut egui::Ui) {
    let mut matches: Vec<ReflectorHost> = app
        .directory
        .search(&app.directory_query)
        .into_iter()
        .take(MAX_ROWS)
        .cloned()
        .collect();
    matches.sort_by_key(|h| !app.is_favorite(h));
    for host in &matches {
        ui.horizontal(|ui| {
            let starred = app.is_favorite(host);
            let star = if starred { "★" } else { "☆" };
            let star_color = if starred {
                theme::LIVE
            } else {
                theme::TEXT_TERTIARY
            };
            if ui
                .small_button(egui::RichText::new(star).color(star_color))
                .on_hover_text("Pin to favorites")
                .clicked()
            {
                app.toggle_favorite(host);
            }
            let label = format!(
                "{}  ·  {:?}  ·  {}  ·  {}",
                host.callsign,
                host.protocol,
                host.host,
                host.source.label()
            );
            if ui.selectable_label(false, label).clicked() {
                app.reflector_callsign.clone_from(&host.callsign);
                app.reflector_host.clone_from(&host.host);
                app.reflector_port = host.port.to_string();
                app.protocol = host.protocol;
                app.try_connect();
                app.overlay = Overlay::None;
            }
        });
    }
}

/// Collapsed manual entry — the raw host form for unlisted
/// reflectors, plus its own Connect button.
fn manual_section(app: &mut App, ui: &mut egui::Ui) {
    let _header = egui::CollapsingHeader::new("Manual…")
        .default_open(false)
        .show(ui, |ui| {
            egui::Grid::new("manual_form")
                .num_columns(2)
                .spacing([8.0, 4.0])
                .show(ui, |ui| {
                    ui.label("Reflector callsign");
                    ui.text_edit_singleline(&mut app.reflector_callsign);
                    ui.end_row();
                    ui.label("Host");
                    ui.text_edit_singleline(&mut app.reflector_host);
                    ui.end_row();
                    ui.label("Port");
                    ui.text_edit_singleline(&mut app.reflector_port);
                    ui.end_row();
                    ui.label("Protocol");
                    protocol_picker(ui, &mut app.protocol);
                    ui.end_row();
                });
            if ui.button("Connect").clicked() {
                app.try_connect();
                app.overlay = Overlay::None;
            }
        });
}

/// Protocol combo (`DExtra` / `DPlus` / DCS).
fn protocol_picker(ui: &mut egui::Ui, value: &mut dstar_gateway_core::types::ProtocolKind) {
    use dstar_gateway_core::types::ProtocolKind;
    egui::ComboBox::from_id_salt("sheet_protocol")
        .selected_text(format!("{value:?}"))
        .show_ui(ui, |ui| {
            let _u = ui.selectable_value(value, ProtocolKind::DExtra, "DExtra");
            let _u = ui.selectable_value(value, ProtocolKind::DPlus, "DPlus");
            let _u = ui.selectable_value(value, ProtocolKind::Dcs, "DCS");
        });
}
