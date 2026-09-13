//! TM-D750 admission and ownership around the model-neutral D-STAR runtime.
//!
//! CAT remains a model policy. Ordinary diagnosis requires one completed
//! initial `ID` write with no received bytes before probing. A separately
//! verified Terminal transition may instead probe its retained connection
//! directly. Successful framing retains that exact transport until modem
//! startup consumes it. Shutdown closes without changing persistent Gateway
//! state.

use std::time::Duration;

use kenwood_tmd750::{Error as Tmd750Error, Radio as CatRadio};
use kenwood_transport::{StreamAdapter, Transport, TransportError};
use mmdvm::AsyncModem;
use mmdvm::dstar::{DstarModem, DstarModemConfig};

use crate::terminal;

/// One absolute budget for the version request and complete response.
const VERSION_PROBE_TIMEOUT: Duration = Duration::from_secs(2);
/// Independent teardown allowance after protocol work has stopped.
const CLOSE_TIMEOUT: Duration = Duration::from_secs(2);

/// Model-neutral runtime over the selected transport adapter.
pub(super) type Gateway<T> = DstarModem<StreamAdapter<T>>;

/// An owned connection whose latest complete exchange proved MMDVM framing.
///
/// The field is private so startup cannot substitute a different connection
/// or manufacture proof from CAT silence alone.
#[derive(Debug)]
pub(super) struct ProvenModem<T>(T);

/// Initialization and independent cleanup evidence retained for restoration.
#[derive(Debug)]
pub(super) struct StartFailure {
    pub(super) message: String,
    pub(super) cleanup_error: Option<String>,
}

impl StartFailure {
    /// Restore through another endpoint only after both pumps and close finish.
    pub(super) const fn owner_released(&self) -> bool {
        self.cleanup_error.is_none()
    }
}

impl std::fmt::Display for StartFailure {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(&self.message)?;
        if let Some(cleanup) = &self.cleanup_error {
            write!(formatter, "; {cleanup}")?;
        }
        Ok(())
    }
}

impl std::error::Error for StartFailure {}

impl<T: Transport> ProvenModem<T> {
    /// Prove a caller-owned transition connection without issuing CAT.
    ///
    /// The caller must independently establish the selected Terminal route and
    /// an acknowledged MCP exit before using this operation. Failure returns
    /// the same owner for explicit close; silence alone never creates proof.
    pub(super) async fn probe(
        mut transport: T,
        timeout: Duration,
    ) -> Result<Self, (T, mmdvm::probe::ProbeError)> {
        match mmdvm::probe::probe_version(&mut transport, timeout).await {
            Ok(_) => Ok(Self(transport)),
            Err(error) => Err((transport, error)),
        }
    }

    /// Consume proof when its owner must be closed instead of admitted.
    pub(super) fn into_transport(self) -> T {
        self.0
    }

    /// Inspect owner metadata without changing or replacing the proved transport.
    pub(super) const fn transport(&self) -> &T {
        &self.0
    }
}

/// What the observer saw while dispatching the initial CAT request.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum InitialWrite {
    /// No request has started.
    Unstarted,
    /// The exact initial `ID` write is in progress.
    Pending,
    /// The exact initial `ID` write completed successfully.
    Completed,
    /// Another request was attempted; this is no longer an initial ID trial.
    Other,
}

/// Observe CAT admission without consuming, buffering, or rewriting bytes.
#[derive(Debug)]
struct CatObservation<T> {
    inner: T,
    initial_write: InitialWrite,
    received_input: bool,
}

impl<T> CatObservation<T> {
    const fn new(inner: T) -> Self {
        Self {
            inner,
            initial_write: InitialWrite::Unstarted,
            received_input: false,
        }
    }

    /// Partial input is not silence, even when CAT's line deadline expires.
    fn is_silent_completed_id(&self) -> bool {
        self.initial_write == InitialWrite::Completed && !self.received_input
    }
}

impl<T: Transport> Transport for CatObservation<T> {
    async fn write(&mut self, data: &[u8]) -> Result<(), TransportError> {
        self.initial_write = if self.initial_write == InitialWrite::Unstarted && data == b"ID\r" {
            InitialWrite::Pending
        } else {
            InitialWrite::Other
        };
        self.inner.write(data).await?;
        if self.initial_write == InitialWrite::Pending {
            self.initial_write = InitialWrite::Completed;
        }
        Ok(())
    }

    async fn read(&mut self, buffer: &mut [u8]) -> Result<usize, TransportError> {
        let count = self.inner.read(buffer).await?;
        self.received_input |= count != 0;
        Ok(count)
    }

    async fn close(&mut self) -> Result<(), TransportError> {
        self.inner.close().await
    }
}

/// Diagnose normal CAT or retain the exact connection with binary proof.
pub(super) async fn prove_mmdvm_or_explain_cat<T: Transport>(
    transport: T,
    connection: terminal::UsbConnection,
) -> Result<ProvenModem<T>, String> {
    prove_mmdvm_or_explain_cat_with_timeout(
        transport,
        kenwood_tmd750::radio::DEFAULT_TIMEOUT,
        connection,
    )
    .await
}

/// Apply the ordinary preflight with an injectable CAT deadline for tests.
pub(super) async fn prove_mmdvm_or_explain_cat_with_timeout<T: Transport>(
    transport: T,
    cat_timeout: Duration,
    connection: terminal::UsbConnection,
) -> Result<ProvenModem<T>, String> {
    let mut cat_radio = CatRadio::new(CatObservation::new(transport));
    cat_radio.set_timeout(cat_timeout);
    match cat_radio.identify().await {
        Ok(identity) => {
            let gateway = cat_radio.get_dv_gateway_mode().await;
            let guidance =
                terminal::cat_startup_guidance(connection, gateway.as_ref().ok().copied());
            let gateway = gateway
                .as_ref()
                .map_or_else(|error| format!("unreadable ({error})"), ToString::to_string);
            let message = format!(
                "{} firmware {} answered normal CAT.\n\
                 DV Gateway state: {gateway}. No setting was changed.\n{guidance}",
                identity.model, identity.firmware,
            );
            Err(close_after_failure(cat_radio.into_transport().inner, message).await)
        }
        Err(error) => {
            let observation = cat_radio.into_transport();
            if matches!(
                &error,
                Tmd750Error::Timeout {
                    operation: "ID",
                    ..
                }
            ) && observation.is_silent_completed_id()
            {
                tracing::debug!(%error, "CAT ID was silent; attempting a bounded MMDVM version probe");
                let mut transport = observation.inner;
                match mmdvm::probe::probe_version(&mut transport, VERSION_PROBE_TIMEOUT).await {
                    Ok(_) => Ok(ProvenModem(transport)),
                    Err(error) => {
                        let message = format!(
                            "CAT was silent, but the endpoint did not answer a complete MMDVM GET_VERSION probe ({error}). No gateway frames were sent. Check Menu 986 routing and Menu 650, or capture the third-party Terminal Mode protocol before trying another transport."
                        );
                        Err(close_after_failure(transport, message).await)
                    }
                }
            } else {
                let message = format!(
                    "TM-D750 CAT identification failed without a completely silent, completed initial ID exchange, so no MMDVM probe was sent: {error}"
                );
                Err(close_after_failure(observation.inner, message).await)
            }
        }
    }
}

/// Start protocol processing only on the connection that passed preflight.
pub(super) async fn start_gateway<T: Transport + Unpin + 'static>(
    proof: ProvenModem<T>,
    config: DstarModemConfig,
) -> Result<Gateway<T>, StartFailure> {
    let modem = AsyncModem::spawn(StreamAdapter::new(proof.0));
    match DstarModem::initialize(modem, config).await {
        Ok(gateway) => Ok(gateway),
        Err((modem, error)) => {
            let message = format!("MMDVM D-STAR initialization failed: {error}");
            Err(StartFailure {
                message,
                cleanup_error: stop_modem(modem).await.err(),
            })
        }
    }
}

/// Stop the runtime, recover its transport, and attempt exactly one close.
///
/// This performs no CAT, MCP, mode-exit command, or automatic reopen.
pub(super) async fn stop_gateway<T: Transport + Unpin + 'static>(
    gateway: Gateway<T>,
) -> Result<(), String> {
    stop_modem(gateway.into_modem()).await
}

/// Complete both asynchronous owners before touching the physical connection.
async fn stop_modem<T: Transport + Unpin + 'static>(
    modem: AsyncModem<StreamAdapter<T>>,
) -> Result<(), String> {
    let adapter = modem
        .shutdown()
        .await
        .map_err(|error| format!("MMDVM shutdown failed; serial ownership was lost: {error}"))?;
    match adapter.shutdown_and_recover().await {
        Ok(transport) => close_transport(transport)
            .await
            .map_err(|error| format!("Serial close failed: {error}")),
        Err(error) => {
            let (transport, error) = error.into_parts();
            let message = format!("Serial adapter shutdown failed: {error}");
            Err(match transport {
                Some(transport) => close_after_failure(transport, message).await,
                None => format!("{message}; serial ownership was lost"),
            })
        }
    }
}

/// Preserve the operation diagnostic and any independent close failure.
async fn close_after_failure<T: Transport>(transport: T, message: String) -> String {
    match close_transport(transport).await {
        Ok(()) => message,
        Err(error) => format!("{message}\nSerial close also failed: {error}."),
    }
}

/// Bound close without dropping the only transport owner before the attempt.
pub(super) async fn close_transport<T: Transport>(mut transport: T) -> Result<(), String> {
    tokio::time::timeout(CLOSE_TIMEOUT, transport.close())
        .await
        .map_err(|_| "connection close exceeded its two-second budget".to_owned())?
        .map_err(|error| transport_error(&error))
}

/// Include the backend cause when the transport category has a terse display.
fn transport_error(error: &TransportError) -> String {
    let cause =
        std::error::Error::source(error).map_or_else(String::new, |cause| format!(": {cause}"));
    format!("{error}{cause}")
}

#[cfg(test)]
mod tests {
    use std::io;
    use std::sync::Arc;
    use std::sync::atomic::{AtomicUsize, Ordering};

    use super::*;

    struct PendingClose {
        closes: Arc<AtomicUsize>,
        drops: Arc<AtomicUsize>,
    }

    impl Transport for PendingClose {
        async fn write(&mut self, _bytes: &[u8]) -> Result<(), TransportError> {
            Err(TransportError::Write(io::Error::other(
                "cleanup must not write protocol data",
            )))
        }

        async fn read(&mut self, _bytes: &mut [u8]) -> Result<usize, TransportError> {
            Err(TransportError::Read(io::Error::other(
                "cleanup must not read protocol data",
            )))
        }

        async fn close(&mut self) -> Result<(), TransportError> {
            let _previous = self.closes.fetch_add(1, Ordering::AcqRel);
            std::future::pending().await
        }
    }

    impl Drop for PendingClose {
        fn drop(&mut self) {
            let _previous = self.drops.fetch_add(1, Ordering::AcqRel);
        }
    }

    #[tokio::test]
    async fn failure_cleanup_bounds_close_and_preserves_original_diagnostic()
    -> Result<(), Box<dyn std::error::Error>> {
        let closes = Arc::new(AtomicUsize::new(0));
        let drops = Arc::new(AtomicUsize::new(0));
        let owner = PendingClose {
            closes: closes.clone(),
            drops: drops.clone(),
        };
        let message = tokio::time::timeout(
            Duration::from_secs(3),
            close_after_failure(owner, "original protocol failure".to_owned()),
        )
        .await?;
        assert!(message.starts_with("original protocol failure"));
        assert!(message.contains("connection close exceeded its two-second budget"));
        assert_eq!(closes.load(Ordering::Acquire), 1);
        assert_eq!(drops.load(Ordering::Acquire), 1);
        Ok(())
    }
}
