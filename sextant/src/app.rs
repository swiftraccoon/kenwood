// SPDX-FileCopyrightText: 2026 Swift Raccoon
// SPDX-License-Identifier: GPL-2.0-or-later OR GPL-3.0-or-later

//! egui front-end.
//!
//! Two-page layout, switched by a toggle in the header (`ui::Page`):
//!
//! - Operator page: the everyday face. A central "deck" carries the
//!   hero (who's transmitting now), the heard list, and link
//!   readouts, with a bottom transmit strip (`tx_strip`) holding the
//!   PTT toggle, mic meter, and slow-data / GPS-beacon controls.
//! - Debug page: the engineering surface, the append-only event log
//!   plus stream-stats, TX, and audio tools.
//!
//! Overlays (`ui::Overlay`, at most one open at a time) draw above
//! whichever page is showing: the connect sheet (searchable reflector
//! directory + manual host form) and the gear settings popup
//! (callsign, behaviour toggles, and audio devices, in
//! `ui::settings_popup`). A dismissable error strip sits under the
//! header on both pages.

use std::num::NonZeroU16;

use dstar_gateway_core::hosts::HostAddress;
use dstar_gateway_core::slowdata::SlowDataTextMessage;
use dstar_gateway_core::types::{Module, ProtocolKind, RoutingCallsign};
use eframe::egui;

use crate::audio::{AudioCommand, AudioHandle, AudioMailbox, AudioStatus, FileProgress};
use crate::geo::{GpsPosition, TxPosition};
use crate::heard::HeardList;
use crate::hosts::{DirectoryUpdate, ReflectorDirectory};
use crate::session::{
    ConnStatus, ConnectConfig, ConnectionIdentity, Endpoint, RxRoute, SessionCommand, SessionEvent,
    SessionEvents, SessionHandle,
};
use crate::settings::{self, RxAudioMode, SavedHost, SavedHostFields, Settings, TimeMode};
use crate::ui;

/// Maximum lines kept in the event-log buffer. Older lines drop off
/// the top when this cap is exceeded.
const LOG_CAPACITY: usize = 500;

/// GUI app state + wiring to the async session task.
///
/// Fields are `pub(crate)` so the per-panel `show` functions in
/// [`crate::ui`] can render and mutate them directly.
pub(crate) struct App {
    // --- form state (what the user is currently editing) ---
    pub(crate) callsign: String,
    pub(crate) reflector_host: String,
    pub(crate) reflector_port: String,
    pub(crate) reflector_callsign: String,
    pub(crate) protocol: ProtocolKind,
    pub(crate) local_module: char,
    pub(crate) reflector_module: char,
    /// Auto-reconnect after a reflector-driven disconnect.
    pub(crate) reconnect_on_drop: bool,
    /// Persist the heard-list across launches.
    pub(crate) persist_heard_list: bool,
    /// RX playback mode (raw decoder output vs live enhancement),
    /// mirrored to the audio worker via `SetRxEnhance`.
    pub(crate) rx_audio: RxAudioMode,

    // --- transmit slow-data inputs (operator-entered) ---
    /// Slow-data text message the operator wants to transmit.
    pub(crate) tx_slow_text: String,
    /// Manual GPS-beacon entry form.
    pub(crate) tx_gps: TxGpsForm,

    // --- session state (what the session task has told us) ---
    pub(crate) status: ConnStatus,
    pub(crate) log: Vec<LogLine>,
    pub(crate) last_error: Option<String>,
    pub(crate) active_tx: bool,
    /// Slow-data text message from the current / most recent RX
    /// stream (20 chars max, trailing whitespace trimmed). Cleared
    /// when a new stream starts (the hero attributes it to the
    /// speaker on screen) and on Disconnect; per-station history
    /// lives in the heard list.
    pub(crate) last_slow_data: Option<SlowDataTextMessage>,
    /// Stations heard this session.
    pub(crate) heard: HeardList,
    /// Most recent decoded RX position (latitude, longitude).
    pub(crate) last_gps: Option<GpsPosition>,
    /// Loss counters for the current (or most recent) RX stream.
    pub(crate) last_rx_stats: Option<RxStreamStats>,
    /// Seconds since the reflector was last heard from (1 Hz sample).
    pub(crate) link_last_heard_secs: Option<f32>,
    /// Callsign of the currently-active incoming stream, captured from
    /// `VoiceStart` so the slow-data / GPS that arrive mid-stream can
    /// be attributed to the right heard-station.
    pub(crate) current_rx_callsign: Option<String>,
    /// Routing fields from the current / most recent RX stream's
    /// header. Replaced per stream; cleared on Disconnect.
    pub(crate) rx_route: Option<RxRoute>,

    // --- presentation state (which page / overlay is showing) ---
    /// Which page the header toggle shows.
    pub(crate) page: ui::Page,
    /// Which overlay is open, if any.
    pub(crate) overlay: ui::Overlay,
    /// Debug-page log filter.
    pub(crate) log_filter: LogFilter,
    /// Set while an RX voice stream is active (drives the hero
    /// elapsed timer); cleared on `VoiceEnd` / disconnect.
    pub(crate) rx_active_since: Option<std::time::Instant>,
    /// Set while transmitting (drives the TX ACTIVE elapsed timer).
    pub(crate) tx_active_since: Option<std::time::Instant>,
    /// Timestamp display mode (heard list + event log).
    pub(crate) time_mode: TimeMode,
    /// The machine's UTC offset, detected once at startup while the
    /// process was still single-threaded. `None` when undetectable;
    /// [`TimeMode::Local`] then falls back to UTC.
    pub(crate) local_offset: Option<time::UtcOffset>,

    /// Audio-worker status mirrored from the `AudioStatus` channel.
    pub(crate) audio_state: AudioState,
    /// Selected audio input device name (empty = host default).
    pub(crate) input_device: String,
    /// Selected audio output device name (empty = host default).
    pub(crate) output_device: String,
    /// WAV file path for the play / transmit-from-file controls.
    pub(crate) wav_path: String,

    /// The reflector directory shown in the connection panel's picker.
    pub(crate) directory: ReflectorDirectory,
    /// Current search query in the reflector picker.
    pub(crate) directory_query: String,
    /// Starred reflectors, pinned atop the directory picker.
    pub(crate) favorites: Vec<SavedHost>,
    /// Recent successful connections, most recent first.
    pub(crate) recents: Vec<SavedHost>,

    // --- channels ---
    cmd_tx: SessionHandle,
    /// The submitted target, retained until connection completion.
    pending_target: Option<SavedHost>,
    evt_rx: SessionEvents,
    audio: AudioHandle,
    /// Status channel from the audio worker.
    audio_status_rx: AudioMailbox,
}

/// One line in the event log.
#[derive(Debug, Clone)]
pub(crate) struct LogLine {
    /// Wall-clock stamp (`HH:MM:SS`) captured at append time, rendered in
    /// the operator's display timezone (see [`App::display_offset`]).
    pub(crate) stamp: String,
    pub(crate) level: LogLevel,
    pub(crate) text: String,
}

/// Severity of an event-log line, used to colour it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum LogLevel {
    Info,
    Event,
    Error,
}

/// Debug-page log filter: which levels the log view shows.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub(crate) enum LogFilter {
    /// Every line.
    #[default]
    All,
    /// Protocol events and errors (drops routine info lines).
    Events,
    /// Errors only.
    Errors,
}

impl LogFilter {
    /// True when a line of `level` passes this filter.
    pub(crate) const fn admits(self, level: LogLevel) -> bool {
        match self {
            Self::All => true,
            Self::Events => matches!(level, LogLevel::Event | LogLevel::Error),
            Self::Errors => matches!(level, LogLevel::Error),
        }
    }
}

/// Per-stream RX loss counters mirrored from the session task.
#[derive(Debug, Clone, Copy)]
pub(crate) struct RxStreamStats {
    /// Voice frames received and played.
    pub(crate) received: u32,
    /// Frames lost to sequence gaps.
    pub(crate) lost: u32,
    /// Frames dropped for arriving late.
    pub(crate) late: u32,
}

/// Audio-worker status mirrored into the GUI from the `AudioStatus`
/// channel: device lists, live levels, and recording state.
#[derive(Debug, Default)]
pub(crate) struct AudioState {
    /// Enumerated input device names.
    pub(crate) inputs: Vec<String>,
    /// Enumerated output device names.
    pub(crate) outputs: Vec<String>,
    /// Live TX mic peak level (`0.0..=1.0`).
    pub(crate) tx_level: f32,
    /// Live RX decoded-audio peak level (`0.0..=1.0`).
    pub(crate) rx_level: f32,
    /// True while a recording is active.
    pub(crate) recording: bool,
    /// Remaining local file audio, while decoding is active.
    pub(crate) playback: Option<FileProgress>,
    /// Remaining outgoing file audio, while decoding is active.
    pub(crate) file_tx: Option<FileProgress>,
}

/// The operator's manual GPS-beacon entry form. Latitude / longitude
/// are kept as edit strings so partial input round-trips; they're
/// parsed only when the beacon is pushed to the audio worker.
#[derive(Debug)]
pub(crate) struct TxGpsForm {
    /// True when the GPS beacon is enabled.
    pub(crate) enabled: bool,
    /// Latitude edit string.
    pub(crate) lat: String,
    /// Longitude edit string.
    pub(crate) lon: String,
    /// APRS symbol glyph.
    pub(crate) symbol: String,
    /// Free-text comment.
    pub(crate) comment: String,
}

impl Default for TxGpsForm {
    fn default() -> Self {
        Self {
            enabled: false,
            lat: String::new(),
            lon: String::new(),
            symbol: "/".into(),
            comment: String::new(),
        }
    }
}

impl App {
    /// Build the GUI app, wiring it to the already-spawned session
    /// task via `cmd_tx` / `evt_rx` and the shared audio worker via
    /// `audio`.
    pub(crate) fn new(
        cc: &eframe::CreationContext<'_>,
        cmd_tx: SessionHandle,
        evt_rx: SessionEvents,
        audio: AudioHandle,
        audio_status_rx: AudioMailbox,
        local_offset: Option<time::UtcOffset>,
    ) -> Self {
        crate::theme::apply(&cc.egui_ctx);
        let settings = Settings::load_or_default();
        let protocol = match settings.protocol.as_str() {
            "DPlus" => ProtocolKind::DPlus,
            "Dcs" | "DCS" => ProtocolKind::Dcs,
            // DExtra is the historical default, and the fallback for
            // unknown / future protocol strings so a forward-compat
            // settings file can't brick the GUI.
            _ => ProtocolKind::DExtra,
        };
        // Apply a persisted device choice on launch: the audio worker
        // starts on host defaults, so re-select only if a name is set.
        if !settings.input_device.is_empty() || !settings.output_device.is_empty() {
            audio.send(AudioCommand::SelectDevices {
                input: (!settings.input_device.is_empty()).then(|| settings.input_device.clone()),
                output: (!settings.output_device.is_empty())
                    .then(|| settings.output_device.clone()),
            });
        }
        // Deliver the persisted RX-enhancement mode: the audio worker
        // boots with enhancement off, so only a saved "enhanced" needs
        // sending.
        if settings.rx_audio.is_enhanced() {
            audio.send(AudioCommand::SetRxEnhance(true));
        }
        // Start from the bundled directory. Legacy generated XLX caches are
        // intentionally not loaded because their plaintext source cannot be
        // authenticated.
        let directory = ReflectorDirectory::bundled_only();
        if let Ok(callsign) = RoutingCallsign::new(&settings.callsign.trim().to_ascii_uppercase())
            && let Err(error) = cmd_tx.try_send(SessionCommand::RefreshDirectory(callsign))
        {
            tracing::error!(%error, "startup directory request was rejected");
        }
        Self {
            callsign: settings.callsign,
            reflector_host: settings.reflector_host,
            reflector_port: settings.reflector_port,
            reflector_callsign: settings.reflector_callsign,
            protocol,
            local_module: settings.local_module,
            reflector_module: settings.reflector_module,
            reconnect_on_drop: settings.reconnect_on_drop,
            persist_heard_list: settings.persist_heard_list,
            rx_audio: settings.rx_audio,
            tx_slow_text: settings.tx_message,
            tx_gps: TxGpsForm {
                enabled: settings.tx_beacon_enabled,
                lat: settings.tx_lat,
                lon: settings.tx_lon,
                symbol: settings.tx_symbol,
                comment: settings.tx_comment,
            },
            status: ConnStatus::Disconnected,
            log: Vec::new(),
            last_error: None,
            active_tx: false,
            last_slow_data: None,
            heard: if settings.persist_heard_list {
                HeardList::load()
            } else {
                HeardList::default()
            },
            last_gps: None,
            last_rx_stats: None,
            link_last_heard_secs: None,
            current_rx_callsign: None,
            rx_route: None,
            page: ui::Page::default(),
            overlay: ui::Overlay::default(),
            log_filter: LogFilter::default(),
            rx_active_since: None,
            tx_active_since: None,
            time_mode: settings.time_mode,
            local_offset,
            audio_state: AudioState::default(),
            input_device: settings.input_device,
            output_device: settings.output_device,
            wav_path: String::new(),
            directory,
            directory_query: String::new(),
            favorites: settings.favorites,
            recents: settings.recents,
            cmd_tx,
            pending_target: None,
            evt_rx,
            audio,
            audio_status_rx,
        }
    }

    /// Drain pending [`AudioStatus`] messages from the audio worker
    /// into the GUI's audio-state fields.
    fn drain_audio_status(&mut self) {
        for status in self.audio_status_rx.drain() {
            match status {
                AudioStatus::DeviceList { inputs, outputs } => {
                    self.audio_state.inputs = inputs;
                    self.audio_state.outputs = outputs;
                }
                AudioStatus::Levels { tx_peak, rx_peak } => {
                    self.audio_state.tx_level = tx_peak;
                    self.audio_state.rx_level = rx_peak;
                }
                AudioStatus::Recording(active) => {
                    self.audio_state.recording = active;
                }
                AudioStatus::Transmitting(active) => {
                    self.active_tx = active;
                    self.tx_active_since = active.then(std::time::Instant::now);
                }
                AudioStatus::Playback(progress) => self.audio_state.playback = progress,
                AudioStatus::FileTx(progress) => self.audio_state.file_tx = progress,
                AudioStatus::Error(error) => {
                    let message = error.to_string();
                    self.last_error = Some(message.clone());
                    self.append_log_line(LogLevel::Error, message);
                }
            }
        }
    }

    /// Apply the completed, session-owned directory operation.
    fn apply_directory(&mut self, update: DirectoryUpdate) {
        match update {
            DirectoryUpdate::AuthLoaded { hosts } => {
                let count = hosts.len();
                self.directory.replace_auth_hosts(hosts);
                self.append_log_line(
                    LogLevel::Info,
                    format!("loaded {count} REF hosts from the dstargateway auth server"),
                );
            }
            DirectoryUpdate::AuthFailed(err) => {
                self.directory.set_status(format!(
                    "reflector list: DPlus auth fetch failed ({err}); bundled list available"
                ));
                self.append_log_line(
                    LogLevel::Error,
                    format!("DPlus reflector directory fetch failed: {err}"),
                );
            }
        }
    }

    /// Queue a refresh of the authoritative dstargateway REF list.
    pub(crate) fn refresh_directory(&mut self) {
        self.directory
            .set_status("reflector list: fetching DPlus auth directory…".into());
        let result = RoutingCallsign::new(&self.callsign.trim().to_ascii_uppercase());
        match result {
            Ok(callsign) => {
                if let Err(error) = self
                    .cmd_tx
                    .try_send(SessionCommand::RefreshDirectory(callsign))
                {
                    self.last_error = Some(format!(
                        "Directory request rejected: {error}; retry when the session is ready"
                    ));
                }
            }
            Err(error) => self.last_error = Some(format!("Directory callsign rejected: {error}")),
        }
    }

    /// The UTC offset timestamps are displayed in, per the operator's
    /// [`TimeMode`] choice.
    pub(crate) fn display_offset(&self) -> time::UtcOffset {
        match self.time_mode {
            TimeMode::Utc => time::UtcOffset::UTC,
            TimeMode::Local => self.local_offset.unwrap_or(time::UtcOffset::UTC),
        }
    }

    /// Append a stamped line to the event log, evicting the oldest
    /// line past [`LOG_CAPACITY`].
    fn append_log_line(&mut self, level: LogLevel, text: String) {
        if self.log.len() >= LOG_CAPACITY {
            // Evict the oldest entry. `swap_remove(0)` would be O(1)
            // but reorders; for a log display we want FIFO order.
            let _removed = self.log.remove(0);
        }
        self.log.push(LogLine {
            stamp: ui::format::fmt_time_hms(time::OffsetDateTime::now_utc(), self.display_offset()),
            level,
            text,
        });
    }

    /// Apply a connection-status change: log it, reset TX and stale
    /// RX state on disconnect, and push the operator's slow-data once
    /// the link comes up.
    fn handle_status(&mut self, s: ConnStatus) {
        self.append_log_line(LogLevel::Info, format!("status: {}", ui::fmt_status(&s)));
        // When we disconnect, make sure the PTT toggle resets so the
        // GUI can't get stuck "transmitting" without an active session.
        if matches!(s, ConnStatus::Disconnected) {
            self.pending_target = None;
            self.active_tx = false;
            self.audio.send(AudioCommand::StopTx);
            // Clear stale RX state from the prior session;
            // a new session will populate fresh values.
            self.last_slow_data = None;
            self.last_gps = None;
            self.last_rx_stats = None;
            self.link_last_heard_secs = None;
            self.current_rx_callsign = None;
            self.rx_route = None;
            self.rx_active_since = None;
            self.tx_active_since = None;
        }
        self.status = s;
        // On reaching Connected, push the operator's slow-data so a
        // beacon configured before connecting takes effect immediately.
        if matches!(self.status, ConnStatus::Connected { .. }) {
            self.push_slow_data();
            // Remember the connection and persist: a successful
            // connect is a natural checkpoint, and saving here means
            // recents survive a crash.
            if let Some(entry) = self.pending_target.take() {
                settings::push_recent(&mut self.recents, entry);
            }
            self.persist_settings();
        }
    }

    fn drain_events(&mut self) {
        while let Ok(evt) = self.evt_rx.try_recv() {
            match evt {
                SessionEvent::Status(s) => self.handle_status(s),
                SessionEvent::Log(t) => self.append_log_line(LogLevel::Info, t),
                SessionEvent::VoiceStart {
                    stream_id,
                    from,
                    route,
                } => {
                    // Decoder reset is driven by the session task
                    // (direct to audio worker); the GUI only shows
                    // the event in the log.
                    self.append_log_line(
                        LogLevel::Event,
                        format!("VoiceStart sid=0x{stream_id:04X} from={from}"),
                    );
                    let callsign = from.trim().to_owned();
                    self.heard
                        .record_stream(&callsign, time::OffsetDateTime::now_utc());
                    self.current_rx_callsign = Some(callsign);
                    self.rx_route = Some(route);
                    self.rx_active_since = Some(std::time::Instant::now());
                    // The hero attributes slow-data / position to the
                    // speaker on screen; a new stream must not
                    // inherit the previous speaker's. Their copy
                    // stays on their heard-list row.
                    self.last_slow_data = None;
                    self.last_gps = None;
                }
                SessionEvent::VoiceEnd {
                    stream_id,
                    frames,
                    reason,
                } => {
                    self.append_log_line(
                        LogLevel::Event,
                        format!("VoiceEnd sid=0x{stream_id:04X} frames={frames} reason={reason:?}"),
                    );
                    self.rx_active_since = None;
                }
                SessionEvent::Error(e) => {
                    self.last_error = Some(e.clone());
                    self.append_log_line(LogLevel::Error, e);
                }
                SessionEvent::SlowDataMessage { stream_id, message } => {
                    let rendered = render_slow_data_text(&message);
                    self.append_log_line(
                        LogLevel::Event,
                        format!("SlowData sid=0x{stream_id:04X}: {rendered:?}"),
                    );
                    if let Some(callsign) = self.current_rx_callsign.clone() {
                        self.heard.record_message(&callsign, rendered);
                    }
                    self.last_slow_data = Some(message);
                }
                SessionEvent::GpsPosition {
                    stream_id,
                    position,
                } => {
                    self.append_log_line(
                        LogLevel::Event,
                        format!(
                            "GPS sid=0x{stream_id:04X} {:.4},{:.4}",
                            position.latitude(),
                            position.longitude()
                        ),
                    );
                    if let Some(callsign) = self.current_rx_callsign.clone() {
                        self.heard.record_gps(&callsign, position);
                    }
                    self.last_gps = Some(position);
                }
                SessionEvent::RxStats {
                    received,
                    lost,
                    late,
                } => {
                    // No log line: this fires up to once per second;
                    // VoiceEnd already logs the final frame count.
                    self.last_rx_stats = Some(RxStreamStats {
                        received,
                        lost,
                        late,
                    });
                }
                SessionEvent::LinkHealth { last_heard_secs } => {
                    // 1 Hz sample: display-only, never logged.
                    self.link_last_heard_secs = Some(last_heard_secs);
                }
                SessionEvent::ReflectorHosts(hosts) => {
                    let count = hosts.len();
                    let merged = hosts
                        .into_iter()
                        .map(|host| crate::hosts::ReflectorHost {
                            callsign: host.callsign.as_str().to_owned(),
                            host: host.address.to_string(),
                            port: 20001,
                            protocol: ProtocolKind::DPlus,
                            source: crate::hosts::HostSource::DPlusAuth,
                        })
                        .collect();
                    self.directory.replace_auth_hosts(merged);
                    self.append_log_line(
                        LogLevel::Info,
                        format!("loaded {count} REF hosts from DPlus auth"),
                    );
                }
                SessionEvent::DirectoryUpdate(update) => self.apply_directory(update),
            }
        }
    }

    pub(crate) fn try_connect(&mut self) {
        if self.pending_target.is_some() || !matches!(self.status, ConnStatus::Disconnected) {
            self.last_error = Some(
                "Connection already active; disconnect before choosing another target".to_owned(),
            );
            return;
        }
        let cfg = match self.build_connect_config() {
            Ok(c) => c,
            Err(e) => {
                self.last_error = Some(e.clone());
                self.append_log_line(LogLevel::Error, format!("invalid config: {e}"));
                return;
            }
        };
        let target = match SavedHost::new(SavedHostFields {
            callsign: &self.reflector_callsign.trim().to_ascii_uppercase(),
            host: self.reflector_host.trim(),
            port: self.reflector_port.trim(),
            protocol: &format!("{:?}", self.protocol),
            module: self.reflector_module,
        }) {
            Ok(target) => target,
            Err(error) => {
                self.last_error = Some(error.to_string());
                return;
            }
        };
        self.append_log_line(
            LogLevel::Info,
            format!(
                "connecting as {} → {} module {}",
                cfg.callsign(),
                self.reflector_callsign.trim().to_uppercase(),
                self.reflector_module
            ),
        );
        match self.cmd_tx.try_send(SessionCommand::Connect(cfg)) {
            Ok(()) => self.pending_target = Some(target),
            Err(error) => {
                self.last_error = Some(format!(
                    "Connection request rejected: {error}; retry when the session is ready"
                ));
            }
        }
    }

    pub(crate) fn try_disconnect(&mut self) {
        self.cmd_tx.disconnect();
        // Persist current form state when the user voluntarily
        // disconnects; this is the natural checkpoint where they're
        // most likely to have settled on the values they want next time.
        self.persist_settings();
        if self.persist_heard_list {
            self.persist_heard();
        }
    }

    pub(crate) fn persist_settings(&mut self) {
        let result = self.snapshot_settings().save();
        self.report_save("settings", result);
    }

    fn persist_heard(&mut self) {
        let result = self.heard.save();
        self.report_save("heard stations", result);
    }

    fn report_save(
        &mut self,
        snapshot: &str,
        result: Result<Option<crate::persistence::SaveOutcome>, crate::persistence::SnapshotError>,
    ) {
        let message = match result {
            Ok(Some(outcome)) => {
                tracing::debug!(snapshot, ?outcome, "snapshot saved");
                return;
            }
            Ok(None) => format!("{snapshot} were not saved: configuration directory unavailable"),
            Err(error) => format!("{snapshot} were not saved completely: {error}"),
        };
        tracing::error!(%message);
        self.last_error = Some(message.clone());
        self.append_log_line(LogLevel::Error, message);
    }

    pub(crate) fn snapshot_settings(&self) -> Settings {
        Settings {
            callsign: self.callsign.clone(),
            reflector_host: self.reflector_host.clone(),
            reflector_port: self.reflector_port.clone(),
            reflector_callsign: self.reflector_callsign.clone(),
            protocol: format!("{:?}", self.protocol),
            local_module: self.local_module,
            reflector_module: self.reflector_module,
            reconnect_on_drop: self.reconnect_on_drop,
            persist_heard_list: self.persist_heard_list,
            time_mode: self.time_mode,
            rx_audio: self.rx_audio,
            tx_message: self.tx_slow_text.clone(),
            tx_beacon_enabled: self.tx_gps.enabled,
            tx_lat: self.tx_gps.lat.clone(),
            tx_lon: self.tx_gps.lon.clone(),
            tx_symbol: self.tx_gps.symbol.clone(),
            tx_comment: self.tx_gps.comment.clone(),
            input_device: self.input_device.clone(),
            output_device: self.output_device.clone(),
            favorites: self.favorites.clone(),
            recents: self.recents.clone(),
        }
    }

    /// True when `host` is starred (matched by callsign + protocol,
    /// the same key the directory dedupes on).
    pub(crate) fn is_favorite(&self, host: &crate::hosts::ReflectorHost) -> bool {
        self.favorites.iter().any(|f| {
            f.callsign()
                .to_string()
                .eq_ignore_ascii_case(&host.callsign)
                && f.protocol() == host.protocol
        })
    }

    /// Star / unstar a directory entry and persist immediately.
    pub(crate) fn toggle_favorite(&mut self, host: &crate::hosts::ReflectorHost) {
        let proto = format!("{:?}", host.protocol);
        if let Some(pos) = self.favorites.iter().position(|f| {
            f.callsign()
                .to_string()
                .eq_ignore_ascii_case(&host.callsign)
                && f.protocol() == host.protocol
        }) {
            let _removed = self.favorites.remove(pos);
        } else {
            if self.favorites.len() >= settings::FAVORITES_CAP {
                self.last_error =
                    Some("Favorite list full; remove a target before adding another".to_owned());
                return;
            }
            let saved = SavedHost::new(SavedHostFields {
                callsign: &host.callsign,
                host: &host.host,
                port: &host.port.to_string(),
                protocol: &proto,
                module: self.reflector_module,
            });
            match saved {
                Ok(saved) => self.favorites.push(saved),
                Err(error) => {
                    self.last_error = Some(error.to_string());
                    return;
                }
            }
        }
        self.persist_settings();
    }

    /// Fill the connection form from a saved favorite / recent.
    pub(crate) fn apply_saved_host(&mut self, saved: &SavedHost) {
        self.reflector_callsign = saved.callsign().to_string();
        self.reflector_host = saved.host().to_string();
        self.reflector_port = saved.port().to_string();
        self.protocol = saved.protocol();
        self.reflector_module = saved.module().as_char();
    }

    pub(crate) fn toggle_ptt(&mut self) {
        if self.active_tx {
            self.audio.send(AudioCommand::StopTx);
        } else if matches!(self.status, ConnStatus::Connected { .. }) {
            self.audio.send(AudioCommand::StartTx);
        } else {
            self.append_log_line(LogLevel::Error, "cannot TX: not connected".into());
        }
    }

    /// Start transmission on Space outside an editable field; always stop on
    /// release, including when focus changes during the transmission.
    fn handle_ptt_keybinding(&self, ctx: &egui::Context) {
        let connected = matches!(self.status, ConnStatus::Connected { .. });
        if let Some(command) = ptt_command(ctx, connected, self.active_tx) {
            self.audio.send(command);
        }
    }

    pub(crate) fn tx_silence_test(&mut self) {
        if let Err(error) = self.cmd_tx.try_send(SessionCommand::TxSilence) {
            self.last_error = Some(format!(
                "Silence request rejected: {error}; retry when the session is ready"
            ));
        }
    }

    /// Apply the selected audio devices to the worker and persist the
    /// choice. Called when the operator changes a device in the panel.
    pub(crate) fn apply_audio_devices(&mut self) {
        self.audio.send(AudioCommand::SelectDevices {
            input: (!self.input_device.is_empty()).then(|| self.input_device.clone()),
            output: (!self.output_device.is_empty()).then(|| self.output_device.clone()),
        });
        self.persist_settings();
    }

    /// Ask the audio worker to re-enumerate available devices.
    pub(crate) fn refresh_devices(&self) {
        self.audio.send(AudioCommand::EnumerateDevices);
    }

    /// Push the RX playback mode (raw vs live-enhanced) to the audio
    /// worker. Called when the operator flips the settings checkbox.
    pub(crate) fn apply_rx_audio_mode(&self) {
        self.audio
            .send(AudioCommand::SetRxEnhance(self.rx_audio.is_enhanced()));
    }

    /// Start recording received audio to a WAV file.
    pub(crate) fn start_recording(&self) {
        self.audio.send(AudioCommand::StartRecording);
    }

    /// Stop the active recording.
    pub(crate) fn stop_recording(&self) {
        self.audio.send(AudioCommand::StopRecording);
    }

    /// Play the WAV at `wav_path` to the speakers locally.
    pub(crate) fn play_wav(&self) {
        let path = self.wav_path.trim();
        if !path.is_empty() {
            self.audio
                .send(AudioCommand::PlayFile { path: path.into() });
        }
    }

    /// Stop local WAV decoding while buffered speaker samples drain.
    pub(crate) fn stop_playback(&self) {
        self.audio.send(AudioCommand::StopPlayback);
    }

    /// Transmit the WAV at `wav_path` as an outgoing voice stream.
    pub(crate) fn transmit_wav(&self) {
        let path = self.wav_path.trim();
        if !path.is_empty() {
            self.audio
                .send(AudioCommand::TransmitFile { path: path.into() });
        }
    }

    /// Push the current slow-data text + GPS beacon to the audio
    /// worker. Called whenever the operator edits a slow-data field.
    pub(crate) fn push_slow_data(&mut self) {
        let gps = if self.tx_gps.enabled {
            self.parse_tx_gps()
        } else {
            None
        };
        if self.tx_gps.enabled && gps.is_none() {
            self.last_error = Some("GPS beacon is disabled: correct the coordinates, one ASCII symbol and a printable comment of at most 43 bytes".to_owned());
        }
        let (command, error) = build_slow_data_update(&self.tx_slow_text, gps);
        self.audio.send(command);
        if let Some(error) = error {
            self.last_error = Some(error);
        }
    }

    /// Parse the manual GPS fields into a [`TxPosition`], or `None` if
    /// coordinates, primary-table symbol or plain-text comment are invalid.
    pub(crate) fn parse_tx_gps(&self) -> Option<TxPosition> {
        let latitude: f64 = self.tx_gps.lat.trim().parse().ok()?;
        let longitude: f64 = self.tx_gps.lon.trim().parse().ok()?;
        let mut symbols = self.tx_gps.symbol.chars();
        let symbol = symbols.next()?;
        if symbols.next().is_some() {
            return None;
        }
        TxPosition::new(latitude, longitude, symbol, &self.tx_gps.comment).ok()
    }

    /// Operator page with the transmit strip below the station deck.
    fn show_operator_page(&mut self, ui: &mut egui::Ui) {
        let _unused = egui::Panel::bottom("tx_strip")
            .frame(
                egui::Frame::NONE
                    .fill(crate::theme::BG_INSET)
                    .inner_margin(egui::Margin::symmetric(16, 10)),
            )
            .show(ui, |ui| {
                ui::operator::tx_strip::show(self, ui);
            });
        let _unused = egui::CentralPanel::default().show(ui, |ui| {
            ui::operator::show_deck(self, ui);
        });
    }

    /// Debug page: event log + engineering tools.
    fn show_debug_page(&mut self, ui: &mut egui::Ui) {
        ui::debug::show(self, ui);
    }

    fn build_connect_config(&self) -> Result<ConnectConfig, String> {
        let callsign = RoutingCallsign::new(&self.callsign.trim().to_ascii_uppercase())
            .map_err(|e| format!("callsign {:?}: {e}", self.callsign))?;
        let reflector_callsign =
            RoutingCallsign::new(&self.reflector_callsign.trim().to_ascii_uppercase())
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
        let port = NonZeroU16::new(port).ok_or("reflector port must be nonzero")?;
        let host = HostAddress::new(self.reflector_host.trim())
            .map_err(|error| format!("reflector address: {error}"))?;
        ConnectConfig::new(
            self.protocol,
            ConnectionIdentity {
                operator: callsign,
                local_module,
                reflector: reflector_callsign,
                reflector_module,
            },
            Endpoint::new(host, port),
            self.reconnect_on_drop,
        )
        .map_err(|error| error.to_string())
    }
}

/// Build the next slow-data worker update without retaining stale text when
/// validation rejects the operator's latest edit.
fn build_slow_data_update(input: &str, gps: Option<TxPosition>) -> (AudioCommand, Option<String>) {
    let text = if input.is_empty() {
        None
    } else {
        match SlowDataTextMessage::try_from_text(input) {
            Ok(message) => Some(message),
            Err(error) => {
                return (
                    AudioCommand::SetSlowData { text: None, gps },
                    Some(format!("Slow-data text rejected: {error}")),
                );
            }
        }
    };
    (AudioCommand::SetSlowData { text, gps }, None)
}

/// Render validated slow-data text or an exact diagnostic for opaque bytes.
pub(crate) fn render_slow_data_text(message: &SlowDataTextMessage) -> String {
    match message.text() {
        Ok(text) => text.to_owned(),
        Err(error) => {
            let hexadecimal = message
                .as_bytes()
                .iter()
                .map(|byte| format!("{byte:02X}"))
                .collect::<Vec<_>>()
                .join(" ");
            format!("<invalid slow-data bytes: {hexadecimal}; {error}>")
        }
    }
}

impl eframe::App for App {
    /// Persist form state when the window closes, covering the common
    /// case of quitting without an explicit disconnect.
    fn on_exit(&mut self, _gl: Option<&eframe::glow::Context>) {
        self.persist_settings();
        if self.persist_heard_list {
            self.persist_heard();
        }
    }

    fn logic(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        self.drain_events();
        self.drain_audio_status();

        // Continue observing the worker while the application window is hidden.
        ctx.request_repaint_after(std::time::Duration::from_millis(50));
    }

    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        let ctx = ui.ctx().clone();
        self.handle_ptt_keybinding(&ctx);

        let _unused = egui::Panel::top("header")
            .frame(
                egui::Frame::NONE
                    .fill(crate::theme::BG_WINDOW)
                    .inner_margin(egui::Margin::symmetric(16, 10)),
            )
            .show(ui, |ui| {
                ui::header(self, ui);
            });
        if self.last_error.is_some() {
            let _unused = egui::Panel::top("error_strip")
                .frame(egui::Frame::NONE)
                .show(ui, |ui| {
                    ui::error_strip(self, ui);
                });
        }
        match self.page {
            ui::Page::Operator => self.show_operator_page(ui),
            ui::Page::Debug => self.show_debug_page(ui),
        }
        // Overlays draw last so they sit above both pages.
        ui::operator::connect_sheet::show(self, &ctx);
        ui::settings_popup::show(self, &ctx);
    }
}

/// Translate keyboard state into a worker request with release taking priority.
fn ptt_command(ctx: &egui::Context, connected: bool, active: bool) -> Option<AudioCommand> {
    let typing = ctx.memory(|memory| memory.focused().is_some());
    ctx.input(|input| {
        if input.key_released(egui::Key::Space) {
            Some(AudioCommand::StopTx)
        } else if input.key_pressed(egui::Key::Space) && !typing && connected && !active {
            Some(AudioCommand::StartTx)
        } else {
            None
        }
    })
}

#[cfg(test)]
mod tests {
    use super::{LogFilter, LogLevel, build_slow_data_update, egui};
    use crate::audio::AudioCommand;
    use crate::geo::TxPosition;

    #[test]
    fn space_release_stops_transmission_after_focus_moves_into_a_field() {
        let context = egui::Context::default();
        let key = |pressed| egui::Event::Key {
            key: egui::Key::Space,
            physical_key: None,
            pressed,
            repeat: false,
            modifiers: egui::Modifiers::NONE,
        };
        context.begin_pass(egui::RawInput {
            events: vec![key(true)],
            ..Default::default()
        });
        assert!(matches!(
            super::ptt_command(&context, true, false),
            Some(AudioCommand::StartTx)
        ));
        context.end_pass().textures_delta.clear();

        context.begin_pass(egui::RawInput {
            events: vec![key(false)],
            ..Default::default()
        });
        context.memory_mut(|memory| memory.request_focus(egui::Id::unique("callsign")));
        assert!(
            context.memory(|memory| memory.focused().is_some()),
            "test must exercise focused input"
        );
        assert!(
            matches!(
                super::ptt_command(&context, true, true),
                Some(AudioCommand::StopTx)
            ),
            "focused input swallowed the stop request"
        );
        context.end_pass().textures_delta.clear();

        context.begin_pass(egui::RawInput {
            events: vec![key(true)],
            ..Default::default()
        });
        context.memory_mut(|memory| memory.request_focus(egui::Id::unique("callsign")));
        assert!(
            super::ptt_command(&context, true, false).is_none(),
            "typing a space started transmission"
        );
        context.end_pass().textures_delta.clear();
    }

    #[test]
    fn status_text_covers_every_variant() -> Result<(), Box<dyn std::error::Error>> {
        use crate::session::{ConnStatus, Endpoint};
        use crate::ui::fmt_status;
        assert_eq!(fmt_status(&ConnStatus::Disconnected), "disconnected");
        let peer = Endpoint::new(
            dstar_gateway_core::hosts::HostAddress::new("127.0.0.1")?,
            std::num::NonZeroU16::new(30001).ok_or("nonzero port")?,
        );
        assert!(
            fmt_status(&ConnStatus::Connecting { peer }).contains("127.0.0.1"),
            "connecting shows the peer"
        );
        assert_eq!(fmt_status(&ConnStatus::Disconnecting), "disconnecting…");
        Ok(())
    }

    #[test]
    fn log_filter_admits_matrix() {
        assert!(LogFilter::All.admits(LogLevel::Info));
        assert!(LogFilter::All.admits(LogLevel::Error));
        assert!(!LogFilter::Events.admits(LogLevel::Info));
        assert!(LogFilter::Events.admits(LogLevel::Event));
        assert!(LogFilter::Events.admits(LogLevel::Error));
        assert!(!LogFilter::Errors.admits(LogLevel::Event));
        assert!(LogFilter::Errors.admits(LogLevel::Error));
    }

    #[test]
    fn rejected_slow_data_text_clears_stale_text_and_keeps_gps()
    -> Result<(), Box<dyn std::error::Error>> {
        let gps = TxPosition::new(35.5951, -82.5515, '/', "test")?;
        let (command, error) = build_slow_data_update(&"X".repeat(21), Some(gps.clone()));

        assert!(
            error.is_some_and(|message| message.contains("maximum is 20 bytes")),
            "validation error should state the wire limit"
        );
        assert!(
            matches!(
                command,
                AudioCommand::SetSlowData {
                    text: None,
                    gps: Some(actual),
                } if actual == gps
            ),
            "a rejected edit must clear scheduled text without dropping valid GPS"
        );
        Ok(())
    }
}
