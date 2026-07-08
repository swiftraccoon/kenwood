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
use std::sync::mpsc as std_mpsc;

use dstar_gateway_core::types::{Callsign, Module, ProtocolKind};
use eframe::egui;
use tokio::runtime::Runtime;
use tokio::sync::mpsc;

use crate::audio::{AudioCommand, AudioHandle, AudioStatus};
use crate::geo::TxPosition;
use crate::heard::HeardList;
use crate::hosts::{DirectoryUpdate, ReflectorDirectory};
use crate::session::{ConnStatus, ConnectConfig, RxRoute, SessionCommand, SessionEvent};
use crate::settings::{self, SavedHost, Settings, TimeMode};
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
    pub(crate) last_slow_data: Option<String>,
    /// Stations heard this session.
    pub(crate) heard: HeardList,
    /// Most recent decoded RX position (latitude, longitude).
    pub(crate) last_gps: Option<(f64, f64)>,
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
    /// Set while transmitting (drives the ON AIR elapsed timer).
    pub(crate) tx_active_since: Option<std::time::Instant>,
    /// Timestamp display mode (heard list + event log).
    pub(crate) time_mode: TimeMode,
    /// The machine's UTC offset, detected once at startup while the
    /// process was still single-threaded. `None` when undetectable —
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
    cmd_tx: mpsc::Sender<SessionCommand>,
    evt_rx: mpsc::Receiver<SessionEvent>,
    audio: AudioHandle,
    /// Status channel from the audio worker.
    audio_status_rx: std_mpsc::Receiver<AudioStatus>,
    /// Result channel for background directory fetches.
    directory_rx: std_mpsc::Receiver<DirectoryUpdate>,
    /// Sender cloned into each spawned fetch task.
    directory_tx: std_mpsc::Sender<DirectoryUpdate>,

    // Owns the runtime so it lives for the whole app lifetime; also
    // used to spawn background directory fetches.
    runtime: Runtime,
}

/// One line in the event log.
#[derive(Debug, Clone)]
pub(crate) struct LogLine {
    /// UTC wall-clock stamp (`HH:MM:SS`) captured at append time.
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

/// Debug-page log filter — which levels the log view shows.
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
/// channel — device lists, live levels, and recording state.
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
        cmd_tx: mpsc::Sender<SessionCommand>,
        evt_rx: mpsc::Receiver<SessionEvent>,
        audio: AudioHandle,
        audio_status_rx: std_mpsc::Receiver<AudioStatus>,
        runtime: Runtime,
        local_offset: Option<time::UtcOffset>,
    ) -> Self {
        crate::theme::apply(&cc.egui_ctx);
        let settings = Settings::load_or_default();
        let protocol = match settings.protocol.as_str() {
            "DPlus" => ProtocolKind::DPlus,
            "Dcs" | "DCS" => ProtocolKind::Dcs,
            // DExtra is the historical default — also the fallback for
            // unknown / future protocol strings so a forward-compat
            // settings file can't brick the GUI.
            _ => ProtocolKind::DExtra,
        };
        // Apply a persisted device choice on launch — the audio worker
        // starts on host defaults, so re-select only if a name is set.
        if !settings.input_device.is_empty() || !settings.output_device.is_empty() {
            audio.send(AudioCommand::SelectDevices {
                input: (!settings.input_device.is_empty()).then(|| settings.input_device.clone()),
                output: (!settings.output_device.is_empty())
                    .then(|| settings.output_device.clone()),
            });
        }
        // Load the reflector directory from cache, then kick off a
        // background refresh so the picker is populated immediately
        // and updated when the network call returns.
        let directory = ReflectorDirectory::load_cached();
        let (directory_tx, directory_rx) = std_mpsc::channel();
        {
            let tx = directory_tx.clone();
            let _join = runtime.spawn(async move {
                let _send = tx.send(crate::hosts::fetch_directory().await);
            });
        }
        {
            // Authoritative REF list from the DPlus auth server — the
            // same startup exchange every DPlus dongle performs. Must
            // outrank the XLX registry's REF-alias entries, which
            // point at unrelated XLX reflectors.
            let tx = directory_tx.clone();
            let callsign = settings.callsign.clone();
            let _join = runtime.spawn(async move {
                let _send = tx.send(crate::hosts::fetch_auth_directory(callsign).await);
            });
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
            evt_rx,
            audio,
            audio_status_rx,
            directory_rx,
            directory_tx,
            runtime,
        }
    }

    /// Drain pending [`AudioStatus`] messages from the audio worker
    /// into the GUI's audio-state fields.
    fn drain_audio_status(&mut self) {
        while let Ok(status) = self.audio_status_rx.try_recv() {
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
            }
        }
    }

    /// Drain pending [`DirectoryUpdate`]s from background fetches into
    /// the directory, refreshing the on-disk cache on success.
    fn drain_directory(&mut self) {
        while let Ok(update) = self.directory_rx.try_recv() {
            match update {
                DirectoryUpdate::Loaded { hosts, when } => {
                    self.directory.replace_fetched(hosts, &when);
                    self.directory.save_cache(&when);
                }
                DirectoryUpdate::AuthLoaded { hosts } => {
                    let count = hosts.len();
                    self.directory.merge_hosts(hosts);
                    self.append_log_line(
                        LogLevel::Info,
                        format!("merged {count} REF hosts from the dstargateway auth server"),
                    );
                }
                DirectoryUpdate::Failed(err) => {
                    self.directory.set_status(format!(
                        "reflector list: fetch failed ({err}) — using cache"
                    ));
                    self.append_log_line(
                        LogLevel::Error,
                        format!("reflector directory fetch failed: {err}"),
                    );
                }
            }
        }
    }

    /// Spawn an on-demand reflector-directory refresh (both the XLX
    /// registry and the authoritative dstargateway REF list).
    pub(crate) fn refresh_directory(&mut self) {
        self.directory
            .set_status("reflector list: fetching…".into());
        let tx = self.directory_tx.clone();
        let _join = self.runtime.spawn(async move {
            let _send = tx.send(crate::hosts::fetch_directory().await);
        });
        let tx = self.directory_tx.clone();
        let callsign = self.callsign.clone();
        let _join = self.runtime.spawn(async move {
            let _send = tx.send(crate::hosts::fetch_auth_directory(callsign).await);
        });
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
            // but reorders — for a log display we want FIFO order.
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
            self.active_tx = false;
            self.audio.send(AudioCommand::StopTx);
            // Clear stale RX state from the prior session —
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
            // Remember the connection and persist — a successful
            // connect is a natural checkpoint, and saving here means
            // recents survive a crash.
            let entry = SavedHost {
                callsign: self.reflector_callsign.trim().to_uppercase(),
                host: self.reflector_host.clone(),
                port: self.reflector_port.clone(),
                protocol: format!("{:?}", self.protocol),
                module: self.reflector_module,
            };
            settings::push_recent(&mut self.recents, entry);
            self.snapshot_settings().save();
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
                    // (direct to audio worker) — the GUI only shows
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
                    // speaker on screen — a new stream must not
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
                        format!("VoiceEnd sid=0x{stream_id:04X} frames={frames} reason={reason}"),
                    );
                    self.rx_active_since = None;
                }
                SessionEvent::Error(e) => {
                    self.last_error = Some(e.clone());
                    self.append_log_line(LogLevel::Error, e);
                }
                SessionEvent::SlowDataMessage { stream_id, text } => {
                    self.append_log_line(
                        LogLevel::Event,
                        format!("SlowData sid=0x{stream_id:04X}: {text:?}"),
                    );
                    if let Some(callsign) = self.current_rx_callsign.clone() {
                        self.heard.record_message(&callsign, text.clone());
                    }
                    self.last_slow_data = Some(text);
                }
                SessionEvent::GpsPosition {
                    stream_id,
                    latitude,
                    longitude,
                } => {
                    self.append_log_line(
                        LogLevel::Event,
                        format!("GPS sid=0x{stream_id:04X} {latitude:.4},{longitude:.4}"),
                    );
                    if let Some(callsign) = self.current_rx_callsign.clone() {
                        self.heard.record_gps(&callsign, latitude, longitude);
                    }
                    self.last_gps = Some((latitude, longitude));
                }
                SessionEvent::RxStats {
                    received,
                    lost,
                    late,
                } => {
                    // No log line — this fires up to once per second;
                    // VoiceEnd already logs the final frame count.
                    self.last_rx_stats = Some(RxStreamStats {
                        received,
                        lost,
                        late,
                    });
                }
                SessionEvent::LinkHealth { last_heard_secs } => {
                    // 1 Hz sample — display-only, never logged.
                    self.link_last_heard_secs = Some(last_heard_secs);
                }
                SessionEvent::ReflectorHosts(hosts) => {
                    let count = hosts.len();
                    let merged = hosts
                        .into_iter()
                        .map(|(callsign, addr)| crate::hosts::ReflectorHost {
                            callsign,
                            host: addr.to_string(),
                            port: 20001,
                            protocol: ProtocolKind::DPlus,
                            source: crate::hosts::HostSource::DPlusAuth,
                        })
                        .collect();
                    self.directory.merge_hosts(merged);
                    self.append_log_line(
                        LogLevel::Info,
                        format!("merged {count} REF hosts from DPlus auth"),
                    );
                }
            }
        }
    }

    pub(crate) fn try_connect(&mut self) {
        let cfg = match self.build_connect_config() {
            Ok(c) => c,
            Err(e) => {
                self.last_error = Some(e.clone());
                self.append_log_line(LogLevel::Error, format!("invalid config: {e}"));
                return;
            }
        };
        // Record the identity every attempt goes out as — a stale
        // callsign silently poisons DPlus auth, so the log must make
        // it visible per-attempt.
        self.append_log_line(
            LogLevel::Info,
            format!(
                "connecting as {} → {} module {}",
                cfg.callsign,
                self.reflector_callsign.trim().to_uppercase(),
                self.reflector_module
            ),
        );
        let _unused = self.cmd_tx.try_send(SessionCommand::Connect(cfg));
    }

    pub(crate) fn try_disconnect(&self) {
        let _unused = self.cmd_tx.try_send(SessionCommand::Disconnect);
        // Persist current form state when the user voluntarily
        // disconnects — this is the natural checkpoint where they're
        // most likely to have settled on the values they want next time.
        self.snapshot_settings().save();
        if self.persist_heard_list {
            self.heard.save();
        }
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
        let proto = format!("{:?}", host.protocol);
        self.favorites
            .iter()
            .any(|f| f.callsign.eq_ignore_ascii_case(&host.callsign) && f.protocol == proto)
    }

    /// Star / unstar a directory entry and persist immediately.
    pub(crate) fn toggle_favorite(&mut self, host: &crate::hosts::ReflectorHost) {
        let proto = format!("{:?}", host.protocol);
        if let Some(pos) = self
            .favorites
            .iter()
            .position(|f| f.callsign.eq_ignore_ascii_case(&host.callsign) && f.protocol == proto)
        {
            let _removed = self.favorites.remove(pos);
        } else {
            self.favorites.push(SavedHost {
                callsign: host.callsign.clone(),
                host: host.host.clone(),
                port: host.port.to_string(),
                protocol: proto,
                module: self.reflector_module,
            });
        }
        self.snapshot_settings().save();
    }

    /// Fill the connection form from a saved favorite / recent.
    pub(crate) fn apply_saved_host(&mut self, saved: &SavedHost) {
        self.reflector_callsign.clone_from(&saved.callsign);
        self.reflector_host.clone_from(&saved.host);
        self.reflector_port.clone_from(&saved.port);
        self.protocol = match saved.protocol.as_str() {
            "DPlus" => ProtocolKind::DPlus,
            "Dcs" | "DCS" => ProtocolKind::Dcs,
            _ => ProtocolKind::DExtra,
        };
        self.reflector_module = saved.module;
    }

    pub(crate) fn toggle_ptt(&mut self) {
        if self.active_tx {
            self.active_tx = false;
            self.tx_active_since = None;
            self.audio.send(AudioCommand::StopTx);
        } else if matches!(self.status, ConnStatus::Connected { .. }) {
            self.active_tx = true;
            self.tx_active_since = Some(std::time::Instant::now());
            self.audio.send(AudioCommand::StartTx {
                my_call: self.callsign.clone(),
            });
        } else {
            self.append_log_line(LogLevel::Error, "cannot TX: not connected".into());
        }
    }

    /// Push-to-talk via the spacebar. Press = key down (TX on), release
    /// = key up (TX off + EOT). Suppressed while a text field has
    /// focus so typing a space in the callsign / host fields doesn't
    /// inadvertently key up the radio.
    fn handle_ptt_keybinding(&mut self, ctx: &egui::Context) {
        let typing = ctx.memory(|m| m.focused().is_some());
        if typing {
            return;
        }
        let connected = matches!(self.status, ConnStatus::Connected { .. });
        let (pressed, released) = ctx.input(|i| {
            (
                i.key_pressed(egui::Key::Space),
                i.key_released(egui::Key::Space),
            )
        });
        if pressed && connected && !self.active_tx {
            self.active_tx = true;
            self.tx_active_since = Some(std::time::Instant::now());
            self.audio.send(AudioCommand::StartTx {
                my_call: self.callsign.clone(),
            });
        }
        if released && self.active_tx {
            self.active_tx = false;
            self.tx_active_since = None;
            self.audio.send(AudioCommand::StopTx);
        }
    }

    pub(crate) fn tx_silence_test(&self) {
        let _unused = self
            .cmd_tx
            .try_send(SessionCommand::TxSilence { seconds: 2.0 });
    }

    /// Apply the selected audio devices to the worker and persist the
    /// choice. Called when the operator changes a device in the panel.
    pub(crate) fn apply_audio_devices(&self) {
        self.audio.send(AudioCommand::SelectDevices {
            input: (!self.input_device.is_empty()).then(|| self.input_device.clone()),
            output: (!self.output_device.is_empty()).then(|| self.output_device.clone()),
        });
        self.snapshot_settings().save();
    }

    /// Ask the audio worker to re-enumerate available devices.
    pub(crate) fn refresh_devices(&self) {
        self.audio.send(AudioCommand::EnumerateDevices);
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
    pub(crate) fn push_slow_data(&self) {
        let text = if self.tx_slow_text.is_empty() {
            None
        } else {
            Some(self.tx_slow_text.clone())
        };
        let gps = if self.tx_gps.enabled {
            self.parse_tx_gps()
        } else {
            None
        };
        self.audio.send(AudioCommand::SetSlowData { text, gps });
    }

    /// Parse the manual GPS fields into a [`TxPosition`], or `None` if
    /// the latitude / longitude fields aren't valid in-range numbers.
    pub(crate) fn parse_tx_gps(&self) -> Option<TxPosition> {
        let latitude: f64 = self.tx_gps.lat.trim().parse().ok()?;
        let longitude: f64 = self.tx_gps.lon.trim().parse().ok()?;
        let symbol = self.tx_gps.symbol.chars().next().unwrap_or('/');
        let pos = TxPosition {
            latitude,
            longitude,
            symbol,
            comment: self.tx_gps.comment.clone(),
        };
        pos.validated().cloned()
    }

    /// Operator page: interim composition re-homing the existing
    /// panels until the deck components land.
    fn show_operator_page(&mut self, ctx: &egui::Context) {
        egui::TopBottomPanel::bottom("tx_strip")
            .frame(
                egui::Frame::none()
                    .fill(crate::theme::BG_INSET)
                    .inner_margin(egui::Margin::symmetric(16.0, 10.0)),
            )
            .show(ctx, |ui| {
                ui::operator::tx_strip::show(self, ui);
            });
        egui::CentralPanel::default().show(ctx, |ui| {
            ui::operator::show_deck(self, ui);
        });
    }

    /// Debug page: event log + engineering tools.
    fn show_debug_page(&mut self, ctx: &egui::Context) {
        ui::debug::show(self, ctx);
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
            reconnect_on_drop: self.reconnect_on_drop,
        })
    }
}

impl eframe::App for App {
    /// Persist form state when the window closes — covers the common
    /// case of quitting without an explicit disconnect.
    fn on_exit(&mut self, _gl: Option<&eframe::glow::Context>) {
        self.snapshot_settings().save();
        if self.persist_heard_list {
            self.heard.save();
        }
    }

    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        self.drain_events();
        self.drain_audio_status();
        self.drain_directory();
        self.handle_ptt_keybinding(ctx);

        egui::TopBottomPanel::top("header")
            .frame(
                egui::Frame::none()
                    .fill(crate::theme::BG_WINDOW)
                    .inner_margin(egui::Margin::symmetric(16.0, 10.0)),
            )
            .show(ctx, |ui| {
                ui::header(self, ui);
            });
        if self.last_error.is_some() {
            egui::TopBottomPanel::top("error_strip")
                .frame(egui::Frame::none())
                .show(ctx, |ui| {
                    ui::error_strip(self, ui);
                });
        }
        match self.page {
            ui::Page::Operator => self.show_operator_page(ctx),
            ui::Page::Debug => self.show_debug_page(ctx),
        }
        // Overlays draw last so they sit above both pages.
        ui::operator::connect_sheet::show(self, ctx);
        ui::settings_popup::show(self, ctx);

        // Repaint frequently so log lines and voice events appear
        // within a few frames of arrival.
        ctx.request_repaint_after(std::time::Duration::from_millis(50));
    }
}

#[cfg(test)]
mod tests {
    use super::{LogFilter, LogLevel};

    #[test]
    fn status_text_covers_every_variant() {
        use std::net::{IpAddr, Ipv4Addr, SocketAddr};

        use crate::session::ConnStatus;
        use crate::ui::fmt_status;
        assert_eq!(fmt_status(&ConnStatus::Disconnected), "disconnected");
        let peer = SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), 30001);
        assert!(
            fmt_status(&ConnStatus::Connecting { peer }).contains("127.0.0.1"),
            "connecting shows the peer"
        );
        assert_eq!(fmt_status(&ConnStatus::Disconnecting), "disconnecting…");
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
}
