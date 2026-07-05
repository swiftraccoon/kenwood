// SPDX-FileCopyrightText: 2026 Swift Raccoon
// SPDX-License-Identifier: GPL-2.0-or-later OR GPL-3.0-or-later

//! Connection panel — reflector address form + connect/disconnect.

use dstar_gateway_core::types::ProtocolKind;
use eframe::egui;

use crate::app::App;
use crate::hosts;
use crate::session::ConnStatus;
use crate::ui::{fmt_status, module_picker};

/// Cap on reflector rows rendered at once — the registry holds
/// thousands of entries; the operator narrows the list with the
/// search box rather than scrolling all of them.
const MAX_PICKER_ROWS: usize = 200;

/// Reflector directory picker: type to filter, click an entry to
/// fill the connection form. The directory is fetched and cached in
/// the background.
fn directory_picker(app: &mut App, ui: &mut egui::Ui) {
    ui.label("Reflector directory");
    ui.horizontal(|ui| {
        ui.label("Search");
        ui.text_edit_singleline(&mut app.directory_query);
        if ui.button("Refresh").clicked() {
            app.refresh_directory();
        }
    });
    // Clone the matched entries out so the click handler can mutate
    // `app` without holding a borrow of the directory.
    let mut matches: Vec<hosts::ReflectorHost> = app
        .directory
        .search(&app.directory_query)
        .into_iter()
        .take(MAX_PICKER_ROWS)
        .cloned()
        .collect();
    // Stable sort: favorites float to the top, directory order
    // preserved within each group.
    matches.sort_by_key(|h| !app.is_favorite(h));
    egui::ScrollArea::vertical()
        .max_height(140.0)
        .show(ui, |ui| {
            for host in &matches {
                ui.horizontal(|ui| {
                    let star = if app.is_favorite(host) { "★" } else { "☆" };
                    if ui
                        .small_button(star)
                        .on_hover_text("Pin to top of the directory")
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
                    }
                });
            }
        });
    ui.label(app.directory.status());
}

/// One-click reconnect strip for recently used reflectors.
fn recents_strip(app: &mut App, ui: &mut egui::Ui) {
    if app.recents.is_empty() {
        return;
    }
    ui.label("Recent");
    ui.horizontal_wrapped(|ui| {
        let recents = app.recents.clone();
        for r in &recents {
            let label = format!("{} · {}", r.callsign, r.protocol);
            if ui
                .button(label)
                .on_hover_text(format!("{}:{} module {}", r.host, r.port, r.module))
                .clicked()
            {
                app.apply_saved_host(r);
                app.try_connect();
            }
        }
    });
    ui.separator();
}

/// Render the connection form and the connect/disconnect controls.
pub(crate) fn show(app: &mut App, ui: &mut egui::Ui) {
    recents_strip(app, ui);
    directory_picker(app, ui);
    ui.separator();

    egui::Grid::new("conn_form")
        .num_columns(2)
        .spacing([8.0, 4.0])
        .show(ui, |ui| {
            ui.label("Callsign");
            ui.text_edit_singleline(&mut app.callsign);
            ui.end_row();

            ui.label("Reflector host");
            ui.text_edit_singleline(&mut app.reflector_host);
            ui.end_row();

            ui.label("Reflector port");
            ui.text_edit_singleline(&mut app.reflector_port);
            ui.end_row();

            ui.label("Reflector callsign");
            ui.text_edit_singleline(&mut app.reflector_callsign);
            ui.end_row();

            ui.label("Protocol");
            egui::ComboBox::from_id_salt("protocol_select")
                .selected_text(format!("{:?}", app.protocol))
                .show_ui(ui, |ui| {
                    let _unused =
                        ui.selectable_value(&mut app.protocol, ProtocolKind::DExtra, "DExtra");
                    let _unused =
                        ui.selectable_value(&mut app.protocol, ProtocolKind::DPlus, "DPlus");
                    let _unused = ui.selectable_value(&mut app.protocol, ProtocolKind::Dcs, "DCS");
                });
            ui.end_row();

            ui.label("Local module");
            module_picker(ui, "local_mod", &mut app.local_module);
            ui.end_row();

            ui.label("Reflector module");
            module_picker(ui, "remote_mod", &mut app.reflector_module);
            ui.end_row();
        });

    ui.separator();

    ui.horizontal(|ui| {
        let connected = matches!(app.status, ConnStatus::Connected { .. });
        let busy = matches!(
            app.status,
            ConnStatus::Connecting { .. } | ConnStatus::Disconnecting
        );
        if ui
            .add_enabled(!connected && !busy, egui::Button::new("Connect"))
            .clicked()
        {
            app.try_connect();
        }
        if ui
            .add_enabled(connected && !busy, egui::Button::new("Disconnect"))
            .clicked()
        {
            app.try_disconnect();
        }
        let heard = app
            .link_last_heard_secs
            .map_or_else(String::new, |s| format!(" · heard {s:.0}s ago"));
        ui.label(format!("Status: {}{heard}", fmt_status(&app.status)));
    });

    ui.separator();
    ui.checkbox(
        &mut app.reconnect_on_drop,
        "Reconnect automatically if dropped",
    );
    ui.checkbox(
        &mut app.persist_heard_list,
        "Remember heard stations across launches",
    );
}
