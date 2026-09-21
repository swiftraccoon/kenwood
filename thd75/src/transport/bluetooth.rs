//! TH-D75 Bluetooth selection, fixed-channel opening, and retry policy.
//!
//! The shared transport owns every native object, helper, pipe, and teardown.
//! This wrapper supplies the default device name `TH-D75`, RFCOMM channel 2,
//! one retry on a selected open, and reopening pinned to the address resolved
//! by the first successful selection. Reopening restores the endpoint only; the
//! radio has answered no CAT command until the caller identifies it.

use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use kenwood_transport::bluetooth::{
    BluetoothAddress, BluetoothDeviceName, BluetoothDeviceSelector, BluetoothOpenCancellation,
    BluetoothService, BluetoothTransport as NativeBluetoothTransport, PairedBluetoothDevice,
    RfcommChannel,
};
use kenwood_transport::error::{BluetoothCloseFailure, BluetoothOpenStage};
use kenwood_transport::{Transport, TransportError};

/// TH-D75 endpoint policy around the shared native RFCOMM transport.
#[derive(Debug)]
pub struct BluetoothTransport {
    inner: NativeBluetoothTransport,
    address: BluetoothAddress,
    helper_executable: PathBuf,
}

impl BluetoothTransport {
    /// Open the default TH-D75 name or an explicit name/address selector.
    ///
    /// Retries once, after a one-second wait and with a fresh helper process,
    /// when the first attempt returns [`TransportError::NotFound`], or returns
    /// [`TransportError::BluetoothOpen`] or a
    /// [`TransportError::BluetoothOpenWithCleanup`] whose cleanup is
    /// [`BluetoothCloseFailure::ChannelUnconfirmed`], and whose stage is
    /// neither [`BluetoothOpenStage::ServiceResolution`] nor
    /// [`BluetoothOpenStage::StartupDeadline`]. Any other close failure, helper
    /// launch failure or cancellation returns immediately. Each attempt is
    /// bounded by the shared transport's opening deadline.
    ///
    /// # Errors
    ///
    /// Returns selector, helper, native-open, or cancellation failures.
    pub fn open(device_name: Option<&str>) -> Result<Self, TransportError> {
        Self::open_with_helper_executable(device_name, executable()?)
    }

    /// Open through an explicit absolute signed helper executable.
    ///
    /// # Errors
    ///
    /// Returns the errors described by [`Self::open`], including invalid paths.
    pub fn open_with_helper_executable(
        device_name: Option<&str>,
        helper_executable: impl AsRef<Path>,
    ) -> Result<Self, TransportError> {
        Self::open_with_helper_executable_cancellable(
            device_name,
            helper_executable,
            &BluetoothOpenCancellation::default(),
        )
    }

    /// Open the selected TH-D75 with sticky synchronous cancellation.
    ///
    /// # Errors
    ///
    /// Returns ordinary open failures or an explicit cancellation error.
    pub fn open_with_helper_executable_cancellable(
        device_name: Option<&str>,
        helper_executable: impl AsRef<Path>,
        cancellation: &BluetoothOpenCancellation,
    ) -> Result<Self, TransportError> {
        let name = device_name.unwrap_or("TH-D75");
        let selector = match name.parse::<BluetoothAddress>() {
            Ok(address) => BluetoothDeviceSelector::Address(address),
            Err(_) => BluetoothDeviceSelector::Name(BluetoothDeviceName::new(name)?),
        };
        Self::open_selected(&selector, helper_executable.as_ref(), cancellation, true)
    }

    /// Open an exact previously enumerated device with the selected-open retry.
    ///
    /// # Errors
    ///
    /// Returns bounded native/helper errors without substituting another address.
    pub fn open_paired_device_with_helper_executable(
        device: &PairedBluetoothDevice,
        helper_executable: impl AsRef<Path>,
    ) -> Result<Self, TransportError> {
        Self::open_paired_device_with_helper_executable_cancellable(
            device,
            helper_executable,
            &BluetoothOpenCancellation::default(),
        )
    }

    /// Open one exact enumerated device with cancellation and at most one retry.
    ///
    /// # Errors
    ///
    /// Returns native/helper errors or a sticky cancellation error.
    pub fn open_paired_device_with_helper_executable_cancellable(
        device: &PairedBluetoothDevice,
        helper_executable: impl AsRef<Path>,
        cancellation: &BluetoothOpenCancellation,
    ) -> Result<Self, TransportError> {
        Self::open_selected(
            &BluetoothDeviceSelector::Address(device.address().clone()),
            helper_executable.as_ref(),
            cancellation,
            true,
        )
    }

    /// Probe one exact enumerated device with one native-open attempt only.
    ///
    /// # Errors
    ///
    /// Returns native-open or helper errors; this method never retries.
    pub fn probe_paired_device_with_helper_executable(
        device: &PairedBluetoothDevice,
        helper_executable: impl AsRef<Path>,
    ) -> Result<Self, TransportError> {
        Self::probe_paired_device_with_helper_executable_cancellable(
            device,
            helper_executable,
            &BluetoothOpenCancellation::default(),
        )
    }

    /// Probe one exact enumerated device with cancellation and no retry.
    ///
    /// # Errors
    ///
    /// Returns bounded open failures or a sticky cancellation error.
    pub fn probe_paired_device_with_helper_executable_cancellable(
        device: &PairedBluetoothDevice,
        helper_executable: impl AsRef<Path>,
        cancellation: &BluetoothOpenCancellation,
    ) -> Result<Self, TransportError> {
        Self::open_selected(
            &BluetoothDeviceSelector::Address(device.address().clone()),
            helper_executable.as_ref(),
            cancellation,
            false,
        )
    }

    fn open_selected(
        selector: &BluetoothDeviceSelector,
        helper_executable: &Path,
        cancellation: &BluetoothOpenCancellation,
        selected_retry: bool,
    ) -> Result<Self, TransportError> {
        let channel = RfcommChannel::new(2)?;
        let inner = open_with_retry(
            selected_retry,
            || {
                NativeBluetoothTransport::open_with_helper_executable(
                    selector,
                    BluetoothService::FixedChannel(channel),
                    helper_executable,
                    cancellation,
                )
            },
            || wait_retry(cancellation),
        )?;
        Ok(Self {
            address: inner.address().clone(),
            inner,
            helper_executable: helper_executable.to_owned(),
        })
    }
}

impl Transport for BluetoothTransport {
    async fn write(&mut self, bytes: &[u8]) -> Result<(), TransportError> {
        self.inner.write(bytes).await
    }

    async fn read(&mut self, bytes: &mut [u8]) -> Result<usize, TransportError> {
        self.inner.read(bytes).await
    }

    async fn close(&mut self) -> Result<(), TransportError> {
        self.inner.close().await
    }

    async fn reopen(&mut self) -> Result<(), TransportError> {
        let closed = self.close().await;
        *self = reopen_after_cleanup(
            &self.address,
            &self.helper_executable,
            closed,
            |selector, helper| {
                Self::open_selected(
                    selector,
                    helper,
                    &BluetoothOpenCancellation::default(),
                    true,
                )
            },
        )?;
        Ok(())
    }
}

fn reopen_after_cleanup<T>(
    address: &BluetoothAddress,
    helper: &Path,
    closed: Result<(), TransportError>,
    open: impl FnOnce(&BluetoothDeviceSelector, &Path) -> Result<T, TransportError>,
) -> Result<T, TransportError> {
    // Reopen always selects the exact address resolved by the first successful
    // open, so a display name reassigned to another device is never followed.
    let selector = BluetoothDeviceSelector::Address(address.clone());
    if let Err(error) = closed {
        if !matches!(error, TransportError::BluetoothClose { .. }) {
            return Err(error);
        }
        // The previous helper was killed, so the RFCOMM channel may not have
        // closed cleanly. Reopening is still safe: the shared helper lease
        // refuses a new launch until the old helper has been reaped.
        tracing::warn!(error = %error, selector = selector.as_str(),
            "reopening TH-D75 endpoint after unconfirmed native cleanup");
    }
    open(&selector, helper)
}

fn executable() -> Result<PathBuf, TransportError> {
    std::env::current_exe().map_err(|source| TransportError::BluetoothHelper {
        context: "locating the current executable".to_owned(),
        source,
    })
}

fn wait_retry(cancellation: &BluetoothOpenCancellation) -> Result<(), TransportError> {
    let deadline = Instant::now() + Duration::from_secs(1);
    loop {
        if cancellation.is_cancelled() {
            return Err(TransportError::BluetoothOpenInterrupted);
        }
        let remaining = deadline.saturating_duration_since(Instant::now());
        if remaining.is_zero() {
            return Ok(());
        }
        std::thread::sleep(remaining.min(Duration::from_millis(5)));
    }
}

fn open_with_retry<T>(
    selected_retry: bool,
    mut open: impl FnMut() -> Result<T, TransportError>,
    wait: impl FnOnce() -> Result<(), TransportError>,
) -> Result<T, TransportError> {
    match open() {
        Err(error) if selected_retry && fixed_channel_retry_eligible(&error) => {
            tracing::warn!(error = %error,
                "TH-D75 selected Bluetooth open failed; waiting before one fresh-helper retry");
            wait()?;
            open()
        }
        result => result,
    }
}

// Retry-eligible failures: device not found, and every opening stage except
// `StartupDeadline` and `ServiceResolution`, provided any reported channel
// cleanup is `ChannelUnconfirmed` (the helper has already been reaped, so a
// fresh launch is safe). `ServiceResolution` is reported only by fresh-SDP
// selectors, never by this fixed-channel path.
const fn fixed_channel_retry_eligible(error: &TransportError) -> bool {
    let stage = match error {
        TransportError::NotFound => return true,
        TransportError::BluetoothOpen { stage }
        | TransportError::BluetoothOpenWithCleanup {
            stage,
            cleanup: BluetoothCloseFailure::ChannelUnconfirmed,
        } => stage,
        _ => return false,
    };
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

#[cfg(test)]
mod tests {
    use super::*;

    type TestResult = Result<(), Box<dyn std::error::Error>>;

    #[tokio::test]
    async fn framed_native_cleanup_retries_selected_open_but_not_probe() -> TestResult {
        use kenwood_transport::error::BluetoothCloseFailure;
        // This is the only fixture in this test module that reserves the real
        // process-global helper lease. Its two cases run serially through reap.
        for selected_retry in [true, false] {
            let fixture = FailureHelperFixture::new()?;
            let selector = BluetoothDeviceSelector::Address("00-11-22-33-44-55".parse()?);
            let result = BluetoothTransport::open_selected(
                &selector,
                &fixture.helper,
                &BluetoothOpenCancellation::default(),
                selected_retry,
            );
            if selected_retry {
                let mut transport = result?;
                assert_eq!(transport.address.as_str(), selector.as_str());
                transport.close().await?;
                assert_eq!(
                    std::fs::read_to_string(&fixture.launches)?,
                    "launch\nlaunch\n"
                );
            } else {
                assert!(matches!(
                    result,
                    Err(TransportError::BluetoothOpenWithCleanup {
                        stage: BluetoothOpenStage::RfcommDeadline,
                        cleanup: BluetoothCloseFailure::ChannelUnconfirmed,
                    })
                ));
                assert_eq!(std::fs::read_to_string(&fixture.launches)?, "launch\n");
            }
        }
        Ok(())
    }

    /// Executable protocol fixture; no native or Bluetooth APIs are linked.
    struct FailureHelperFixture {
        directory: PathBuf,
        helper: PathBuf,
        launches: PathBuf,
        first: PathBuf,
    }

    impl FailureHelperFixture {
        fn new() -> Result<Self, Box<dyn std::error::Error>> {
            use std::io::Write as _;
            use std::os::unix::fs::{DirBuilderExt as _, OpenOptionsExt as _};
            let nonce = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)?
                .as_nanos();
            let directory = std::env::temp_dir().join(format!(
                "kenwood-d75-open-failure-{}-{nonce}",
                std::process::id()
            ));
            std::fs::DirBuilder::new().mode(0o700).create(&directory)?;
            let fixture = Self {
                helper: directory.join("helper"),
                launches: directory.join("helper.launches"),
                first: directory.join("helper.first"),
                directory,
            };
            let mut file = std::fs::OpenOptions::new()
                .write(true)
                .create_new(true)
                .mode(0o700)
                .open(&fixture.helper)?;
            file.write_all(
                br#"#!/bin/sh
[ "$#" -eq 1 ] && [ "$1" = '--kenwood-bluetooth-helper' ] || exit 122
[ "$KENWOOD_BT_HELPER_PROCESS_V2" = '4d7f29c8b35a' ] || exit 122
[ "$KENWOOD_BT_HELPER_DEVICE" = '00-11-22-33-44-55' ] || exit 122
[ "$KENWOOD_BT_HELPER_CHANNEL" = '2' ] || exit 122
[ "$KENWOOD_BT_HELPER_LIVENESS_FD" = '3' ] || exit 122
printf 'launch\n' >> "$0.launches"
if [ ! -f "$0.first" ]; then
    : > "$0.first"
    printf 'KENWBT-ERROR-v1!\153\001'
    exit 89
fi
printf 'KENWBT-READY-v2!00-11-22-33-44-55\002'
if IFS= read -r unexpected; then exit 123; fi
exit 0
"#,
            )?;
            Ok(fixture)
        }
    }

    impl Drop for FailureHelperFixture {
        fn drop(&mut self) {
            let _helper = std::fs::remove_file(&self.helper);
            let _launches = std::fs::remove_file(&self.launches);
            let _first = std::fs::remove_file(&self.first);
            let _directory = std::fs::remove_dir(&self.directory);
        }
    }

    #[test]
    fn fixed_channel_native_stages_preserve_selected_retry_but_never_probe_retry() {
        use kenwood_transport::error::BluetoothOpenStage;
        for stage in [
            BluetoothOpenStage::ContextAllocation,
            BluetoothOpenStage::SdpStart,
            BluetoothOpenStage::SdpCompletion,
            BluetoothOpenStage::SdpDeadline,
            BluetoothOpenStage::RfcommStart,
            BluetoothOpenStage::RfcommCompletion,
            BluetoothOpenStage::RfcommDeadline,
            BluetoothOpenStage::RfcommEndpoint,
        ] {
            for retry in [false, true] {
                let mut attempts = 0;
                let mut waits = 0;
                let result = open_with_retry::<()>(
                    retry,
                    || {
                        attempts += 1;
                        Err(TransportError::BluetoothOpen { stage })
                    },
                    || {
                        waits += 1;
                        Ok(())
                    },
                );
                assert!(
                    matches!(result, Err(TransportError::BluetoothOpen { stage: actual }) if actual == stage)
                );
                assert_eq!(attempts, if retry { 2 } else { 1 });
                assert_eq!(waits, usize::from(retry));
            }
        }
    }

    #[test]
    fn combined_cleanup_does_not_broaden_retry_authority() {
        for (stage, cleanup) in [
            (
                BluetoothOpenStage::StartupDeadline,
                BluetoothCloseFailure::ChannelUnconfirmed,
            ),
            (
                BluetoothOpenStage::ServiceResolution,
                BluetoothCloseFailure::ChannelUnconfirmed,
            ),
            (
                BluetoothOpenStage::RfcommDeadline,
                BluetoothCloseFailure::ForcedTermination,
            ),
            (
                BluetoothOpenStage::RfcommDeadline,
                BluetoothCloseFailure::ReapPending,
            ),
            (
                BluetoothOpenStage::RfcommDeadline,
                BluetoothCloseFailure::HelperExited { code: Some(89) },
            ),
        ] {
            let mut attempts = 0;
            let result = open_with_retry::<()>(
                true,
                || {
                    attempts += 1;
                    Err(TransportError::BluetoothOpenWithCleanup { stage, cleanup })
                },
                || Err(TransportError::NotFound),
            );
            assert!(
                matches!(result, Err(TransportError::BluetoothOpenWithCleanup {
                stage: actual_stage, cleanup: actual_cleanup,
            }) if actual_stage == stage && actual_cleanup == cleanup)
            );
            assert_eq!(attempts, 1);
        }
    }

    #[test]
    fn recovery_preserves_exact_selector_and_helper_after_unproved_cleanup() -> TestResult {
        use kenwood_transport::error::BluetoothCloseFailure;
        let address: BluetoothAddress = "00-11-22-33-44-55".parse()?;
        let selector = BluetoothDeviceSelector::Address(address.clone());
        let helper = Path::new("/absolute/qualified-helper");
        let recovered = reopen_after_cleanup(
            &address,
            helper,
            Err(TransportError::BluetoothClose {
                failure: BluetoothCloseFailure::ForcedTermination,
            }),
            |actual, executable| {
                assert_eq!(actual, &selector);
                assert_eq!(executable, helper);
                Ok(42)
            },
        )?;
        assert_eq!(recovered, 42);
        let busy = reopen_after_cleanup::<()>(
            &address,
            helper,
            Err(TransportError::BluetoothClose {
                failure: BluetoothCloseFailure::ReapPending,
            }),
            |_, _| {
                Err(TransportError::BluetoothHelper {
                    context: "reserving the process slot".to_owned(),
                    source: std::io::Error::from(std::io::ErrorKind::WouldBlock),
                })
            },
        );
        assert!(
            matches!(busy, Err(TransportError::BluetoothHelper { source, .. }) if source.kind() == std::io::ErrorKind::WouldBlock)
        );
        Ok(())
    }

    #[test]
    fn selected_open_retries_once_but_probe_and_other_errors_do_not() -> TestResult {
        for retry in [false, true] {
            let mut attempts = 0;
            let mut waits = 0;
            let result = open_with_retry::<()>(
                retry,
                || {
                    attempts += 1;
                    Err(TransportError::NotFound)
                },
                || {
                    waits += 1;
                    Ok(())
                },
            );
            assert!(matches!(result, Err(TransportError::NotFound)));
            assert_eq!(attempts, if retry { 2 } else { 1 });
            assert_eq!(waits, usize::from(retry));
        }
        for error in [
            TransportError::BluetoothDeviceNameAmbiguous,
            TransportError::BluetoothOpenInterrupted,
            TransportError::BluetoothOpen {
                stage: BluetoothOpenStage::ServiceResolution,
            },
            TransportError::BluetoothClose {
                failure: BluetoothCloseFailure::ChannelUnconfirmed,
            },
        ] {
            let mut attempts = 0;
            let mut original = Some(error);
            let result = open_with_retry::<()>(
                true,
                || {
                    attempts += 1;
                    Err(original.take().unwrap_or(TransportError::NotFound))
                },
                || Err(TransportError::NotFound),
            );
            assert!(result.is_err());
            assert_eq!(attempts, 1);
        }
        let mut attempts = 0;
        let result = open_with_retry(
            true,
            || {
                attempts += 1;
                if attempts == 1 {
                    Err(TransportError::NotFound)
                } else {
                    Ok(42)
                }
            },
            || Ok(()),
        )?;
        assert_eq!(result, 42);
        assert_eq!(attempts, 2);
        Ok(())
    }

    #[test]
    fn success_never_waits_and_cancellation_during_retry_prevents_second_open() -> TestResult {
        let mut waits = 0;
        assert_eq!(
            open_with_retry(
                true,
                || Ok(42),
                || {
                    waits += 1;
                    Ok(())
                }
            )?,
            42
        );
        assert_eq!(waits, 0);
        let cancellation = BluetoothOpenCancellation::default();
        let mut attempts = 0;
        let result = open_with_retry::<()>(
            true,
            || {
                attempts += 1;
                Err(TransportError::NotFound)
            },
            || {
                cancellation.cancel();
                wait_retry(&cancellation)
            },
        );
        assert!(matches!(
            result,
            Err(TransportError::BluetoothOpenInterrupted)
        ));
        assert_eq!(attempts, 1);
        Ok(())
    }

    #[test]
    fn default_and_exact_device_opens_remain_cancellable_before_launch() {
        let cancellation = BluetoothOpenCancellation::default();
        cancellation.cancel();
        for selector in [None, Some("00-11-22-33-44-55"), Some("Field Radio")] {
            let result = BluetoothTransport::open_with_helper_executable_cancellable(
                selector,
                "/nonexistent/kenwood-helper",
                &cancellation,
            );
            assert!(matches!(
                result,
                Err(TransportError::BluetoothOpenInterrupted)
            ));
        }
    }
}
