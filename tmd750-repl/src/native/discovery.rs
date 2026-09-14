//! One paired-device selection policy for native CAT and D-STAR startup.
//!
//! Remote names identify candidates, not radio models. Selection retains one
//! exact address and the helper used for its inventory; live CAT identity and
//! workflow-specific admission remain mandatory before protocol operations.

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

/// Optional exact device selection and a trusted native helper executable.
#[derive(Clone, Debug, Default)]
pub(crate) struct Request {
    /// Bypass name-based inventory when an exact address was supplied.
    pub(crate) address: Option<BluetoothAddress>,
    /// Use this executable for both paired-device inventory and later opening.
    pub(crate) helper: Option<PathBuf>,
}

/// Resolve metadata without opening a radio or sending protocol traffic.
///
/// Automatic selection requires one unique recognized paired device. The
/// bounded native inventory runs on a blocking worker that is always joined,
/// including after cancellation. Callers must retain this future until it
/// completes; dropping it would abandon its worker's ownership. Sticky
/// cancellation is forwarded to the helper and blocks late endpoint admission.
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

/// Refuse unsupported platforms before discovery or connection opening.
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

/// Observed remote names, never proof of the radio model or physical continuity.
#[cfg(any(target_os = "macos", test))]
const CANDIDATE_NAMES: [&str; 2] = ["TM-D750", "stm32mp1-ex5240"];

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
