//! Pre-automation recovery from a USB interface that answers as MMDVM.

use std::sync::{
    Arc,
    atomic::{AtomicU8, Ordering},
};

#[cfg(target_os = "macos")]
use kenwood_thd75::{
    PairedBluetoothDevice,
    error::TransportError,
    transport::{BluetoothOpenCancellation, BluetoothTransport},
};
use kenwood_thd75::{
    Radio,
    error::Error as RadioError,
    radio::programming::DetachedMcpPageUpdate,
    transport::Transport,
    types::{PcOutputInterface, SerialNumber},
};

use crate::aprs::TncDataBand;
use crate::transport::{ByteTransport, SwiftByteTransport};

#[cfg(target_os = "macos")]
const BLUETOOTH_HELPER_EXECUTABLE: &str = "AzimuthBluetoothHelper";

/// Paired-device qualification must fit inside this complete wall-clock budget.
#[cfg(target_os = "macos")]
const BLUETOOTH_DEVICE_PROBE_WINDOW: std::time::Duration = std::time::Duration::from_secs(100);

/// One probe can use a 22-second native open, followed by 800ms of packet-exit
/// delays, up to five seconds of residue drain, one five-second CAT command,
/// and bounded teardown. Sixty seconds admits that complete operation with
/// margin; do not begin another device when less remains.
#[cfg(target_os = "macos")]
const BLUETOOTH_DEVICE_PROBE_RESERVE: std::time::Duration = std::time::Duration::from_secs(60);

/// Independent count ceiling below the signed helper's framing limit.
#[cfg(target_os = "macos")]
const MAX_BLUETOOTH_DEVICE_PROBES: usize = 8;

#[cfg(target_os = "macos")]
fn bluetooth_device_probe_fits(remaining: std::time::Duration) -> bool {
    remaining >= BLUETOOTH_DEVICE_PROBE_RESERVE
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
/// sequential open/ID/AE/close operations used by serial matching. The lower
/// process lease still makes a scan fail
/// closed when a normal Bluetooth link already owns the radio.
#[cfg(target_os = "macos")]
static BLUETOOTH_DEVICE_QUALIFICATION_GATE: tokio::sync::Mutex<()> =
    tokio::sync::Mutex::const_new(());

const CANCELLATION_REQUESTED: u8 = 1 << 0;
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
enum BluetoothDeviceProbe {
    Unavailable,
    Identified(SerialNumber),
    IdentityFailed(String),
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
/// paired-device qualification and radio I/O remain separate foreground product
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
pub(crate) async fn enumerate_paired_bluetooth_devices(
    helper_executable: std::path::PathBuf,
) -> Result<Vec<PairedBluetoothDevice>, DvGatewayRecoveryError> {
    let enumeration_guard = BLUETOOTH_HELPER_ENUMERATION_GATE.lock().await;
    enumerate_paired_bluetooth_devices_with_guard(
        helper_executable,
        enumeration_guard,
        BluetoothOpenCancellation::default(),
    )
    .await
}

#[cfg(target_os = "macos")]
async fn enumerate_paired_bluetooth_devices_with_guard(
    helper_executable: std::path::PathBuf,
    enumeration_guard: tokio::sync::MutexGuard<'static, ()>,
    open_cancellation: BluetoothOpenCancellation,
) -> Result<Vec<PairedBluetoothDevice>, DvGatewayRecoveryError> {
    tokio::task::spawn_blocking(move || {
        // Keep the guard inside the blocking closure. Dropping the async
        // waiter must not admit another helper before this process exits.
        let _enumeration_guard = enumeration_guard;
        BluetoothTransport::paired_devices_with_helper_executable_cancellable(
            helper_executable,
            &open_cancellation,
        )
    })
    .await
    .map_err(|error| map_helper_task_failure(&error))?
    .map_err(|error| map_transport_failure(&error))
}

#[cfg(target_os = "macos")]
async fn enumerate_paired_bluetooth_devices_cancellable(
    helper_executable: std::path::PathBuf,
    cancellation: &RecoveryCancellation,
) -> Result<Vec<PairedBluetoothDevice>, DvGatewayRecoveryError> {
    let result = enumerate_paired_bluetooth_devices_with_guard(
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
async fn open_exact_bluetooth_device(
    device: PairedBluetoothDevice,
    helper_executable: std::path::PathBuf,
    cancellation: &RecoveryCancellation,
) -> Result<Option<BluetoothTransport>, DvGatewayRecoveryError> {
    let open_cancellation = cancellation.bluetooth_open_cancellation();
    let task_result = tokio::task::spawn_blocking(move || {
        BluetoothTransport::open_paired_device_with_helper_executable_cancellable(
            &device,
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
async fn probe_exact_bluetooth_device(
    device: PairedBluetoothDevice,
    helper_executable: std::path::PathBuf,
    cancellation: &RecoveryCancellation,
) -> Result<Option<BluetoothTransport>, DvGatewayRecoveryError> {
    let open_cancellation = cancellation.bluetooth_open_cancellation();
    let task_result = tokio::task::spawn_blocking(move || {
        BluetoothTransport::probe_paired_device_with_helper_executable_cancellable(
            &device,
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
async fn probe_bluetooth_device_identity(
    device: &PairedBluetoothDevice,
    helper_executable: &std::path::Path,
    cancellation: &RecoveryCancellation,
) -> Result<BluetoothDeviceProbe, DvGatewayRecoveryError> {
    let opened =
        probe_exact_bluetooth_device(device.clone(), helper_executable.to_owned(), cancellation)
            .await?;
    let Some(transport) = opened else {
        return Ok(BluetoothDeviceProbe::Unavailable);
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
        Err(error) => return Ok(BluetoothDeviceProbe::IdentityFailed(error.to_string())),
    };
    let identity = query_bluetooth_device_identity(&mut radio, cancellation).await;
    drop(radio.disconnect().await);
    identity
}

#[cfg(target_os = "macos")]
async fn query_bluetooth_device_identity<T: Transport>(
    radio: &mut Radio<T>,
    cancellation: &RecoveryCancellation,
) -> Result<BluetoothDeviceProbe, DvGatewayRecoveryError> {
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
        return Ok(BluetoothDeviceProbe::IdentityFailed(format!(
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
        Ok(information) => BluetoothDeviceProbe::Identified(information.into_parts().0),
        Err(error) => BluetoothDeviceProbe::IdentityFailed(format!("CAT AE failed: {error}")),
    })
}

#[cfg(target_os = "macos")]
async fn acquire_bluetooth_device_qualification_gate(
    cancellation: &RecoveryCancellation,
) -> Result<tokio::sync::MutexGuard<'static, ()>, DvGatewayRecoveryError> {
    cancellation.check()?;
    let guard = tokio::select! {
        biased;
        () = cancellation.cancelled() => return Err(DvGatewayRecoveryError::Cancelled),
        guard = BLUETOOTH_DEVICE_QUALIFICATION_GATE.lock() => guard,
    };
    cancellation.check().map(|()| guard)
}

#[cfg(target_os = "macos")]
async fn select_matching_bluetooth_device(
    devices: &[PairedBluetoothDevice],
    expected: &SerialNumber,
    helper_executable: &std::path::Path,
    cancellation: &RecoveryCancellation,
) -> Result<PairedBluetoothDevice, DvGatewayRecoveryError> {
    cancellation.check()?;
    if devices.is_empty() {
        return Err(DvGatewayRecoveryError::BluetoothUnavailable {
            detail: "macOS reported no paired Bluetooth devices; pair the TH-D75 and retry"
                .to_owned(),
        });
    }
    let _qualification_guard = acquire_bluetooth_device_qualification_gate(cancellation).await?;
    let deadline = std::time::Instant::now() + BLUETOOTH_DEVICE_PROBE_WINDOW;
    let mut attempted = 0_usize;
    let mut nonmatching = Vec::new();
    let mut identity_failures = Vec::new();
    let mut stopped_reason = None;

    for device in devices.iter().take(MAX_BLUETOOTH_DEVICE_PROBES) {
        cancellation.check()?;
        let remaining = deadline.saturating_duration_since(std::time::Instant::now());
        if !bluetooth_device_probe_fits(remaining) {
            stopped_reason = Some(
                "too little time remained in the 100-second qualification window to safely begin another bounded probe"
                    .to_owned(),
            );
            break;
        }
        attempted += 1;

        match probe_bluetooth_device_identity(device, helper_executable, cancellation).await? {
            BluetoothDeviceProbe::Unavailable => {}
            BluetoothDeviceProbe::Identified(actual) => {
                if &actual == expected {
                    // CAT serial is the stable USB identity used for this
                    // recovery. Reopen this exact address and verify the same
                    // serial again before the MCP mutation gate. Unrelated
                    // paired devices cannot invalidate that positive proof.
                    return Ok(device.clone());
                }
                nonmatching.push(format!(
                    "{} ({}) reported {actual}",
                    device.display_name(),
                    device.address()
                ));
            }
            BluetoothDeviceProbe::IdentityFailed(error) => {
                identity_failures.push(format!(
                    "{} ({}) did not answer CAT AE: {error}",
                    device.display_name(),
                    device.address()
                ));
            }
        }
        if std::time::Instant::now() > deadline {
            stopped_reason = Some(
                "the 100-second Bluetooth device qualification deadline was exceeded".to_owned(),
            );
            break;
        }
    }

    if stopped_reason.is_none() && attempted < devices.len() {
        stopped_reason = Some(format!(
            "the {MAX_BLUETOOTH_DEVICE_PROBES}-device qualification cap was reached"
        ));
    }

    if let Some(reason) = stopped_reason {
        return Err(DvGatewayRecoveryError::BluetoothIdentityUnavailable {
            detail: format!(
                "Bluetooth device qualification was incomplete: tried {attempted} of {} paired devices because {reason}; no setting was changed",
                devices.len()
            ),
        });
    }

    let mut detail = format!(
        "no paired Bluetooth device proved USB radio serial {expected}; tried {attempted} of {} devices",
        devices.len()
    );
    if !nonmatching.is_empty() {
        detail.push_str("; different radios: ");
        detail.push_str(&nonmatching.join(", "));
    }
    if !identity_failures.is_empty() {
        detail.push_str("; unresolved devices: ");
        detail.push_str(&identity_failures.join(", "));
    }
    Err(DvGatewayRecoveryError::BluetoothIdentityUnavailable { detail })
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

    // With no explicit selection, never trust a paired device's display name.
    // Enumerate the bounded snapshot and qualify devices by the exact serial
    // learned from USB before opening
    // the selected address for the setting operation.
    let devices =
        enumerate_paired_bluetooth_devices_cancellable(helper_executable.clone(), cancellation)
            .await?;
    let selected =
        select_matching_bluetooth_device(&devices, expected, &helper_executable, cancellation)
            .await?;
    let exact_address = selected.address().to_owned();
    let transport = open_exact_bluetooth_device(selected, helper_executable, cancellation)
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

/// Result of routing persistent Reflector Terminal traffic to USB-C.
#[derive(Debug, Clone, Copy, PartialEq, Eq, uniffi::Enum)]
pub enum DvGatewayUsbRoutingOutcome {
    /// Menu 985 and/or Menu 650 changed and the radio is rebooting.
    ChangedRadioRebooting,
    /// Menu 985 already selected USB and Menu 650 was already Reflector Terminal.
    ///
    /// The MCP exit still resets the radio, so the caller must complete its
    /// reboot wait before accepting Bluetooth CAT.
    AlreadyRouted,
}

impl From<DetachedMcpPageUpdate> for DvGatewayUsbRoutingOutcome {
    fn from(value: DetachedMcpPageUpdate) -> Self {
        match value {
            DetachedMcpPageUpdate::ChangedRadioRebooting => Self::ChangedRadioRebooting,
            DetachedMcpPageUpdate::UnchangedCatReady => Self::AlreadyRouted,
        }
    }
}

/// Completed USB routing result and the radio identity needed for Bluetooth.
///
/// The USB descriptor is not an identity source: a conforming TH-D75 can
/// expose `iSerialNumber = 0`. `radio_serial_number` is instead the exact CAT
/// `AE` identity read from the schema-qualified radio immediately before the
/// Menu 985 / Menu 650 mutation gate. The caller must require this same serial
/// when it reopens the retained Bluetooth address after the reboot.
#[derive(Debug, Clone, PartialEq, Eq, uniffi::Record)]
pub struct DvGatewayUsbRoutingResult {
    /// Whether the verified routing transaction changed persistent settings.
    pub outcome: DvGatewayUsbRoutingOutcome,
    /// Exact radio serial returned by CAT `AE` on the qualified USB link.
    pub radio_serial_number: String,
}

/// Completed Menu 650 disable result for an already-open CAT connection.
///
/// `radio_serial_number` is the exact CAT `AE` identity read immediately
/// before the mutation gate. The caller must bind its post-reboot reconnect to
/// this serial instead of accepting an unqualified transport reopen.
#[derive(Debug, Clone, PartialEq, Eq, uniffi::Record)]
pub struct DvGatewayCatDisableResult {
    /// Whether the verified Menu 650 transaction changed persistent storage.
    pub outcome: DvGatewayRecoveryOutcome,
    /// Exact radio serial returned by CAT `AE` before the Menu 650 operation.
    pub radio_serial_number: String,
}

/// Completed caller-approved recovery for an APRS KISS current-mode refusal.
#[derive(Debug, Clone, PartialEq, Eq, uniffi::Record)]
pub struct AprsCurrentModeRecoveryResult {
    /// Whether Menu 650 changed or was already Off.
    pub outcome: DvGatewayRecoveryOutcome,
    /// Exact CAT `AE` identity proved immediately before MCP.
    pub radio_serial_number: String,
    /// Live Menu 983 value proved in the same MCP transaction as Menu 650.
    pub kiss_interface_raw_value: u8,
    /// Live Menu 506 value proved in the same MCP transaction as Menu 650.
    pub data_band: TncDataBand,
}

/// Failure of caller-approved APRS current-mode recovery.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error, uniffi::Error)]
pub enum AprsCurrentModeRecoveryError {
    /// Cancellation won before the MCP operation gate.
    #[error("APRS current-mode recovery was cancelled before any setting could be changed")]
    Cancelled,
    /// The authenticated automation actor could not accept the operation.
    #[error("the authenticated automation controller is no longer available: {detail}")]
    ControllerUnavailable {
        /// Exact actor/channel failure.
        detail: String,
    },
    /// The approved CAT identity was malformed.
    #[error("the approved CAT radio serial is invalid: {detail}")]
    InvalidExpectedRadioSerial {
        /// Exact validation failure.
        detail: String,
    },
    /// The caller supplied a Menu 983 value outside its closed 0/1 domain.
    #[error("approved Menu 983 route {value} is invalid; expected 0 (USB-C) or 1 (Bluetooth)")]
    InvalidExpectedKissInterface {
        /// Invalid raw value.
        value: u8,
    },
    /// Fresh CAT `AE` identity was unavailable before MCP.
    #[error("could not read the selected CAT radio's serial identity: {detail}")]
    CatIdentityUnavailable {
        /// CAT query detail.
        detail: String,
    },
    /// Fresh CAT identity did not match the approved actor identity.
    #[error(
        "selected CAT radio serial {actual} does not match approved radio serial {expected}; no setting was changed"
    )]
    RadioIdentityMismatch {
        /// Approved identity.
        expected: String,
        /// Fresh identity.
        actual: String,
    },
    /// Model, firmware, or MCP schema qualification failed before the gate.
    #[error("could not qualify the selected CAT radio for APRS recovery: {detail}")]
    RadioQualification {
        /// Qualification detail.
        detail: String,
    },
    /// Live Menu 983 no longer matched the route the caller approved.
    #[error(
        "Menu 983 now routes KISS to raw interface {actual}, not approved interface {expected}; no setting was changed"
    )]
    KissInterfaceMismatch {
        /// Approved raw route.
        expected: u8,
        /// Fresh raw route read inside MCP.
        actual: u8,
    },
    /// Menu 983 mismatch proved zero writes, but MCP/CAT cleanup could not be
    /// completed and the endpoint must be reopened explicitly.
    #[error(
        "Menu 983 routes KISS to raw interface {actual}, not approved interface {expected}; no setting was changed, but MCP cleanup failed: {detail}"
    )]
    KissInterfaceMismatchAndCleanupFailed {
        /// Approved raw route.
        expected: u8,
        /// Fresh raw route read inside MCP.
        actual: u8,
        /// Cleanup and recovery detail.
        detail: String,
    },
    /// Live Menu 506 was outside its strict A/B domain.
    #[error("Menu 506 has invalid raw TNC data band {actual}; no setting was changed")]
    InvalidTncDataBand {
        /// Fresh raw value read inside MCP.
        actual: u8,
    },
    /// Invalid Menu 506 proved zero writes, but MCP/CAT cleanup failed.
    #[error(
        "Menu 506 has invalid raw TNC data band {actual}; no setting was changed, but MCP cleanup failed: {detail}"
    )]
    InvalidTncDataBandAndCleanupFailed {
        /// Fresh raw value read inside MCP.
        actual: u8,
        /// Cleanup and recovery detail.
        detail: String,
    },
    /// The same-session Menu 983/Menu 506 proof or Menu 650 operation failed safely.
    #[error("could not complete APRS current-mode recovery: {detail}")]
    RadioOperation {
        /// Underlying operation detail.
        detail: String,
    },
    /// The guarded transaction proved that no persistent write started, but
    /// its operation or CAT cleanup did not complete normally.
    #[error("APRS current-mode recovery did not complete, but no setting was changed: {detail}")]
    NoSettingChanged {
        /// Exact operation, cleanup, recovery, or transport-release detail.
        detail: String,
    },
    /// Menu 983, Menu 506, and Menu 650 completed with a verified result, but
    /// releasing the consumed CAT transport failed afterward.
    #[error("APRS current-mode recovery completed, but releasing CAT failed: {detail}")]
    CompletedButReleaseFailed {
        /// Complete verified recovery result retained despite the close error.
        result: AprsCurrentModeRecoveryResult,
        /// Exact transport-release failure.
        detail: String,
    },
    /// A write may have started and neither its result nor cleanup was proved.
    #[error(
        "the APRS recovery write outcome is uncertain: {detail}; power-cycle the radio and inspect Menu 650 before retrying"
    )]
    OutcomeUncertain {
        /// Original operation and failed recovery detail.
        detail: String,
    },
}

/// Failure while disabling DV Gateway mode over an already-open CAT link.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error, uniffi::Error)]
pub enum DvGatewayCatDisableError {
    /// The operation was cancelled before the Menu 650 mutation gate.
    #[error("DV Gateway disable was cancelled before Menu 650 could be changed")]
    Cancelled,
    /// The authenticated automation actor ended before it could accept the
    /// approved operation.
    #[error("the authenticated automation controller is no longer available: {detail}")]
    ControllerUnavailable {
        /// Exact actor/channel failure.
        detail: String,
    },
    /// The serial retained from the approved CAT session is not a valid exact
    /// CAT `AE` identity.
    #[error("the approved CAT radio serial is invalid: {detail}")]
    InvalidExpectedRadioSerial {
        /// Exact validation failure.
        detail: String,
    },
    /// CAT `AE` did not return a usable radio serial.
    #[error("could not read the selected CAT radio's serial identity: {detail}")]
    CatIdentityUnavailable {
        /// CAT identity-query detail.
        detail: String,
    },
    /// The selected CAT endpoint now answers as a different physical radio
    /// than the one proved before the user approved the operation.
    #[error(
        "selected CAT radio serial {actual} does not match the approved radio serial {expected}; no setting was changed"
    )]
    RadioIdentityMismatch {
        /// CAT serial retained from the approved connected session.
        expected: String,
        /// Fresh CAT `AE` serial read immediately before the mutation gate.
        actual: String,
    },
    /// Exact model, firmware, or MCP schema qualification failed before the
    /// mutation gate.
    #[error("could not qualify the selected CAT radio for DV Gateway disable: {detail}")]
    RadioQualification {
        /// Exact qualification detail.
        detail: String,
    },
    /// The verified Menu 650 update failed after the mutation gate opened.
    #[error("could not turn Menu 650 (DV Gateway) off: {detail}")]
    RadioOperation {
        /// Underlying radio-operation detail after MCP recovery completed.
        detail: String,
    },
    /// The Menu 650 update failed and its final persistent value is uncertain.
    #[error(
        "the Menu 650 write outcome is uncertain: {detail}; power-cycle the radio and inspect Menu 650 before retrying"
    )]
    OutcomeUncertain {
        /// Original operation and failed-recovery detail.
        detail: String,
    },
}

fn check_cat_disable_cancellation(
    cancellation: &RecoveryCancellation,
) -> Result<(), DvGatewayCatDisableError> {
    cancellation
        .check()
        .map_err(|_cancelled| DvGatewayCatDisableError::Cancelled)
}

fn detached_update_has_possible_writes(error: &RadioError) -> bool {
    let RadioError::DetachedMcpPageUpdate(error) = error else {
        return false;
    };
    !error.possibly_written_pages().is_empty()
}

async fn verify_cat_disable_schema<T: Transport>(
    radio: &mut Radio<T>,
    cancellation: &RecoveryCancellation,
) -> Result<(), DvGatewayCatDisableError> {
    check_cat_disable_cancellation(cancellation)?;
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
        .ok_or(DvGatewayCatDisableError::Cancelled)?
        .map_err(|error| DvGatewayCatDisableError::RadioQualification {
            detail: error.to_string(),
        })
}

async fn verify_cat_disable_serial<T: Transport>(
    radio: &mut Radio<T>,
    cancellation: &RecoveryCancellation,
) -> Result<SerialNumber, DvGatewayCatDisableError> {
    check_cat_disable_cancellation(cancellation)?;
    let result = {
        let query = radio.get_serial_information();
        tokio::pin!(query);
        tokio::select! {
            biased;
            () = cancellation.cancelled() => None,
            result = &mut query => Some(result),
        }
    };
    Ok(result
        .ok_or(DvGatewayCatDisableError::Cancelled)?
        .map_err(|error| DvGatewayCatDisableError::CatIdentityUnavailable {
            detail: error.to_string(),
        })?
        .into_parts()
        .0)
}

pub(crate) async fn disable_dv_gateway_over_cat<T: Transport>(
    mut radio: Radio<T>,
    expected_radio_serial_number: &SerialNumber,
    cancellation: &RecoveryCancellation,
) -> Result<DvGatewayCatDisableResult, DvGatewayCatDisableError> {
    let result =
        disable_dv_gateway_over_cat_inner(&mut radio, expected_radio_serial_number, cancellation)
            .await;
    let release = radio.disconnect().await;
    match (result, release) {
        (result, Ok(())) => result,
        (Ok(_), Err(error)) => Err(DvGatewayCatDisableError::RadioOperation {
            detail: format!(
                "the Menu 650 outcome completed, but releasing the CAT transport failed: {error}"
            ),
        }),
        (Err(error), Err(release_error)) => Err(cat_disable_release_failure(error, &release_error)),
    }
}

async fn disable_dv_gateway_over_cat_inner<T: Transport>(
    radio: &mut Radio<T>,
    expected_radio_serial_number: &SerialNumber,
    cancellation: &RecoveryCancellation,
) -> Result<DvGatewayCatDisableResult, DvGatewayCatDisableError> {
    verify_cat_disable_schema(radio, cancellation).await?;
    let radio_serial_number = verify_cat_disable_serial(radio, cancellation).await?;
    if radio_serial_number != *expected_radio_serial_number {
        return Err(DvGatewayCatDisableError::RadioIdentityMismatch {
            expected: expected_radio_serial_number.to_string(),
            actual: radio_serial_number.to_string(),
        });
    }

    cancellation
        .begin_mcp_operation()
        .map_err(|_cancelled| DvGatewayCatDisableError::Cancelled)?;
    let update = match radio.disable_dv_gateway_detached_unverified().await {
        Ok(update) => update,
        Err(operation_error) => {
            let possibly_wrote = detached_update_has_possible_writes(&operation_error);
            let recovery = radio.recover_from_interrupted_mcp().await;
            return match recovery {
                Ok(()) => Err(DvGatewayCatDisableError::RadioOperation {
                    detail: operation_error.to_string(),
                }),
                Err(recovery_error) if possibly_wrote => {
                    Err(DvGatewayCatDisableError::OutcomeUncertain {
                        detail: format!(
                            "operation failed ({operation_error}); MCP recovery also failed ({recovery_error})"
                        ),
                    })
                }
                Err(recovery_error) => Err(DvGatewayCatDisableError::RadioOperation {
                    detail: format!(
                        "operation failed before any persistent write ({operation_error}); MCP recovery also failed ({recovery_error}); no setting was changed"
                    ),
                }),
            };
        }
    };

    Ok(DvGatewayCatDisableResult {
        outcome: update.into(),
        radio_serial_number: radio_serial_number.to_string(),
    })
}

fn cat_disable_release_failure(
    error: DvGatewayCatDisableError,
    release_error: &RadioError,
) -> DvGatewayCatDisableError {
    match error {
        DvGatewayCatDisableError::OutcomeUncertain { detail } => {
            DvGatewayCatDisableError::OutcomeUncertain {
                detail: format!(
                    "{detail}; releasing the CAT transport also failed: {release_error}"
                ),
            }
        }
        other => DvGatewayCatDisableError::RadioOperation {
            detail: format!("{other}; releasing the CAT transport also failed: {release_error}"),
        },
    }
}

fn aprs_kiss_interface(raw: u8) -> Result<PcOutputInterface, AprsCurrentModeRecoveryError> {
    match raw {
        0 => Ok(PcOutputInterface::Usb),
        1 => Ok(PcOutputInterface::Bluetooth),
        value => Err(AprsCurrentModeRecoveryError::InvalidExpectedKissInterface { value }),
    }
}

fn check_aprs_recovery_cancellation(
    cancellation: &RecoveryCancellation,
) -> Result<(), AprsCurrentModeRecoveryError> {
    cancellation
        .check()
        .map_err(|_cancelled| AprsCurrentModeRecoveryError::Cancelled)
}

async fn verify_aprs_recovery_schema<T: Transport>(
    radio: &mut Radio<T>,
    cancellation: &RecoveryCancellation,
) -> Result<(), AprsCurrentModeRecoveryError> {
    check_aprs_recovery_cancellation(cancellation)?;
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
        .ok_or(AprsCurrentModeRecoveryError::Cancelled)?
        .map_err(|error| AprsCurrentModeRecoveryError::RadioQualification {
            detail: error.to_string(),
        })
}

async fn verify_aprs_recovery_serial<T: Transport>(
    radio: &mut Radio<T>,
    cancellation: &RecoveryCancellation,
) -> Result<SerialNumber, AprsCurrentModeRecoveryError> {
    check_aprs_recovery_cancellation(cancellation)?;
    let result = {
        let query = radio.get_serial_information();
        tokio::pin!(query);
        tokio::select! {
            biased;
            () = cancellation.cancelled() => None,
            result = &mut query => Some(result),
        }
    };
    Ok(result
        .ok_or(AprsCurrentModeRecoveryError::Cancelled)?
        .map_err(
            |error| AprsCurrentModeRecoveryError::CatIdentityUnavailable {
                detail: error.to_string(),
            },
        )?
        .into_parts()
        .0)
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum ZeroWriteAprsRecoveryMismatch {
    KissInterfaceClean {
        expected: u8,
        actual: u8,
    },
    KissInterfaceCleanupFailed {
        expected: u8,
        actual: u8,
        cleanup: String,
    },
    InvalidTncDataBandClean {
        actual: u8,
    },
    InvalidTncDataBandCleanupFailed {
        actual: u8,
        cleanup: String,
    },
}

fn kiss_interface_mismatch(source: &RadioError) -> Option<(u8, u8)> {
    let RadioError::KissInterfaceMismatch { expected, actual } = source else {
        return None;
    };
    Some((u8::from(*expected), *actual))
}

fn invalid_tnc_data_band(source: &RadioError) -> Option<u8> {
    let RadioError::InvalidTncDataBand { actual } = source else {
        return None;
    };
    Some(*actual)
}

fn zero_write_aprs_recovery_mismatch(error: &RadioError) -> Option<ZeroWriteAprsRecoveryMismatch> {
    let RadioError::DetachedMcpPageUpdate(error) = error else {
        return None;
    };
    match error {
        kenwood_thd75::radio::programming::DetachedMcpPageUpdateError::Operation {
            possibly_written_pages,
            verified_written_pages,
            source,
        } if possibly_written_pages.is_empty() && verified_written_pages.is_empty() => {
            if let Some((expected, actual)) = kiss_interface_mismatch(source) {
                Some(ZeroWriteAprsRecoveryMismatch::KissInterfaceClean { expected, actual })
            } else {
                let actual = invalid_tnc_data_band(source)?;
                Some(ZeroWriteAprsRecoveryMismatch::InvalidTncDataBandClean { actual })
            }
        }
        kenwood_thd75::radio::programming::DetachedMcpPageUpdateError::OperationAndCleanup {
            possibly_written_pages,
            verified_written_pages,
            operation,
            cleanup,
        } if possibly_written_pages.is_empty() && verified_written_pages.is_empty() => {
            if let Some((expected, actual)) = kiss_interface_mismatch(operation) {
                Some(ZeroWriteAprsRecoveryMismatch::KissInterfaceCleanupFailed {
                    expected,
                    actual,
                    cleanup: cleanup.to_string(),
                })
            } else {
                let actual = invalid_tnc_data_band(operation)?;
                Some(
                    ZeroWriteAprsRecoveryMismatch::InvalidTncDataBandCleanupFailed {
                        actual,
                        cleanup: cleanup.to_string(),
                    },
                )
            }
        }
        _ => None,
    }
}

pub(crate) async fn recover_aprs_current_mode_over_cat<T: Transport>(
    mut radio: Radio<T>,
    expected_radio_serial_number: &SerialNumber,
    expected_kiss_interface_raw_value: u8,
    cancellation: &RecoveryCancellation,
) -> Result<AprsCurrentModeRecoveryResult, AprsCurrentModeRecoveryError> {
    let result = recover_aprs_current_mode_over_cat_inner(
        &mut radio,
        expected_radio_serial_number,
        expected_kiss_interface_raw_value,
        cancellation,
    )
    .await;
    let release = radio.disconnect().await;
    match (result, release) {
        (result, Ok(())) => result,
        (Ok(result), Err(error)) => Err(AprsCurrentModeRecoveryError::CompletedButReleaseFailed {
            result,
            detail: error.to_string(),
        }),
        (Err(error), Err(release_error)) => {
            Err(aprs_recovery_release_failure(error, &release_error))
        }
    }
}

async fn recover_aprs_current_mode_over_cat_inner<T: Transport>(
    radio: &mut Radio<T>,
    expected_radio_serial_number: &SerialNumber,
    expected_kiss_interface_raw_value: u8,
    cancellation: &RecoveryCancellation,
) -> Result<AprsCurrentModeRecoveryResult, AprsCurrentModeRecoveryError> {
    let expected_interface = aprs_kiss_interface(expected_kiss_interface_raw_value)?;
    verify_aprs_recovery_schema(radio, cancellation).await?;
    let radio_serial_number = verify_aprs_recovery_serial(radio, cancellation).await?;
    if radio_serial_number != *expected_radio_serial_number {
        return Err(AprsCurrentModeRecoveryError::RadioIdentityMismatch {
            expected: expected_radio_serial_number.to_string(),
            actual: radio_serial_number.to_string(),
        });
    }

    cancellation
        .begin_mcp_operation()
        .map_err(|_cancelled| AprsCurrentModeRecoveryError::Cancelled)?;
    let recovery_update = match radio
        .disable_dv_gateway_for_kiss_interface_detached_unverified(expected_interface)
        .await
    {
        Ok(update) => update,
        Err(operation_error) => {
            if let Some(mismatch) = zero_write_aprs_recovery_mismatch(&operation_error) {
                return match mismatch {
                    ZeroWriteAprsRecoveryMismatch::KissInterfaceClean { expected, actual } => {
                        Err(AprsCurrentModeRecoveryError::KissInterfaceMismatch {
                            expected,
                            actual,
                        })
                    }
                    ZeroWriteAprsRecoveryMismatch::KissInterfaceCleanupFailed {
                        expected,
                        actual,
                        cleanup,
                    } => match radio.recover_from_interrupted_mcp().await {
                        Ok(()) => Err(AprsCurrentModeRecoveryError::KissInterfaceMismatch {
                            expected,
                            actual,
                        }),
                        Err(recovery) => Err(
                            AprsCurrentModeRecoveryError::KissInterfaceMismatchAndCleanupFailed {
                                expected,
                                actual,
                                detail: format!(
                                    "MCP cleanup failed ({cleanup}); explicit MCP recovery also failed ({recovery})"
                                ),
                            },
                        ),
                    },
                    ZeroWriteAprsRecoveryMismatch::InvalidTncDataBandClean { actual } => {
                        Err(AprsCurrentModeRecoveryError::InvalidTncDataBand { actual })
                    }
                    ZeroWriteAprsRecoveryMismatch::InvalidTncDataBandCleanupFailed {
                        actual,
                        cleanup,
                    } => match radio.recover_from_interrupted_mcp().await {
                        Ok(()) => Err(AprsCurrentModeRecoveryError::InvalidTncDataBand { actual }),
                        Err(recovery) => Err(
                            AprsCurrentModeRecoveryError::InvalidTncDataBandAndCleanupFailed {
                                actual,
                                detail: format!(
                                    "MCP cleanup failed ({cleanup}); explicit MCP recovery also failed ({recovery})"
                                ),
                            },
                        ),
                    },
                };
            }
            let recovery = radio.recover_from_interrupted_mcp().await;
            return Err(classify_aprs_recovery_operation_failure(
                &operation_error,
                recovery,
            ));
        }
    };

    Ok(AprsCurrentModeRecoveryResult {
        outcome: recovery_update.update.into(),
        radio_serial_number: radio_serial_number.to_string(),
        kiss_interface_raw_value: expected_kiss_interface_raw_value,
        data_band: recovery_update.data_band.into(),
    })
}

fn classify_aprs_recovery_operation_failure(
    operation_error: &RadioError,
    recovery: Result<(), RadioError>,
) -> AprsCurrentModeRecoveryError {
    let possibly_wrote = detached_update_has_possible_writes(operation_error);
    match recovery {
        Ok(()) if possibly_wrote => AprsCurrentModeRecoveryError::RadioOperation {
            detail: operation_error.to_string(),
        },
        Ok(()) => AprsCurrentModeRecoveryError::NoSettingChanged {
            detail: operation_error.to_string(),
        },
        Err(recovery_error) if possibly_wrote => AprsCurrentModeRecoveryError::OutcomeUncertain {
            detail: format!(
                "operation failed ({operation_error}); MCP recovery also failed ({recovery_error})"
            ),
        },
        Err(recovery_error) => AprsCurrentModeRecoveryError::NoSettingChanged {
            detail: format!(
                "operation failed ({operation_error}); MCP recovery also failed ({recovery_error})"
            ),
        },
    }
}

fn aprs_recovery_release_failure(
    error: AprsCurrentModeRecoveryError,
    release_error: &RadioError,
) -> AprsCurrentModeRecoveryError {
    match error {
        AprsCurrentModeRecoveryError::KissInterfaceMismatch { expected, actual } => {
            AprsCurrentModeRecoveryError::KissInterfaceMismatchAndCleanupFailed {
                expected,
                actual,
                detail: format!("releasing the CAT transport failed: {release_error}"),
            }
        }
        AprsCurrentModeRecoveryError::KissInterfaceMismatchAndCleanupFailed {
            expected,
            actual,
            detail,
        } => AprsCurrentModeRecoveryError::KissInterfaceMismatchAndCleanupFailed {
            expected,
            actual,
            detail: format!("{detail}; releasing the CAT transport also failed: {release_error}"),
        },
        AprsCurrentModeRecoveryError::InvalidTncDataBand { actual } => {
            AprsCurrentModeRecoveryError::InvalidTncDataBandAndCleanupFailed {
                actual,
                detail: format!("releasing the CAT transport failed: {release_error}"),
            }
        }
        AprsCurrentModeRecoveryError::InvalidTncDataBandAndCleanupFailed { actual, detail } => {
            AprsCurrentModeRecoveryError::InvalidTncDataBandAndCleanupFailed {
                actual,
                detail: format!(
                    "{detail}; releasing the CAT transport also failed: {release_error}"
                ),
            }
        }
        AprsCurrentModeRecoveryError::OutcomeUncertain { detail } => {
            AprsCurrentModeRecoveryError::OutcomeUncertain {
                detail: format!(
                    "{detail}; releasing the CAT transport also failed: {release_error}"
                ),
            }
        }
        AprsCurrentModeRecoveryError::NoSettingChanged { detail } => {
            AprsCurrentModeRecoveryError::NoSettingChanged {
                detail: format!(
                    "{detail}; releasing the CAT transport also failed: {release_error}"
                ),
            }
        }
        error @ (AprsCurrentModeRecoveryError::Cancelled
        | AprsCurrentModeRecoveryError::ControllerUnavailable { .. }
        | AprsCurrentModeRecoveryError::InvalidExpectedRadioSerial { .. }
        | AprsCurrentModeRecoveryError::InvalidExpectedKissInterface { .. }
        | AprsCurrentModeRecoveryError::CatIdentityUnavailable { .. }
        | AprsCurrentModeRecoveryError::RadioIdentityMismatch { .. }
        | AprsCurrentModeRecoveryError::RadioQualification { .. }) => {
            AprsCurrentModeRecoveryError::NoSettingChanged {
                detail: format!(
                    "{error}; releasing the CAT transport also failed: {release_error}"
                ),
            }
        }
        other => AprsCurrentModeRecoveryError::RadioOperation {
            detail: format!("{other}; releasing the CAT transport also failed: {release_error}"),
        },
    }
}

/// Failure while routing Reflector Terminal traffic from Bluetooth to USB-C.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error, uniffi::Error)]
pub enum DvGatewayUsbRoutingError {
    /// Routing was cancelled before the Menu 985 / Menu 650 mutation gate.
    #[error("DV Gateway USB routing was cancelled before any setting could be changed")]
    Cancelled,
    /// This single-use routing object has already run.
    #[error("this DV Gateway USB routing operation has already run")]
    OperationAlreadyRun,
    /// CAT `AE` did not return a usable radio serial over the selected USB link.
    #[error("could not read the selected USB radio's CAT serial identity: {detail}")]
    UsbCatIdentityUnavailable {
        /// CAT identity-query detail.
        detail: String,
    },
    /// Exact model/firmware qualification failed before the mutation gate.
    #[error("could not qualify the selected USB radio for DV Gateway routing: {detail}")]
    RadioQualification {
        /// Exact CAT model/firmware qualification detail.
        detail: String,
    },
    /// The verified two-page update failed after the mutation gate opened.
    #[error("could not route DV Gateway to USB-C: {detail}")]
    RadioOperation {
        /// Underlying radio-operation detail after MCP recovery completed.
        detail: String,
    },
    /// A two-page update failed and the final Menu 985 / Menu 650 state is uncertain.
    #[error(
        "the Menu 985 / Menu 650 routing outcome is uncertain: {detail}; power-cycle the radio and inspect both settings before retrying"
    )]
    OutcomeUncertain {
        /// Original operation and failed-recovery detail.
        detail: String,
    },
}

/// One single-use, synchronously cancellable operation that routes persistent
/// Reflector Terminal traffic to the already-open USB-C connection.
///
/// Swift owns exact USB discovery and transport opening. This object owns the
/// radio protocol from CAT serial qualification through the verified Menu 985 /
/// Menu 650 transaction and its cleanup. It never discovers another transport.
#[derive(Debug, uniffi::Object)]
pub struct DvGatewayUsbRoutingOperation {
    transport: Arc<dyn ByteTransport>,
    cancellation: RecoveryCancellation,
    run_state: AtomicU8,
}

impl DvGatewayUsbRoutingOperation {
    fn begin_run(&self) -> Result<OperationRunGuard<'_>, DvGatewayUsbRoutingError> {
        let _previous = self
            .run_state
            .compare_exchange(
                OPERATION_FRESH,
                OPERATION_RUNNING,
                Ordering::AcqRel,
                Ordering::Acquire,
            )
            .map_err(|_| DvGatewayUsbRoutingError::OperationAlreadyRun)?;
        Ok(OperationRunGuard {
            state: &self.run_state,
        })
    }
}

#[uniffi::export(async_runtime = "tokio")]
impl DvGatewayUsbRoutingOperation {
    /// Create one routing attempt for an already-open USB radio transport.
    ///
    /// The caller must retain the exact Bluetooth endpoint which positively
    /// answered MMDVM, select and open the intended USB radio, and complete CAT
    /// preflight before constructing this object. A USB descriptor serial is
    /// deliberately neither accepted nor required; the operation obtains the
    /// authoritative radio identity from qualified CAT instead.
    #[uniffi::constructor]
    pub fn new(transport: Arc<dyn ByteTransport>) -> Arc<Self> {
        Arc::new(Self {
            transport,
            cancellation: RecoveryCancellation::default(),
            run_state: AtomicU8::new(OPERATION_FRESH),
        })
    }

    /// Request cancellation synchronously.
    ///
    /// Cancellation before the atomic mutation gate prevents all Menu 985 and
    /// Menu 650 writes. Once the gate opens, the bounded two-page operation and
    /// any required MCP recovery finish before `run` returns.
    pub fn cancel(&self) {
        self.cancellation.request();
    }

    /// Route Reflector Terminal traffic to USB-C exactly once.
    ///
    /// The operation first proves the exact supported model, firmware, and MCP
    /// schema. It then obtains the authoritative radio serial from CAT `AE`,
    /// atomically crosses the mutation gate, and applies the typed
    /// `set_reflector_terminal_mode_detached(USB)` transaction. The caller owns
    /// the reboot wait and must use the returned serial for its exact-address
    /// Bluetooth CAT identity proof.
    ///
    /// # Errors
    ///
    /// Returns [`DvGatewayUsbRoutingError::Cancelled`] only when cancellation
    /// wins before the mutation gate, or
    /// [`DvGatewayUsbRoutingError::OperationAlreadyRun`] after an earlier run.
    /// Post-gate failures distinguish completed cleanup from an uncertain radio
    /// outcome.
    pub async fn run(
        self: Arc<Self>,
    ) -> Result<DvGatewayUsbRoutingResult, DvGatewayUsbRoutingError> {
        let _run_guard = self.begin_run()?;
        check_usb_routing_cancellation(&self.cancellation)?;
        let radio = Radio::new(SwiftByteTransport::new(Arc::clone(&self.transport)));
        route_dv_gateway_to_usb(radio, &self.cancellation).await
    }
}

fn check_usb_routing_cancellation(
    cancellation: &RecoveryCancellation,
) -> Result<(), DvGatewayUsbRoutingError> {
    cancellation
        .check()
        .map_err(|_cancelled| DvGatewayUsbRoutingError::Cancelled)
}

async fn verify_usb_cat_serial<T: Transport>(
    radio: &mut Radio<T>,
    cancellation: &RecoveryCancellation,
) -> Result<SerialNumber, DvGatewayUsbRoutingError> {
    check_usb_routing_cancellation(cancellation)?;
    let result = {
        let query = radio.get_serial_information();
        tokio::pin!(query);
        tokio::select! {
            biased;
            () = cancellation.cancelled() => None,
            result = &mut query => Some(result),
        }
    };
    Ok(result
        .ok_or(DvGatewayUsbRoutingError::Cancelled)?
        .map_err(
            |error| DvGatewayUsbRoutingError::UsbCatIdentityUnavailable {
                detail: error.to_string(),
            },
        )?
        .into_parts()
        .0)
}

async fn verify_usb_routing_schema<T: Transport>(
    radio: &mut Radio<T>,
    cancellation: &RecoveryCancellation,
) -> Result<(), DvGatewayUsbRoutingError> {
    check_usb_routing_cancellation(cancellation)?;
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
        .ok_or(DvGatewayUsbRoutingError::Cancelled)?
        .map_err(|error| DvGatewayUsbRoutingError::RadioQualification {
            detail: error.to_string(),
        })
}

async fn route_dv_gateway_to_usb<T: Transport>(
    mut radio: Radio<T>,
    cancellation: &RecoveryCancellation,
) -> Result<DvGatewayUsbRoutingResult, DvGatewayUsbRoutingError> {
    verify_usb_routing_schema(&mut radio, cancellation).await?;
    let radio_serial_number = verify_usb_cat_serial(&mut radio, cancellation).await?;

    // This atomic transition is the mutation boundary. Cancellation which
    // linearized first prevents MCP entry. Once MCP wins, never drop the
    // two-page future at an arbitrary binary-protocol boundary.
    cancellation
        .begin_mcp_operation()
        .map_err(|_cancelled| DvGatewayUsbRoutingError::Cancelled)?;
    let update = match radio
        .set_reflector_terminal_mode_detached_unverified(PcOutputInterface::Usb)
        .await
    {
        Ok(update) => update,
        Err(operation_error) => {
            let possibly_wrote = detached_update_has_possible_writes(&operation_error);
            let recovery = radio.recover_from_interrupted_mcp().await;
            drop(radio.disconnect().await);
            return match recovery {
                Ok(()) => Err(DvGatewayUsbRoutingError::RadioOperation {
                    detail: operation_error.to_string(),
                }),
                Err(recovery_error) if possibly_wrote => {
                    Err(DvGatewayUsbRoutingError::OutcomeUncertain {
                        detail: format!(
                            "operation failed ({operation_error}); MCP recovery also failed ({recovery_error})"
                        ),
                    })
                }
                Err(recovery_error) => Err(DvGatewayUsbRoutingError::RadioOperation {
                    detail: format!(
                        "operation failed before any persistent write ({operation_error}); MCP recovery also failed ({recovery_error}); no setting was changed"
                    ),
                }),
            };
        }
    };

    // A changed update deliberately reboots. An unchanged terminal-mode update
    // still completes an MCP reset, so neither branch is returned as a live CAT
    // handle. Swift owns the subsequent exact-address Bluetooth proof.
    drop(radio.disconnect().await);
    Ok(DvGatewayUsbRoutingResult {
        outcome: update.into(),
        radio_serial_number: radio_serial_number.to_string(),
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
    // transient KISS or MMDVM mode after the paired-device probe disconnected.
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
    let update = match radio.disable_dv_gateway_detached_unverified().await {
        Ok(update) => update,
        Err(operation_error) => {
            let possibly_wrote = detached_update_has_possible_writes(&operation_error);
            let recovery = radio.recover_from_interrupted_mcp().await;
            drop(radio.disconnect().await);
            return match recovery {
                Ok(()) => Err(DvGatewayRecoveryError::RadioOperation {
                    detail: operation_error.to_string(),
                }),
                Err(recovery_error) if possibly_wrote => {
                    Err(DvGatewayRecoveryError::OutcomeUncertain {
                        detail: format!(
                            "operation failed ({operation_error}); MCP recovery also failed ({recovery_error})"
                        ),
                    })
                }
                Err(recovery_error) => Err(DvGatewayRecoveryError::RadioOperation {
                    detail: format!(
                        "operation failed before any persistent write ({operation_error}); MCP recovery also failed ({recovery_error}); no setting was changed"
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

    use kenwood_thd75::{
        MockTransport,
        protocol::programming,
        protocol::programming::{McpPage, WritableMcpPage},
    };

    use crate::transport::ByteTransportError;

    const GATEWAY_INTERFACE_PAGE: u16 = 0x10;
    const GATEWAY_INTERFACE_BYTE: usize = 0x93;
    const KISS_INTERFACE_BYTE: usize = 0x90;
    const TNC_DATA_BAND_PAGE: u16 = 0x12;
    const TNC_DATA_BAND_BYTE: usize = 0x0B;
    const GATEWAY_MODE_PAGE: u16 = 0x1C;
    const GATEWAY_MODE_BYTE: usize = 0xA0;

    #[derive(Debug)]
    struct UnusedByteTransport;

    #[async_trait::async_trait]
    impl ByteTransport for UnusedByteTransport {
        async fn write(&self, _bytes: Vec<u8>) -> Result<(), ByteTransportError> {
            Err(ByteTransportError::Platform {
                message: "unused test transport was written".to_owned(),
            })
        }

        async fn read(&self, _max_length: u32) -> Result<Vec<u8>, ByteTransportError> {
            Err(ByteTransportError::Platform {
                message: "unused test transport was read".to_owned(),
            })
        }

        async fn close(&self) -> Result<(), ByteTransportError> {
            Ok(())
        }

        async fn reopen(&self) -> Result<(), ByteTransportError> {
            Err(ByteTransportError::Platform {
                message: "unused test transport was reopened".to_owned(),
            })
        }

        fn set_baud_rate(&self, _baud: u32) -> Result<(), ByteTransportError> {
            Ok(())
        }
    }

    fn mcp_read_response(page: u16, data: &[u8; programming::PAGE_SIZE]) -> Vec<u8> {
        let [high, low] = page.to_be_bytes();
        let mut response = vec![b'W', high, low, 0, 0];
        response.extend_from_slice(data);
        response
    }

    fn expect_mcp_page_read(
        transport: &mut MockTransport,
        page: McpPage,
        data: &[u8; programming::PAGE_SIZE],
    ) {
        transport.expect(
            &programming::build_read_command(page),
            &mcp_read_response(page.as_raw(), data),
        );
        transport.expect(&[programming::ACK], &[programming::ACK]);
    }

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

    #[test]
    fn aprs_zero_write_entry_and_read_failures_remain_no_setting_changed_after_recovery() {
        use kenwood_thd75::radio::programming::DetachedMcpPageUpdateError;

        let entry_failure = RadioError::DetachedMcpPageUpdate(DetachedMcpPageUpdateError::Entry {
            source: Box::new(RadioError::Timeout(std::time::Duration::from_secs(1))),
        });
        let read_failure =
            RadioError::DetachedMcpPageUpdate(DetachedMcpPageUpdateError::Operation {
                possibly_written_pages: Vec::new(),
                verified_written_pages: Vec::new(),
                source: Box::new(RadioError::Timeout(std::time::Duration::from_secs(1))),
            });

        for operation_error in [&entry_failure, &read_failure] {
            let classified = classify_aprs_recovery_operation_failure(operation_error, Ok(()));
            assert!(
                matches!(
                    classified,
                    AprsCurrentModeRecoveryError::NoSettingChanged { .. }
                ),
                "a zero-write failure followed by successful MCP recovery must retain its no-write proof: {classified:?}"
            );
        }
    }

    #[test]
    fn usb_routing_outcomes_keep_reboot_ownership_explicit() {
        assert_eq!(
            DvGatewayUsbRoutingOutcome::from(DetachedMcpPageUpdate::ChangedRadioRebooting),
            DvGatewayUsbRoutingOutcome::ChangedRadioRebooting
        );
        assert_eq!(
            DvGatewayUsbRoutingOutcome::from(DetachedMcpPageUpdate::UnchangedCatReady),
            DvGatewayUsbRoutingOutcome::AlreadyRouted
        );
    }

    #[tokio::test]
    async fn pre_cancelled_usb_routing_is_single_use_and_never_touches_transport()
    -> Result<(), Box<dyn std::error::Error>> {
        let operation = DvGatewayUsbRoutingOperation::new(Arc::new(UnusedByteTransport));
        operation.cancel();

        let first = Arc::clone(&operation).run().await;
        assert!(matches!(first, Err(DvGatewayUsbRoutingError::Cancelled)));
        let second = Arc::clone(&operation).run().await;
        assert!(matches!(
            second,
            Err(DvGatewayUsbRoutingError::OperationAlreadyRun)
        ));
        Ok(())
    }

    #[tokio::test]
    async fn usb_routing_rejects_invalid_cat_serial_before_mcp_gate()
    -> Result<(), Box<dyn std::error::Error>> {
        let mut transport = MockTransport::new();
        transport.expect(b"ID\r", b"ID TH-D75\r");
        transport.expect(b"FV\r", b"FV 1.03.AZM\r");
        transport.expect(b"AE\r", b"AE NOT-A-SERIAL,K01\r");
        let radio = Radio::new(transport);
        let cancellation = RecoveryCancellation::default();

        let result = route_dv_gateway_to_usb(radio, &cancellation).await;

        assert!(matches!(
            result,
            Err(DvGatewayUsbRoutingError::UsbCatIdentityUnavailable { .. })
        ));
        assert_eq!(
            cancellation.state.load(Ordering::Acquire) & MCP_OPERATION_STARTED,
            0
        );
        Ok(())
    }

    #[tokio::test]
    async fn usb_routing_schema_rejection_stays_before_mcp_gate()
    -> Result<(), Box<dyn std::error::Error>> {
        let mut transport = MockTransport::new();
        transport.expect(b"ID\r", b"ID TH-D75\r");
        transport.expect(b"FV\r", b"FV 1.04\r");
        let radio = Radio::new(transport);
        let cancellation = RecoveryCancellation::default();

        let result = route_dv_gateway_to_usb(radio, &cancellation).await;

        assert!(matches!(
            result,
            Err(DvGatewayUsbRoutingError::RadioQualification { .. })
        ));
        assert_eq!(
            cancellation.state.load(Ordering::Acquire) & MCP_OPERATION_STARTED,
            0
        );
        Ok(())
    }

    #[tokio::test]
    async fn usb_routing_changes_only_menu_985_when_reflector_mode_is_already_enabled()
    -> Result<(), Box<dyn std::error::Error>> {
        let mut transport = MockTransport::new();
        transport.expect(b"ID\r", b"ID TH-D75\r");
        transport.expect(b"FV\r", b"FV 1.03.AZM\r");
        transport.expect(b"AE\r", b"AE C3C10368,K01\r");
        transport.expect(programming::ENTER_PROGRAMMING, programming::ENTER_RESPONSE);

        let mut interface_before = [0_u8; programming::PAGE_SIZE];
        interface_before[GATEWAY_INTERFACE_BYTE] = u8::from(PcOutputInterface::Bluetooth);
        let mut interface_after = interface_before;
        interface_after[GATEWAY_INTERFACE_BYTE] = u8::from(PcOutputInterface::Usb);
        let mut mode = [0_u8; programming::PAGE_SIZE];
        mode[GATEWAY_MODE_BYTE] = 1;
        let interface_page = McpPage::new(GATEWAY_INTERFACE_PAGE)?;
        let mode_page = McpPage::new(GATEWAY_MODE_PAGE)?;
        expect_mcp_page_read(&mut transport, interface_page, &interface_before);
        expect_mcp_page_read(&mut transport, mode_page, &mode);
        let write = programming::build_write_command(
            WritableMcpPage::new(GATEWAY_INTERFACE_PAGE)?,
            &interface_after,
        );
        transport.expect(&write, &[programming::ACK]);
        expect_mcp_page_read(&mut transport, interface_page, &interface_after);
        transport.expect(&[programming::EXIT], &[programming::ACK]);

        let radio = Radio::new(transport);
        let cancellation = RecoveryCancellation::default();

        let result = route_dv_gateway_to_usb(radio, &cancellation).await?;

        assert_eq!(
            result,
            DvGatewayUsbRoutingResult {
                outcome: DvGatewayUsbRoutingOutcome::ChangedRadioRebooting,
                radio_serial_number: "C3C10368".to_owned(),
            }
        );
        assert_ne!(
            cancellation.state.load(Ordering::Acquire) & MCP_OPERATION_STARTED,
            0
        );
        Ok(())
    }

    #[tokio::test]
    async fn cat_disable_schema_rejection_stays_before_mcp_gate()
    -> Result<(), Box<dyn std::error::Error>> {
        let mut transport = MockTransport::new();
        transport.expect(b"ID\r", b"ID TH-D75\r");
        transport.expect(b"FV\r", b"FV 1.04\r");
        let radio = Radio::new(transport);
        let cancellation = RecoveryCancellation::default();

        let expected_serial = SerialNumber::new("C3C10368")?;
        let result = disable_dv_gateway_over_cat(radio, &expected_serial, &cancellation).await;

        assert!(matches!(
            result,
            Err(DvGatewayCatDisableError::RadioQualification { .. })
        ));
        assert_eq!(
            cancellation.state.load(Ordering::Acquire) & MCP_OPERATION_STARTED,
            0
        );
        Ok(())
    }

    #[tokio::test]
    async fn cat_disable_returns_serial_and_verified_reboot_outcome()
    -> Result<(), Box<dyn std::error::Error>> {
        let mut transport = MockTransport::new();
        transport.expect(b"ID\r", b"ID TH-D75\r");
        transport.expect(b"FV\r", b"FV 1.03.AZM\r");
        transport.expect(b"AE\r", b"AE C3C10368,K01\r");
        transport.expect(programming::ENTER_PROGRAMMING, programming::ENTER_RESPONSE);

        let mut mode_before = [0_u8; programming::PAGE_SIZE];
        mode_before[GATEWAY_MODE_BYTE] = 1;
        let mut mode_after = mode_before;
        mode_after[GATEWAY_MODE_BYTE] = 0;
        let mode_page = McpPage::new(GATEWAY_MODE_PAGE)?;
        expect_mcp_page_read(&mut transport, mode_page, &mode_before);
        let write =
            programming::build_write_command(WritableMcpPage::new(GATEWAY_MODE_PAGE)?, &mode_after);
        transport.expect(&write, &[programming::ACK]);
        expect_mcp_page_read(&mut transport, mode_page, &mode_after);
        transport.expect(&[programming::EXIT], &[programming::ACK]);

        let radio = Radio::new(transport);
        let cancellation = RecoveryCancellation::default();
        let expected_serial = SerialNumber::new("C3C10368")?;
        let result = disable_dv_gateway_over_cat(radio, &expected_serial, &cancellation).await?;

        assert_eq!(
            result,
            DvGatewayCatDisableResult {
                outcome: DvGatewayRecoveryOutcome::ChangedRadioRebooting,
                radio_serial_number: "C3C10368".to_owned(),
            }
        );
        assert_ne!(
            cancellation.state.load(Ordering::Acquire) & MCP_OPERATION_STARTED,
            0
        );
        Ok(())
    }

    #[tokio::test]
    async fn aprs_recovery_rejects_stale_menu_983_with_zero_writes()
    -> Result<(), Box<dyn std::error::Error>> {
        let mut transport = MockTransport::new();
        transport.expect(b"ID\r", b"ID TH-D75\r");
        transport.expect(b"FV\r", b"FV 1.03.AZM\r");
        transport.expect(b"AE\r", b"AE C3C10368,K01\r");
        transport.expect(programming::ENTER_PROGRAMMING, programming::ENTER_RESPONSE);
        let interface_page = McpPage::new(GATEWAY_INTERFACE_PAGE)?;
        let mut interface = [0_u8; programming::PAGE_SIZE];
        interface[KISS_INTERFACE_BYTE] = u8::from(PcOutputInterface::Usb);
        expect_mcp_page_read(&mut transport, interface_page, &interface);
        let data_band_page = McpPage::new(TNC_DATA_BAND_PAGE)?;
        let mut data_band = [0_u8; programming::PAGE_SIZE];
        data_band[TNC_DATA_BAND_BYTE] = u8::from(kenwood_thd75::types::TncDataBand::A);
        expect_mcp_page_read(&mut transport, data_band_page, &data_band);
        let mode_page = McpPage::new(GATEWAY_MODE_PAGE)?;
        let mut mode = [0_u8; programming::PAGE_SIZE];
        mode[GATEWAY_MODE_BYTE] = 1;
        expect_mcp_page_read(&mut transport, mode_page, &mode);
        transport.expect(&[programming::EXIT], &[programming::ACK]);
        transport.expect_reopen(Ok(()));
        transport.expect(b"ID\r", b"ID TH-D75\r");

        let radio = Radio::new(transport);
        let cancellation = RecoveryCancellation::default();
        let expected_serial = SerialNumber::new("C3C10368")?;
        let result = recover_aprs_current_mode_over_cat(
            radio,
            &expected_serial,
            u8::from(PcOutputInterface::Bluetooth),
            &cancellation,
        )
        .await;

        assert_eq!(
            result,
            Err(AprsCurrentModeRecoveryError::KissInterfaceMismatch {
                expected: 1,
                actual: 0,
            })
        );
        assert_ne!(
            cancellation.state.load(Ordering::Acquire) & MCP_OPERATION_STARTED,
            0,
            "the fresh Menu 983 proof belongs inside the approved MCP gate"
        );
        Ok(())
    }

    #[tokio::test]
    async fn aprs_recovery_returns_same_session_menu_983_proof()
    -> Result<(), Box<dyn std::error::Error>> {
        let mut transport = MockTransport::new();
        transport.expect(b"ID\r", b"ID TH-D75\r");
        transport.expect(b"FV\r", b"FV 1.03.AZM\r");
        transport.expect(b"AE\r", b"AE C3C10368,K01\r");
        transport.expect(programming::ENTER_PROGRAMMING, programming::ENTER_RESPONSE);
        let interface_page = McpPage::new(GATEWAY_INTERFACE_PAGE)?;
        let mut interface = [0_u8; programming::PAGE_SIZE];
        interface[KISS_INTERFACE_BYTE] = u8::from(PcOutputInterface::Bluetooth);
        expect_mcp_page_read(&mut transport, interface_page, &interface);
        let data_band_page = McpPage::new(TNC_DATA_BAND_PAGE)?;
        let mut data_band = [0_u8; programming::PAGE_SIZE];
        data_band[TNC_DATA_BAND_BYTE] = u8::from(kenwood_thd75::types::TncDataBand::B);
        expect_mcp_page_read(&mut transport, data_band_page, &data_band);
        let mode_page = McpPage::new(GATEWAY_MODE_PAGE)?;
        let mut mode_before = [0_u8; programming::PAGE_SIZE];
        mode_before[GATEWAY_MODE_BYTE] = 1;
        let mut mode_after = mode_before;
        mode_after[GATEWAY_MODE_BYTE] = 0;
        expect_mcp_page_read(&mut transport, mode_page, &mode_before);
        let write =
            programming::build_write_command(WritableMcpPage::new(GATEWAY_MODE_PAGE)?, &mode_after);
        transport.expect(&write, &[programming::ACK]);
        expect_mcp_page_read(&mut transport, mode_page, &mode_after);
        transport.expect(&[programming::EXIT], &[programming::ACK]);

        let radio = Radio::new(transport);
        let cancellation = RecoveryCancellation::default();
        let expected_serial = SerialNumber::new("C3C10368")?;
        let result = recover_aprs_current_mode_over_cat(
            radio,
            &expected_serial,
            u8::from(PcOutputInterface::Bluetooth),
            &cancellation,
        )
        .await?;

        assert_eq!(
            result,
            AprsCurrentModeRecoveryResult {
                outcome: DvGatewayRecoveryOutcome::ChangedRadioRebooting,
                radio_serial_number: "C3C10368".to_owned(),
                kiss_interface_raw_value: 1,
                data_band: TncDataBand::B,
            }
        );
        Ok(())
    }

    #[tokio::test]
    async fn aprs_recovery_rejects_invalid_menu_506_values_with_zero_writes()
    -> Result<(), Box<dyn std::error::Error>> {
        for invalid_band in [2_u8, 3_u8] {
            let mut transport = MockTransport::new();
            transport.expect(b"ID\r", b"ID TH-D75\r");
            transport.expect(b"FV\r", b"FV 1.03.AZM\r");
            transport.expect(b"AE\r", b"AE C3C10368,K01\r");
            transport.expect(programming::ENTER_PROGRAMMING, programming::ENTER_RESPONSE);

            let interface_page = McpPage::new(GATEWAY_INTERFACE_PAGE)?;
            let mut interface = [0_u8; programming::PAGE_SIZE];
            interface[KISS_INTERFACE_BYTE] = u8::from(PcOutputInterface::Bluetooth);
            expect_mcp_page_read(&mut transport, interface_page, &interface);

            let data_band_page = McpPage::new(TNC_DATA_BAND_PAGE)?;
            let mut data_band = [0_u8; programming::PAGE_SIZE];
            data_band[TNC_DATA_BAND_BYTE] = invalid_band;
            expect_mcp_page_read(&mut transport, data_band_page, &data_band);

            let mode_page = McpPage::new(GATEWAY_MODE_PAGE)?;
            let mut mode = [0_u8; programming::PAGE_SIZE];
            mode[GATEWAY_MODE_BYTE] = 1;
            expect_mcp_page_read(&mut transport, mode_page, &mode);
            transport.expect(&[programming::EXIT], &[programming::ACK]);
            transport.expect_reopen(Ok(()));
            transport.expect(b"ID\r", b"ID TH-D75\r");

            let radio = Radio::new(transport);
            let cancellation = RecoveryCancellation::default();
            let expected_serial = SerialNumber::new("C3C10368")?;
            let result = recover_aprs_current_mode_over_cat(
                radio,
                &expected_serial,
                u8::from(PcOutputInterface::Bluetooth),
                &cancellation,
            )
            .await;

            assert_eq!(
                result,
                Err(AprsCurrentModeRecoveryError::InvalidTncDataBand {
                    actual: invalid_band,
                })
            );
        }
        Ok(())
    }

    #[tokio::test]
    async fn cat_disable_refuses_changed_radio_identity_before_mcp_gate()
    -> Result<(), Box<dyn std::error::Error>> {
        let mut transport = MockTransport::new();
        transport.expect(b"ID\r", b"ID TH-D75\r");
        transport.expect(b"FV\r", b"FV 1.03.AZM\r");
        transport.expect(b"AE\r", b"AE C5310165,K01\r");
        let radio = Radio::new(transport);
        let expected_serial = SerialNumber::new("C3C10368")?;
        let cancellation = RecoveryCancellation::default();

        let result = disable_dv_gateway_over_cat(radio, &expected_serial, &cancellation).await;

        assert!(matches!(
            result,
            Err(DvGatewayCatDisableError::RadioIdentityMismatch {
                expected,
                actual,
            }) if expected == "C3C10368" && actual == "C5310165"
        ));
        assert_eq!(
            cancellation.state.load(Ordering::Acquire) & MCP_OPERATION_STARTED,
            0
        );
        Ok(())
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
    fn device_probe_admission_keeps_the_complete_recovery_budget() {
        assert!(!bluetooth_device_probe_fits(
            std::time::Duration::from_secs(59)
        ));
        assert!(bluetooth_device_probe_fits(BLUETOOTH_DEVICE_PROBE_RESERVE));
        assert!(BLUETOOTH_DEVICE_PROBE_RESERVE < BLUETOOTH_DEVICE_PROBE_WINDOW);
    }

    #[cfg(target_os = "macos")]
    #[tokio::test]
    async fn cancelled_recovery_does_not_wait_for_or_launch_queued_enumeration()
    -> Result<(), Box<dyn std::error::Error>> {
        let first = BLUETOOTH_HELPER_ENUMERATION_GATE.lock().await;
        let cancellation = RecoveryCancellation::default();
        let operation = async {
            let enumeration = enumerate_paired_bluetooth_devices_cancellable(
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
    async fn paired_device_proof_requires_exact_thd75_id_before_ae()
    -> Result<(), Box<dyn std::error::Error>> {
        let cancellation = RecoveryCancellation::default();

        let mut exact_transport = MockTransport::new();
        exact_transport.expect(b"ID\r", b"ID TH-D75\r");
        exact_transport.expect(b"AE\r", b"AE C3C10368,K01\r");
        let mut exact_radio = Radio::new(exact_transport);
        let exact = query_bluetooth_device_identity(&mut exact_radio, &cancellation).await?;
        assert!(matches!(
            exact,
            BluetoothDeviceProbe::Identified(serial) if serial.as_str() == "C3C10368"
        ));

        let mut other_transport = MockTransport::new();
        other_transport.expect(b"ID\r", b"ID TH-D74\r");
        let mut other_radio = Radio::new(other_transport);
        let other = query_bluetooth_device_identity(&mut other_radio, &cancellation).await?;
        assert!(matches!(
            other,
            BluetoothDeviceProbe::IdentityFailed(detail)
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
