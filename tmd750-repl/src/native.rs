//! Exact-address native Bluetooth ownership and captured control workflows.

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

/// A selected physical address, never a paired display-name match or USB alias.
#[derive(Clone, Debug)]
pub(crate) struct Endpoint {
    pub(crate) address: BluetoothAddress,
    pub(crate) helper: Option<PathBuf>,
}

/// The exact endpoint returned by one native opening operation.
#[derive(Clone, Debug, Serialize)]
pub(crate) struct Resolved {
    pub(crate) address: String,
    pub(crate) rfcomm_channel: u8,
}

/// Return endpoint evidence with the owner, not in a later disconnected query.
pub(crate) struct Opened<T> {
    pub(crate) connection: T,
    pub(crate) resolved: Resolved,
}

/// Native opening failures retain cleanup from any late successful open.
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

/// Retry admission is derived from typed transport evidence, never error text.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
enum RetryAdmission {
    Refused,
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

    /// Whether application ownership ended, not whether the OS cancelled RFCOMM.
    ///
    /// Known native opening stages require an already reaped helper. Unknown or
    /// unframed failures do not carry that proof, even when no owner was returned.
    /// A failed explicit close also prevents this stronger handoff assurance.
    pub(crate) const fn host_retirement_confirmed(&self) -> bool {
        self.host_retirement_confirmed && self.close_error.is_none()
    }

    /// Record a refusal at a boundary that has not dispatched native opening.
    pub(crate) fn before_open(error: &(dyn std::error::Error + 'static)) -> Self {
        let mut failure = Self::from_error(error);
        failure.host_retirement_confirmed = true;
        failure
    }

    /// Preserve the actual retirement of an acquired but inadmissible owner.
    pub(crate) fn record_close(&mut self, error: Option<Failure>) {
        self.host_retirement_confirmed = error.is_none();
        self.close_error = error;
    }

    /// An outer deadline cannot manufacture or erase inner retirement evidence.
    pub(crate) fn retain_open_failure(&mut self, cause: Self) {
        self.host_retirement_confirmed = cause.host_retirement_confirmed();
        self.close_error = cause.close_error;
        self.error.causes.push(cause.error.message);
        self.error.causes.extend(cause.error.causes);
    }

    /// Only a completed native opening failure may receive one selected retry.
    /// A late successful owner's cleanup failure never creates retry authority.
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

/// Host dispatch policy, not a firmware-readiness or scheduling guarantee.
pub(crate) const OPEN_BUDGET: Duration = Duration::from_secs(25);
#[cfg(any(target_os = "macos", test))]
const CANCELLATION_POLL: Duration = Duration::from_millis(10);
pub(crate) const CLOSE_BUDGET: Duration = Duration::from_secs(2);

/// Replaceable native dependency; fake tests cannot open a real endpoint.
pub(crate) trait Backend {
    type Connection: Transport;
    async fn open(
        &mut self,
        endpoint: &Endpoint,
        service: BluetoothService,
        cancelled: &AtomicBool,
    ) -> Result<Opened<Self::Connection>, OpenFailure>;
    /// Join opening under a caller's absolute lifecycle deadline. The default
    /// retains ownership; callers must still reject and retire late returns.
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

/// Finish a started helper worker even after cancellation; do not detach its owner.
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

/// Close is always attempted once and retains failure independently of observations.
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
