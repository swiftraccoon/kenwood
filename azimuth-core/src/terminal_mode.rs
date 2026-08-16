//! Pre-automation recovery from a USB interface that answers as MMDVM.

use std::sync::{
    Arc,
    atomic::{AtomicU8, Ordering},
};

use kenwood_thd75::radio::programming::DetachedMcpPageUpdate;
#[cfg(target_os = "macos")]
use kenwood_thd75::{
    PairedBluetoothCandidate, Radio,
    error::TransportError,
    transport::{BluetoothTransport, Transport},
    types::{DvGatewayMode, SerialNumber},
};

#[cfg(target_os = "macos")]
const BLUETOOTH_HELPER_EXECUTABLE: &str = "AzimuthBluetoothHelper";

/// Candidate qualification must fit inside this complete wall-clock budget.
#[cfg(target_os = "macos")]
const BLUETOOTH_CANDIDATE_PROBE_WINDOW: std::time::Duration = std::time::Duration::from_secs(100);

/// A D75-likely probe can use two 22-second native opens with a one-second
/// retry delay, followed by one five-second CAT command and bounded teardown.
/// Do not begin another candidate unless the worst case fits in the window.
#[cfg(target_os = "macos")]
const BLUETOOTH_CANDIDATE_PROBE_RESERVE: std::time::Duration = std::time::Duration::from_secs(52);

/// Independent count ceiling below the signed helper's framing limit.
#[cfg(target_os = "macos")]
const MAX_BLUETOOTH_CANDIDATE_PROBES: usize = 8;

const CANCELLATION_REQUESTED: u8 = 1 << 0;
#[cfg(any(target_os = "macos", test))]
const MCP_OPERATION_STARTED: u8 = 1 << 1;
const OPERATION_FRESH: u8 = 0;
const OPERATION_RUNNING: u8 = 1;
const OPERATION_FINISHED: u8 = 2;

#[derive(Debug, Default)]
struct RecoveryCancellation {
    state: AtomicU8,
    notification: tokio::sync::Notify,
}

impl RecoveryCancellation {
    fn request(&self) {
        let previous = self
            .state
            .fetch_or(CANCELLATION_REQUESTED, Ordering::AcqRel);
        if previous & CANCELLATION_REQUESTED == 0 {
            // `notify_one` retains a permit when the run future is between its
            // atomic check and registering its single cancellation waiter.
            self.notification.notify_one();
        }
    }

    fn check(&self) -> Result<(), DvGatewayRecoveryError> {
        if self.state.load(Ordering::Acquire) & CANCELLATION_REQUESTED == 0 {
            Ok(())
        } else {
            Err(DvGatewayRecoveryError::Cancelled)
        }
    }

    #[cfg(any(target_os = "macos", test))]
    fn begin_mcp_operation(&self) -> Result<(), DvGatewayRecoveryError> {
        let mut state = self.state.load(Ordering::Acquire);
        loop {
            if state & CANCELLATION_REQUESTED != 0 {
                return Err(DvGatewayRecoveryError::Cancelled);
            }
            match self.state.compare_exchange_weak(
                state,
                state | MCP_OPERATION_STARTED,
                Ordering::AcqRel,
                Ordering::Acquire,
            ) {
                Ok(_) => return Ok(()),
                Err(actual) => state = actual,
            }
        }
    }

    #[cfg(any(target_os = "macos", test))]
    async fn cancelled(&self) {
        if self.state.load(Ordering::Acquire) & CANCELLATION_REQUESTED != 0 {
            return;
        }
        self.notification.notified().await;
    }
}

struct OperationRunGuard<'a> {
    state: &'a AtomicU8,
}

impl Drop for OperationRunGuard<'_> {
    fn drop(&mut self) {
        self.state.store(OPERATION_FINISHED, Ordering::Release);
    }
}

#[cfg(target_os = "macos")]
enum BluetoothCandidateProbe {
    Unavailable,
    Identified(SerialNumber),
    IdentityFailed(String),
}

#[cfg(target_os = "macos")]
fn transport_error_detail(error: &TransportError) -> String {
    use std::error::Error as _;

    let mut detail = error.to_string();
    let mut source = error.source();
    while let Some(cause) = source {
        detail.push_str(": ");
        detail.push_str(&cause.to_string());
        source = cause.source();
    }
    detail
}

#[cfg(target_os = "macos")]
fn bundled_bluetooth_helper_executable() -> Result<std::path::PathBuf, DvGatewayRecoveryError> {
    let host =
        std::env::current_exe().map_err(|error| DvGatewayRecoveryError::BluetoothUnavailable {
            detail: format!("could not locate the signed Azimuth executable: {error}"),
        })?;
    let directory = host
        .parent()
        .ok_or_else(|| DvGatewayRecoveryError::BluetoothUnavailable {
            detail: format!(
                "the Azimuth executable has no parent directory: {}",
                host.display()
            ),
        })?;
    let helper = directory.join(BLUETOOTH_HELPER_EXECUTABLE);
    if !helper.is_file() {
        return Err(DvGatewayRecoveryError::BluetoothUnavailable {
            detail: format!(
                "the signed Bluetooth helper is missing from {}",
                helper.display()
            ),
        });
    }
    Ok(helper)
}

#[cfg(target_os = "macos")]
fn map_helper_task_failure(error: &tokio::task::JoinError) -> DvGatewayRecoveryError {
    DvGatewayRecoveryError::BluetoothUnavailable {
        detail: format!("Bluetooth helper task failed: {error}"),
    }
}

#[cfg(target_os = "macos")]
fn map_transport_failure(error: &TransportError) -> DvGatewayRecoveryError {
    DvGatewayRecoveryError::BluetoothUnavailable {
        detail: transport_error_detail(error),
    }
}

/// Launch and validate the embedded Bluetooth recovery helper without opening
/// a radio or changing any setting.
///
/// On macOS this exercises the signed sandbox-inheriting helper's readiness
/// handshake, bounded paired-device framing, parser, and clean exit. The
/// returned count is diagnostic only. Candidate qualification and radio I/O
/// still occur exclusively after the user approves an actual recovery.
///
/// # Errors
///
/// Returns [`DvGatewayRecoveryError::UnsupportedPlatform`] outside macOS, or
/// [`DvGatewayRecoveryError::BluetoothUnavailable`] when the embedded helper
/// is absent, cannot launch under the host sandbox, times out, or returns an
/// invalid candidate snapshot.
#[uniffi::export(async_runtime = "tokio")]
pub async fn validate_bluetooth_recovery_helper() -> Result<u32, DvGatewayRecoveryError> {
    #[cfg(target_os = "macos")]
    {
        let helper_executable = bundled_bluetooth_helper_executable()?;
        let candidates = enumerate_bluetooth_candidates(helper_executable).await?;
        u32::try_from(candidates.len()).map_err(|error| {
            DvGatewayRecoveryError::BluetoothUnavailable {
                detail: format!("paired Bluetooth candidate count did not fit u32: {error}"),
            }
        })
    }

    #[cfg(not(target_os = "macos"))]
    {
        Err(DvGatewayRecoveryError::UnsupportedPlatform)
    }
}

#[cfg(target_os = "macos")]
async fn enumerate_bluetooth_candidates(
    helper_executable: std::path::PathBuf,
) -> Result<Vec<PairedBluetoothCandidate>, DvGatewayRecoveryError> {
    tokio::task::spawn_blocking(move || {
        BluetoothTransport::paired_spp_candidates_with_helper_executable(helper_executable)
    })
    .await
    .map_err(|error| map_helper_task_failure(&error))?
    .map_err(|error| map_transport_failure(&error))
}

#[cfg(target_os = "macos")]
async fn enumerate_bluetooth_candidates_cancellable(
    helper_executable: std::path::PathBuf,
    cancellation: &RecoveryCancellation,
) -> Result<Vec<PairedBluetoothCandidate>, DvGatewayRecoveryError> {
    let result = enumerate_bluetooth_candidates(helper_executable).await;
    cancellation.check()?;
    result
}

#[cfg(target_os = "macos")]
async fn open_exact_bluetooth_candidate(
    candidate: PairedBluetoothCandidate,
    helper_executable: std::path::PathBuf,
    cancellation: &RecoveryCancellation,
) -> Result<Option<BluetoothTransport>, DvGatewayRecoveryError> {
    let task_result = tokio::task::spawn_blocking(move || {
        BluetoothTransport::open_paired_candidate_with_helper_executable(
            &candidate,
            helper_executable,
        )
    })
    .await;
    cancellation.check()?;
    let result = task_result.map_err(|error| map_helper_task_failure(&error))?;
    match result {
        Ok(transport) => Ok(Some(transport)),
        Err(TransportError::NotFound) => Ok(None),
        Err(error) => Err(map_transport_failure(&error)),
    }
}

#[cfg(target_os = "macos")]
async fn probe_exact_bluetooth_candidate(
    candidate: PairedBluetoothCandidate,
    helper_executable: std::path::PathBuf,
    cancellation: &RecoveryCancellation,
) -> Result<Option<BluetoothTransport>, DvGatewayRecoveryError> {
    let task_result = tokio::task::spawn_blocking(move || {
        BluetoothTransport::probe_paired_candidate_with_helper_executable(
            &candidate,
            helper_executable,
        )
    })
    .await;
    cancellation.check()?;
    let result = task_result.map_err(|error| map_helper_task_failure(&error))?;
    match result {
        Ok(transport) => Ok(Some(transport)),
        Err(TransportError::NotFound) => Ok(None),
        Err(error) => Err(map_transport_failure(&error)),
    }
}

#[cfg(target_os = "macos")]
async fn probe_bluetooth_candidate_identity(
    candidate: &PairedBluetoothCandidate,
    helper_executable: &std::path::Path,
    cancellation: &RecoveryCancellation,
) -> Result<BluetoothCandidateProbe, DvGatewayRecoveryError> {
    let opened = probe_exact_bluetooth_candidate(
        candidate.clone(),
        helper_executable.to_owned(),
        cancellation,
    )
    .await?;
    let Some(transport) = opened else {
        return Ok(BluetoothCandidateProbe::Unavailable);
    };

    let mut radio = Radio::new(transport);
    if let Err(error) = cancellation.check() {
        drop(radio.disconnect().await);
        return Err(error);
    }
    let identity = {
        let query = radio.get_serial_information();
        tokio::pin!(query);
        tokio::select! {
            biased;
            () = cancellation.cancelled() => None,
            result = &mut query => Some(result),
        }
    };
    drop(radio.disconnect().await);
    let Some(identity) = identity else {
        return Err(DvGatewayRecoveryError::Cancelled);
    };
    cancellation.check()?;
    Ok(match identity {
        Ok(information) => BluetoothCandidateProbe::Identified(information.into_parts().0),
        Err(error) => BluetoothCandidateProbe::IdentityFailed(error.to_string()),
    })
}

#[cfg(target_os = "macos")]
async fn select_matching_bluetooth_candidate(
    candidates: &[PairedBluetoothCandidate],
    expected: &SerialNumber,
    helper_executable: &std::path::Path,
    cancellation: &RecoveryCancellation,
) -> Result<PairedBluetoothCandidate, DvGatewayRecoveryError> {
    cancellation.check()?;
    if candidates.is_empty() {
        return Err(DvGatewayRecoveryError::BluetoothUnavailable {
            detail: "macOS reported no paired Bluetooth devices; pair the TH-D75 and retry"
                .to_owned(),
        });
    }
    if candidates.len() > MAX_BLUETOOTH_CANDIDATE_PROBES {
        return Err(DvGatewayRecoveryError::BluetoothIdentityUnavailable {
            detail: format!(
                "Bluetooth candidate qualification was incomplete: the bounded snapshot contains {} paired candidates, exceeding the {}-candidate probe cap; remove stale Bluetooth pairings and retry; no setting was changed",
                candidates.len(),
                MAX_BLUETOOTH_CANDIDATE_PROBES
            ),
        });
    }

    let deadline = std::time::Instant::now() + BLUETOOTH_CANDIDATE_PROBE_WINDOW;
    let mut attempted = 0_usize;
    let mut nonmatching = Vec::new();
    let mut identity_failures = Vec::new();
    let mut stopped_reason = None;

    for candidate in candidates {
        cancellation.check()?;
        let remaining = deadline.saturating_duration_since(std::time::Instant::now());
        if remaining < BLUETOOTH_CANDIDATE_PROBE_RESERVE {
            stopped_reason = Some(
                "too little time remained in the 100-second qualification window to safely begin another bounded probe"
                    .to_owned(),
            );
            break;
        }
        attempted += 1;

        match probe_bluetooth_candidate_identity(candidate, helper_executable, cancellation).await?
        {
            BluetoothCandidateProbe::Unavailable => {}
            BluetoothCandidateProbe::Identified(actual) => {
                if &actual == expected {
                    // CAT serial is the stable USB identity used for this
                    // recovery. Reopen this exact address and verify the same
                    // serial again before the MCP mutation gate. Unrelated
                    // paired devices cannot invalidate that positive proof.
                    return Ok(candidate.clone());
                }
                nonmatching.push(format!(
                    "{} ({}) reported {actual}",
                    candidate.display_name(),
                    candidate.address()
                ));
            }
            BluetoothCandidateProbe::IdentityFailed(error) => {
                identity_failures.push(format!(
                    "{} ({}) did not answer CAT AE: {error}",
                    candidate.display_name(),
                    candidate.address()
                ));
            }
        }
        if std::time::Instant::now() > deadline {
            stopped_reason = Some(
                "the 100-second Bluetooth candidate qualification deadline was exceeded".to_owned(),
            );
            break;
        }
    }

    if let Some(reason) = stopped_reason {
        return Err(DvGatewayRecoveryError::BluetoothIdentityUnavailable {
            detail: format!(
                "Bluetooth candidate qualification was incomplete: tried {attempted} of {} paired candidates because {reason}; no setting was changed",
                candidates.len()
            ),
        });
    }

    let mut detail = format!(
        "no paired Bluetooth candidate proved USB radio serial {expected}; tried {attempted} of {} candidates",
        candidates.len()
    );
    if !nonmatching.is_empty() {
        detail.push_str("; different radios: ");
        detail.push_str(&nonmatching.join(", "));
    }
    if !identity_failures.is_empty() {
        detail.push_str("; unresolved candidates: ");
        detail.push_str(&identity_failures.join(", "));
    }
    Err(DvGatewayRecoveryError::BluetoothIdentityUnavailable { detail })
}

#[cfg(target_os = "macos")]
async fn open_selected_bluetooth_transport(
    helper_executable: std::path::PathBuf,
    bluetooth_device_name: Option<String>,
    expected: &SerialNumber,
    cancellation: &RecoveryCancellation,
) -> Result<BluetoothTransport, DvGatewayRecoveryError> {
    let explicit_device_name = bluetooth_device_name.is_some();
    let preferred_device_name = bluetooth_device_name.clone();
    let preferred_helper = helper_executable.clone();
    let preferred_open = tokio::task::spawn_blocking(move || {
        BluetoothTransport::open_with_helper_executable(
            preferred_device_name.as_deref(),
            preferred_helper,
        )
    })
    .await;
    cancellation.check()?;
    match preferred_open.map_err(|error| map_helper_task_failure(&error))? {
        Ok(transport) => return Ok(transport),
        Err(error) if explicit_device_name => return Err(map_transport_failure(&error)),
        // The normal one-radio case can use the radio's factory Bluetooth
        // name without enumerating every paired phone. A custom name or more
        // than one paired TH-D75 falls back to exact-address discovery. The
        // CAT serial check after this function remains the mutation gate.
        Err(TransportError::NotFound | TransportError::BluetoothDeviceNameAmbiguous) => {}
        Err(error) => return Err(map_transport_failure(&error)),
    }

    let candidates =
        enumerate_bluetooth_candidates_cancellable(helper_executable.clone(), cancellation).await?;
    let selected = select_matching_bluetooth_candidate(
        &candidates,
        expected,
        &helper_executable,
        cancellation,
    )
    .await?;
    open_exact_bluetooth_candidate(selected, helper_executable, cancellation)
        .await?
        .ok_or_else(|| DvGatewayRecoveryError::BluetoothUnavailable {
            detail: "the serial-matched Bluetooth radio became unavailable before the verified Menu 650 operation; bring the radio within range and retry"
                .to_owned(),
        })
}

#[cfg(target_os = "macos")]
async fn verify_matching_radio_serial<T: Transport>(
    radio: &mut Radio<T>,
    expected: &SerialNumber,
    cancellation: &RecoveryCancellation,
) -> Result<(), DvGatewayRecoveryError> {
    cancellation.check()?;
    let result = {
        let query = radio.get_serial_information();
        tokio::pin!(query);
        tokio::select! {
            biased;
            () = cancellation.cancelled() => None,
            result = &mut query => Some(result),
        }
    };
    let actual = result
        .ok_or(DvGatewayRecoveryError::Cancelled)?
        .map_err(
            |error| DvGatewayRecoveryError::BluetoothIdentityUnavailable {
                detail: error.to_string(),
            },
        )?
        .into_parts()
        .0;
    if &actual == expected {
        Ok(())
    } else {
        Err(DvGatewayRecoveryError::RadioIdentityMismatch {
            expected: expected.to_string(),
            actual: actual.to_string(),
        })
    }
}

#[cfg(target_os = "macos")]
async fn verify_schema_target_before_mcp<T: Transport>(
    radio: &mut Radio<T>,
    cancellation: &RecoveryCancellation,
) -> Result<(), DvGatewayRecoveryError> {
    cancellation.check()?;
    let result = {
        let verification = radio.verify_mcp_schema_target();
        tokio::pin!(verification);
        tokio::select! {
            biased;
            () = cancellation.cancelled() => None,
            result = &mut verification => Some(result),
        }
    };
    result
        .ok_or(DvGatewayRecoveryError::Cancelled)?
        .map_err(|error| DvGatewayRecoveryError::RadioOperation {
            detail: error.to_string(),
        })
}

/// Result of clearing Menu 650 over the radio's alternate control link.
#[derive(Debug, Clone, Copy, PartialEq, Eq, uniffi::Enum)]
pub enum DvGatewayRecoveryOutcome {
    /// Menu 650 changed and the radio is rebooting into ordinary CAT mode.
    ChangedRadioRebooting,
    /// Menu 650 was already off and the alternate link returned to CAT.
    AlreadyOffCatReady,
}

impl From<DetachedMcpPageUpdate> for DvGatewayRecoveryOutcome {
    fn from(value: DetachedMcpPageUpdate) -> Self {
        match value {
            DetachedMcpPageUpdate::ChangedRadioRebooting => Self::ChangedRadioRebooting,
            DetachedMcpPageUpdate::UnchangedCatReady => Self::AlreadyOffCatReady,
        }
    }
}

/// Failure while asking the paired radio to leave DV Gateway mode.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error, uniffi::Error)]
pub enum DvGatewayRecoveryError {
    /// Recovery was cancelled before the Menu 650 mutation gate opened.
    #[error("DV Gateway recovery was cancelled before Menu 650 could be changed")]
    Cancelled,
    /// This single-use recovery object has already run.
    #[error("this DV Gateway recovery operation has already run")]
    OperationAlreadyRun,
    /// Bluetooth Classic SPP is not available on this operating system.
    #[error(
        "automatic DV Gateway recovery requires the macOS Bluetooth Classic link; iPadOS cannot access the TH-D75 SPP service"
    )]
    UnsupportedPlatform,
    /// The paired radio's alternate Bluetooth control link could not be opened.
    #[error("could not open the paired TH-D75 Bluetooth control link: {detail}")]
    BluetoothUnavailable {
        /// Underlying native transport detail.
        detail: String,
    },
    /// The USB device did not expose a CAT-compatible stable serial identity.
    #[error("could not qualify the USB radio identity: {detail}")]
    UsbIdentityUnavailable {
        /// USB descriptor validation detail.
        detail: String,
    },
    /// The alternate Bluetooth radio did not answer its serial query.
    #[error("could not read the Bluetooth radio identity: {detail}")]
    BluetoothIdentityUnavailable {
        /// CAT identity-query detail.
        detail: String,
    },
    /// Bluetooth reached a different physical radio than the USB device.
    #[error(
        "Bluetooth radio serial {actual} does not match USB radio serial {expected}; no setting was changed"
    )]
    RadioIdentityMismatch {
        /// Stable serial reported by the USB device descriptor.
        expected: String,
        /// Exact serial returned by CAT `AE` over Bluetooth.
        actual: String,
    },
    /// The verified Menu 650 operation failed and its cleanup completed.
    #[error("could not turn Menu 650 (DV Gateway) off: {detail}")]
    RadioOperation {
        /// Underlying radio or cleanup detail.
        detail: String,
    },
    /// The Menu 650 operation was interrupted and its final value is uncertain.
    #[error(
        "the Menu 650 write outcome is uncertain: {detail}; power-cycle the radio and inspect Menu 650 before retrying"
    )]
    OutcomeUncertain {
        /// Original operation and failed-recovery detail.
        detail: String,
    },
}

/// One single-use, synchronously cancellable Menu 650 recovery attempt.
///
/// Swift cancellation does not implicitly cancel a `UniFFI` async Rust future.
/// This object provides an explicit synchronous cancellation signal while
/// retaining native ownership until all helper, radio, and cleanup work has
/// finished.
#[derive(uniffi::Object)]
pub struct DvGatewayRecoveryOperation {
    expected_radio_serial_number: String,
    bluetooth_device_name: Option<String>,
    cancellation: RecoveryCancellation,
    run_state: AtomicU8,
}

impl std::fmt::Debug for DvGatewayRecoveryOperation {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("DvGatewayRecoveryOperation")
            .field(
                "expected_radio_serial_number",
                &self.expected_radio_serial_number,
            )
            .field("bluetooth_device_name", &self.bluetooth_device_name)
            .field("run_state", &self.run_state.load(Ordering::Acquire))
            .finish_non_exhaustive()
    }
}

impl DvGatewayRecoveryOperation {
    fn begin_run(&self) -> Result<OperationRunGuard<'_>, DvGatewayRecoveryError> {
        let _previous = self
            .run_state
            .compare_exchange(
                OPERATION_FRESH,
                OPERATION_RUNNING,
                Ordering::AcqRel,
                Ordering::Acquire,
            )
            .map_err(|_| DvGatewayRecoveryError::OperationAlreadyRun)?;
        Ok(OperationRunGuard {
            state: &self.run_state,
        })
    }
}

#[uniffi::export(async_runtime = "tokio")]
impl DvGatewayRecoveryOperation {
    /// Create one recovery attempt for the USB radio's exact stable serial.
    ///
    /// `bluetooth_device_name` may explicitly identify one paired device by
    /// exact name or address. Passing `None` performs bounded paired-device
    /// enumeration and exact CAT serial matching.
    #[uniffi::constructor]
    #[must_use]
    pub fn new(
        expected_radio_serial_number: String,
        bluetooth_device_name: Option<String>,
    ) -> Arc<Self> {
        Arc::new(Self {
            expected_radio_serial_number,
            bluetooth_device_name,
            cancellation: RecoveryCancellation::default(),
            run_state: AtomicU8::new(OPERATION_FRESH),
        })
    }

    /// Request cancellation synchronously.
    ///
    /// Cancellation before the MCP mutation gate prevents all Menu 650 writes.
    /// Once the gate has opened, the radio operation and any required cleanup
    /// run to completion; a completed outcome or radio error takes precedence
    /// over the later cancellation request.
    pub fn cancel(&self) {
        self.cancellation.request();
    }

    /// Run this recovery attempt exactly once.
    ///
    /// The caller must first close the USB connection that positively answered
    /// MMDVM and obtain explicit user consent. The operation opens Bluetooth,
    /// proves the same physical radio by exact CAT serial, verifies the exact
    /// supported radio and firmware, and conditionally writes Menu 650 with
    /// read-back verification. The caller still owns the USB reboot wait.
    ///
    /// # Errors
    ///
    /// Returns [`DvGatewayRecoveryError::Cancelled`] only when cancellation
    /// wins before the MCP mutation gate, or
    /// [`DvGatewayRecoveryError::OperationAlreadyRun`] after any earlier run.
    /// Other errors retain the identity, radio-operation, and uncertain-outcome
    /// distinctions documented by [`DvGatewayRecoveryError`].
    pub async fn run(self: Arc<Self>) -> Result<DvGatewayRecoveryOutcome, DvGatewayRecoveryError> {
        let _run_guard = self.begin_run()?;
        self.cancellation.check()?;

        #[cfg(target_os = "macos")]
        {
            disable_dv_gateway_mode_via_bluetooth(
                &self.expected_radio_serial_number,
                self.bluetooth_device_name.clone(),
                &self.cancellation,
            )
            .await
        }

        #[cfg(not(target_os = "macos"))]
        {
            std::future::ready(Err(DvGatewayRecoveryError::UnsupportedPlatform)).await
        }
    }
}

#[cfg(target_os = "macos")]
async fn disable_dv_gateway_mode_via_bluetooth(
    expected_radio_serial_number: &str,
    bluetooth_device_name: Option<String>,
    cancellation: &RecoveryCancellation,
) -> Result<DvGatewayRecoveryOutcome, DvGatewayRecoveryError> {
    cancellation.check()?;
    let expected_serial = SerialNumber::new(expected_radio_serial_number).map_err(|error| {
        DvGatewayRecoveryError::UsbIdentityUnavailable {
            detail: error.to_string(),
        }
    })?;
    cancellation.check()?;
    let helper_executable = bundled_bluetooth_helper_executable()?;
    let transport = open_selected_bluetooth_transport(
        helper_executable,
        bluetooth_device_name,
        &expected_serial,
        cancellation,
    )
    .await?;

    // The alternate interface already speaks CAT. Avoid the TNC-exit
    // preamble here because it would mutate unrelated radio state before
    // the setting operation begins.
    let mut radio = Radio::new(transport);
    if let Err(error) =
        verify_matching_radio_serial(&mut radio, &expected_serial, cancellation).await
    {
        drop(radio.disconnect().await);
        return Err(error);
    }

    if let Err(error) = verify_schema_target_before_mcp(&mut radio, cancellation).await {
        drop(radio.disconnect().await);
        return Err(error);
    }

    // This atomic transition is the mutation boundary. If cancel linearized
    // first, MCP never begins. Once MCP wins, the setter future is never
    // dropped and cancellation cannot obscure its completed outcome.
    if let Err(error) = cancellation.begin_mcp_operation() {
        drop(radio.disconnect().await);
        return Err(error);
    }
    let update = match radio
        .set_dv_gateway_mode_detached_unverified(DvGatewayMode::Off)
        .await
    {
        Ok(update) => update,
        Err(operation_error) => {
            let recovery = radio.recover_from_interrupted_mcp().await;
            drop(radio.disconnect().await);
            return match recovery {
                Ok(()) => Err(DvGatewayRecoveryError::RadioOperation {
                    detail: operation_error.to_string(),
                }),
                Err(recovery_error) => Err(DvGatewayRecoveryError::OutcomeUncertain {
                    detail: format!(
                        "operation failed ({operation_error}); MCP recovery also failed ({recovery_error})"
                    ),
                }),
            };
        }
    };

    match update {
        DetachedMcpPageUpdate::ChangedRadioRebooting => {
            // The reboot normally tears down this link. Its final close is
            // best effort; USB CAT proof is the caller's acceptance gate.
            drop(radio.disconnect().await);
        }
        DetachedMcpPageUpdate::UnchangedCatReady => drop(radio.disconnect().await),
    }
    Ok(update.into())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[cfg(target_os = "macos")]
    use kenwood_thd75::{MockTransport, Radio, types::SerialNumber};

    #[test]
    fn detached_update_outcomes_keep_reboot_ownership_explicit() {
        assert_eq!(
            DvGatewayRecoveryOutcome::from(DetachedMcpPageUpdate::ChangedRadioRebooting),
            DvGatewayRecoveryOutcome::ChangedRadioRebooting
        );
        assert_eq!(
            DvGatewayRecoveryOutcome::from(DetachedMcpPageUpdate::UnchangedCatReady),
            DvGatewayRecoveryOutcome::AlreadyOffCatReady
        );
    }

    #[tokio::test]
    async fn pre_cancelled_operation_stops_before_identity_or_mutation()
    -> Result<(), Box<dyn std::error::Error>> {
        let operation = DvGatewayRecoveryOperation::new(
            "not-a-valid-radio-serial".to_owned(),
            Some("not-a-paired-device".to_owned()),
        );
        operation.cancel();

        let first = Arc::clone(&operation).run().await;
        assert!(matches!(first, Err(DvGatewayRecoveryError::Cancelled)));

        let second = Arc::clone(&operation).run().await;
        assert!(matches!(
            second,
            Err(DvGatewayRecoveryError::OperationAlreadyRun)
        ));
        Ok(())
    }

    #[tokio::test]
    async fn cancellation_signal_wakes_and_mcp_gate_has_a_total_order()
    -> Result<(), Box<dyn std::error::Error>> {
        let pre_gate = Arc::new(RecoveryCancellation::default());
        let waiter_signal = Arc::clone(&pre_gate);
        let waiter = tokio::spawn(async move {
            waiter_signal.cancelled().await;
        });
        tokio::task::yield_now().await;
        pre_gate.request();
        tokio::time::timeout(std::time::Duration::from_secs(1), waiter).await??;
        assert!(matches!(
            pre_gate.begin_mcp_operation(),
            Err(DvGatewayRecoveryError::Cancelled)
        ));

        let post_gate = RecoveryCancellation::default();
        post_gate.begin_mcp_operation()?;
        post_gate.request();
        let state = post_gate.state.load(Ordering::Acquire);
        assert_ne!(state & MCP_OPERATION_STARTED, 0);
        assert_ne!(state & CANCELLATION_REQUESTED, 0);
        Ok(())
    }

    #[cfg(target_os = "macos")]
    #[tokio::test]
    async fn mismatched_bluetooth_serial_fails_before_any_mcp_write()
    -> Result<(), Box<dyn std::error::Error>> {
        let mut transport = MockTransport::new();
        transport.expect(b"AE\r", b"AE C5310165,K01\r");
        let mut radio = Radio::new(transport);
        let expected = SerialNumber::new("C3C10368")?;
        let cancellation = RecoveryCancellation::default();

        let Err(error) = verify_matching_radio_serial(&mut radio, &expected, &cancellation).await
        else {
            return Err("different physical radios were accepted".into());
        };

        assert!(
            matches!(
                &error,
                DvGatewayRecoveryError::RadioIdentityMismatch { expected, actual }
                    if expected == "C3C10368" && actual == "C5310165"
            ),
            "serial mismatch lost the two exact identities: {error}"
        );
        Ok(())
    }

    #[cfg(target_os = "macos")]
    #[tokio::test]
    async fn schema_rejection_stays_before_the_mcp_gate() -> Result<(), Box<dyn std::error::Error>>
    {
        let mut transport = MockTransport::new();
        transport.expect(b"ID\r", b"ID TH-D75\r");
        transport.expect(b"FV\r", b"FV 1.04\r");
        let mut radio = Radio::new(transport);
        let cancellation = RecoveryCancellation::default();

        let result = verify_schema_target_before_mcp(&mut radio, &cancellation).await;

        assert!(matches!(
            result,
            Err(DvGatewayRecoveryError::RadioOperation { .. })
        ));
        assert_eq!(
            cancellation.state.load(Ordering::Acquire) & MCP_OPERATION_STARTED,
            0
        );
        Ok(())
    }
}
