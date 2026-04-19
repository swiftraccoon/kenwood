// SPDX-FileCopyrightText: 2026 Swift Raccoon
// SPDX-License-Identifier: GPL-2.0-or-later OR GPL-3.0-or-later

// egui's immediate-mode API returns `Response` / `InnerResponse` from
// nearly every widget method. Idiomatic egui code doesn't use those
// return values for simple layouts — every `.label()`, `.heading()`,
// `.separator()` etc. would otherwise need a `let _unused = ` prefix,
// tripling the GUI code. Scoping the expectation to this module
// (rather than the workspace) keeps the escape hatch narrow.
#![expect(
    unused_results,
    reason = "egui widget methods return Response; not using the layout-chaining return value is idiomatic for immediate-mode GUI code"
)]

//! egui front-end.
//!
//! Single-window layout:
//!
//! - Settings panel (left): callsign, reflector host:port, protocol,
//!   local / remote module letters.
//! - Connection bar: Connect / Disconnect button + status indicator.
//! - Transmit section: big PTT toggle (click to start TX, click again
//!   to stop and send EOT). Optional "TX silence (2 s)" button for
//!   sanity-checking the protocol path without the mic.
//! - Event log: append-only list of recent session events.

use std::net::SocketAddr;

use dstar_gateway_core::types::{Callsign, Module, ProtocolKind};
use eframe::egui;
use tokio::runtime::Runtime;
use tokio::sync::mpsc;

use crate::audio::{AudioCommand, AudioHandle};
use crate::session::{ConnStatus, ConnectConfig, SessionCommand, SessionEvent};

/// Maximum lines kept in the event-log buffer. Older lines drop off
/// the top when this cap is exceeded.
const LOG_CAPACITY: usize = 500;

/// GUI app state + wiring to the async session task.
pub(crate) struct App {
    // --- form state (what the user is currently editing) ---
    callsign: String,
    reflector_host: String,
    reflector_port: String,
    reflector_callsign: String,
    protocol: ProtocolKind,
    local_module: char,
    reflector_module: char,

    // --- session state (what the session task has told us) ---
    status: ConnStatus,
    log: Vec<LogLine>,
    last_error: Option<String>,
    active_tx: bool,

    // --- channels ---
    cmd_tx: mpsc::Sender<SessionCommand>,
    evt_rx: mpsc::Receiver<SessionEvent>,
    audio: AudioHandle,

    // Owns the runtime so it lives for the whole app lifetime.
    _runtime: Runtime,
}

#[derive(Debug, Clone)]
struct LogLine {
    level: LogLevel,
    text: String,
}

#[derive(Debug, Clone, Copy)]
enum LogLevel {
    Info,
    Event,
    Error,
}

impl App {
    /// Build the GUI app, wiring it to the already-spawned session
    /// task via `cmd_tx` / `evt_rx` and the shared audio worker via
    /// `audio`.
    pub(crate) fn new(
        _cc: &eframe::CreationContext<'_>,
        cmd_tx: mpsc::Sender<SessionCommand>,
        evt_rx: mpsc::Receiver<SessionEvent>,
        audio: AudioHandle,
        runtime: Runtime,
    ) -> Self {
        Self {
            callsign: "W1TEST".into(),
            reflector_host: "127.0.0.1".into(),
            reflector_port: "30001".into(),
            reflector_callsign: "POLARIS".into(),
            protocol: ProtocolKind::DExtra,
            local_module: 'C',
            reflector_module: 'C',
            status: ConnStatus::Disconnected,
            log: Vec::new(),
            last_error: None,
            active_tx: false,
            cmd_tx,
            evt_rx,
            audio,
            _runtime: runtime,
        }
    }

    fn append_log(&mut self, line: LogLine) {
        if self.log.len() >= LOG_CAPACITY {
            // Evict the oldest entry. `swap_remove(0)` would be O(1)
            // but reorders — for a log display we want FIFO order.
            let _removed = self.log.remove(0);
        }
        self.log.push(line);
    }

    fn drain_events(&mut self) {
        while let Ok(evt) = self.evt_rx.try_recv() {
            match evt {
                SessionEvent::Status(s) => {
                    self.append_log(LogLine {
                        level: LogLevel::Info,
                        text: format!("status: {}", fmt_status(&s)),
                    });
                    // When we disconnect, make sure the PTT toggle
                    // resets so the GUI can't get stuck "transmitting"
                    // without an active session.
                    if matches!(s, ConnStatus::Disconnected) {
                        self.active_tx = false;
                        self.audio.send(AudioCommand::StopTx);
                    }
                    self.status = s;
                }
                SessionEvent::Log(t) => self.append_log(LogLine {
                    level: LogLevel::Info,
                    text: t,
                }),
                SessionEvent::VoiceStart { stream_id, from } => {
                    // Decoder reset is driven by the session task
                    // (direct to audio worker) — the GUI only shows
                    // the event in the log.
                    self.append_log(LogLine {
                        level: LogLevel::Event,
                        text: format!("VoiceStart sid=0x{stream_id:04X} from={from}"),
                    });
                }
                SessionEvent::VoiceEnd {
                    stream_id,
                    frames,
                    reason,
                } => self.append_log(LogLine {
                    level: LogLevel::Event,
                    text: format!(
                        "VoiceEnd sid=0x{stream_id:04X} frames={frames} reason={reason}"
                    ),
                }),
                SessionEvent::Error(e) => {
                    self.last_error = Some(e.clone());
                    self.append_log(LogLine {
                        level: LogLevel::Error,
                        text: e,
                    });
                }
            }
        }
    }

    fn try_connect(&mut self) {
        let cfg = match self.build_connect_config() {
            Ok(c) => c,
            Err(e) => {
                self.last_error = Some(e.clone());
                self.append_log(LogLine {
                    level: LogLevel::Error,
                    text: format!("invalid config: {e}"),
                });
                return;
            }
        };
        let _unused =self.cmd_tx.try_send(SessionCommand::Connect(cfg));
    }

    fn try_disconnect(&self) {
        let _unused = self.cmd_tx.try_send(SessionCommand::Disconnect);
    }

    fn toggle_ptt(&mut self) {
        if self.active_tx {
            self.active_tx = false;
            self.audio.send(AudioCommand::StopTx);
        } else if matches!(self.status, ConnStatus::Connected { .. }) {
            self.active_tx = true;
            self.audio.send(AudioCommand::StartTx {
                my_call: self.callsign.clone(),
            });
        } else {
            self.append_log(LogLine {
                level: LogLevel::Error,
                text: "cannot TX: not connected".into(),
            });
        }
    }

    fn tx_silence_test(&self) {
        let _unused =self
            .cmd_tx
            .try_send(SessionCommand::TxSilence { seconds: 2.0 });
    }

    fn build_connect_config(&self) -> Result<ConnectConfig, String> {
        let callsign = Callsign::try_from_str(&self.callsign)
            .map_err(|e| format!("callsign {:?}: {e}", self.callsign))?;
        let reflector_callsign = Callsign::try_from_str(&self.reflector_callsign)
            .map_err(|e| format!("reflector callsign {:?}: {e}", self.reflector_callsign))?;
        let local_module = Module::try_from_char(self.local_module)
            .map_err(|e| format!("local module {}: {e}", self.local_module))?;
        let reflector_module = Module::try_from_char(self.reflector_module)
            .map_err(|e| format!("reflector module {}: {e}", self.reflector_module))?;
        let port: u16 = self
            .reflector_port
            .trim()
            .parse()
            .map_err(|e| format!("port {:?}: {e}", self.reflector_port))?;
        let addr_str = format!("{}:{}", self.reflector_host.trim(), port);
        let peer: SocketAddr = addr_str
            .parse()
            .map_err(|e| format!("peer {addr_str:?}: {e}"))?;
        Ok(ConnectConfig {
            protocol: self.protocol,
            callsign,
            local_module,
            reflector_callsign,
            reflector_module,
            peer,
        })
    }
}

impl eframe::App for App {
    #[expect(
        clippy::too_many_lines,
        reason = "immediate-mode GUI layout code is naturally long; splitting panels into helpers hurts readability"
    )]
    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        self.drain_events();

        egui::CentralPanel::default().show(ctx, |ui| {
            ui.heading("SEXTANT — D-STAR client");
            ui.separator();

            // Connection form.
            egui::Grid::new("conn_form")
                .num_columns(2)
                .spacing([8.0, 4.0])
                .show(ui, |ui| {
                    ui.label("Callsign");
                    ui.text_edit_singleline(&mut self.callsign);
                    ui.end_row();

                    ui.label("Reflector host");
                    ui.text_edit_singleline(&mut self.reflector_host);
                    ui.end_row();

                    ui.label("Reflector port");
                    ui.text_edit_singleline(&mut self.reflector_port);
                    ui.end_row();

                    ui.label("Reflector callsign");
                    ui.text_edit_singleline(&mut self.reflector_callsign);
                    ui.end_row();

                    ui.label("Protocol");
                    egui::ComboBox::from_id_salt("protocol_select")
                        .selected_text(format!("{:?}", self.protocol))
                        .show_ui(ui, |ui| {
                            let _unused =ui.selectable_value(
                                &mut self.protocol,
                                ProtocolKind::DExtra,
                                "DExtra",
                            );
                            let _unused =ui.selectable_value(
                                &mut self.protocol,
                                ProtocolKind::DPlus,
                                "DPlus",
                            );
                            let _unused =ui.selectable_value(&mut self.protocol, ProtocolKind::Dcs, "DCS");
                        });
                    ui.end_row();

                    ui.label("Local module");
                    module_picker(ui, "local_mod", &mut self.local_module);
                    ui.end_row();

                    ui.label("Reflector module");
                    module_picker(ui, "remote_mod", &mut self.reflector_module);
                    ui.end_row();
                });

            ui.separator();

            ui.horizontal(|ui| {
                let connected = matches!(self.status, ConnStatus::Connected { .. });
                let busy = matches!(
                    self.status,
                    ConnStatus::Connecting { .. } | ConnStatus::Disconnecting
                );
                if ui
                    .add_enabled(!connected && !busy, egui::Button::new("Connect"))
                    .clicked()
                {
                    self.try_connect();
                }
                if ui
                    .add_enabled(connected && !busy, egui::Button::new("Disconnect"))
                    .clicked()
                {
                    self.try_disconnect();
                }
                ui.label(format!("Status: {}", fmt_status(&self.status)));
            });

            ui.separator();

            ui.horizontal(|ui| {
                let connected = matches!(self.status, ConnStatus::Connected { .. });
                let ptt_label = if self.active_tx { "PTT ON (click to stop)" } else { "PTT" };
                let colour = if self.active_tx {
                    egui::Color32::from_rgb(180, 40, 40)
                } else {
                    egui::Color32::DARK_GRAY
                };
                let ptt_btn = egui::Button::new(egui::RichText::new(ptt_label).color(egui::Color32::WHITE))
                    .fill(colour)
                    .min_size(egui::vec2(180.0, 40.0));
                if ui.add_enabled(connected, ptt_btn).clicked() {
                    self.toggle_ptt();
                }
                if ui
                    .add_enabled(connected && !self.active_tx, egui::Button::new("TX silence (2 s)"))
                    .clicked()
                {
                    self.tx_silence_test();
                }
            });

            if let Some(err) = &self.last_error {
                ui.colored_label(egui::Color32::LIGHT_RED, format!("Last error: {err}"));
            }

            ui.separator();

            ui.label("Event log");
            egui::ScrollArea::vertical()
                .auto_shrink([false, false])
                .stick_to_bottom(true)
                .max_height(220.0)
                .show(ui, |ui| {
                    for line in &self.log {
                        let colour = match line.level {
                            LogLevel::Info => egui::Color32::from_gray(200),
                            LogLevel::Event => egui::Color32::LIGHT_BLUE,
                            LogLevel::Error => egui::Color32::LIGHT_RED,
                        };
                        ui.colored_label(colour, &line.text);
                    }
                });
        });

        // Repaint frequently so log lines and voice events appear
        // within a few frames of arrival.
        ctx.request_repaint_after(std::time::Duration::from_millis(50));
    }
}

fn module_picker(ui: &mut egui::Ui, id: &str, value: &mut char) {
    egui::ComboBox::from_id_salt(id)
        .selected_text(String::from(*value))
        .show_ui(ui, |ui| {
            for ch in ['A', 'B', 'C', 'D', 'E'] {
                let _unused =ui.selectable_value(value, ch, String::from(ch));
            }
        });
}

fn fmt_status(s: &ConnStatus) -> String {
    match s {
        ConnStatus::Disconnected => "disconnected".into(),
        ConnStatus::Connecting { peer } => format!("connecting to {peer}"),
        ConnStatus::Connected { reflector, module } => {
            format!("connected — {reflector} / {module}")
        }
        ConnStatus::Disconnecting => "disconnecting…".into(),
    }
}
