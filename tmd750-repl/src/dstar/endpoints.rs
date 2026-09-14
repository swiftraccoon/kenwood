//! Select a native modem and independent USB control without protocol traffic.
//!
//! Recognized paired-device names resolve only a candidate exact address. USB
//! selection admits observed connector metadata, not physical radio continuity;
//! the lifecycle must independently verify both selected interfaces before use.

use std::io;
use std::sync::atomic::AtomicBool;
#[cfg(any(target_os = "macos", test))]
use std::sync::atomic::Ordering;

use kenwood_tmd750::transport::SerialCandidate;
#[cfg(any(target_os = "macos", test))]
use kenwood_tmd750::transport::TMD750_MAIN_PID;

use crate::AppResult;
#[cfg(any(target_os = "macos", test))]
use crate::CommandError;
use crate::native::{Endpoint, discovery};

#[cfg(test)]
mod tests;

/// Selected roles remain distinct transport types throughout the lifecycle.
#[derive(Debug)]
pub(crate) struct Endpoints {
    /// Exact native Bluetooth device; no serial-port alias or name fallback.
    pub(crate) bluetooth: Endpoint,
    /// Independently selected, unambiguous TM-D750 USB control connector.
    pub(crate) control: SerialCandidate,
}

/// Resolve metadata before opening either selected radio interface.
///
/// Explicit Bluetooth selection bypasses paired-device enumeration. Default
/// discovery runs once in the shared bounded helper, on a blocking worker that
/// is always joined before this future returns. Callers must retain this future
/// through cancellation; cancellation prevents admission after the worker ends.
#[cfg(target_os = "macos")]
pub(crate) async fn resolve(
    bluetooth: &discovery::Request,
    control_port: Option<&str>,
    cancelled: &AtomicBool,
) -> AppResult<Endpoints> {
    use kenwood_tmd750::transport::discover_serial;
    check_cancelled(cancelled)?;
    resolve_with(
        bluetooth,
        control_port,
        cancelled,
        &discover_serial()?,
        discovery::resolve,
    )
    .await
}

/// Refuse unsupported platforms before discovery or connection opening.
#[cfg(not(target_os = "macos"))]
pub(crate) fn resolve(
    _bluetooth: &discovery::Request,
    _control_port: Option<&str>,
    _cancelled: &AtomicBool,
) -> std::future::Ready<AppResult<Endpoints>> {
    std::future::ready(Err(io::Error::new(
        io::ErrorKind::Unsupported,
        "automatic TM-D750 D-STAR startup currently requires native Bluetooth on macOS",
    )
    .into()))
}

/// Admit independent USB metadata before consulting Bluetooth inventory.
#[cfg(any(target_os = "macos", test))]
async fn resolve_with<F>(
    bluetooth: &discovery::Request,
    control_port: Option<&str>,
    cancelled: &AtomicBool,
    candidates: &[SerialCandidate],
    resolve_bluetooth: F,
) -> AppResult<Endpoints>
where
    F: AsyncFnOnce(&discovery::Request, &AtomicBool) -> AppResult<Endpoint>,
{
    check_cancelled(cancelled)?;
    let control = select_control(control_port, candidates)?;
    check_cancelled(cancelled)?;
    let bluetooth = resolve_bluetooth(bluetooth, cancelled).await?;
    check_cancelled(cancelled)?;
    Ok(Endpoints { bluetooth, control })
}

#[cfg(any(target_os = "macos", test))]
fn check_cancelled(cancelled: &AtomicBool) -> io::Result<()> {
    if cancelled.load(Ordering::Relaxed) {
        Err(io::Error::new(
            io::ErrorKind::Interrupted,
            "TM-D750 endpoint selection cancelled",
        ))
    } else {
        Ok(())
    }
}

/// Preserve explicit paths; automatic selection considers only macOS callout ports.
#[cfg(any(target_os = "macos", test))]
fn select_control(
    requested: Option<&str>,
    candidates: &[SerialCandidate],
) -> Result<SerialCandidate, CommandError> {
    use crate::mcp::reconnect::endpoint_is_unambiguous;

    let path = if let Some(path) = requested {
        path
    } else {
        let known: Vec<_> = candidates.iter().filter(|port| port.is_tmd750()).collect();
        if known
            .iter()
            .any(|port| !endpoint_is_unambiguous(port, candidates))
        {
            return Err(CommandError(
                "TM-D750 USB metadata is ambiguous; no automatic control endpoint was selected"
                    .to_owned(),
            ));
        }
        let callout: Vec<_> = known
            .into_iter()
            .filter(|port| {
                port.path
                    .strip_prefix("/dev/cu.")
                    .is_some_and(|name| !name.is_empty())
            })
            .collect();
        match callout.as_slice() {
            [only] => only.path.as_str(),
            [first, second] if first.pid != second.pid => {
                if first.pid == Some(TMD750_MAIN_PID) {
                    first.path.as_str()
                } else {
                    second.path.as_str()
                }
            }
            _ => {
                return Err(CommandError(
                    "automatic D-STAR startup requires an unambiguous TM-D750 USB control connector; connect USB or select an exact --control-port".to_owned(),
                ));
            }
        }
    };
    let selected = super::probe::select_endpoint(path, candidates.to_vec())?;
    if !endpoint_is_unambiguous(&selected, candidates) {
        return Err(CommandError(format!(
            "USB control endpoint {path} has ambiguous or conflicting metadata; no substitute was selected"
        )));
    }
    Ok(selected)
}
