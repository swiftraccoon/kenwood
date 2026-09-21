//! Paired-device selection shared by native CAT and D-STAR startup.
//!
//! Selection returns one exact address and the helper that listed it. A remote
//! name only nominates a candidate; the CAT identity exchange confirms the
//! model before any workflow uses the connection.

use std::io;
use std::path::PathBuf;
use std::sync::atomic::AtomicBool;
#[cfg(any(target_os = "macos", test))]
use std::sync::atomic::Ordering;

use kenwood_transport::bluetooth::BluetoothAddress;
#[cfg(any(target_os = "macos", test))]
use kenwood_transport::bluetooth::BluetoothOpenCancellation;

use crate::AppResult;
#[cfg(any(target_os = "macos", test))]
use crate::CommandError;

use super::Endpoint;

#[cfg(test)]
mod tests;

/// What to resolve: an optional exact address and an optional helper binary.
#[derive(Clone, Debug, Default)]
pub(crate) struct Request {
    /// An exact address, which skips the paired-device inventory.
    pub(crate) address: Option<BluetoothAddress>,
    /// Use this executable for both paired-device inventory and later opening.
    pub(crate) helper: Option<PathBuf>,
}

/// Resolve one endpoint address, opening no radio and sending no traffic.
///
/// Automatic selection requires exactly one paired device with a recognized
/// name. The inventory runs on a blocking worker that is always joined, so this
/// future must be polled to completion; dropping it would abandon that worker.
/// Cancellation is forwarded to the helper and discards a late result.
#[cfg(target_os = "macos")]
pub(crate) async fn resolve(request: &Request, cancelled: &AtomicBool) -> AppResult<Endpoint> {
    use kenwood_transport::bluetooth::BluetoothTransport;

    resolve_with(request, cancelled, |helper, cancellation| {
        Ok(
            BluetoothTransport::paired_devices_with_helper_executable_cancellable(
                helper,
                &cancellation,
            )?
            .into_iter()
            .map(|device| (device.address().clone(), device.display_name().to_owned()))
            .collect(),
        )
    })
    .await
}

/// Return a ready `Unsupported` error: discovery needs macOS Bluetooth.
#[cfg(not(target_os = "macos"))]
pub(crate) fn resolve(
    request: &Request,
    _cancelled: &AtomicBool,
) -> std::future::Ready<AppResult<Endpoint>> {
    let address = request
        .address
        .as_ref()
        .map_or("automatic selection", BluetoothAddress::as_str);
    let helper = request.helper.as_ref().map_or_else(
        || "the current executable".to_owned(),
        |helper| helper.display().to_string(),
    );
    std::future::ready(Err(io::Error::new(
        io::ErrorKind::Unsupported,
        format!(
            "native Bluetooth is currently available only on macOS; cannot resolve {address} using {helper}"
        ),
    )
    .into()))
}

#[cfg(any(target_os = "macos", test))]
async fn resolve_with<F>(
    request: &Request,
    cancelled: &AtomicBool,
    inventory: F,
) -> AppResult<Endpoint>
where
    F: FnOnce(PathBuf, BluetoothOpenCancellation) -> AppResult<Vec<(BluetoothAddress, String)>>
        + Send
        + 'static,
{
    check_cancelled(cancelled)?;
    if let Some(address) = &request.address {
        return Ok(Endpoint {
            address: address.clone(),
            helper: request.helper.clone(),
        });
    }
    let helper = request
        .helper
        .clone()
        .map_or_else(std::env::current_exe, Ok)?;
    let cancellation = BluetoothOpenCancellation::default();
    let worker_cancellation = cancellation.clone();
    let worker_helper = helper.clone();
    let mut worker =
        tokio::task::spawn_blocking(move || inventory(worker_helper, worker_cancellation));
    let observed = loop {
        tokio::select! {
            biased;
            result = &mut worker => break result,
            () = tokio::time::sleep(super::CANCELLATION_POLL) => {
                if cancelled.load(Ordering::Acquire) {
                    cancellation.cancel();
                    break worker.await;
                }
            }
        }
    };
    let observed = observed
        .map_err(Into::into)
        .and_then(std::convert::identity);
    if let Err(stopped) = check_cancelled(cancelled) {
        return Err(match observed {
            Ok(_) => stopped.into(),
            Err(error) => {
                CommandError(format!("{stopped}; paired-device inventory: {error}")).into()
            }
        });
    }
    let devices = observed?;
    let address = select(
        devices
            .iter()
            .map(|(address, name)| (address, name.as_str())),
    )?;
    check_cancelled(cancelled)?;
    Ok(Endpoint {
        address,
        helper: Some(helper),
    })
}

/// Bluetooth remote names accepted for automatic TM-D750 selection.
///
/// A TM-D750 on firmware 1.02 advertises `stm32mp1-ex5240`, the name of its
/// system-on-chip, rather than `TM-D750`. A name selects a candidate address
/// only; the model is confirmed by the CAT identity exchange.
#[cfg(any(target_os = "macos", test))]
const CANDIDATE_NAMES: [&str; 2] = ["TM-D750", "stm32mp1-ex5240"];

/// Select the one paired device whose name is in `CANDIDATE_NAMES`.
///
/// Returns `CommandError` when no name matches, when several do, or when the
/// chosen address appears more than once in the inventory.
#[cfg(any(target_os = "macos", test))]
fn select<'a>(
    devices: impl IntoIterator<Item = (&'a BluetoothAddress, &'a str)>,
) -> Result<BluetoothAddress, CommandError> {
    let devices: Vec<_> = devices.into_iter().collect();
    let matching: Vec<_> = devices
        .iter()
        .copied()
        .filter(|(_, name)| CANDIDATE_NAMES.contains(name))
        .collect();
    let Some((address, _)) = matching.first() else {
        return Err(CommandError(
            "no paired TM-D750 candidate named TM-D750 or stm32mp1-ex5240; pair the radio or select its exact address with --bluetooth-address".to_owned(),
        ));
    };
    if matching.len() != 1
        || devices
            .iter()
            .filter(|(candidate, _)| *candidate == *address)
            .count()
            != 1
    {
        let candidates = devices
            .iter()
            .filter(|(candidate, name)| CANDIDATE_NAMES.contains(name) || *candidate == *address)
            .map(|(address, name)| format!("{name:?} ({address})"))
            .collect::<Vec<_>>()
            .join(", ");
        return Err(CommandError(format!(
            "paired TM-D750 selection is ambiguous: {candidates}; select one exact address with --bluetooth-address"
        )));
    }
    Ok((*address).clone())
}

#[cfg(any(target_os = "macos", test))]
fn check_cancelled(cancelled: &AtomicBool) -> io::Result<()> {
    if cancelled.load(Ordering::Acquire) {
        Err(io::Error::new(
            io::ErrorKind::Interrupted,
            "TM-D750 paired-device selection cancelled",
        ))
    } else {
        Ok(())
    }
}
