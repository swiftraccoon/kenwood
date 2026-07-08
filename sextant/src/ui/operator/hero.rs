// SPDX-FileCopyrightText: 2026 Swift Raccoon
// SPDX-License-Identifier: GPL-2.0-or-later OR GPL-3.0-or-later

//! Hero display — the deck's main instrument. One of four states:
//! not-linked invitation, dimmed listening, amber receiving, red
//! on-air.

use eframe::egui;

use crate::app::App;
use crate::session::ConnStatus;
use crate::theme;
use crate::ui::Overlay;
use crate::ui::format::elapsed_mmss;

/// Which face the hero shows. Precedence: on-air beats receiving
/// (if you key up over a stream, YOUR state is what matters).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum HeroState {
    /// No session — invite the operator to pick a reflector.
    NotLinked,
    /// Linked and idle.
    Listening,
    /// An RX voice stream is playing.
    Receiving,
    /// We are transmitting.
    OnAir,
}

/// Derive the hero state from session flags.
pub(crate) const fn derive_state(connected: bool, active_tx: bool, rx_active: bool) -> HeroState {
    if !connected {
        HeroState::NotLinked
    } else if active_tx {
        HeroState::OnAir
    } else if rx_active {
        HeroState::Receiving
    } else {
        HeroState::Listening
    }
}

/// Render the hero for the current app state.
pub(crate) fn show(app: &mut App, ui: &mut egui::Ui) {
    let connected = matches!(app.status, ConnStatus::Connected { .. });
    let state = derive_state(connected, app.active_tx, app.rx_active_since.is_some());
    ui.vertical_centered(|ui| {
        ui.add_space(ui.available_height() * 0.18);
        match state {
            HeroState::NotLinked => not_linked(app, ui),
            HeroState::Listening => listening(app, ui),
            HeroState::Receiving => receiving(app, ui),
            HeroState::OnAir => on_air(app, ui),
        }
    });
}

/// The 44 px monospace callsign — the loudest element in the app.
fn big_callsign(ui: &mut egui::Ui, text: &str, color: egui::Color32) {
    ui.label(
        egui::RichText::new(text)
            .text_style(theme::hero_text_style())
            .color(color),
    );
}

/// Invitation state: directory button + one-click favorite chips.
fn not_linked(app: &mut App, ui: &mut egui::Ui) {
    ui.label(theme::section_label("not linked"));
    ui.add_space(8.0);
    ui.label(egui::RichText::new("Choose a reflector to begin").color(theme::TEXT_SECONDARY));
    ui.add_space(8.0);
    if ui.button("⌕ Open directory").clicked() {
        app.overlay = Overlay::ConnectSheet { focus_search: true };
    }
    if !app.favorites.is_empty() {
        ui.add_space(8.0);
        ui.horizontal_wrapped(|ui| {
            for saved in app.favorites.clone().iter().take(4) {
                let chip = format!("★ {} {}", saved.callsign, saved.module);
                if ui
                    .button(egui::RichText::new(chip).color(theme::LIVE))
                    .clicked()
                {
                    app.apply_saved_host(saved);
                    app.try_connect();
                }
            }
        });
    }
}

/// Dimmed idle state with a slow breathing pulse on the placeholder.
fn listening(app: &App, ui: &mut egui::Ui) {
    ui.label(theme::section_label("listening"));
    // 6-second sine breathe between 25% and 55% brightness.
    let t = ui.input(|i| i.time);
    #[expect(
        clippy::cast_possible_truncation,
        reason = "sine phase is in 0..=1, well within f32 range"
    )]
    let phase = (t * std::f64::consts::TAU / 6.0).sin().mul_add(0.5, 0.5) as f32;
    let alpha = phase.mul_add(0.30, 0.25);
    big_callsign(ui, "──", theme::TEXT_TERTIARY.gamma_multiply(alpha + 0.4));
    if let Some(station) = app.heard.recent().first() {
        let line = station.last_heard.map_or_else(
            || format!("last: {}", station.callsign),
            |ts| {
                let ago = crate::ui::format::relative_time(crate::ui::format::secs_since(ts));
                format!("last: {} · {ago} ago", station.callsign)
            },
        );
        ui.label(
            egui::RichText::new(line)
                .text_style(egui::TextStyle::Small)
                .color(theme::TEXT_TERTIARY),
        );
    }
}

/// Live RX: amber callsign (click → QRZ), routing, slow-data, GPS
/// with distance/bearing, elapsed, loss stats, live audio level.
fn receiving(app: &App, ui: &mut egui::Ui) {
    ui.label(theme::section_label("now receiving"));
    let callsign = app.current_rx_callsign.as_deref().unwrap_or("——");
    let resp = ui
        .add(
            egui::Label::new(
                egui::RichText::new(callsign)
                    .text_style(theme::hero_text_style())
                    .color(theme::LIVE),
            )
            .sense(egui::Sense::click()),
        )
        .on_hover_text("Open on QRZ");
    if resp.clicked() {
        ui.ctx()
            .open_url(egui::OpenUrl::new_tab(crate::ui::format::qrz_url(callsign)));
    }
    route_line(app, ui);
    if let Some(text) = &app.last_slow_data {
        ui.label(egui::RichText::new(format!("“{text}”")).color(theme::TEXT_SECONDARY));
    }
    position_line(app, ui);
    ui.add_space(6.0);
    if let Some(since) = app.rx_active_since {
        ui.label(
            egui::RichText::new(elapsed_mmss(since.elapsed().as_secs()))
                .font(egui::FontId::new(12.0, egui::FontFamily::Monospace))
                .color(theme::TEXT_TERTIARY),
        );
    }
    if let Some(s) = app.last_rx_stats {
        let total = s.received.saturating_add(s.lost);
        let pct = if total == 0 {
            0.0
        } else {
            f64::from(s.lost) * 100.0 / f64::from(total)
        };
        ui.label(
            egui::RichText::new(format!(
                "rx {} · loss {pct:.1}% · late {}",
                s.received, s.late
            ))
            .font(egui::FontId::new(10.0, egui::FontFamily::Monospace))
            .color(theme::TEXT_TERTIARY),
        );
    }
    // Live decoded-audio level — confidence the stream is audible.
    ui.add_space(4.0);
    ui.scope(|ui| {
        ui.set_max_width(200.0);
        theme::level_meter(ui, app.audio_state.rx_level, theme::OK);
    });
}

/// Routing line from the stream header: suffix, destination, path.
fn route_line(app: &App, ui: &mut egui::Ui) {
    let Some(route) = &app.rx_route else {
        return;
    };
    let mut parts = Vec::new();
    if !route.suffix.is_empty() {
        parts.push(format!("/{}", route.suffix));
    }
    if !route.to.is_empty() {
        parts.push(format!("→ {}", route.to));
    }
    if !route.rpt1.is_empty() || !route.rpt2.is_empty() {
        parts.push(format!("{} › {}", route.rpt1, route.rpt2));
    }
    if parts.is_empty() {
        return;
    }
    ui.label(
        egui::RichText::new(parts.join("  ·  "))
            .font(egui::FontId::new(10.0, egui::FontFamily::Monospace))
            .color(theme::TEXT_TERTIARY),
    );
}

/// Speaker position, with distance + bearing from the operator's own
/// beacon coordinates when those are set.
fn position_line(app: &App, ui: &mut egui::Ui) {
    let Some((lat, lon)) = app.last_gps else {
        return;
    };
    let mut line = format!("⌖ {lat:.4}°  {lon:.4}°");
    if let Some(me) = app.parse_tx_gps() {
        let from = (me.latitude, me.longitude);
        let km = crate::geo::haversine_km(from, (lat, lon));
        let bearing = crate::geo::initial_bearing_deg(from, (lat, lon));
        let _w =
            std::fmt::Write::write_fmt(&mut line, format_args!("  ·  {km:.0} km @ {bearing:.0}°"));
    }
    ui.label(
        egui::RichText::new(line)
            .text_style(egui::TextStyle::Small)
            .color(theme::TEXT_TERTIARY),
    );
}

/// Transmitting: our own callsign in primary on a red-tinted panel.
fn on_air(app: &App, ui: &mut egui::Ui) {
    ui.label(
        egui::RichText::new("● ON AIR")
            .text_style(egui::TextStyle::Small)
            .color(theme::TX),
    );
    big_callsign(ui, app.callsign.trim(), theme::TEXT_PRIMARY);
    if let Some(since) = app.tx_active_since {
        ui.label(
            egui::RichText::new(format!(
                "{} · release SPACE or click to end",
                elapsed_mmss(since.elapsed().as_secs())
            ))
            .text_style(egui::TextStyle::Small)
            .color(theme::TEXT_SECONDARY),
        );
    }
}

#[cfg(test)]
mod tests {
    use super::{HeroState, derive_state};

    #[test]
    fn state_precedence() {
        assert_eq!(derive_state(false, false, false), HeroState::NotLinked);
        // Disconnected wins even over stale flags.
        assert_eq!(derive_state(false, true, true), HeroState::NotLinked);
        assert_eq!(derive_state(true, false, false), HeroState::Listening);
        assert_eq!(derive_state(true, false, true), HeroState::Receiving);
        assert_eq!(derive_state(true, true, false), HeroState::OnAir);
        // On-air beats receiving.
        assert_eq!(derive_state(true, true, true), HeroState::OnAir);
    }
}
