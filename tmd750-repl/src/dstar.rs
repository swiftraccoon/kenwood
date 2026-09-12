//! D-STAR reflector startup over a positively identified MMDVM link.
//!
//! The TM-D750 Terminal Mode wire protocol is not assumed. Startup requires a
//! completed initial CAT `ID` write with a silent reply timeout before sending
//! one MMDVM `GET_VERSION` probe. Standard modem setup and reflector traffic
//! are allowed only after a complete MMDVM version response proves that
//! protocol on this exact link.
//! MMDVM framing cannot distinguish Reflector Terminal from Access Point mode,
//! so the documented Menu 670 and 650 selections remain operator preconditions.

use std::net::ToSocketAddrs;
use std::pin::pin;
use std::sync::Arc;
use std::time::{Duration, Instant};

use dstar_gateway::auth::AuthClient;
use dstar_gateway::tokio_shell::{
    AnyAsyncSession, AnyEvent, AsyncSession, ShellError, fresh_stream_id,
};
use dstar_gateway_core::session::client::{Connected, Connecting, DExtra, DPlus, Dcs, Session};
use dstar_gateway_core::{Callsign, DstarHeader, Module, ProtocolKind, StreamId, VoiceFrame};
use kenwood_tmd750::transport::{DEFAULT_BAUD, SerialTransport, open_serial};
use mmdvm::dstar::{DstarEvent, DstarModemConfig};
use rustyline::DefaultEditor;
use rustyline::error::ReadlineError;

use crate::{hosts, output, terminal};

mod lifecycle;
mod modem;

use lifecycle::{
    RelayMode, StreamLifecycle, complete_cycle_or_input, next_event_before_quiet, settle_streams,
};
use modem::{prove_mmdvm_or_explain_cat, start_gateway, stop_gateway};

const DSTAR_PROMPT: &str = "dstar> ";
const EVENT_POLL_TIMEOUT: Duration = Duration::from_millis(5);
const RADIO_DRAIN_TIMEOUT: Duration = Duration::from_millis(5);
const MAX_EVENTS_PER_CYCLE: usize = 24;
const PAD_INTERVAL: Duration = Duration::from_millis(20);
const PAD_INITIAL_THRESHOLD: Duration = Duration::from_millis(100);
const PAD_FRAMES_MAX: u32 = 30;
const REFLECTOR_CONNECT_TIMEOUT: Duration = Duration::from_secs(5);

type Gateway = modem::Gateway<SerialTransport>;

/// Fully validated arguments for `dstar start`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct StartRequest {
    modem_config: DstarModemConfig,
    reflector: Option<LinkArg>,
}

impl StartRequest {
    /// Validate startup arguments before any radio or network I/O.
    pub(super) fn parse(arguments: &[&str]) -> Result<Self, String> {
        let Some(raw_callsign) = arguments.first() else {
            return Err(
                "callsign required; expected dstar start <callsign> [reflector]".to_owned(),
            );
        };
        if arguments.len() > 2 {
            return Err(
                "too many arguments; expected dstar start <callsign> [reflector]".to_owned(),
            );
        }

        let callsign = raw_callsign.to_ascii_uppercase();
        let modem_config = DstarModemConfig::new(&callsign)
            .map_err(|error| format!("invalid station callsign {callsign:?}: {error}"))?;
        let reflector = arguments
            .get(1)
            .map(|value| LinkArg::parse(&value.to_ascii_uppercase()))
            .transpose()?;
        Ok(Self {
            modem_config,
            reflector,
        })
    }

    const fn network_callsign(&self) -> Callsign {
        self.modem_config.callsign()
    }
}

/// Parsed reflector and local-module selection.
#[derive(Clone, Debug, PartialEq, Eq)]
struct LinkArg {
    reflector_name: Callsign,
    reflector_module: Module,
    local_module: Module,
}

impl LinkArg {
    fn parse(value: &str) -> Result<Self, String> {
        let (local_prefix, reflector) = value
            .split_once(':')
            .map_or((None, value), |(left, right)| (Some(left), right));
        if reflector.len() < 4 {
            return Err(format!(
                "invalid reflector {reflector:?}; expected a name and module such as REF030C"
            ));
        }
        let Some(module_char) = reflector.chars().last() else {
            return Err("reflector is empty".to_owned());
        };
        let name_end = reflector.len().saturating_sub(module_char.len_utf8());
        let Some(name) = reflector.get(..name_end) else {
            return Err(format!("invalid reflector {reflector:?}"));
        };
        let reflector_module = Module::try_from_char(module_char)
            .map_err(|error| format!("invalid reflector module: {error}"))?;
        let reflector_name = Callsign::try_from_str(name)
            .map_err(|error| format!("invalid reflector name {name:?}: {error}"))?;
        let local_module = match local_prefix {
            Some(prefix) if prefix.len() == 1 => {
                let Some(module) = prefix.chars().next() else {
                    return Err("local module prefix is empty".to_owned());
                };
                Module::try_from_char(module)
                    .map_err(|error| format!("invalid local module: {error}"))?
            }
            Some(prefix) => {
                return Err(format!(
                    "local module prefix must be one letter, not {prefix:?}"
                ));
            }
            None => reflector_module,
        };
        Ok(Self {
            reflector_name,
            reflector_module,
            local_module,
        })
    }
}

/// Run the requested gateway session until the operator stops it.
pub(super) async fn run(path: &str, baud: u32, request: StartRequest) -> Result<(), String> {
    // Construct the fallible editor before taking ownership of a live modem
    // session, so an editor initialization failure needs no radio cleanup.
    let editor = DefaultEditor::new().map_err(|error| error.to_string())?;
    let mut session = DstarSession::start(path, baud, request).await?;
    if session.reflector.is_some() {
        session.monitor().await;
    }
    session.command_loop(editor).await
}

struct DstarSession {
    gateway: Gateway,
    reflector: Option<AnyAsyncSession>,
    callsign: Callsign,
    link: Option<LinkArg>,
    tx_stream_id: Option<StreamId>,
    tx_sequence: u8,
    rx_stream_id: Option<StreamId>,
    last_rx_voice_frame: Option<VoiceFrame>,
    last_relay_at: Option<Instant>,
    pad_frames_emitted: u32,
    radio_link_lost: bool,
}

impl DstarSession {
    async fn start(path: &str, baud: u32, request: StartRequest) -> Result<Self, String> {
        if baud != DEFAULT_BAUD {
            tracing::warn!(
                baud,
                "using a non-default TM-D750 serial rate for Terminal Mode"
            );
        }
        let connection = terminal::connection_for_path(path);
        let transport = open_serial(path, baud).map_err(|error| error.to_string())?;
        let proof = prove_mmdvm_or_explain_cat(transport, connection).await?;
        let callsign = request.network_callsign();

        output::line(format_args!(
            "MMDVM framing proved on {path}; it cannot distinguish Reflector Terminal from Access Point mode. Continuing on the operator precondition that Menus 670 and 650 are set to Reflector TERM Mode and Terminal Mode."
        ));
        let gateway = start_gateway(proof, request.modem_config).await?;
        output::line(format_args!("MMDVM modem initialized for D-STAR."));

        let reflector = if let Some(link) = request.reflector.as_ref() {
            match connect_reflector(callsign, link).await {
                Ok(reflector) => Some(reflector),
                Err(error) => {
                    let cleanup_error = stop_gateway(gateway).await.err();
                    return Err(cleanup_error.map_or_else(
                        || format!("reflector connection failed: {error}"),
                        |cleanup| format!("reflector connection failed: {error}; {cleanup}"),
                    ));
                }
            }
        } else {
            output::line(format_args!(
                "D-STAR modem is active without a reflector; link startup is not yet exposed."
            ));
            None
        };

        Ok(Self {
            gateway,
            reflector,
            callsign,
            link: request.reflector,
            tx_stream_id: None,
            tx_sequence: 0,
            rx_stream_id: None,
            last_rx_voice_frame: None,
            last_relay_at: None,
            pad_frames_emitted: 0,
            radio_link_lost: false,
        })
    }

    async fn monitor(&mut self) {
        output::line(format_args!(
            "Monitoring. Press Ctrl-C to return to the D-STAR prompt."
        ));
        self.gateway.set_event_timeout(Duration::from_millis(10));
        let mut interrupt = pin!(tokio::signal::ctrl_c());
        loop {
            if let Some(result) =
                complete_cycle_or_input(self.poll_cycle(RelayMode::Monitoring), interrupt.as_mut())
                    .await
            {
                if let Err(error) = result {
                    output::error(format_args!("Error: interrupt listener failed: {error}"));
                }
                break;
            }
            if self.radio_link_lost || self.reflector.is_none() {
                break;
            }
        }
        settle_streams(self, "monitor stopped").await;
        self.gateway.set_event_timeout(Duration::from_millis(500));
        output::line(format_args!(
            "Monitor stopped. Type monitor to resume, or dstar stop to close the gateway."
        ));
    }

    async fn command_loop(mut self, editor: DefaultEditor) -> Result<(), String> {
        let mut editor = Some(editor);
        loop {
            let Some(owned_editor) = editor.take() else {
                output::error(format_args!(
                    "Error: D-STAR prompt editor ownership was lost."
                ));
                break;
            };
            let prompt = tokio::task::spawn_blocking(move || {
                let mut editor = owned_editor;
                let input = editor.readline(DSTAR_PROMPT);
                (editor, input)
            });
            let mut prompt = pin!(prompt);
            let prompt_result = loop {
                if let Some(result) =
                    complete_cycle_or_input(self.poll_cycle(RelayMode::Paused), prompt.as_mut())
                        .await
                {
                    break result;
                }
            };
            let (returned_editor, input) = match prompt_result {
                Ok(result) => result,
                Err(error) => {
                    output::error(format_args!("Error: D-STAR prompt task failed: {error}"));
                    break;
                }
            };
            editor = Some(returned_editor);
            let line = match input {
                Ok(line) => line,
                Err(ReadlineError::Interrupted) => {
                    output::line(format_args!("Use dstar stop or quit to close the gateway."));
                    continue;
                }
                Err(ReadlineError::Eof) => break,
                Err(error) => {
                    output::error(format_args!("Error: D-STAR prompt failed: {error}"));
                    break;
                }
            };
            if !line.trim().is_empty()
                && let Some(editor) = editor.as_mut()
                && let Err(error) = editor.add_history_entry(line.as_str())
            {
                output::error(format_args!(
                    "Warning: history entry was not recorded: {error}"
                ));
            }
            match line.trim().to_ascii_lowercase().as_str() {
                "monitor" | "listen" => {
                    self.discard_pending_events().await;
                    if self.reflector.is_some() && !self.radio_link_lost {
                        self.monitor().await;
                    } else {
                        output::line(format_args!(
                            "Monitoring requires an active reflector and radio link."
                        ));
                    }
                }
                "status" => self.print_status(),
                "help" | "?" => print_dstar_help(),
                "dstar stop" | "stop" | "quit" | "exit" => break,
                "" => {}
                other => output::error(format_args!(
                    "Command error: unknown D-STAR command {other:?}; enter help for syntax"
                )),
            }
        }
        self.shutdown().await
    }

    fn print_status(&self) {
        let reflector = self.link.as_ref().map_or_else(
            || "none".to_owned(),
            |link| format!("{}{}", link.reflector_name, link.reflector_module),
        );
        output::line(format_args!("D-STAR callsign: {}.", self.callsign));
        output::line(format_args!("Reflector: {reflector}."));
        output::line(format_args!(
            "Radio MMDVM link: {}.",
            if self.radio_link_lost {
                "failed"
            } else {
                "active"
            }
        ));
    }

    async fn shutdown(mut self) -> Result<(), String> {
        settle_streams(&mut self, "gateway shutdown").await;
        if let Some(ref mut reflector) = self.reflector {
            disconnect_reflector(reflector).await;
        }
        output::line(format_args!("Stopping the D-STAR modem session."));
        stop_gateway(self.gateway).await?;
        output::line(format_args!(
            "D-STAR session stopped. No Gateway setting was changed; set Menu 650 to Off before using CAT on this port."
        ));
        Ok(())
    }

    async fn poll_cycle(&mut self, mode: RelayMode) {
        self.poll_reflector(mode).await;
        self.poll_radio(mode).await;
        if mode == RelayMode::Monitoring {
            self.emit_silence_pad_if_needed().await;
        }
        if self.reflector.is_none() && self.radio_link_lost {
            tokio::time::sleep(EVENT_POLL_TIMEOUT).await;
        }
    }

    async fn poll_reflector(&mut self, mode: RelayMode) {
        let received = {
            let Some(reflector) = self.reflector.as_mut() else {
                return;
            };
            tokio::time::timeout(EVENT_POLL_TIMEOUT, reflector.next_event()).await
        };
        if let Ok(event) = received {
            self.handle_reflector_event(event, mode).await;
        }
    }

    async fn handle_reflector_event(&mut self, event: Option<AnyEvent>, mode: RelayMode) {
        let Some(event) = event else {
            output::error(format_args!("Error: reflector session closed."));
            self.finish_rx_stream("reflector session closed").await;
            self.reset_tx_stream();
            self.reflector = None;
            return;
        };
        trace_reflector_event(&event);
        if mode == RelayMode::Monitoring {
            let repeated_header = matches!(
                &event,
                AnyEvent::VoiceStart { stream_id, .. }
                    if self.rx_stream_id == Some(*stream_id)
            );
            if !repeated_header {
                print_reflector_event(&event);
            }
            self.relay_reflector_to_radio(&event).await;
        }
        if matches!(event, AnyEvent::Disconnected { .. }) {
            if mode == RelayMode::Paused {
                print_reflector_event(&event);
            }
            self.finish_rx_stream("reflector disconnected").await;
            self.reset_tx_stream();
            self.reflector = None;
        }
    }

    async fn poll_radio(&mut self, mode: RelayMode) {
        if self.radio_link_lost {
            return;
        }
        let saved_timeout = self.gateway.event_timeout();
        self.gateway.set_event_timeout(RADIO_DRAIN_TIMEOUT);
        for _ in 0..MAX_EVENTS_PER_CYCLE {
            let event = match self.gateway.next_event().await {
                Ok(Some(event)) => event,
                Ok(None) => break,
                Err(error) => {
                    if !self.radio_link_lost {
                        output::error(format_args!("Error: radio MMDVM link failed: {error}"));
                    }
                    self.radio_link_lost = true;
                    self.finish_tx_stream("radio link failed").await;
                    break;
                }
            };
            trace_radio_event(&event);
            if mode == RelayMode::Monitoring {
                print_radio_event(&event);
                self.relay_radio_to_reflector(&event).await;
            }
        }
        self.gateway.set_event_timeout(saved_timeout);
    }

    /// Drop voice queued while paused before accepting another live stream.
    async fn discard_pending_events(&mut self) {
        self.discard_pending_radio_events().await;
        // Drain the reflector last: a radio operation can await modem I/O,
        // during which the reflector task may have queued more events.
        while let Some(reflector) = self.reflector.as_mut() {
            let Some(event) =
                next_event_before_quiet(reflector.next_event(), EVENT_POLL_TIMEOUT).await
            else {
                break;
            };
            self.handle_reflector_event(event, RelayMode::Paused).await;
        }
    }

    async fn discard_pending_radio_events(&mut self) {
        if self.radio_link_lost {
            return;
        }
        let saved_timeout = self.gateway.event_timeout();
        self.gateway.set_event_timeout(RADIO_DRAIN_TIMEOUT);
        loop {
            // Gateway event dispatch can perform echo playback. Await it
            // fully; unlike the reflector's channel receive, it is not a
            // cancellation-safe operation to poll once and then drop.
            match self.gateway.next_event().await {
                Ok(Some(event)) => trace_radio_event(&event),
                Ok(None) => break,
                Err(error) => {
                    output::error(format_args!("Error: radio MMDVM link failed: {error}"));
                    self.radio_link_lost = true;
                    break;
                }
            }
        }
        self.gateway.set_event_timeout(saved_timeout);
    }

    async fn relay_radio_to_reflector(&mut self, event: &DstarEvent) {
        let Some(link) = self.link.clone() else {
            return;
        };
        match event {
            DstarEvent::VoiceStart(header) => {
                self.finish_tx_stream("radio stream preempted").await;
                tracing::trace!(
                    target: "tmd750_repl::radio",
                    original_rpt1 = %header.rpt1,
                    original_rpt2 = %header.rpt2,
                    original_ur = %header.ur_call,
                    "TM-D750 outbound header observed before relay rewrite"
                );
                let stream_id = fresh_stream_id();
                let outgoing = DstarHeader::for_relay(
                    self.callsign,
                    link.local_module,
                    link.reflector_name,
                    link.reflector_module,
                    header.my_call,
                    header.my_suffix,
                )
                .with_flags(header.flag1, header.flag2, header.flag3);
                let Some(reflector) = self.reflector.as_mut() else {
                    return;
                };
                match reflector.send_header(outgoing, stream_id).await {
                    Ok(()) => {
                        self.tx_stream_id = Some(stream_id);
                        self.tx_sequence = 0;
                    }
                    Err(error) => output::error(format_args!(
                        "Error: relaying radio header to reflector: {error}"
                    )),
                }
            }
            DstarEvent::VoiceData(frame) => {
                let Some(stream_id) = self.tx_stream_id else {
                    return;
                };
                let Some(reflector) = self.reflector.as_mut() else {
                    self.reset_tx_stream();
                    return;
                };
                if let Err(error) = reflector
                    .send_voice(stream_id, self.tx_sequence, *frame)
                    .await
                {
                    output::error(format_args!(
                        "Error: relaying radio voice to reflector: {error}"
                    ));
                }
                self.tx_sequence = (self.tx_sequence + 1) % 21;
            }
            DstarEvent::VoiceEnd => self.finish_tx_stream("radio transmission ended").await,
            DstarEvent::VoiceLost | DstarEvent::EventsDropped { .. } => {
                self.finish_tx_stream("radio voice stream lost").await;
            }
            _ => {}
        }
    }

    async fn relay_reflector_to_radio(&mut self, event: &AnyEvent) {
        match event {
            AnyEvent::VoiceStart {
                stream_id, header, ..
            } => {
                if self.rx_stream_id == Some(*stream_id) {
                    return;
                }
                self.finish_rx_stream("reflector stream preempted").await;
                let Some(link) = self.link.as_ref() else {
                    return;
                };
                let radio_header =
                    DstarHeader::for_radio_relay(self.callsign, link.local_module, header);
                match self.gateway.send_header(&radio_header).await {
                    Ok(()) => {
                        self.rx_stream_id = Some(*stream_id);
                        self.last_rx_voice_frame = None;
                        self.last_relay_at = None;
                        self.pad_frames_emitted = 0;
                    }
                    Err(error) => output::error(format_args!(
                        "Error: relaying reflector header to radio: {error}"
                    )),
                }
            }
            AnyEvent::VoiceFrame {
                stream_id, frame, ..
            } => {
                if self.rx_stream_id != Some(*stream_id) {
                    tracing::debug!(
                        %stream_id,
                        "dropping reflector voice frame without an accepted matching header"
                    );
                    return;
                }
                if let Err(error) = self.gateway.send_voice(frame).await {
                    output::error(format_args!(
                        "Error: relaying reflector voice to radio: {error}"
                    ));
                } else {
                    self.last_rx_voice_frame = Some(*frame);
                    self.last_relay_at = Some(Instant::now());
                    self.pad_frames_emitted = 0;
                }
            }
            AnyEvent::VoiceEnd { stream_id, .. } => {
                if self.rx_stream_id == Some(*stream_id) {
                    self.finish_rx_stream("reflector transmission ended").await;
                }
            }
            _ => {}
        }
    }

    const fn reset_tx_stream(&mut self) {
        self.tx_stream_id = None;
        self.tx_sequence = 0;
    }

    async fn finish_tx_stream(&mut self, context: &'static str) {
        let Some(stream_id) = self.tx_stream_id else {
            self.tx_sequence = 0;
            return;
        };
        let sequence = self.tx_sequence;
        let Some(reflector) = self.reflector.as_mut() else {
            self.reset_tx_stream();
            return;
        };
        let result = reflector.send_eot(stream_id, sequence).await;
        self.reset_tx_stream();
        if let Err(error) = result {
            output::error(format_args!(
                "Warning: ending reflector-bound stream after {context}: {error}"
            ));
        }
    }

    const fn reset_rx_stream(&mut self) -> bool {
        let was_active = self.rx_stream_id.take().is_some();
        self.last_rx_voice_frame = None;
        self.last_relay_at = None;
        self.pad_frames_emitted = 0;
        was_active
    }

    async fn finish_rx_stream(&mut self, context: &'static str) {
        if self.rx_stream_id.is_none() {
            return;
        }
        let result = self.gateway.send_eot().await;
        let _was_active = self.reset_rx_stream();
        if let Err(error) = result {
            output::error(format_args!(
                "Warning: ending radio-bound stream after {context}: {error}"
            ));
        }
    }

    async fn emit_silence_pad_if_needed(&mut self) {
        if self.rx_stream_id.is_none() || self.pad_frames_emitted >= PAD_FRAMES_MAX {
            return;
        }
        let (Some(last_at), Some(frame)) = (self.last_relay_at, self.last_rx_voice_frame) else {
            return;
        };
        let threshold = if self.pad_frames_emitted == 0 {
            PAD_INITIAL_THRESHOLD
        } else {
            PAD_INTERVAL
        };
        if last_at.elapsed() < threshold {
            return;
        }
        if let Err(error) = self.gateway.send_voice(&frame).await {
            tracing::warn!(%error, "silence padding failed");
            self.pad_frames_emitted = PAD_FRAMES_MAX;
            return;
        }
        self.last_relay_at = Some(Instant::now());
        self.pad_frames_emitted = self.pad_frames_emitted.saturating_add(1);
    }
}

impl StreamLifecycle for DstarSession {
    async fn discard_queued_events(&mut self) {
        self.discard_pending_events().await;
    }

    async fn finish_network_stream(&mut self, context: &'static str) {
        self.finish_tx_stream(context).await;
    }

    async fn finish_radio_stream(&mut self, context: &'static str) {
        if self.radio_link_lost {
            let _was_active = self.reset_rx_stream();
        } else {
            self.finish_rx_stream(context).await;
        }
    }
}

fn print_reflector_event(event: &AnyEvent) {
    match event {
        AnyEvent::VoiceStart { header, .. } => output::line(format_args!(
            "Reflector voice started: {} / {} to {}.",
            header.my_call, header.my_suffix, header.ur_call
        )),
        AnyEvent::VoiceEnd { .. } => {
            output::line(format_args!("Reflector voice ended."));
        }
        AnyEvent::Disconnected { reason } => {
            output::error(format_args!("Error: reflector disconnected: {reason:?}."));
        }
        _ => {}
    }
}

async fn connect_reflector(callsign: Callsign, link: &LinkArg) -> Result<AnyAsyncSession, String> {
    let entry = hosts::resolve(link.reflector_name).map_err(|error| error.to_string())?;
    let protocol = ProtocolKind::from_reflector_prefix(&link.reflector_name.as_str())
        .or_else(|| ProtocolKind::from_port(entry.port))
        .unwrap_or(ProtocolKind::DExtra);
    let address = format!("{}:{}", entry.address, entry.port)
        .to_socket_addrs()
        .map_err(|error| format!("address resolution failed for {}: {error}", entry.address))?
        .next()
        .ok_or_else(|| format!("no address resolved for {}", entry.address))?;
    let reflector_callsign = link.reflector_name;

    output::line(format_args!(
        "Connecting to {} module {} at {address} using {protocol:?}.",
        link.reflector_name, link.reflector_module
    ));
    let session = match protocol {
        ProtocolKind::DPlus => connect_dplus(callsign, address, link, reflector_callsign)
            .await
            .map(AnyAsyncSession::DPlus)?,
        ProtocolKind::DExtra => connect_dextra(callsign, address, link, reflector_callsign)
            .await
            .map(AnyAsyncSession::DExtra)?,
        ProtocolKind::Dcs => connect_dcs(callsign, address, link, reflector_callsign)
            .await
            .map(AnyAsyncSession::Dcs)?,
        _ => return Err(format!("unsupported reflector protocol {protocol:?}")),
    };
    output::line(format_args!(
        "Connected to {} module {}.",
        link.reflector_name, link.reflector_module
    ));
    Ok(session)
}

async fn bind_socket() -> Result<Arc<tokio::net::UdpSocket>, String> {
    tokio::net::UdpSocket::bind("0.0.0.0:0")
        .await
        .map(Arc::new)
        .map_err(|error| format!("UDP bind failed: {error}"))
}

async fn drive_handshake<P>(
    session: Session<P, Connecting>,
    socket: &tokio::net::UdpSocket,
) -> Result<Session<P, Connected>, String>
where
    P: dstar_gateway_core::session::client::Protocol,
{
    dstar_gateway::tokio_shell::drive_connecting(session, socket, REFLECTOR_CONNECT_TIMEOUT)
        .await
        .map_err(|error| error.to_string())
}

async fn connect_dextra(
    callsign: Callsign,
    peer: std::net::SocketAddr,
    link: &LinkArg,
    reflector_callsign: Callsign,
) -> Result<AsyncSession<DExtra>, String> {
    let socket = bind_socket().await?;
    let connecting = Session::<DExtra, _>::builder()
        .callsign(callsign)
        .local_module(link.local_module)
        .reflector_module(link.reflector_module)
        .reflector_callsign(reflector_callsign)
        .peer(peer)
        .build()
        .connect(Instant::now())
        .map_err(|failure| format!("DExtra link request failed: {}", failure.error))?;
    let connected = drive_handshake(connecting, &socket).await?;
    Ok(AsyncSession::spawn(connected, socket))
}

async fn connect_dplus(
    callsign: Callsign,
    peer: std::net::SocketAddr,
    link: &LinkArg,
    reflector_callsign: Callsign,
) -> Result<AsyncSession<DPlus>, String> {
    output::line(format_args!(
        "Authenticating with the DPlus gateway server."
    ));
    let hosts = match AuthClient::new().authenticate(callsign).await {
        Ok(hosts) => hosts,
        Err(error) => {
            output::error(format_args!(
                "Warning: DPlus authentication failed: {error}; trying the UDP link anyway."
            ));
            dstar_gateway_core::codec::dplus::HostList::new()
        }
    };
    let socket = bind_socket().await?;
    let authenticated = Session::<DPlus, _>::builder()
        .callsign(callsign)
        .local_module(link.local_module)
        .reflector_module(link.reflector_module)
        .reflector_callsign(reflector_callsign)
        .peer(peer)
        .build()
        .authenticate(hosts)
        .map_err(|failure| format!("DPlus host-list setup failed: {}", failure.error))?;
    let connecting = authenticated
        .connect(Instant::now())
        .map_err(|failure| format!("DPlus link request failed: {}", failure.error))?;
    let connected = drive_handshake(connecting, &socket).await?;
    Ok(AsyncSession::spawn(connected, socket))
}

async fn connect_dcs(
    callsign: Callsign,
    peer: std::net::SocketAddr,
    link: &LinkArg,
    reflector_callsign: Callsign,
) -> Result<AsyncSession<Dcs>, String> {
    let socket = bind_socket().await?;
    let connecting = Session::<Dcs, _>::builder()
        .callsign(callsign)
        .local_module(link.local_module)
        .reflector_module(link.reflector_module)
        .reflector_callsign(reflector_callsign)
        .peer(peer)
        .build()
        .connect(Instant::now())
        .map_err(|failure| format!("DCS link request failed: {}", failure.error))?;
    let connected = drive_handshake(connecting, &socket).await?;
    Ok(AsyncSession::spawn(connected, socket))
}

async fn disconnect_reflector(reflector: &mut AnyAsyncSession) {
    match reflector.disconnect().await {
        Ok(()) => output::line(format_args!("Disconnected from reflector.")),
        Err(ShellError::DisconnectUnacknowledged) => output::error(format_args!(
            "Warning: reflector did not acknowledge unlink; its protocol timeout closed the local session."
        )),
        Err(ShellError::DisconnectedBeforeUnlink { reason }) => output::line(format_args!(
            "Reflector session had already closed: {reason:?}."
        )),
        Err(error) => output::error(format_args!(
            "Warning: reflector disconnect did not complete: {error}; continuing radio shutdown."
        )),
    }
}

fn trace_reflector_event(event: &AnyEvent) {
    tracing::trace!(target: "tmd750_repl::reflector", ?event, "reflector event");
}

fn trace_radio_event(event: &DstarEvent) {
    if matches!(event, DstarEvent::VoiceData(_)) {
        tracing::trace!(target: "tmd750_repl::radio", "radio voice frame");
    } else {
        tracing::trace!(target: "tmd750_repl::radio", ?event, "radio event");
    }
}

fn print_radio_event(event: &DstarEvent) {
    match event {
        DstarEvent::VoiceStart(header) => output::line(format_args!(
            "Radio voice started: {} / {} to {}.",
            header.my_call, header.my_suffix, header.ur_call
        )),
        DstarEvent::VoiceEnd => output::line(format_args!("Radio voice ended.")),
        DstarEvent::VoiceLost => output::error(format_args!("Warning: radio voice stream lost.")),
        DstarEvent::EventsDropped { count } => output::error(format_args!(
            "Warning: {count} radio modem events were dropped."
        )),
        DstarEvent::ProtocolViolation(violation) => output::error(format_args!(
            "Warning: radio D-STAR protocol violation: {violation}."
        )),
        DstarEvent::TextMessage(message) => {
            if let Ok(text) = message.text() {
                output::line(format_args!("D-STAR text: {text}"));
            }
        }
        DstarEvent::StationHeard(entry) => {
            output::line(format_args!("D-STAR station heard: {}.", entry.callsign));
        }
        DstarEvent::StatusUpdate(_)
        | DstarEvent::UrCallCommand(_)
        | DstarEvent::ModemEvent(_)
        | DstarEvent::VoiceData(_) => {}
    }
}

fn print_dstar_help() {
    output::line(format_args!("D-STAR commands:"));
    output::line(format_args!(
        "monitor       Resume reflector and radio monitoring."
    ));
    output::line(format_args!(
        "status        Show the active callsign, reflector, and link state."
    ));
    output::line(format_args!(
        "dstar stop    Disconnect and close the MMDVM session."
    ));
    output::line(format_args!("quit | exit   Same as dstar stop."));
}

#[cfg(test)]
mod tests {
    use super::*;
    use kenwood_transport::{MockTransport, Transport, TransportError};
    use modem::prove_mmdvm_or_explain_cat_with_timeout;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use tokio::sync::Mutex;

    const MMDVM_VERSION_PROBE: [u8; 3] = [0xE0, 0x03, 0x00];

    #[derive(Clone, Copy, Debug)]
    enum WriteBehavior {
        Normal,
        Fail,
        Hang,
    }

    #[derive(Clone, Debug)]
    struct SharedMock {
        inner: Arc<Mutex<MockTransport>>,
        closes: Arc<AtomicUsize>,
        fail_close: bool,
        write_behavior: WriteBehavior,
    }

    impl SharedMock {
        fn new(mock: MockTransport) -> Self {
            Self {
                inner: Arc::new(Mutex::new(mock)),
                closes: Arc::new(AtomicUsize::new(0)),
                fail_close: false,
                write_behavior: WriteBehavior::Normal,
            }
        }

        async fn writes(&self) -> Vec<Vec<u8>> {
            self.inner.lock().await.writes().to_vec()
        }
    }

    impl Transport for SharedMock {
        async fn write(&mut self, data: &[u8]) -> Result<(), TransportError> {
            assert_eq!(
                self.closes.load(Ordering::SeqCst),
                0,
                "no requests may follow a close attempt"
            );
            self.inner.lock().await.write(data).await?;
            match self.write_behavior {
                WriteBehavior::Normal => Ok(()),
                WriteBehavior::Fail => Err(TransportError::Write(std::io::Error::other(
                    "injected serial write failure",
                ))),
                WriteBehavior::Hang => std::future::pending().await,
            }
        }

        async fn read(&mut self, buffer: &mut [u8]) -> Result<usize, TransportError> {
            self.inner.lock().await.read(buffer).await
        }

        async fn close(&mut self) -> Result<(), TransportError> {
            let _previous = self.closes.fetch_add(1, Ordering::SeqCst);
            if self.fail_close {
                return Err(TransportError::Disconnected(std::io::Error::other(
                    "injected serial close failure",
                )));
            }
            self.inner.lock().await.close().await
        }
    }

    fn cat_gateway_mock(response: &[u8]) -> SharedMock {
        let mut mock = MockTransport::new();
        mock.expect(b"ID\r", b"ID TM-D750\r");
        mock.expect(b"FV\r", b"FV 1.02\r");
        mock.expect(b"TY\r", b"TY K,2,1\r");
        mock.expect(b"GW\r", response);
        SharedMock::new(mock)
    }

    async fn cat_startup_diagnostic(
        shared: &SharedMock,
        connection: terminal::UsbConnection,
    ) -> Result<String, String> {
        let result = prove_mmdvm_or_explain_cat(shared.clone(), connection).await;
        let Err(guidance) = result else {
            return Err("CAT must return a diagnostic, not a modem owner".to_owned());
        };
        assert_eq!(
            shared.writes().await,
            [b"ID\r", b"FV\r", b"TY\r", b"GW\r"],
            "CAT guidance must send only the four read-only queries"
        );
        assert_eq!(
            shared.closes.load(Ordering::SeqCst),
            1,
            "every CAT diagnostic must attempt exactly one close"
        );
        shared.inner.lock().await.assert_complete();
        Ok(guidance)
    }

    #[test]
    fn favorite_command_arguments_validate() -> Result<(), String> {
        let request = StartRequest::parse(&["KQ4NIT", "REF030C"])?;
        assert_eq!(request.network_callsign().as_str(), "KQ4NIT");
        assert_eq!(
            request.network_callsign(),
            Callsign::from_wire_bytes(*b"KQ4NIT  ")
        );
        let Some(link) = request.reflector else {
            return Err("reflector was not retained".to_owned());
        };
        assert_eq!(link.reflector_name, Callsign::from_wire_bytes(*b"REF030  "));
        assert_eq!(link.reflector_module, Module::C);
        assert_eq!(link.local_module, Module::C);
        Ok(())
    }

    #[test]
    fn startup_input_is_normalized_before_io() -> Result<(), String> {
        let request = StartRequest::parse(&["kq4nit", "b:ref030c"])?;
        assert_eq!(request.network_callsign().as_str(), "KQ4NIT");
        let Some(link) = request.reflector else {
            return Err("reflector was not retained".to_owned());
        };
        assert_eq!(link.reflector_name, Callsign::from_wire_bytes(*b"REF030  "));
        assert_eq!(link.local_module, Module::B);
        assert_eq!(link.reflector_module, Module::C);
        Ok(())
    }

    #[test]
    fn malformed_startup_input_is_rejected() {
        assert!(StartRequest::parse(&[]).is_err());
        assert!(StartRequest::parse(&[""]).is_err());
        assert!(StartRequest::parse(&["   "]).is_err());
        assert!(StartRequest::parse(&["KQ4NIT", "REF0301"]).is_err());
        assert!(StartRequest::parse(&["KQ4NIT", "REF030C", "extra"]).is_err());
        assert!(StartRequest::parse(&["KQ4,NIT"]).is_err());
        assert!(StartRequest::parse(&["KQ4NIT\r"]).is_err());
        assert!(StartRequest::parse(&["KQ4NIT   "]).is_err());
        assert!(StartRequest::parse(&["KQ4NITé"]).is_err());
    }

    #[tokio::test]
    async fn normal_cat_guidance_matches_usb_endpoint_without_gateway_writes() -> Result<(), String>
    {
        for (connection, routing, forbidden) in [
            (
                terminal::UsbConnection::MainUnit,
                "USB (Main Unit)",
                "USB (Panel)",
            ),
            (
                terminal::UsbConnection::Panel,
                "USB (Panel)",
                "USB (Main Unit)",
            ),
        ] {
            let shared = cat_gateway_mock(b"GW 0\r");
            let guidance = cat_startup_diagnostic(&shared, connection).await?;
            assert!(guidance.contains(routing));
            assert!(!guidance.contains(forbidden));
            assert!(guidance.contains("No setting was changed"));
            assert!(guidance.contains("Configure the radio manually"));
        }
        Ok(())
    }

    #[tokio::test]
    async fn observed_terminal_reports_control_link_without_repeating_setup() -> Result<(), String>
    {
        for connection in [
            terminal::UsbConnection::MainUnit,
            terminal::UsbConnection::Panel,
            terminal::UsbConnection::Unknown,
        ] {
            let shared = cat_gateway_mock(b"GW 2\r");
            let guidance = cat_startup_diagnostic(&shared, connection).await?;
            for expected in [
                "DV Gateway state: Terminal",
                "Terminal Mode is already selected (GW 2)",
                "This endpoint answered CAT; MMDVM is not proved",
                "may be routed to another endpoint",
                "GW does not identify its route or pair USB endpoints to one radio",
                "select it with --port",
                "No automatic setup was attempted",
            ] {
                assert!(
                    guidance.contains(expected),
                    "missing {expected}: {guidance}"
                );
            }
            for forbidden in [
                "Configure the radio manually",
                "Menu 650:",
                "TERM indicator",
            ] {
                assert!(
                    !guidance.contains(forbidden),
                    "observed Terminal must not repeat {forbidden}"
                );
            }
        }
        Ok(())
    }

    #[tokio::test]
    async fn unnamed_and_unreadable_gateway_never_infer_mode_or_route() -> Result<(), String> {
        for (reply, expected) in [
            (&b"GW 1\r"[..], "DV Gateway state: unqualified value 1"),
            (&b"N\r"[..], "DV Gateway state: unreadable ("),
            (&b"GW invalid\r"[..], "DV Gateway state: unreadable ("),
        ] {
            let shared = cat_gateway_mock(reply);
            let guidance =
                cat_startup_diagnostic(&shared, terminal::UsbConnection::MainUnit).await?;
            assert!(
                guidance.contains(expected),
                "missing {expected}: {guidance}"
            );
            assert!(
                guidance.contains("Gateway state is not confirmed as Off or Terminal"),
                "an unnamed value or query error does not establish either named state"
            );
            for forbidden in [
                "DV Gateway state: Off.",
                "Terminal Mode is already selected",
                "Configure the radio manually",
                "Menu 986:",
                "USB (Panel)",
                "USB (Main Unit)",
            ] {
                assert!(
                    !guidance.contains(forbidden),
                    "uncertain Gateway state must not infer {forbidden}"
                );
            }
        }
        Ok(())
    }

    #[tokio::test]
    async fn cat_diagnostic_retains_gateway_result_when_serial_close_fails() -> Result<(), String> {
        for (reply, expected) in [
            (&b"GW 2\r"[..], "DV Gateway state: Terminal"),
            (&b"N\r"[..], "DV Gateway state: unreadable ("),
        ] {
            let mut shared = cat_gateway_mock(reply);
            shared.fail_close = true;
            let guidance =
                cat_startup_diagnostic(&shared, terminal::UsbConnection::MainUnit).await?;
            assert!(
                guidance.contains(expected),
                "cleanup failure must preserve the original Gateway result"
            );
            assert!(
                guidance.contains("Serial close also failed:")
                    && guidance.contains("injected serial close failure"),
                "cleanup failure must remain visible: {guidance}"
            );
        }
        Ok(())
    }

    #[tokio::test]
    async fn cat_timeout_then_strict_version_proof_sends_no_modem_configuration()
    -> Result<(), String> {
        let mut mock = MockTransport::new();
        mock.expect_hang(b"ID\r");
        mock.expect(&MMDVM_VERSION_PROBE, b"\xE0\x0E\x00\x01MMDVM 2018");
        let shared = SharedMock::new(mock);

        let _proved_radio = prove_mmdvm_or_explain_cat_with_timeout(
            shared.clone(),
            Duration::from_millis(1),
            terminal::UsbConnection::Unknown,
        )
        .await?;

        assert_eq!(
            shared.writes().await,
            vec![b"ID\r".to_vec(), MMDVM_VERSION_PROBE.to_vec()],
            "preflight must stop after strict version proof and leave configuration to gateway startup"
        );
        assert_eq!(shared.closes.load(Ordering::SeqCst), 0);
        shared.inner.lock().await.assert_complete();
        Ok(())
    }

    #[tokio::test]
    async fn echoed_version_probe_is_not_protocol_proof() {
        let mut mock = MockTransport::new();
        mock.expect_hang(b"ID\r");
        mock.expect(&MMDVM_VERSION_PROBE, &MMDVM_VERSION_PROBE);
        let shared = SharedMock::new(mock);

        let result = prove_mmdvm_or_explain_cat_with_timeout(
            shared.clone(),
            Duration::from_millis(1),
            terminal::UsbConnection::Unknown,
        )
        .await;

        assert!(result.is_err(), "an echoed request must fail closed");
        assert_eq!(
            shared.writes().await,
            vec![b"ID\r".to_vec(), MMDVM_VERSION_PROBE.to_vec()],
            "failed proof must not send SetConfig, SetMode, or gateway traffic"
        );
        assert_eq!(shared.closes.load(Ordering::SeqCst), 1);
    }

    #[tokio::test]
    async fn partial_id_timeout_never_admits_a_binary_probe() {
        for prefix in [&b"ID TM-D750"[..], &b"?"[..]] {
            let mut mock = MockTransport::new();
            mock.expect_partial_then_hang(b"ID\r", prefix);
            let shared = SharedMock::new(mock);
            let result = prove_mmdvm_or_explain_cat_with_timeout(
                shared.clone(),
                Duration::from_millis(1),
                terminal::UsbConnection::Unknown,
            )
            .await;
            assert!(
                result.is_err_and(|error| error.contains("no MMDVM probe was sent")),
                "a timed-out partial CAT line is not silence"
            );
            assert_eq!(shared.writes().await, vec![b"ID\r".to_vec()]);
            assert_eq!(shared.closes.load(Ordering::SeqCst), 1);
            shared.inner.lock().await.assert_complete();
        }
    }

    #[tokio::test]
    async fn incomplete_initial_write_never_admits_a_binary_probe() {
        for write_behavior in [WriteBehavior::Fail, WriteBehavior::Hang] {
            let mut mock = MockTransport::new();
            mock.expect_hang(b"ID\r");
            let mut shared = SharedMock::new(mock);
            shared.write_behavior = write_behavior;
            let result = prove_mmdvm_or_explain_cat_with_timeout(
                shared.clone(),
                Duration::from_millis(1),
                terminal::UsbConnection::Unknown,
            )
            .await;
            assert!(result.is_err_and(|error| error.contains("no MMDVM probe was sent")));
            assert_eq!(shared.writes().await, vec![b"ID\r".to_vec()]);
            assert_eq!(shared.closes.load(Ordering::SeqCst), 1);
        }
    }

    #[tokio::test]
    async fn later_identity_timeout_never_admits_a_binary_probe() {
        let mut mock = MockTransport::new();
        mock.expect(b"ID\r", b"ID TM-D750\r");
        mock.expect_hang(b"FV\r");
        let shared = SharedMock::new(mock);
        let result = prove_mmdvm_or_explain_cat_with_timeout(
            shared.clone(),
            Duration::from_millis(1),
            terminal::UsbConnection::Unknown,
        )
        .await;
        assert!(result.is_err_and(|error| error.contains("no MMDVM probe was sent")));
        assert_eq!(shared.writes().await, [b"ID\r", b"FV\r"]);
        assert_eq!(shared.closes.load(Ordering::SeqCst), 1);
    }

    #[tokio::test]
    async fn preflight_failures_retain_the_original_error_and_close_failure() {
        for cat_reply in [&b"ID TM-D740\r"[..], &b"?\r"[..]] {
            let mut mock = MockTransport::new();
            mock.expect(b"ID\r", cat_reply);
            let mut shared = SharedMock::new(mock);
            shared.fail_close = true;
            let result = prove_mmdvm_or_explain_cat_with_timeout(
                shared.clone(),
                Duration::from_millis(1),
                terminal::UsbConnection::Unknown,
            )
            .await;
            assert!(result.is_err_and(|error| {
                error.contains("CAT identification failed")
                    && error.contains("Serial close also failed")
                    && error.contains("injected serial close failure")
            }));
            assert_eq!(shared.writes().await, vec![b"ID\r".to_vec()]);
            assert_eq!(shared.closes.load(Ordering::SeqCst), 1);
        }
    }

    #[tokio::test]
    async fn binary_proof_failure_retains_its_diagnostic_and_close_failure() {
        let mut mock = MockTransport::new();
        mock.expect_hang(b"ID\r");
        mock.expect(&MMDVM_VERSION_PROBE, &MMDVM_VERSION_PROBE);
        let mut shared = SharedMock::new(mock);
        shared.fail_close = true;
        let result = prove_mmdvm_or_explain_cat_with_timeout(
            shared.clone(),
            Duration::from_millis(1),
            terminal::UsbConnection::Unknown,
        )
        .await;
        assert!(result.is_err_and(|error| {
            error.contains("complete MMDVM GET_VERSION probe")
                && error.contains("Serial close also failed")
                && error.contains("injected serial close failure")
        }));
        assert_eq!(
            shared.writes().await,
            vec![b"ID\r".to_vec(), MMDVM_VERSION_PROBE.to_vec()]
        );
        assert_eq!(shared.closes.load(Ordering::SeqCst), 1);
    }

    fn modem_startup_mock(config_reply: &[u8], mode_reply: Option<&[u8]>) -> SharedMock {
        let mut mock = MockTransport::new();
        mock.expect_hang(b"ID\r");
        mock.expect(&MMDVM_VERSION_PROBE, b"\xE0\x0E\x00\x01MMDVM 2018");
        mock.expect(b"\xE0\x09\x02\x00\x01\x0A\x01\x80\x80", config_reply);
        if let Some(mode_reply) = mode_reply {
            mock.expect(b"\xE0\x04\x03\x01", mode_reply);
        }
        mock.expect_any_write();
        mock.pend_when_empty();
        SharedMock::new(mock)
    }

    async fn prove_test_modem(
        shared: &SharedMock,
    ) -> Result<modem::ProvenModem<SharedMock>, String> {
        prove_mmdvm_or_explain_cat_with_timeout(
            shared.clone(),
            Duration::from_millis(1),
            terminal::UsbConnection::Unknown,
        )
        .await
    }

    #[tokio::test]
    async fn runtime_shutdown_closes_the_proved_transport_without_mode_exit() -> Result<(), String>
    {
        for fail_close in [false, true] {
            let mut shared = modem_startup_mock(b"\xE0\x04\x70\x02", Some(b"\xE0\x04\x70\x03"));
            shared.fail_close = fail_close;
            let proof = prove_test_modem(&shared).await?;
            let config = DstarModemConfig::new("KQ4NIT").map_err(|error| error.to_string())?;
            let gateway = start_gateway(proof, config).await?;
            let result = stop_gateway(gateway).await;
            if fail_close {
                assert!(result.is_err_and(|error| {
                    error.contains("Serial close failed")
                        && error.contains("injected serial close failure")
                }));
            } else {
                result?;
            }
            assert_eq!(shared.closes.load(Ordering::SeqCst), 1);
            let writes = shared.writes().await;
            assert_eq!(writes.first(), Some(&b"ID\r".to_vec()));
            assert!(
                writes
                    .iter()
                    .skip(1)
                    .all(|bytes| bytes.first() == Some(&0xE0)
                        && matches!(bytes.get(2), Some(0x00..=0x03)))
            );
            assert_eq!(
                writes
                    .iter()
                    .filter(|bytes| bytes.get(2) == Some(&0x03))
                    .count(),
                1,
                "only the startup D-STAR SetMode is permitted; shutdown cannot change persistent mode"
            );
            shared.inner.lock().await.assert_complete();
        }
        Ok(())
    }

    #[tokio::test]
    async fn initialization_rejection_closes_once_and_retains_cleanup_failure() -> Result<(), String>
    {
        for fail_close in [false, true] {
            let mut shared = modem_startup_mock(b"\xE0\x05\x7F\x02\x04", None);
            shared.fail_close = fail_close;
            let proof = prove_test_modem(&shared).await?;
            let config = DstarModemConfig::new("KQ4NIT").map_err(|error| error.to_string())?;
            let result = start_gateway(proof, config).await;
            let Err(error) = result else {
                return Err("SetConfig rejection must fail startup".to_owned());
            };
            assert!(error.contains("MMDVM D-STAR initialization failed"));
            assert_eq!(error.contains("injected serial close failure"), fail_close);
            assert_eq!(shared.closes.load(Ordering::SeqCst), 1);
            let writes = shared.writes().await;
            assert!(
                writes
                    .iter()
                    .skip(1)
                    .all(|bytes| bytes.first() == Some(&0xE0)
                        && matches!(bytes.get(2), Some(0x00..=0x02)))
            );
            assert!(
                !writes.iter().any(|bytes| bytes.get(2) == Some(&0x03)),
                "rejected setup cannot continue to SetMode or send a mode-exit command"
            );
            shared.inner.lock().await.assert_complete();
        }
        Ok(())
    }
}
