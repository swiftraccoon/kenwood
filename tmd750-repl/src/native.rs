//! Native Bluetooth opening by exact address, and the captured CAT and MCP
//! workflows that run over it.

use std::path::PathBuf;
use std::sync::atomic::AtomicBool;
#[cfg(any(target_os = "macos", test))]
use std::sync::atomic::Ordering;
use std::time::Duration;

#[cfg(any(target_os = "macos", test))]
use kenwood_transport::bluetooth::BluetoothOpenCancellation;
use kenwood_transport::bluetooth::{BluetoothAddress, BluetoothService};
use kenwood_transport::error::{BluetoothCloseFailure, BluetoothOpenStage};
use kenwood_transport::{Transport, TransportError};
use serde::Serialize;
use serde::ser::SerializeStruct;

use crate::capture::Failure;
use crate::connection::Connection;

pub(crate) mod cat;
pub(crate) mod discovery;
mod input;
pub(crate) mod opening;
pub(crate) mod repl;
#[cfg(test)]
mod tests;
pub(crate) mod workflow;

/// One Bluetooth device to open, selected by address rather than by name.
#[derive(Clone, Debug)]
pub(crate) struct Endpoint {
    pub(crate) address: BluetoothAddress,
    pub(crate) helper: Option<PathBuf>,
}

/// The address and RFCOMM channel one open actually used.
#[derive(Clone, Debug, Serialize)]
pub(crate) struct Resolved {
    pub(crate) address: String,
    pub(crate) rfcomm_channel: u8,
}

/// A native connection paired with the address and channel it opened on.
pub(crate) struct Opened<T> {
    pub(crate) connection: T,
    pub(crate) resolved: Resolved,
}

/// A failed native open, including the close of any connection that arrived
/// after the failure was decided.
#[derive(Debug)]
pub(crate) struct OpenFailure {
    pub(crate) error: Failure,
    pub(crate) close_error: Option<Failure>,
    retry_admission: RetryAdmission,
    host_retirement_confirmed: bool,
}

impl Serialize for OpenFailure {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        let mut evidence = serializer.serialize_struct("OpenFailure", 4)?;
        evidence.serialize_field("error", &self.error)?;
        evidence.serialize_field("close_error", &self.close_error)?;
        evidence.serialize_field("retry_admission", &self.retry_admission)?;
        evidence.serialize_field(
            "host_retirement_confirmed",
            &self.host_retirement_confirmed(),
        )?;
        evidence.end()
    }
}

/// Whether a failure may be retried, decided from `TransportError` variants
/// rather than from error text.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
enum RetryAdmission {
    /// Not retryable.
    Refused,
    /// A native opening stage that may be attempted once more.
    NativeOpening,
}

impl std::fmt::Display for OpenFailure {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(formatter, "{}", self.error)?;
        if let Some(error) = &self.close_error {
            write!(formatter, "; late-open cleanup: {error}")?;
        }
        Ok(())
    }
}

impl std::error::Error for OpenFailure {}

impl OpenFailure {
    pub(crate) fn from_error(error: &(dyn std::error::Error + 'static)) -> Self {
        let retry_admission = match error.downcast_ref::<TransportError>() {
            Some(TransportError::NotFound) => RetryAdmission::NativeOpening,
            Some(
                TransportError::BluetoothOpen { stage }
                | TransportError::BluetoothOpenWithCleanup {
                    stage,
                    cleanup: BluetoothCloseFailure::ChannelUnconfirmed,
                },
            ) if retryable_open_stage(*stage) => RetryAdmission::NativeOpening,
            _ => RetryAdmission::Refused,
        };
        Self {
            error: Failure::from_error(error),
            close_error: None,
            retry_admission,
            host_retirement_confirmed: matches!(
                error.downcast_ref::<TransportError>(),
                Some(
                    TransportError::NotFound
                        | TransportError::BluetoothOpen { .. }
                        | TransportError::BluetoothOpenWithCleanup {
                            cleanup: BluetoothCloseFailure::ChannelUnconfirmed,
                            ..
                        }
                        | TransportError::BluetoothParameter { .. }
                )
            ),
        }
    }

    /// Whether this process no longer holds the connection.
    ///
    /// True only for the typed opening failures whose helper was reaped, and
    /// only when no close error was recorded. It says nothing about the RFCOMM
    /// channel's state in the operating system.
    pub(crate) const fn host_retirement_confirmed(&self) -> bool {
        self.host_retirement_confirmed && self.close_error.is_none()
    }

    /// Build a failure for a refusal made before any open was dispatched.
    pub(crate) fn before_open(error: &(dyn std::error::Error + 'static)) -> Self {
        let mut failure = Self::from_error(error);
        failure.host_retirement_confirmed = true;
        failure
    }

    /// Record the close of a connection that arrived after this failure.
    pub(crate) fn record_close(&mut self, error: Option<Failure>) {
        self.host_retirement_confirmed = error.is_none();
        self.close_error = error;
    }

    /// Fold an inner open failure into this outer deadline or cancellation
    /// failure: adopt its close state and append its message and causes.
    pub(crate) fn retain_open_failure(&mut self, cause: Self) {
        self.host_retirement_confirmed = cause.host_retirement_confirmed();
        self.close_error = cause.close_error;
        self.error.causes.push(cause.error.message);
        self.error.causes.extend(cause.error.causes);
    }

    /// Whether this failure is eligible for one retry.
    ///
    /// True only when it came from a retryable native opening stage (see
    /// `retryable_open_stage`) or `TransportError::NotFound`, and no close
    /// error was recorded for a connection that arrived late.
    pub(crate) const fn retry_allowed(&self) -> bool {
        matches!(self.retry_admission, RetryAdmission::NativeOpening) && self.close_error.is_none()
    }
}

const fn retryable_open_stage(stage: BluetoothOpenStage) -> bool {
    matches!(
        stage,
        BluetoothOpenStage::ContextAllocation
            | BluetoothOpenStage::SdpStart
            | BluetoothOpenStage::SdpCompletion
            | BluetoothOpenStage::SdpDeadline
            | BluetoothOpenStage::RfcommStart
            | BluetoothOpenStage::RfcommCompletion
            | BluetoothOpenStage::RfcommDeadline
            | BluetoothOpenStage::RfcommEndpoint
    )
}

/// Wall-clock budget for one native open, from dispatching the helper worker
/// until it returns a connection or a typed failure. Host policy, not a
/// firmware timing bound.
pub(crate) const OPEN_BUDGET: Duration = Duration::from_secs(25);
/// Interval between cancellation and deadline checks while the worker runs.
#[cfg(any(target_os = "macos", test))]
const CANCELLATION_POLL: Duration = Duration::from_millis(10);
/// Wall-clock budget for the single close of a native connection.
pub(crate) const CLOSE_BUDGET: Duration = Duration::from_secs(2);

/// Opens native connections; tests substitute an implementation that never
/// touches a real device.
pub(crate) trait Backend {
    type Connection: Transport;
    async fn open(
        &mut self,
        endpoint: &Endpoint,
        service: BluetoothService,
        cancelled: &AtomicBool,
    ) -> Result<Opened<Self::Connection>, OpenFailure>;
    /// Open under the caller's absolute deadline as well as `OPEN_BUDGET`.
    ///
    /// The default implementation ignores `_deadline`, so a caller that has one
    /// must still reject and close a connection returned after it.
    async fn open_until(
        &mut self,
        endpoint: &Endpoint,
        service: BluetoothService,
        cancelled: &AtomicBool,
        _deadline: tokio::time::Instant,
    ) -> Result<Opened<Self::Connection>, OpenFailure> {
        self.open(endpoint, service, cancelled).await
    }
    async fn wait(&mut self, duration: Duration) {
        tokio::time::sleep(duration).await;
    }
}

pub(crate) struct SystemBackend;

impl Backend for SystemBackend {
    type Connection = Connection;

    async fn open(
        &mut self,
        endpoint: &Endpoint,
        service: BluetoothService,
        cancelled: &AtomicBool,
    ) -> Result<Opened<Connection>, OpenFailure> {
        open(endpoint, service, cancelled, OPEN_BUDGET).await
    }

    async fn open_until(
        &mut self,
        endpoint: &Endpoint,
        service: BluetoothService,
        cancelled: &AtomicBool,
        deadline: tokio::time::Instant,
    ) -> Result<Opened<Connection>, OpenFailure> {
        let remaining = deadline.saturating_duration_since(tokio::time::Instant::now());
        if remaining.is_zero() {
            return Err(OpenFailure::before_open(&std::io::Error::new(
                std::io::ErrorKind::TimedOut,
                "native lifecycle deadline expired before opening",
            )));
        }
        open(endpoint, service, cancelled, OPEN_BUDGET.min(remaining)).await
    }
}

/// Run `worker` on a blocking task, joining it even after cancellation or the
/// budget expires; a connection it returns late is closed, never leaked.
#[cfg(any(target_os = "macos", test))]
async fn open_worker<T, F>(
    cancelled: &AtomicBool,
    budget: Duration,
    worker: F,
) -> Result<Opened<T>, OpenFailure>
where
    T: Transport + 'static,
    F: FnOnce(BluetoothOpenCancellation) -> Result<Opened<T>, TransportError> + Send + 'static,
{
    if cancelled.load(Ordering::Relaxed) {
        return Err(OpenFailure::before_open(
            &TransportError::BluetoothOpenInterrupted,
        ));
    }
    let cancellation = BluetoothOpenCancellation::default();
    let worker_cancellation = cancellation.clone();
    let deadline = tokio::time::Instant::now() + budget;
    let mut task = tokio::task::spawn_blocking(move || worker(worker_cancellation));
    let completed = tokio::select! {
        biased;
        result = &mut task => Some(result),
        () = cancelled_or_deadline(cancelled, deadline) => None,
    };
    let stopped = cancelled.load(Ordering::Relaxed);
    if !stopped
        && tokio::time::Instant::now() < deadline
        && let Some(result) = completed
    {
        return result
            .map_err(|error| OpenFailure::from_error(&error))?
            .map_err(|error| OpenFailure::from_error(&error));
    }
    cancellation.cancel();
    let reason = if stopped {
        TransportError::BluetoothOpenInterrupted
    } else {
        TransportError::Open {
            path: "native Bluetooth".to_owned(),
            source: std::io::Error::new(
                std::io::ErrorKind::TimedOut,
                "native helper open exceeded its host dispatch budget",
            ),
        }
    };
    let mut failure = OpenFailure::from_error(&reason);
    let result = match completed {
        Some(result) => result,
        None => task.await,
    };
    match result {
        Ok(Ok(mut opened)) => {
            failure.record_close(close(&mut opened.connection).await);
            drop(opened);
        }
        Ok(Err(error)) => failure.retain_open_failure(OpenFailure::from_error(&error)),
        Err(error) => failure.error.causes.push(error.to_string()),
    }
    Err(failure)
}

#[cfg(any(target_os = "macos", test))]
async fn cancelled_or_deadline(cancelled: &AtomicBool, deadline: tokio::time::Instant) {
    while !cancelled.load(Ordering::Relaxed) && tokio::time::Instant::now() < deadline {
        tokio::time::sleep_until((tokio::time::Instant::now() + CANCELLATION_POLL).min(deadline))
            .await;
    }
}

/// Close `transport` once within `CLOSE_BUDGET`, returning any failure.
pub(crate) async fn close(transport: &mut impl Transport) -> Option<Failure> {
    match tokio::time::timeout(CLOSE_BUDGET, transport.close()).await {
        Ok(Ok(())) => None,
        Ok(Err(error)) => Some(Failure::from_error(&error)),
        Err(error) => Some(Failure::from_error(&error)),
    }
}

#[cfg(target_os = "macos")]
async fn open(
    endpoint: &Endpoint,
    service: BluetoothService,
    cancelled: &AtomicBool,
    budget: Duration,
) -> Result<Opened<Connection>, OpenFailure> {
    use kenwood_transport::bluetooth::{BluetoothDeviceSelector, BluetoothTransport};

    let endpoint = endpoint.clone();
    open_worker(cancelled, budget, move |cancellation| {
        let selector = BluetoothDeviceSelector::Address(endpoint.address.clone());
        let transport = endpoint.helper.map_or_else(
            || BluetoothTransport::open(&selector, service, &cancellation),
            |helper| {
                BluetoothTransport::open_with_helper_executable(
                    &selector,
                    service,
                    helper,
                    &cancellation,
                )
            },
        )?;
        let resolved = Resolved {
            address: transport.address().to_string(),
            rfcomm_channel: transport.channel().get(),
        };
        Ok(Opened {
            connection: Connection::NativeBluetooth(transport),
            resolved,
        })
    })
    .await
}

#[cfg(not(target_os = "macos"))]
fn open(
    _endpoint: &Endpoint,
    _service: BluetoothService,
    _cancelled: &AtomicBool,
    _budget: Duration,
) -> std::future::Ready<Result<Opened<Connection>, OpenFailure>> {
    std::future::ready(Err(OpenFailure::before_open(&std::io::Error::new(
        std::io::ErrorKind::Unsupported,
        "native Bluetooth is currently available only on macOS",
    ))))
}
