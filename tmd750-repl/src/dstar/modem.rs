//! TM-D750 admission and ownership around the model-neutral D-STAR runtime.
//!
//! CAT remains a model policy: only one completed initial `ID` write with no
//! received bytes admits a binary probe. A successful probe retains ownership
//! of that exact transport until modem startup consumes it. Shutdown recovers
//! and closes the transport without changing persistent Gateway state.

use std::time::Duration;

use kenwood_tmd750::{Error as Tmd750Error, Radio as CatRadio};
use kenwood_transport::{StreamAdapter, Transport, TransportError};
use mmdvm::AsyncModem;
use mmdvm::dstar::{DstarModem, DstarModemConfig};

use crate::terminal;

/// One absolute budget for the version request and complete response.
const VERSION_PROBE_TIMEOUT: Duration = Duration::from_secs(2);

/// Model-neutral runtime over the selected transport adapter.
pub(super) type Gateway<T> = DstarModem<StreamAdapter<T>>;

/// An owned connection whose latest complete exchange proved MMDVM framing.
///
/// The field is private so startup cannot substitute a different connection
/// or manufacture proof from CAT silence alone.
#[derive(Debug)]
pub(super) struct ProvenModem<T>(T);

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
) -> Result<Gateway<T>, String> {
    let modem = AsyncModem::spawn(StreamAdapter::new(proof.0));
    match DstarModem::initialize(modem, config).await {
        Ok(gateway) => Ok(gateway),
        Err((modem, error)) => {
            let message = format!("MMDVM D-STAR initialization failed: {error}");
            Err(match stop_modem(modem).await {
                Ok(()) => message,
                Err(cleanup) => format!("{message}; {cleanup}"),
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
        Ok(mut transport) => transport
            .close()
            .await
            .map_err(|error| format!("Serial close failed: {}", transport_error(&error))),
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
async fn close_after_failure<T: Transport>(mut transport: T, message: String) -> String {
    match transport.close().await {
        Ok(()) => message,
        Err(error) => format!(
            "{message}\nSerial close also failed: {}.",
            transport_error(&error)
        ),
    }
}

/// Include the backend cause when the transport category has a terse display.
fn transport_error(error: &TransportError) -> String {
    let cause =
        std::error::Error::source(error).map_or_else(String::new, |cause| format!(": {cause}"));
    format!("{error}{cause}")
}
