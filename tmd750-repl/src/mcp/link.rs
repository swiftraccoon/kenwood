//! The connection an MCP write workflow runs over: the selected USB endpoint,
//! or the paired radio's native Bluetooth link.
//!
//! A link opens each programming session's connection, sets its CAT reply
//! deadlines, checks the state it requires before programming entry, holds
//! the connection after an acknowledged exit, and runs the fresh post-exit
//! check. The USB link waits for the operation-panel endpoint to settle and
//! reopens the same serial path. The native link pins the RFCOMM channel of
//! its first open, requires Gateway Off before every programming entry, holds
//! the original connection for `POST_EXIT_SETTLE` after the exit, and reads
//! the fresh identity and Gateway Off with a first-reply deadline counted from
//! the exit.

use std::fs::File;
use std::io;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use kenwood_tmd750::radio::bluetooth_first_reply_timeout_after_exit;
use kenwood_tmd750::transport::{SerialCandidate, TMD750_PANEL_PID};
use kenwood_tmd750::{DvGatewayMode, Identity, Radio};
use kenwood_transport::bluetooth::{BluetoothService, RfcommChannel};
use serde::Serialize;
use tokio::time::Instant;

use super::reconnect::{
    self, Backend, PostExit, SkipReason, VerificationOutcome, VerificationStage,
};
use crate::capture::{CaptureTransport, Event, Failure, Recorder, TranscriptSummary};
use crate::native;
use crate::native::cat::{self as native_cat, Observation, Scope};
use crate::native::opening::{self, History};
use crate::native::workflow::POST_EXIT_SETTLE;

/// Why a session's connection was not opened.
#[derive(Debug)]
pub(super) enum Refusal {
    /// A transcript record made before the open could not be synchronized.
    Capture(Failure),
    /// Cancellation arrived before the open was dispatched.
    Cancelled,
    /// Settling, opening or admitting the selected endpoint failed.
    Open(Failure),
}

/// The connection opened for one programming session, or why there is none.
pub(super) enum Opening<C> {
    /// The connection, wrapped in the session's required capture.
    Opened(CaptureTransport<C, File>),
    /// No connection: the session's recorder, for its remaining records.
    Refused {
        recorder: Recorder<File>,
        refusal: Refusal,
    },
}

/// What a link observed before programming entry on the session connection.
#[derive(Debug, Default)]
pub(super) struct Admission {
    /// Gateway mode read before entry, when the link requires one.
    pub(super) gateway: Option<DvGatewayMode>,
    /// The failed or refused check; programming is not entered.
    pub(super) error: Option<Failure>,
}

/// The endpoint a write report names.
#[derive(Debug, Serialize)]
#[serde(untagged)]
pub(super) enum ReportEndpoint {
    /// The selected serial endpoint and its CAT baud rate.
    Usb(super::Endpoint),
    /// The paired radio's address and the RFCOMM channel its sessions used.
    NativeBluetooth {
        address: String,
        rfcomm_channel: Option<u8>,
    },
}

/// The connection lifecycle of an MCP write workflow.
pub(super) trait WriteLink {
    /// The raw connection each session opens.
    type Connection: kenwood_transport::Transport;

    /// Open the connection for one programming session, recording the request
    /// and its result in `recorder`. `later` marks a session that follows an
    /// earlier programming exit of the same workflow.
    async fn open_session(
        &mut self,
        recorder: Recorder<File>,
        later: bool,
        cancelled: &AtomicBool,
    ) -> Opening<Self::Connection>;

    /// The attempt history of the last `open_session`, when this link keeps one.
    fn take_opening_history(&mut self) -> Option<History>;

    /// Wrap a newly opened connection with this link's reply deadlines.
    fn radio(
        &self,
        transport: CaptureTransport<Self::Connection, File>,
    ) -> Radio<CaptureTransport<Self::Connection, File>>;

    /// Check the state this link requires before programming entry, on the
    /// session's own connection.
    async fn admit(
        &mut self,
        radio: &mut Radio<CaptureTransport<Self::Connection, File>>,
    ) -> Admission;

    /// Hold the original connection after an acknowledged exit, recording the
    /// wait in `post_exit`.
    async fn hold_after_exit(
        &mut self,
        post_exit: &mut Recorder<File>,
        cancelled: &AtomicBool,
    ) -> io::Result<()>;

    /// The fresh post-exit check of `identity`; `gateway_off` additionally
    /// requires Gateway Off on the fresh connection.
    async fn verify_post_exit(
        &mut self,
        identity: &Identity,
        gateway_off: bool,
        post_exit: Recorder<File>,
        cancelled: &AtomicBool,
    ) -> PostExit;

    /// A post-exit check that never started, naming `reason`.
    fn skipped(&self, reason: SkipReason, transcript: TranscriptSummary) -> PostExit;

    /// The endpoint the report names.
    fn report_endpoint(&self) -> ReportEndpoint;

    /// Whether sessions run over native Bluetooth.
    fn native(&self) -> bool;
}

/// A write workflow over the selected USB endpoint at `baud`.
pub(super) struct UsbLink<'a, B> {
    backend: &'a mut B,
    endpoint: &'a SerialCandidate,
    baud: std::num::NonZeroU32,
}

impl<'a, B: Backend> UsbLink<'a, B> {
    /// A link over `endpoint` through `backend`.
    pub(super) const fn new(
        backend: &'a mut B,
        endpoint: &'a SerialCandidate,
        baud: std::num::NonZeroU32,
    ) -> Self {
        Self {
            backend,
            endpoint,
            baud,
        }
    }
}

impl<B: Backend> WriteLink for UsbLink<'_, B> {
    type Connection = B::Connection;

    /// Wait for the operation-panel endpoint, or any later session's endpoint,
    /// to settle, then open the serial path once.
    async fn open_session(
        &mut self,
        mut recorder: Recorder<File>,
        later: bool,
        cancelled: &AtomicBool,
    ) -> Opening<Self::Connection> {
        if (later || self.endpoint.pid == Some(TMD750_PANEL_PID))
            && let Err(error) = reconnect::await_settled_endpoint(
                self.backend,
                self.endpoint,
                &mut recorder,
                cancelled,
            )
            .await
        {
            let failure = Failure::from_error(&error);
            recorder.record(Event::OpenFailed {
                error: failure.clone(),
            });
            return Opening::Refused {
                recorder,
                refusal: Refusal::Open(failure),
            };
        }
        recorder.record(Event::OpenRequested {
            path: &self.endpoint.path,
            baud: self.baud,
        });
        if let Err(error) = recorder.synchronize() {
            return Opening::Refused {
                recorder,
                refusal: Refusal::Capture(Failure::from_error(&error)),
            };
        }
        if cancelled.load(Ordering::Relaxed) {
            return Opening::Refused {
                recorder,
                refusal: Refusal::Cancelled,
            };
        }
        match self.backend.open(self.endpoint, self.baud) {
            Ok(connection) => {
                recorder.record(Event::OpenCompleted);
                Opening::Opened(CaptureTransport::required(connection, recorder))
            }
            Err(error) => {
                let failure = Failure::from_error(&error);
                recorder.record(Event::OpenFailed {
                    error: failure.clone(),
                });
                Opening::Refused {
                    recorder,
                    refusal: Refusal::Open(failure),
                }
            }
        }
    }

    fn take_opening_history(&mut self) -> Option<History> {
        None
    }

    fn radio(
        &self,
        transport: CaptureTransport<Self::Connection, File>,
    ) -> Radio<CaptureTransport<Self::Connection, File>> {
        Radio::new(transport)
    }

    fn admit(
        &mut self,
        _radio: &mut Radio<CaptureTransport<Self::Connection, File>>,
    ) -> impl Future<Output = Admission> {
        std::future::ready(Admission::default())
    }

    fn hold_after_exit(
        &mut self,
        _post_exit: &mut Recorder<File>,
        _cancelled: &AtomicBool,
    ) -> impl Future<Output = io::Result<()>> {
        std::future::ready(Ok(()))
    }

    /// The bounded silent-`ID` retry that backups use, on either USB role.
    async fn verify_post_exit(
        &mut self,
        identity: &Identity,
        gateway_off: bool,
        post_exit: Recorder<File>,
        cancelled: &AtomicBool,
    ) -> PostExit {
        let (backend, endpoint, baud) = (&mut *self.backend, self.endpoint, self.baud);
        PostExit::Readiness(Box::new(if gateway_off {
            reconnect::verify_readiness_gateway_off(
                backend, endpoint, baud, identity, post_exit, cancelled,
            )
            .await
        } else {
            reconnect::verify_readiness(backend, endpoint, baud, identity, post_exit, cancelled)
                .await
        }))
    }

    fn skipped(&self, reason: SkipReason, transcript: TranscriptSummary) -> PostExit {
        PostExit::skipped(reason, transcript)
    }

    fn report_endpoint(&self) -> ReportEndpoint {
        ReportEndpoint::Usb(super::Endpoint {
            path: self.endpoint.path.clone(),
            usb_vendor_id: self.endpoint.vid,
            usb_product_id: self.endpoint.pid,
            cat_baud: self.baud,
        })
    }

    fn native(&self) -> bool {
        false
    }
}

/// A write workflow over the paired radio's native Bluetooth link.
pub(super) struct NativeLink<'a, B> {
    backend: &'a mut B,
    endpoint: &'a native::Endpoint,
    /// The RFCOMM channel of the first successful open, pinned for every later
    /// connection of the workflow.
    channel: Option<RfcommChannel>,
    opening: Option<History>,
    /// When the last hold after an acknowledged exit began; the exit came no
    /// later.
    exited: Option<Instant>,
}

impl<'a, B: native::Backend> NativeLink<'a, B> {
    /// A link to the radio at `endpoint` through `backend`.
    pub(super) const fn new(backend: &'a mut B, endpoint: &'a native::Endpoint) -> Self {
        Self {
            backend,
            endpoint,
            channel: None,
            opening: None,
            exited: None,
        }
    }

    const fn service(&self) -> BluetoothService {
        match self.channel {
            Some(channel) => BluetoothService::FixedChannel(channel),
            None => BluetoothService::SerialPort,
        }
    }
}

impl<B: native::Backend> WriteLink for NativeLink<'_, B> {
    type Connection = B::Connection;

    /// Open the exact address with the native attempt policy: a Serial Port
    /// service query the first time, the pinned channel afterwards.
    async fn open_session(
        &mut self,
        recorder: Recorder<File>,
        _later: bool,
        cancelled: &AtomicBool,
    ) -> Opening<Self::Connection> {
        let (history, opened) = opening::open_recorded(
            self.backend,
            self.endpoint,
            self.service(),
            recorder,
            cancelled,
        )
        .await;
        let refusal = refusal(&history, cancelled);
        self.opening = Some(history);
        match opened {
            Ok(captured) => {
                let _pinned = self.channel.get_or_insert(captured.channel);
                Opening::Opened(captured.transport)
            }
            Err(recorder) => Opening::Refused { recorder, refusal },
        }
    }

    fn take_opening_history(&mut self) -> Option<History> {
        self.opening.take()
    }

    fn radio(
        &self,
        transport: CaptureTransport<Self::Connection, File>,
    ) -> Radio<CaptureTransport<Self::Connection, File>> {
        Radio::new_bluetooth(transport)
    }

    /// Require Gateway Off before programming entry, as native probes and
    /// backups do.
    async fn admit(
        &mut self,
        radio: &mut Radio<CaptureTransport<Self::Connection, File>>,
    ) -> Admission {
        match radio.get_dv_gateway_mode().await {
            Ok(DvGatewayMode::Off) => Admission {
                gateway: Some(DvGatewayMode::Off),
                error: None,
            },
            Ok(mode) => Admission {
                gateway: Some(mode),
                error: Some(Failure::from_error(&io::Error::other(format!(
                    "native Bluetooth writes require Gateway Off; observed {mode}"
                )))),
            },
            Err(error) => Admission {
                gateway: None,
                error: Some(Failure::from_error(&error)),
            },
        }
    }

    async fn hold_after_exit(
        &mut self,
        post_exit: &mut Recorder<File>,
        cancelled: &AtomicBool,
    ) -> io::Result<()> {
        self.exited = Some(Instant::now());
        native::workflow::settle(self.backend, post_exit, cancelled).await
    }

    /// Reopen the pinned channel and read the identity tuple and Gateway Off,
    /// the first reply allowed until `BLUETOOTH_EXIT_REPLY_BOUND` after the
    /// exit. `gateway_off` is implied: native sessions always require it.
    async fn verify_post_exit(
        &mut self,
        identity: &Identity,
        _gateway_off: bool,
        post_exit: Recorder<File>,
        cancelled: &AtomicBool,
    ) -> PostExit {
        let Some(channel) = self.channel else {
            return self.skipped(SkipReason::OriginalOpenFailed, post_exit.summary());
        };
        let (history, opened) = opening::open_recorded(
            self.backend,
            self.endpoint,
            BluetoothService::FixedChannel(channel),
            post_exit,
            cancelled,
        )
        .await;
        let verification = match opened {
            Err(recorder) => NativeVerification::unopened(history, recorder.summary(), cancelled),
            Ok(captured) => {
                let first_reply_timeout = bluetooth_first_reply_timeout_after_exit(
                    self.exited
                        .map_or(Duration::ZERO, |exited| exited.elapsed()),
                );
                let observation = native_cat::observe(
                    captured.transport,
                    native_cat::Request {
                        scope: Scope::Gateway,
                        expected_identity: Some(identity),
                        expected_gateway: Some(DvGatewayMode::Off),
                        first_reply_timeout,
                    },
                    cancelled,
                )
                .await;
                NativeVerification::observed(identity, history, observation, first_reply_timeout)
            }
        };
        PostExit::Native(Box::new(verification))
    }

    fn skipped(&self, reason: SkipReason, transcript: TranscriptSummary) -> PostExit {
        PostExit::Native(Box::new(NativeVerification::skipped(reason, transcript)))
    }

    fn report_endpoint(&self) -> ReportEndpoint {
        ReportEndpoint::NativeBluetooth {
            address: self.endpoint.address.to_string(),
            rfcomm_channel: self.channel.map(RfcommChannel::get),
        }
    }

    fn native(&self) -> bool {
        true
    }
}

/// Why a native opening produced no connection.
fn refusal(history: &History, cancelled: &AtomicBool) -> Refusal {
    if let Some(error) = &history.capture_error {
        return Refusal::Capture(error.clone());
    }
    if cancelled.load(Ordering::Relaxed) {
        return Refusal::Cancelled;
    }
    Refusal::Open(opening_failure(history))
}

/// The first recorded reason a native opening failed.
fn opening_failure(history: &History) -> Failure {
    history
        .attempts
        .iter()
        .rev()
        .find_map(|attempt| attempt.error.as_ref().map(|error| error.error.clone()))
        .or_else(|| history.retry_error.clone())
        .or_else(|| history.capture_error.clone())
        .unwrap_or_else(|| {
            Failure::from_error(&io::Error::other("native opening produced no connection"))
        })
}

/// The fresh post-exit check over native Bluetooth.
///
/// The original connection was held without traffic for `POST_EXIT_SETTLE`
/// after the acknowledged exit and then closed; this reopens the pinned
/// RFCOMM channel and reads the identity tuple and Gateway Off, the first
/// reply allowed `first_reply_timeout_milliseconds`. A matching tuple on the
/// same address names the model and firmware, not physical unit continuity.
#[derive(Debug, Serialize)]
pub(super) struct NativeVerification {
    identity_assurance: &'static str,
    service: &'static str,
    hold_milliseconds: u128,
    first_reply_timeout_milliseconds: Option<u128>,
    required_gateway_mode: &'static str,
    opening: Option<History>,
    observation: Option<Observation>,
    /// Summary of the transcript reserved for this check.
    pub(super) transcript: TranscriptSummary,
    /// Final outcome of this check.
    pub(super) outcome: VerificationOutcome,
}

impl NativeVerification {
    const fn base(transcript: TranscriptSummary, outcome: VerificationOutcome) -> Self {
        Self {
            identity_assurance: "exact_bluetooth_address_and_cat_tuple_not_physical_unit_continuity",
            service: "fixed_previously_opened_channel",
            hold_milliseconds: POST_EXIT_SETTLE.as_millis(),
            first_reply_timeout_milliseconds: None,
            required_gateway_mode: "off",
            opening: None,
            observation: None,
            transcript,
            outcome,
        }
    }

    /// A check that never started, naming `reason`.
    pub(super) const fn skipped(reason: SkipReason, transcript: TranscriptSummary) -> Self {
        Self::base(transcript, VerificationOutcome::Skipped { reason })
    }

    /// A check whose fresh connection did not open.
    fn unopened(history: History, transcript: TranscriptSummary, cancelled: &AtomicBool) -> Self {
        let outcome = if cancelled.load(Ordering::Relaxed) {
            VerificationOutcome::Cancelled
        } else {
            VerificationOutcome::Failed {
                stage: VerificationStage::Open,
                error: opening_failure(&history),
            }
        };
        Self {
            opening: Some(history),
            ..Self::base(transcript, outcome)
        }
    }

    /// A check whose fresh connection opened and ran the CAT observation.
    fn observed(
        expected: &Identity,
        history: History,
        observation: Observation,
        first_reply_timeout: Duration,
    ) -> Self {
        let outcome = conclusion(expected, &history, &observation);
        let mut verification = Self::base(observation.transcript.clone(), outcome);
        verification.first_reply_timeout_milliseconds = Some(first_reply_timeout.as_millis());
        verification.opening = Some(history);
        verification.observation = Some(observation);
        verification
    }

    /// True when the outcome is `Matched` and the transcript is complete.
    pub(super) const fn succeeded(&self) -> bool {
        self.transcript.complete && matches!(self.outcome, VerificationOutcome::Matched)
    }

    /// The identity and Gateway mode of a successful check.
    pub(super) fn gateway_off_evidence(&self) -> Option<(&Identity, DvGatewayMode)> {
        if !self.succeeded() {
            return None;
        }
        let observation = self.observation.as_ref()?;
        let mode = DvGatewayMode::from(observation.gateway?);
        if mode != DvGatewayMode::Off {
            return None;
        }
        Some((&observation.identity.as_ref()?.0, mode))
    }
}

/// The outcome of an opened native check, from its first failed stage.
fn conclusion(
    expected: &Identity,
    history: &History,
    observation: &Observation,
) -> VerificationOutcome {
    let failed = |stage, error: &Failure| VerificationOutcome::Failed {
        stage,
        error: error.clone(),
    };
    if !history.succeeded() {
        return failed(VerificationStage::Open, &opening_failure(history));
    }
    if let Some(error) = &observation.operation_error {
        let stage = match (&observation.identity, observation.gateway) {
            (None, _) => VerificationStage::Identity,
            (Some(identity), _) if identity.0 != *expected => VerificationStage::IdentityMismatch,
            (Some(_), None) => VerificationStage::Gateway,
            (Some(_), Some(_)) => VerificationStage::GatewayMismatch,
        };
        return failed(stage, error);
    }
    if let Some(error) = &observation.close_error {
        return failed(VerificationStage::Close, error);
    }
    if let Some(error) = &observation.capture_error {
        return failed(VerificationStage::Capture, error);
    }
    if observation.cancelled {
        return VerificationOutcome::Cancelled;
    }
    if observation.succeeded() {
        VerificationOutcome::Matched
    } else {
        failed(
            VerificationStage::Capture,
            &Failure::from_error(&io::Error::other(
                "fresh native CAT transcript is incomplete",
            )),
        )
    }
}
