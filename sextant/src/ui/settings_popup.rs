// SPDX-FileCopyrightText: 2026 Swift Raccoon
// SPDX-License-Identifier: GPL-2.0-or-later OR GPL-3.0-or-later

//! Settings popup — operator identity and behaviour toggles, anchored
//! under the header gear. Audio devices are duplicated here from the
//! debug page deliberately: device choice is an operator concern too.

use eframe::egui;

use crate::app::App;
use crate::settings::{RxAudioMode, TimeMode};
use crate::theme;
use crate::ui::Overlay;
use crate::ui::debug::tools::device_combo;

/// Render the popup while the settings overlay is open. Esc closes.
pub(crate) fn show(app: &mut App, ctx: &egui::Context) {
    if app.overlay != Overlay::Settings {
        return;
    }
    let mut save = false;
    let _window = egui::Window::new("settings_popup")
        .title_bar(false)
        .resizable(false)
        .anchor(egui::Align2::RIGHT_TOP, [-12.0, 44.0])
        .fixed_size([300.0, 0.0])
        .show(ctx, |ui| {
            ui.label(theme::section_label("operator"));
            ui.horizontal(|ui| {
                ui.label("Callsign");
                let resp = ui.add(
                    egui::TextEdit::singleline(&mut app.callsign)
                        .font(egui::TextStyle::Monospace)
                        .desired_width(120.0),
                );
                save |= resp.lost_focus();
            });
            ui.separator();
            save |= behaviour_section(app, ui);
            ui.separator();
            ui.label(theme::section_label("clock"));
            ui.horizontal(|ui| {
                let local_label = app.local_offset.map_or_else(
                    || "Local (undetected — uses UTC)".to_owned(),
                    |off| {
                        format!(
                            "Local (UTC{:+03}:{:02})",
                            off.whole_hours(),
                            off.minutes_past_hour().abs()
                        )
                    },
                );
                for (mode, label) in [
                    (TimeMode::Local, local_label.as_str()),
                    (TimeMode::Utc, "UTC"),
                ] {
                    let selected = app.time_mode == mode;
                    if ui.selectable_label(selected, label).clicked() {
                        app.time_mode = mode;
                        save = true;
                    }
                }
            });
            ui.separator();
            ui.label(theme::section_label("audio devices"));
            let inputs = app.audio_state.inputs.clone();
            let outputs = app.audio_state.outputs.clone();
            let mut devices_changed = false;
            device_combo(
                ui,
                "set_in",
                "in",
                &mut app.input_device,
                &inputs,
                &mut devices_changed,
            );
            device_combo(
                ui,
                "set_out",
                "out",
                &mut app.output_device,
                &outputs,
                &mut devices_changed,
            );
            if devices_changed {
                app.apply_audio_devices();
            }
            if ui.small_button("Refresh device list").clicked() {
                app.refresh_devices();
            }
            ui.add_space(4.0);
            if ui.button("Done").clicked() {
                app.overlay = Overlay::None;
            }
        });
    if save {
        app.snapshot_settings().save();
    }
    if ctx.input(|i| i.key_pressed(egui::Key::Escape)) {
        app.overlay = Overlay::None;
    }
}

/// Behaviour toggles: reconnect, heard-list persistence, RX
/// enhancement. Returns `true` when any setting changed.
fn behaviour_section(app: &mut App, ui: &mut egui::Ui) -> bool {
    let mut save = false;
    ui.label(theme::section_label("behaviour"));
    save |= ui
        .checkbox(
            &mut app.reconnect_on_drop,
            "Reconnect automatically if dropped",
        )
        .changed();
    save |= ui
        .checkbox(
            &mut app.persist_heard_list,
            "Remember heard stations across launches",
        )
        .changed();
    let mut enhance_rx = app.rx_audio.is_enhanced();
    if ui.checkbox(&mut enhance_rx, "Enhance RX audio").changed() {
        app.rx_audio = if enhance_rx {
            RxAudioMode::Enhanced
        } else {
            RxAudioMode::Raw
        };
        app.apply_rx_audio_mode();
        save = true;
    }
    save
}
