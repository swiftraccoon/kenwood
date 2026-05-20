// SPDX-FileCopyrightText: 2026 Swift Raccoon
// SPDX-License-Identifier: GPL-2.0-or-later OR GPL-3.0-or-later

// egui's immediate-mode API returns `Response` / `InnerResponse` from
// nearly every widget method. Idiomatic egui code doesn't use those
// return values for simple layouts. Scoping the expectation to this
// module tree (it cascades to the per-panel submodules) keeps the
// escape hatch narrow.
#![expect(
    unused_results,
    reason = "egui widget methods return Response; not using the layout-chaining return value is idiomatic for immediate-mode GUI code"
)]

//! egui front-end panels.
//!
//! `App::update` composes the dashboard from these per-panel `show`
//! functions. Each panel is pure presentation: it reads `App` state
//! and calls `App` action methods — no async, no I/O.

use eframe::egui;

use crate::app::App;
use crate::session::ConnStatus;

pub(crate) mod audio_panel;
pub(crate) mod connection;
pub(crate) mod log;
pub(crate) mod receive;
pub(crate) mod transmit;

/// Persistent top bar: connection status + the primary PTT toggle.
///
/// The PTT control lives here, never inside a collapsible region, so
/// "stop transmitting" is always one click away.
pub(crate) fn top_bar(app: &mut App, ui: &mut egui::Ui) {
    ui.horizontal(|ui| {
        ui.label(format!("Status: {}", fmt_status(&app.status)));
        let connected = matches!(app.status, ConnStatus::Connected { .. });
        let ptt_label = if app.active_tx {
            "PTT ON (click to stop)"
        } else {
            "PTT"
        };
        let colour = if app.active_tx {
            egui::Color32::from_rgb(180, 40, 40)
        } else {
            egui::Color32::DARK_GRAY
        };
        let ptt_btn = egui::Button::new(egui::RichText::new(ptt_label).color(egui::Color32::WHITE))
            .fill(colour)
            .min_size(egui::vec2(180.0, 40.0));
        if ui.add_enabled(connected, ptt_btn).clicked() {
            app.toggle_ptt();
        }
    });
}

/// Module-letter combo box (A–E).
pub(crate) fn module_picker(ui: &mut egui::Ui, id: &str, value: &mut char) {
    egui::ComboBox::from_id_salt(id)
        .selected_text(String::from(*value))
        .show_ui(ui, |ui| {
            for ch in ['A', 'B', 'C', 'D', 'E'] {
                let _unused = ui.selectable_value(value, ch, String::from(ch));
            }
        });
}

/// Human-readable connection status line.
pub(crate) fn fmt_status(s: &ConnStatus) -> String {
    match s {
        ConnStatus::Disconnected => "disconnected".into(),
        ConnStatus::Connecting { peer } => format!("connecting to {peer}"),
        ConnStatus::Connected { reflector, module } => {
            format!("connected — {reflector} / {module}")
        }
        ConnStatus::Disconnecting => "disconnecting…".into(),
    }
}
