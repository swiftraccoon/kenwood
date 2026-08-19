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
    transport::{BluetoothOpenCancellation, BluetoothTransport, Transport},
    types::{DvGatewayMode, SerialNumber},
};

#[cfg(target_os = "macos")]
const BLUETOOTH_HELPER_EXECUTABLE: &str = "AzimuthBluetoothHelper";

/// Candidate qualification must fit inside this complete wall-clock budget.
#[cfg(target_os = "macos")]
const BLUETOOTH_CANDIDATE_PROBE_WINDOW: std::time::Duration = std::time::Duration::from_secs(100);

/// A D75-likely probe can use two 22-second native opens with a one-second
/// retry delay, followed by 800ms of packet-exit delays, up to five seconds of
/// residue drain, one five-second CAT command, and bounded teardown. Sixty
/// seconds admits that complete operation with margin; do not begin another
/// candidate when less remains.
#[cfg(target_os = "macos")]
const BLUETOOTH_CANDIDATE_PROBE_RESERVE: std::time::Duration = std::time::Duration::from_secs(60);

/// Independent count ceiling below the signed helper's framing limit.
#[cfg(target_os = "macos")]
const MAX_BLUETOOTH_CANDIDATE_PROBES: usize = 8;

#[cfg(target_os = "macos")]
fn bluetooth_candidate_probe_fits(remaining: std::time::Duration) -> bool {
    remaining >= BLUETOOTH_CANDIDATE_PROBE_RESERVE
}

/// Serialize only Azimuth's short helper validation/enumeration processes.
///
/// The lower transport's process-wide lease remains authoritative. In
/// particular, this gate never covers a live RFCOMM transport, so discovery
/// still fails closed instead of queueing behind a connected radio.
#[cfg(target_os = "macos")]
static BLUETOOTH_HELPER_ENUMERATION_GATE: tokio::sync::Mutex<()> =
    tokio::sync::Mutex::const_new(());

/// Serialize bounded identity scans without queueing live RFCOMM ownership.
///
/// Enumeration has its own shorter gate above. This gate covers only the
/// sequential open/ID/AE/close probes used by serial matching and explicit
/// custom-name discovery. The lower process lease still makes a scan fail
/// closed when a normal Bluetooth link already owns the radio.
#[cfg(target_os = "macos")]
static BLUETOOTH_CANDIDATE_QUALIFICATION_GATE: tokio::sync::Mutex<()> =
    tokio::sync::Mutex::const_new(());

const CANCELLATION_REQUESTED: u8 = 1 << 0;
#[cfg(any(target_os = "macos", test))]
const MCP_OPERATION_STARTED: u8 = 1 << 1;
const OPERATION_FRESH: u8 = 0;
const OPERATION_RUNNING: u8 = 1;
const OPERATION_FINISHED: u8 = 2;

pub(crate) fn is_exact_bluetooth_address(address: &str) -> bool {
    let bytes = address.as_bytes();
    if bytes.len() != 17 {
        return false;
    }
    let mut separator = None;
    for (index, byte) in bytes.iter().copied().enumerate() {
        if index % 3 == 2 {
            if byte != b'-' && byte != b':' {
                return false;
            }
            if let Some(expected) = separator {
                if byte != expected {
                    return false;
                }
            } else {
                separator = Some(byte);
            }
        } else if !byte.is_ascii_hexdigit() {
            return false;
        }
    }
    true
}

pub(crate) fn canonicalize_bluetooth_address(address: &str) -> Option<String> {
    if !is_exact_bluetooth_address(address) {
        return None;
    }
    Some(
        address
            .bytes()
            .map(|byte| match byte {
                b':' | b'-' => '-',
                hexadecimal => char::from(hexadecimal.to_ascii_uppercase()),
            })
            .collect(),
    )
}

#[derive(Debug, Default)]
pub(crate) struct RecoveryCancellation {
    state: AtomicU8,
    notification: tokio::sync::Notify,
    #[cfg(target_os = "macos")]
    bluetooth_open: BluetoothOpenCancellation,
}

impl RecoveryCancellation {
    pub(crate) fn request(&self) {
        #[cfg(target_os = "macos")]
        self.bluetooth_open.cancel();
        let previous = self
            .state
            .fetch_or(CANCELLATION_REQUESTED, Ordering::AcqRel);
        if previous & CANCELLATION_REQUESTED == 0 {
            // `notify_one` retains a permit when the run future is between its
            // atomic check and registering its single cancellation waiter.
            self.notification.notify_one();
        }
    }

    #[cfg(target_os = "macos")]
    pub(crate) fn bluetooth_open_cancellation(&self) -> BluetoothOpenCancellation {
        self.bluetooth_open.clone()
    }

    pub(crate) fn check(&self) -> Result<(), DvGatewayRecoveryError> {
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
    pub(crate) async fn cancelled(&self) {
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
#[derive(Debug)]
pub(crate) struct QualifiedBluetoothCandidate {
    pub(crate) candidate: PairedBluetoothCandidate,
    pub(crate) serial_number: SerialNumber,
}

#[cfg(target_os = "macos")]
#[derive(Debug)]
pub(crate) struct BluetoothCandidateScan {
    /// Complete paired-device snapshot used to derive the unhinted page.
    ///
    /// This remains empty only when cancellation interrupted enumeration
    /// before a snapshot was available.
    pub(crate) paired_candidates: Vec<PairedBluetoothCandidate>,
    pub(crate) qualified: Vec<QualifiedBluetoothCandidate>,
    pub(crate) completed_probe_addresses: Vec<String>,
    pub(crate) current_completed_probe_addresses: Vec<String>,
    pub(crate) completed_probe_count: usize,
    pub(crate) total_unhinted_candidate_count: usize,
    pub(crate) is_complete: bool,
    pub(crate) was_cancelled: bool,
    pub(crate) has_inventory_snapshot: bool,
}

#[cfg(target_os = "macos")]
pub(crate) fn transport_error_detail(error: &TransportError) -> String {
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
pub(crate) fn bundled_bluetooth_helper_executable()
-> Result<std::path::PathBuf, DvGatewayRecoveryError> {
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
    match error {
        TransportError::BluetoothOpenInterrupted => DvGatewayRecoveryError::Cancelled,
        _other => DvGatewayRecoveryError::BluetoothUnavailable {
            detail: transport_error_detail(error),
        },
    }
}

/// Launch and validate the embedded Bluetooth recovery helper without opening
/// a radio, consulting paired devices, or changing any setting.
///
/// On macOS this exercises the signed sandbox-inheriting helper's private
/// sentinel dispatch, readiness handshake, bidirectional pipe framing, and
/// clean exit using a fixed no-radio echo. Bluetooth authorization, discovery,
/// candidate qualification, and radio I/O remain separate foreground product
/// operations.
///
/// # Errors
///
/// Returns [`DvGatewayRecoveryError::UnsupportedPlatform`] outside macOS, or
/// [`DvGatewayRecoveryError::BluetoothUnavailable`] when the embedded helper
/// is absent, cannot launch under the host sandbox, times out, or returns an
/// invalid echo.
#[uniffi::export(async_runtime = "tokio")]
pub async fn validate_bluetooth_recovery_helper() -> Result<(), DvGatewayRecoveryError> {
    #[cfg(target_os = "macos")]
    {
        let helper_executable = bundled_bluetooth_helper_executable()?;
        let validation_guard = BLUETOOTH_HELPER_ENUMERATION_GATE.lock().await;
        tokio::task::spawn_blocking(move || {
            // Keep the guard until the native child is confirmed reaped, even
            // if the async caller drops its waiter.
            let _validation_guard = validation_guard;
            BluetoothTransport::validate_helper_launch_with_executable(helper_executable)
        })
        .await
        .map_err(|error| map_helper_task_failure(&error))?
        .map_err(|error| map_transport_failure(&error))
    }

    #[cfg(not(target_os = "macos"))]
    {
        Err(DvGatewayRecoveryError::UnsupportedPlatform)
    }
}

#[cfg(target_os = "macos")]
pub(crate) async fn enumerate_bluetooth_candidates(
    helper_executable: std::path::PathBuf,
) -> Result<Vec<PairedBluetoothCandidate>, DvGatewayRecoveryError> {
    let enumeration_guard = BLUETOOTH_HELPER_ENUMERATION_GATE.lock().await;
    enumerate_bluetooth_candidates_with_guard(
        helper_executable,
        enumeration_guard,
        BluetoothOpenCancellation::default(),
    )
    .await
}

#[cfg(target_os = "macos")]
async fn enumerate_bluetooth_candidates_with_guard(
    helper_executable: std::path::PathBuf,
    enumeration_guard: tokio::sync::MutexGuard<'static, ()>,
    open_cancellation: BluetoothOpenCancellation,
) -> Result<Vec<PairedBluetoothCandidate>, DvGatewayRecoveryError> {
    tokio::task::spawn_blocking(move || {
        // Keep the guard inside the blocking closure. Dropping the async
        // waiter must not admit another helper before this process exits.
        let _enumeration_guard = enumeration_guard;
        BluetoothTransport::paired_spp_candidates_with_helper_executable_cancellable(
            helper_executable,
            &open_cancellation,
        )
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
    let result = enumerate_bluetooth_candidates_with_guard(
        helper_executable,
        acquire_bluetooth_helper_enumeration_gate(cancellation).await?,
        cancellation.bluetooth_open_cancellation(),
    )
    .await;
    cancellation.check()?;
    result
}

#[cfg(target_os = "macos")]
async fn acquire_bluetooth_helper_enumeration_gate(
    cancellation: &RecoveryCancellation,
) -> Result<tokio::sync::MutexGuard<'static, ()>, DvGatewayRecoveryError> {
    cancellation.check()?;
    let guard = tokio::select! {
        biased;
        () = cancellation.cancelled() => return Err(DvGatewayRecoveryError::Cancelled),
        guard = BLUETOOTH_HELPER_ENUMERATION_GATE.lock() => guard,
    };
    cancellation.check().map(|()| guard)
}

#[cfg(target_os = "macos")]
async fn open_exact_bluetooth_candidate(
    candidate: PairedBluetoothCandidate,
    helper_executable: std::path::PathBuf,
    cancellation: &RecoveryCancellation,
) -> Result<Option<BluetoothTransport>, DvGatewayRecoveryError> {
    let open_cancellation = cancellation.bluetooth_open_cancellation();
    let task_result = tokio::task::spawn_blocking(move || {
        BluetoothTransport::open_paired_candidate_with_helper_executable_cancellable(
            &candidate,
            helper_executable,
            &open_cancellation,
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
    let open_cancellation = cancellation.bluetooth_open_cancellation();
    let task_result = tokio::task::spawn_blocking(move || {
        BluetoothTransport::probe_paired_candidate_with_helper_executable_cancellable(
            &candidate,
            helper_executable,
            &open_cancellation,
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

    let connection = Radio::connect_with_tnc_exit(transport);
    tokio::pin!(connection);
    let connected = tokio::select! {
        biased;
        () = cancellation.cancelled() => None,
        result = &mut connection => Some(result),
    };
    let Some(connected) = connected else {
        return Err(DvGatewayRecoveryError::Cancelled);
    };
    let mut radio = match connected {
        Ok(radio) => radio,
        Err(error) => return Ok(BluetoothCandidateProbe::IdentityFailed(error.to_string())),
    };
    let identity = query_bluetooth_candidate_identity(&mut radio, cancellation).await;
    drop(radio.disconnect().await);
    identity
}

#[cfg(target_os = "macos")]
async fn query_bluetooth_candidate_identity<T: Transport>(
    radio: &mut Radio<T>,
    cancellation: &RecoveryCancellation,
) -> Result<BluetoothCandidateProbe, DvGatewayRecoveryError> {
    let model_identity = {
        let query = radio.identify();
        tokio::pin!(query);
        tokio::select! {
            biased;
            () = cancellation.cancelled() => None,
            result = &mut query => Some(result),
        }
    };
    let Some(model_identity) = model_identity else {
        return Err(DvGatewayRecoveryError::Cancelled);
    };
    if let Err(error) = model_identity {
        return Ok(BluetoothCandidateProbe::IdentityFailed(format!(
            "exact CAT ID failed: {error}"
        )));
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
    let Some(identity) = identity else {
        return Err(DvGatewayRecoveryError::Cancelled);
    };
    cancellation.check()?;
    Ok(match identity {
        Ok(information) => BluetoothCandidateProbe::Identified(information.into_parts().0),
        Err(error) => BluetoothCandidateProbe::IdentityFailed(format!("CAT AE failed: {error}")),
    })
}

#[cfg(target_os = "macos")]
async fn acquire_bluetooth_candidate_qualification_gate(
    cancellation: &RecoveryCancellation,
) -> Result<tokio::sync::MutexGuard<'static, ()>, DvGatewayRecoveryError> {
    cancellation.check()?;
    let guard = tokio::select! {
        biased;
        () = cancellation.cancelled() => return Err(DvGatewayRecoveryError::Cancelled),
        guard = BLUETOOTH_CANDIDATE_QUALIFICATION_GATE.lock() => guard,
    };
    cancellation.check().map(|()| guard)
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
    let _qualification_guard = acquire_bluetooth_candidate_qualification_gate(cancellation).await?;
    let deadline = std::time::Instant::now() + BLUETOOTH_CANDIDATE_PROBE_WINDOW;
    let mut attempted = 0_usize;
    let mut nonmatching = Vec::new();
    let mut identity_failures = Vec::new();
    let mut stopped_reason = None;

    let prioritized = candidates
        .iter()
        .filter(|candidate| candidate.is_thd75_candidate())
        .chain(
            candidates
                .iter()
                .filter(|candidate| !candidate.is_thd75_candidate()),
        )
        .take(MAX_BLUETOOTH_CANDIDATE_PROBES);
    for candidate in prioritized {
        cancellation.check()?;
        let remaining = deadline.saturating_duration_since(std::time::Instant::now());
        if !bluetooth_candidate_probe_fits(remaining) {
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

    if stopped_reason.is_none() && attempted < candidates.len() {
        stopped_reason = Some(format!(
            "the {MAX_BLUETOOTH_CANDIDATE_PROBES}-candidate qualification cap was reached after prioritizing cached-SPP and D75-name evidence"
        ));
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
pub(crate) async fn scan_unhinted_bluetooth_candidates(
    helper_executable: std::path::PathBuf,
    previously_completed_probe_addresses: &[String],
    cancellation: &RecoveryCancellation,
) -> Result<BluetoothCandidateScan, DvGatewayRecoveryError> {
    let candidates =
        match enumerate_bluetooth_candidates_cancellable(helper_executable.clone(), cancellation)
            .await
        {
            Ok(candidates) => candidates,
            Err(DvGatewayRecoveryError::Cancelled) => {
                return Ok(cancelled_bluetooth_candidate_scan_without_snapshot(
                    previously_completed_probe_addresses,
                ));
            }
            Err(error) => return Err(error),
        };
    let unhinted = candidates
        .iter()
        .filter(|candidate| !candidate.is_thd75_candidate())
        .cloned()
        .collect::<Vec<_>>();
    let mut scan = scan_unhinted_bluetooth_snapshot(
        unhinted,
        helper_executable,
        previously_completed_probe_addresses,
        cancellation,
    )
    .await?;
    scan.paired_candidates = candidates;
    Ok(scan)
}

#[cfg(target_os = "macos")]
async fn scan_unhinted_bluetooth_snapshot(
    unhinted: Vec<PairedBluetoothCandidate>,
    helper_executable: std::path::PathBuf,
    previously_completed_probe_addresses: &[String],
    cancellation: &RecoveryCancellation,
) -> Result<BluetoothCandidateScan, DvGatewayRecoveryError> {
    let total_unhinted_candidate_count = unhinted.len();
    let current_previous_completed_probe_addresses = retained_completed_probe_addresses(
        &unhinted,
        previously_completed_probe_addresses,
        PairedBluetoothCandidate::address,
    );
    if unhinted.is_empty() {
        return Ok(BluetoothCandidateScan {
            paired_candidates: Vec::new(),
            qualified: Vec::new(),
            completed_probe_addresses: Vec::new(),
            current_completed_probe_addresses: Vec::new(),
            completed_probe_count: 0,
            total_unhinted_candidate_count,
            is_complete: true,
            was_cancelled: false,
            has_inventory_snapshot: true,
        });
    }

    let _qualification_guard =
        match acquire_bluetooth_candidate_qualification_gate(cancellation).await {
            Ok(guard) => guard,
            Err(DvGatewayRecoveryError::Cancelled) => {
                return Ok(cancelled_bluetooth_candidate_scan_with_snapshot(
                    total_unhinted_candidate_count,
                    current_previous_completed_probe_addresses,
                ));
            }
            Err(error) => return Err(error),
        };
    let page = bounded_incomplete_probe_page(
        &unhinted,
        previously_completed_probe_addresses,
        PairedBluetoothCandidate::address,
    );
    let remaining_candidate_count = total_unhinted_candidate_count
        .saturating_sub(current_previous_completed_probe_addresses.len());
    let deadline = std::time::Instant::now() + BLUETOOTH_CANDIDATE_PROBE_WINDOW;
    let mut qualified = Vec::new();
    let mut completed_probe_addresses = Vec::new();
    let mut was_cancelled = false;
    for candidate in page {
        if cancellation.check().is_err() {
            was_cancelled = true;
            break;
        }
        let remaining = deadline.saturating_duration_since(std::time::Instant::now());
        if !bluetooth_candidate_probe_fits(remaining) {
            break;
        }
        let probe =
            match probe_bluetooth_candidate_identity(candidate, &helper_executable, cancellation)
                .await
            {
                Ok(probe) => probe,
                Err(DvGatewayRecoveryError::Cancelled) => {
                    was_cancelled = true;
                    break;
                }
                Err(error) => return Err(error),
            };
        completed_probe_addresses.push(candidate.address().to_owned());
        if let BluetoothCandidateProbe::Identified(serial_number) = probe {
            qualified.push(QualifiedBluetoothCandidate {
                candidate: candidate.clone(),
                serial_number,
            });
        }
        if std::time::Instant::now() > deadline {
            break;
        }
    }
    let completed_probe_count = completed_probe_addresses.len();
    let mut current_completed_probe_addresses = current_previous_completed_probe_addresses;
    current_completed_probe_addresses.extend(completed_probe_addresses.iter().cloned());
    current_completed_probe_addresses.sort_unstable();
    current_completed_probe_addresses.dedup();

    Ok(BluetoothCandidateScan {
        paired_candidates: Vec::new(),
        qualified,
        completed_probe_addresses,
        current_completed_probe_addresses,
        completed_probe_count,
        total_unhinted_candidate_count,
        is_complete: !was_cancelled && completed_probe_count == remaining_candidate_count,
        was_cancelled,
        has_inventory_snapshot: true,
    })
}

#[cfg(target_os = "macos")]
fn cancelled_bluetooth_candidate_scan_without_snapshot(
    previously_completed_probe_addresses: &[String],
) -> BluetoothCandidateScan {
    BluetoothCandidateScan {
        paired_candidates: Vec::new(),
        qualified: Vec::new(),
        completed_probe_addresses: Vec::new(),
        current_completed_probe_addresses: previously_completed_probe_addresses.to_vec(),
        completed_probe_count: 0,
        total_unhinted_candidate_count: 0,
        is_complete: false,
        was_cancelled: true,
        has_inventory_snapshot: false,
    }
}

#[cfg(target_os = "macos")]
fn cancelled_bluetooth_candidate_scan_with_snapshot(
    total_unhinted_candidate_count: usize,
    current_completed_probe_addresses: Vec<String>,
) -> BluetoothCandidateScan {
    BluetoothCandidateScan {
        paired_candidates: Vec::new(),
        qualified: Vec::new(),
        completed_probe_addresses: Vec::new(),
        current_completed_probe_addresses,
        completed_probe_count: 0,
        total_unhinted_candidate_count,
        is_complete: false,
        was_cancelled: true,
        has_inventory_snapshot: true,
    }
}

#[cfg(target_os = "macos")]
fn bounded_incomplete_probe_page<'candidate, T, Address>(
    candidates: &'candidate [T],
    previously_completed_probe_addresses: &[String],
    address: Address,
) -> Vec<&'candidate T>
where
    Address: for<'item> Fn(&'item T) -> &'item str,
{
    let previously_completed = previously_completed_probe_addresses
        .iter()
        .map(String::as_str)
        .collect::<std::collections::BTreeSet<_>>();
    let mut page = candidates.iter().collect::<Vec<_>>();
    page.sort_by(|left, right| address(left).cmp(address(right)));
    page.into_iter()
        .filter(|candidate| !previously_completed.contains(address(candidate)))
        .take(MAX_BLUETOOTH_CANDIDATE_PROBES)
        .collect()
}

#[cfg(target_os = "macos")]
fn retained_completed_probe_addresses<T, Address>(
    candidates: &[T],
    previously_completed_probe_addresses: &[String],
    address: Address,
) -> Vec<String>
where
    Address: for<'candidate> Fn(&'candidate T) -> &'candidate str,
{
    let current_addresses = candidates
        .iter()
        .map(address)
        .collect::<std::collections::BTreeSet<_>>();
    let mut retained = previously_completed_probe_addresses
        .iter()
        .filter(|candidate| current_addresses.contains(candidate.as_str()))
        .cloned()
        .collect::<Vec<_>>();
    retained.sort_unstable();
    retained.dedup();
    retained
}

#[cfg(target_os = "macos")]
#[derive(Debug)]
pub(crate) struct OpenedBluetoothSelection {
    pub(crate) transport: BluetoothTransport,
    pub(crate) exact_address: Option<String>,
}

#[cfg(target_os = "macos")]
pub(crate) async fn open_selected_bluetooth_transport(
    helper_executable: std::path::PathBuf,
    bluetooth_address: Option<String>,
    expected: &SerialNumber,
    cancellation: &RecoveryCancellation,
) -> Result<OpenedBluetoothSelection, DvGatewayRecoveryError> {
    if let Some(address) = bluetooth_address {
        let explicit_helper = helper_executable.clone();
        let open_cancellation = cancellation.bluetooth_open_cancellation();
        let selected_address = address.clone();
        let explicit_open = tokio::task::spawn_blocking(move || {
            BluetoothTransport::open_with_helper_executable_cancellable(
                Some(&selected_address),
                explicit_helper,
                &open_cancellation,
            )
        })
        .await;
        cancellation.check()?;
        let transport = explicit_open
            .map_err(|error| map_helper_task_failure(&error))?
            .map_err(|error| map_transport_failure(&error))?;
        return Ok(OpenedBluetoothSelection {
            transport,
            exact_address: Some(address),
        });
    }

    // With no explicit selection, never trust whichever paired device happens
    // to carry the factory default name. Enumerate the bounded snapshot and
    // qualify candidates by the exact serial learned from USB before opening
    // the selected address for the setting operation.
    let candidates =
        enumerate_bluetooth_candidates_cancellable(helper_executable.clone(), cancellation).await?;
    let selected = select_matching_bluetooth_candidate(
        &candidates,
        expected,
        &helper_executable,
        cancellation,
    )
    .await?;
    let exact_address = selected.address().to_owned();
    let transport = open_exact_bluetooth_candidate(selected, helper_executable, cancellation)
        .await?
        .ok_or_else(|| DvGatewayRecoveryError::BluetoothUnavailable {
            detail: "the serial-matched Bluetooth radio became unavailable before the selected Bluetooth handoff completed; bring the radio within range and retry"
                .to_owned(),
        })?;
    Ok(OpenedBluetoothSelection {
        transport,
        exact_address: Some(exact_address),
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

/// Optional exact paired endpoint for a Menu 650 recovery attempt.
#[derive(Debug, Clone, PartialEq, Eq, uniffi::Enum)]
pub enum BluetoothRecoverySelector {
    /// Open one exact address and then re-prove the USB radio serial.
    ExactAddress {
        /// Six hexadecimal octets separated consistently by `-` or `:`.
        address: String,
    },
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
    /// An explicit Bluetooth selector was not an exact address.
    #[error("Bluetooth address must contain six two-digit hexadecimal octets: {address}")]
    InvalidBluetoothAddress {
        /// Rejected selector.
        address: String,
    },
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
    bluetooth_address: Option<String>,
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
            .field("bluetooth_address", &self.bluetooth_address)
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
    /// `bluetooth_selector` may identify one paired device by exact address.
    /// Passing `None` performs bounded paired-device enumeration and exact CAT
    /// serial matching.
    ///
    /// # Errors
    ///
    /// Returns [`DvGatewayRecoveryError::InvalidBluetoothAddress`] when the
    /// explicit selector is not an exact six-octet Bluetooth address.
    #[uniffi::constructor]
    pub fn new(
        expected_radio_serial_number: String,
        bluetooth_selector: Option<BluetoothRecoverySelector>,
    ) -> Result<Arc<Self>, DvGatewayRecoveryError> {
        let bluetooth_address = bluetooth_selector
            .map(|selector| match selector {
                BluetoothRecoverySelector::ExactAddress { address } => {
                    canonicalize_bluetooth_address(&address)
                        .ok_or(DvGatewayRecoveryError::InvalidBluetoothAddress { address })
                }
            })
            .transpose()?;
        Ok(Arc::new(Self {
            expected_radio_serial_number,
            bluetooth_address,
            cancellation: RecoveryCancellation::default(),
            run_state: AtomicU8::new(OPERATION_FRESH),
        }))
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
                self.bluetooth_address.clone(),
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
    bluetooth_address: Option<String>,
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
    let selection = open_selected_bluetooth_transport(
        helper_executable,
        bluetooth_address,
        &expected_serial,
        cancellation,
    )
    .await?;

    // A prior client may have left this independently selected SPP endpoint in
    // transient KISS or MMDVM mode after the candidate probe disconnected.
    // Recover the CAT boundary again before trusting AE or crossing the MCP
    // mutation gate. The owned future is cancellation-selected so dropping it
    // also drops the helper-backed transport during preamble sleeps or drain.
    let connection = Radio::connect_with_tnc_exit(selection.transport);
    tokio::pin!(connection);
    let connected = tokio::select! {
        biased;
        () = cancellation.cancelled() => None,
        result = &mut connection => Some(result),
    };
    let Some(connected) = connected else {
        return Err(DvGatewayRecoveryError::Cancelled);
    };
    let mut radio = connected.map_err(|error| DvGatewayRecoveryError::RadioOperation {
        detail: format!("Bluetooth packet-mode recovery failed: {error}"),
    })?;
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
        let operation =
            DvGatewayRecoveryOperation::new("not-a-valid-radio-serial".to_owned(), None)?;
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

    #[test]
    fn recovery_selector_requires_and_canonicalizes_an_exact_address()
    -> Result<(), Box<dyn std::error::Error>> {
        let operation = DvGatewayRecoveryOperation::new(
            "C3C10368".to_owned(),
            Some(BluetoothRecoverySelector::ExactAddress {
                address: "40:f3:b0:ae:1c:95".to_owned(),
            }),
        )?;
        assert_eq!(
            operation.bluetooth_address.as_deref(),
            Some("40-F3-B0-AE-1C-95")
        );

        assert!(matches!(
            DvGatewayRecoveryOperation::new(
                "C3C10368".to_owned(),
                Some(BluetoothRecoverySelector::ExactAddress {
                    address: "TH-D75".to_owned(),
                }),
            ),
            Err(DvGatewayRecoveryError::InvalidBluetoothAddress { .. })
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
    async fn short_helper_enumerations_share_one_gate() -> Result<(), Box<dyn std::error::Error>> {
        let first = BLUETOOTH_HELPER_ENUMERATION_GATE.lock().await;
        let (started_sender, started_receiver) = tokio::sync::oneshot::channel();
        let (acquired_sender, mut acquired_receiver) = tokio::sync::oneshot::channel();
        let waiter = tokio::spawn(async move {
            let _started_result = started_sender.send(());
            let _second = BLUETOOTH_HELPER_ENUMERATION_GATE.lock().await;
            let _acquired_result = acquired_sender.send(());
        });

        started_receiver.await?;
        assert!(matches!(
            acquired_receiver.try_recv(),
            Err(tokio::sync::oneshot::error::TryRecvError::Empty)
        ));
        drop(first);
        tokio::time::timeout(std::time::Duration::from_secs(1), acquired_receiver).await??;
        waiter.await?;
        Ok(())
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn candidate_probe_admission_keeps_the_complete_recovery_budget() {
        assert!(!bluetooth_candidate_probe_fits(
            std::time::Duration::from_secs(59)
        ));
        assert!(bluetooth_candidate_probe_fits(
            BLUETOOTH_CANDIDATE_PROBE_RESERVE
        ));
        assert!(BLUETOOTH_CANDIDATE_PROBE_RESERVE < BLUETOOTH_CANDIDATE_PROBE_WINDOW);
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn custom_candidate_pages_advance_past_eight_in_stable_address_order()
    -> Result<(), Box<dyn std::error::Error>> {
        let candidates = (0_u8..10)
            .rev()
            .map(|suffix| {
                let colon_form = format!("40:f3:b0:ae:1c:{suffix:02x}");
                canonicalize_bluetooth_address(&colon_form)
                    .ok_or_else(|| format!("test address was invalid: {colon_form}"))
            })
            .collect::<Result<Vec<_>, _>>()?;
        let first = bounded_incomplete_probe_page(&candidates, &[], String::as_str);
        let first_addresses = first
            .iter()
            .map(|address| (*address).clone())
            .collect::<Vec<_>>();
        assert_eq!(first_addresses.len(), MAX_BLUETOOTH_CANDIDATE_PROBES);
        assert_eq!(
            first_addresses.first().map(String::as_str),
            Some("40-F3-B0-AE-1C-00")
        );
        assert_eq!(
            first_addresses.last().map(String::as_str),
            Some("40-F3-B0-AE-1C-07")
        );

        let second = bounded_incomplete_probe_page(&candidates, &first_addresses, String::as_str);
        let second_addresses = second
            .iter()
            .map(|address| address.as_str())
            .collect::<Vec<_>>();
        assert_eq!(
            second_addresses,
            vec!["40-F3-B0-AE-1C-08", "40-F3-B0-AE-1C-09"]
        );

        // Before the second pass, -00 was unpaired and -01 gained a native
        // radio hint, so neither remains in the unhinted scan inventory.
        let changed_inventory = candidates
            .iter()
            .filter(|address| !address.ends_with("-00") && !address.ends_with("-01"))
            .cloned()
            .collect::<Vec<_>>();
        let retained = retained_completed_probe_addresses(
            &changed_inventory,
            &first_addresses,
            String::as_str,
        );
        assert_eq!(
            retained,
            vec![
                "40-F3-B0-AE-1C-02",
                "40-F3-B0-AE-1C-03",
                "40-F3-B0-AE-1C-04",
                "40-F3-B0-AE-1C-05",
                "40-F3-B0-AE-1C-06",
                "40-F3-B0-AE-1C-07",
            ]
        );
        let changed_second =
            bounded_incomplete_probe_page(&changed_inventory, &first_addresses, String::as_str);
        let changed_second_addresses = changed_second
            .iter()
            .map(|address| address.as_str())
            .collect::<Vec<_>>();
        assert_eq!(
            changed_second_addresses,
            vec!["40-F3-B0-AE-1C-08", "40-F3-B0-AE-1C-09"]
        );
        Ok(())
    }

    #[cfg(target_os = "macos")]
    #[tokio::test]
    async fn cancelled_recovery_does_not_wait_for_or_launch_queued_enumeration()
    -> Result<(), Box<dyn std::error::Error>> {
        let first = BLUETOOTH_HELPER_ENUMERATION_GATE.lock().await;
        let cancellation = RecoveryCancellation::default();
        let operation = async {
            let enumeration = enumerate_bluetooth_candidates_cancellable(
                std::path::PathBuf::from("unused-helper-path"),
                &cancellation,
            );
            let cancel = async {
                tokio::task::yield_now().await;
                cancellation.request();
            };
            let (result, ()) = tokio::join!(enumeration, cancel);
            result
        };

        let result = tokio::time::timeout(std::time::Duration::from_secs(1), operation).await?;
        assert!(matches!(result, Err(DvGatewayRecoveryError::Cancelled)));
        drop(first);
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
    async fn custom_candidate_proof_requires_exact_thd75_id_before_ae()
    -> Result<(), Box<dyn std::error::Error>> {
        let cancellation = RecoveryCancellation::default();

        let mut exact_transport = MockTransport::new();
        exact_transport.expect(b"ID\r", b"ID TH-D75\r");
        exact_transport.expect(b"AE\r", b"AE C3C10368,K01\r");
        let mut exact_radio = Radio::new(exact_transport);
        let exact = query_bluetooth_candidate_identity(&mut exact_radio, &cancellation).await?;
        assert!(matches!(
            exact,
            BluetoothCandidateProbe::Identified(serial) if serial.as_str() == "C3C10368"
        ));

        let mut other_transport = MockTransport::new();
        other_transport.expect(b"ID\r", b"ID TH-D74\r");
        let mut other_radio = Radio::new(other_transport);
        let other = query_bluetooth_candidate_identity(&mut other_radio, &cancellation).await?;
        assert!(matches!(
            other,
            BluetoothCandidateProbe::IdentityFailed(detail)
                if detail.contains("exact CAT ID failed")
        ));
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
