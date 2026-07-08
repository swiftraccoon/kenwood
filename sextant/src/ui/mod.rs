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

pub(crate) mod debug;
pub(crate) mod format;
pub(crate) mod operator;
pub(crate) mod settings_popup;

/// Which page the header toggle has selected.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub(crate) enum Page {
    /// Clean operator face: hero display, heard list, TX strip.
    #[default]
    Operator,
    /// Engineering surface: event log and tools.
    Debug,
}

/// Which overlay is showing, if any. At most one overlay is open at
/// a time — opening another replaces it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub(crate) enum Overlay {
    /// No overlay.
    #[default]
    None,
    /// The connect sheet. `focus_search` requests a one-shot focus
    /// of the search field on the next frame.
    ConnectSheet {
        /// One-shot autofocus request for the search field.
        focus_search: bool,
    },
    /// The settings popup.
    Settings,
    /// The GPS-beacon entry form, floated above the TX strip.
    Beacon,
}

/// 8 px status dot.
pub(crate) fn link_dot(ui: &mut egui::Ui, color: egui::Color32) {
    let (rect, _resp) = ui.allocate_exact_size(egui::Vec2::splat(8.0), egui::Sense::hover());
    let _shape = ui.painter().circle_filled(rect.center(), 3.5, color);
}

/// Persistent header: reflector identity (opens the connect sheet),
/// link health, the OPERATOR | DEBUG page toggle, and the settings
/// gear. Never hidden by either page.
pub(crate) fn header(app: &mut App, ui: &mut egui::Ui) {
    ui.horizontal(|ui| {
        let connected = matches!(app.status, ConnStatus::Connected { .. });
        let identity = if app.reflector_callsign.trim().is_empty() {
            "Choose reflector…".to_owned()
        } else {
            format!(
                "{} {}",
                app.reflector_callsign.trim().to_uppercase(),
                app.reflector_module
            )
        };
        let identity_btn = egui::Button::new(
            egui::RichText::new(identity)
                .font(egui::FontId::new(15.0, crate::theme::family_mono_medium()))
                .color(crate::theme::TEXT_PRIMARY),
        )
        .fill(egui::Color32::TRANSPARENT);
        if ui
            .add(identity_btn)
            .on_hover_text("Choose a reflector")
            .clicked()
        {
            app.overlay = Overlay::ConnectSheet { focus_search: true };
        }
        ui.label(
            egui::RichText::new(format!("{:?}", app.protocol).to_uppercase())
                .text_style(egui::TextStyle::Small)
                .color(crate::theme::TEXT_TERTIARY),
        );

        ui.add_space(12.0);
        let dot = if connected {
            crate::theme::OK
        } else {
            crate::theme::TEXT_TERTIARY
        };
        link_dot(ui, dot);
        let heard = app
            .link_last_heard_secs
            .map_or_else(String::new, |s| format!(" · heard {s:.0}s ago"));
        ui.label(
            egui::RichText::new(format!("{}{heard}", fmt_status(&app.status)).to_uppercase())
                .text_style(egui::TextStyle::Small)
                .color(crate::theme::TEXT_SECONDARY),
        );
        // Third on-air indicator (with the hero and the TX strip
        // button) — a PTT state change must be visible everywhere.
        if app.active_tx {
            ui.label(
                egui::RichText::new("● ON AIR")
                    .text_style(egui::TextStyle::Small)
                    .color(crate::theme::TX),
            );
        }

        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            if ui.button("⚙").on_hover_text("Settings").clicked() {
                app.overlay = if app.overlay == Overlay::Settings {
                    Overlay::None
                } else {
                    Overlay::Settings
                };
            }
            mycall_indicator(app, ui);
            page_toggle(app, ui, Page::Debug, "DEBUG");
            page_toggle(app, ui, Page::Operator, "OPERATOR");
            if connected && ui.button("Unlink").clicked() {
                app.try_disconnect();
            }
        });
    });
}

/// The operator's own callsign, always visible in the header — a
/// stale value here silently poisons `DPlus` auth, so it must never
/// hide behind the gear alone. Click opens settings.
fn mycall_indicator(app: &mut App, ui: &mut egui::Ui) {
    let mycall = app.callsign.trim().to_uppercase();
    let (text, color) = if mycall.is_empty() {
        ("SET CALLSIGN".to_owned(), crate::theme::ERR)
    } else {
        (mycall, crate::theme::TEXT_TERTIARY)
    };
    let btn = egui::Button::new(
        egui::RichText::new(text)
            .font(egui::FontId::new(12.0, egui::FontFamily::Monospace))
            .color(color),
    )
    .fill(egui::Color32::TRANSPARENT);
    if ui
        .add(btn)
        .on_hover_text("Operator callsign — click to edit")
        .clicked()
    {
        app.overlay = Overlay::Settings;
    }
}

/// One side of the OPERATOR | DEBUG toggle: amber-underlined when
/// selected, tertiary otherwise.
fn page_toggle(app: &mut App, ui: &mut egui::Ui, page: Page, label: &str) {
    let selected = app.page == page;
    let color = if selected {
        crate::theme::TEXT_PRIMARY
    } else {
        crate::theme::TEXT_TERTIARY
    };
    let text = egui::RichText::new(label)
        .text_style(egui::TextStyle::Small)
        .color(color);
    let resp = ui.selectable_label(false, text);
    if selected {
        let underline = egui::Rect::from_min_max(
            egui::pos2(resp.rect.left(), resp.rect.bottom()),
            egui::pos2(resp.rect.right(), resp.rect.bottom() + 2.0),
        );
        let _shape = ui.painter().rect_filled(underline, 0.0, crate::theme::LIVE);
    }
    if resp.clicked() {
        app.page = page;
    }
}

/// Slim dismissable error banner under the header — shown on BOTH
/// pages so an error can't hide behind the page toggle.
pub(crate) fn error_strip(app: &mut App, ui: &mut egui::Ui) {
    let Some(err) = app.last_error.clone() else {
        return;
    };
    egui::Frame::none()
        .fill(egui::Color32::from_rgba_unmultiplied(
            0xFF, 0x5F, 0x57, 0x22,
        ))
        .inner_margin(egui::Margin::symmetric(12.0, 6.0))
        .show(ui, |ui| {
            ui.horizontal(|ui| {
                ui.colored_label(crate::theme::ERR, &err);
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    if ui.small_button("✕").on_hover_text("Dismiss").clicked() {
                        app.last_error = None;
                    }
                });
            });
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
