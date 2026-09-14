//! Model-neutral native Bluetooth endpoints and isolated macOS RFCOMM transport.
//!
//! Opens `IOBluetoothRFCOMMChannel` directly, without the OS serial-device
//! layer. Protocol and model qualification remain the caller's responsibility.
//!
//! `IOBluetooth` writes can block forever when RFCOMM flow-control credit
//! stalls, including its nominally asynchronous API (which blocks the main
//! dispatch queue later). The framework therefore runs in a killable helper
//! process. The parent communicates with it through non-blocking raw byte
//! pipes and never calls an `IOBluetooth` write or close routine itself.
//!
//! Each construction makes one bounded attempt. Model-specific retry and
//! recovery admission belongs to the caller. Neither the device's baseband
//! nor any system Bluetooth process is torn down as part of open or cleanup.
//!
//! This module requires `native-bluetooth`. Its validated selectors are
//! portable; `BluetoothTransport` exists only on macOS. Start with
//! [`BluetoothAddress`] for offline normalization/validation and
//! [`BluetoothService`] for explicit service policy. Native callers must also
//! follow the transport type's process-wide exclusivity and cleanup contracts.

mod types;
pub use types::{
    BluetoothAddress, BluetoothDeviceName, BluetoothDeviceSelector, BluetoothOpenCancellation,
    BluetoothService, PairedBluetoothDevice, RfcommChannel,
};

#[cfg(target_os = "macos")]
#[expect(
    unsafe_code,
    reason = "The macOS transport uses a small audited C ABI to anchor the Objective-C constructor, configure pipe flags, and install the child's liveness descriptor. Each unsafe call documents its ownership or fd invariant."
)]
mod inner {
    use std::io::{self, Read as _, Write as _};
    use std::os::fd::{AsRawFd as _, FromRawFd as _, OwnedFd};
    use std::os::unix::process::CommandExt as _;
    use std::path::{Path, PathBuf};
    use std::process::{Child, ChildStdin, ChildStdout, Command, ExitStatus, Stdio};
    use std::sync::{
        Mutex,
        atomic::{AtomicBool, Ordering},
        mpsc,
    };
    use std::time::{Duration, Instant};

    use super::{
        BluetoothAddress, BluetoothDeviceSelector, BluetoothOpenCancellation, BluetoothService,
        PairedBluetoothDevice, RfcommChannel,
    };
    use crate::error::{BluetoothCloseFailure, BluetoothOpenStage};
    use crate::{Transport, TransportError};

    unsafe extern "C" {
        fn bt_helper_link_anchor();
        #[cfg(test)]
        fn bt_device_identifier_matches_display_name(
            identifier: *const std::ffi::c_char,
            display_name: *const std::ffi::c_char,
        ) -> i32;
        fn bt_fd_set_nonblocking(fd: i32) -> i32;
        fn bt_liveness_pipe_create(read_fd: *mut i32, write_fd: *mut i32) -> i32;
        fn bt_helper_prepare_liveness_fd(source_fd: i32, target_fd: i32) -> i32;
    }

    /// Private launch sentinel recognized by the Objective-C constructor
    /// before the selected helper reaches ordinary `main`.
    const HELPER_SENTINEL_ENV: &str = "KENWOOD_BT_HELPER_PROCESS_V2";
    const HELPER_SENTINEL_VALUE: &str = "4d7f29c8b35a";
    const HELPER_DEVICE_ENV: &str = "KENWOOD_BT_HELPER_DEVICE";
    const HELPER_CHANNEL_ENV: &str = "KENWOOD_BT_HELPER_CHANNEL";
    const HELPER_CONTROL_ENV: &str = "KENWOOD_BT_HELPER_CONTROL_MODE";
    const HELPER_PAIRED_CONTROL_MODE: &str = "paired";
    const HELPER_TEST_ENV: &str = "KENWOOD_BT_HELPER_TEST_MODE";
    const HELPER_LIVENESS_FD_ENV: &str = "KENWOOD_BT_HELPER_LIVENESS_FD";
    const HELPER_LIVENESS_FD: i32 = 3;

    /// Prefix emitted by the helper after RFCOMM is open and before it
    /// enables radio ingress on stdout.
    const HELPER_READY_MAGIC: &[u8; 16] = b"KENWBT-READY-v2!";

    /// Failed-open prefix followed by one stage byte and one cleanup byte.
    const HELPER_OPEN_FAILURE_MAGIC: &[u8; 16] = b"KENWBT-ERROR-v1!";

    /// Maximum time one helper attempt waits for RFCOMM open.
    /// Two seconds of scheduling margin sit above the native single
    /// 20-second SDP/baseband/channel-open deadline.
    const HELPER_OPEN_TIMEOUT: Duration = Duration::from_secs(22);

    /// Cold App Sandbox initialization of `IOBluetooth` can exceed five
    /// seconds before `pairedDevices` returns. Give the signed helper's
    /// ready/list/exit observation cycle the same budget as one RFCOMM open;
    /// synchronous launch and additional cleanup are separate.
    const HELPER_ENUMERATION_TIMEOUT: Duration = Duration::from_secs(22);

    /// The no-radio helper packaging probe performs only process launch,
    /// constructor dispatch, and one short pipe echo.
    const HELPER_VALIDATION_TIMEOUT: Duration = Duration::from_secs(5);

    /// Sentinel-gated native helper mode used only to validate packaging and
    /// process/pipe lifecycle without consulting `IOBluetooth`.
    const HELPER_ECHO_TEST_MODE: &str = "echo-v1";

    /// Fixed challenge proving that both helper pipe directions are live.
    const HELPER_VALIDATION_CHALLENGE: &[u8] = b"AZIMUTH-BT-HELPER-v1";

    /// Maximum paired records accepted from one signed helper invocation.
    const MAX_PAIRED_DEVICES: usize = 64;

    /// Bluetooth names are normally limited to 248 bytes. This larger bound
    /// tolerates framework formatting while keeping the helper payload finite.
    const MAX_PAIRED_DISPLAY_NAME_BYTES: usize = 1024;

    /// Four length bytes, one exact address, and one bounded display name per
    /// record, followed by the four-byte terminator.
    const MAX_PAIRED_PAYLOAD_BYTES: usize =
        MAX_PAIRED_DEVICES * (4 + 17 + MAX_PAIRED_DISPLAY_NAME_BYTES) + 4;

    /// Native helper exit for a display name shared by multiple paired radios.
    const HELPER_EXIT_AMBIGUOUS_DEVICE_NAME: i32 = 87;

    /// Native helper exit when the paired-device set exceeds the wire bound.
    const HELPER_EXIT_TOO_MANY_PAIRED_DEVICES: i32 = 88;

    /// Poll cadence for non-blocking helper pipes.
    const PIPE_POLL_INTERVAL: Duration = Duration::from_millis(5);

    /// Maximum time to reap a helper after its stdout has already reached EOF.
    ///
    /// Pipe EOF can become observable just before `try_wait` publishes the
    /// process exit status. Waiting briefly preserves the native exit-code 71
    /// classification without allowing a helper that merely closed stdout to
    /// stall construction indefinitely.
    const HELPER_EOF_EXIT_BUDGET: Duration = Duration::from_millis(250);

    /// Deadline checked when a direct transport write encounters pipe
    /// backpressure. Successful non-blocking chunks do not yield or recheck
    /// this clock; this is not a preemptive whole-call wall-clock ceiling.
    const PIPE_WRITE_TIMEOUT: Duration = Duration::from_secs(5);

    /// POSIX guarantees atomic non-blocking pipe writes through `PIPE_BUF`;
    /// macOS reports 512 bytes; chunking accepts arbitrary logical frames.
    const MACOS_PIPE_BUF: usize = 512;

    /// Healthy helpers get a short EOF-driven graceful-close opportunity
    /// after the parent drops both pipes.
    const GRACEFUL_EXIT_BUDGET: Duration = Duration::from_millis(600);

    /// Maximum synchronous time spent checking that SIGKILL reaped the
    /// helper. A detached waiter owns the child after this additional bound.
    const SYNC_REAP_BUDGET: Duration = Duration::from_millis(100);

    /// Preserve one native helper owner per process, independent of the
    /// selected address. This host policy also covers inventory and validation;
    /// it is not a claim about every device's SPP capacity.
    static HELPER_PROCESS_SLOT_RESERVED: AtomicBool = AtomicBool::new(false);

    /// Native macOS Bluetooth transport using an isolated `IOBluetooth` helper.
    ///
    /// Available only on macOS with the `native-bluetooth` feature. Opening
    /// selects a paired address/service, not a model or protocol. The helper
    /// owns all native objects; this parent owns its pipes and process lifetime.
    ///
    /// # Process-wide exclusivity
    ///
    /// Every open, paired-device inventory and helper-launch validation shares
    /// one process-global lease, including operations for different addresses.
    /// A live transport holds it until its helper is reaped. Deferred cleanup
    /// retains the lease in its reaper, even after the transport is dropped.
    /// Overlap fails with [`TransportError::BluetoothHelper`] whose source kind
    /// is [`io::ErrorKind::WouldBlock`]; it does not wait or try another helper.
    /// Discover and validate before opening, never alongside a held connection.
    ///
    /// # I/O completion and cancellation
    ///
    /// A successful write means all bytes entered the helper's stdin pipe.
    /// It does not acknowledge an RFCOMM completion, peer receipt or a radio
    /// command. [`crate::StreamAdapter`] flush retains exactly this boundary.
    /// Protocol responses/readback are separate caller obligations. Pipe
    /// backpressure is checked against a five-second write deadline; this is
    /// not a preemptive bound on uninterrupted synchronous progress.
    /// Canceling a pending write invalidates and retires the helper because an
    /// uncertain prefix may have been sent. A fresh owner and model-specific
    /// recovery policy are needed before another exchange, not a blind retry.
    ///
    /// Reads have no intrinsic deadline and honor [`Transport::read`]'s
    /// cancellation contract. EOF is a read error with `UnexpectedEof`, not
    /// `Ok(0)` for a nonempty buffer. Empty reads return zero and empty writes
    /// succeed even after close; neither is a connection-health probe.
    ///
    /// # Execution and cleanup
    ///
    /// Open and inventory are synchronous. Async callers must use a retained,
    /// joined blocking worker and keep its cancellation token. A returned
    /// owner must still be closed if the caller no longer wants its result.
    /// Canceling or dropping a worker's join handle is not native retirement.
    ///
    /// [`Transport::close`] is async-shaped but performs synchronous helper
    /// teardown without yielding: a 600 ms graceful wait, then up to 100 ms
    /// of synchronous reap checking when necessary. OS calls and scheduling
    /// are not a real-time guarantee. An async timeout cannot interrupt that
    /// poll. Drop can perform the same work, so do not rely on another task on
    /// a single-thread executor to remain responsive during cleanup.
    ///
    /// Explicit close retains its first outcome, including failure, for later
    /// calls. Preserve it separately from I/O errors. `ChannelUnconfirmed`,
    /// forced termination and pending reaping are not clean native closure;
    /// reaping alone is not proof that the OS canceled its Bluetooth operation.
    /// Drop is best-effort and supplies no successful-close observation.
    pub struct BluetoothTransport {
        child: Option<Child>,
        helper_stdin: Option<ChildStdin>,
        helper_stdout: Option<ChildStdout>,
        /// Parent-owned write end of a dedicated liveness pipe. The helper's
        /// watchdog exits the process if this end disappears, even when its
        /// main thread is wedged inside `IOBluetooth`.
        parent_liveness: Option<OwnedFd>,
        /// Cleared synchronously by every failed/cancelled write guard and by
        /// EOF/close, so a killed helper cannot look reusable before reap.
        helper_healthy: bool,
        /// Held until this helper has exited (including by the detached
        /// reaper), preventing two helpers from competing for one SPP channel.
        process_slot: Option<HelperProcessSlot>,
        /// Exact address and channel reported by the successful native open.
        address: BluetoothAddress,
        channel: RfcommChannel,
        /// Retain failed cleanup evidence after the child leaves this owner.
        close_outcome: Option<Result<(), BluetoothCloseFailure>>,
        /// Signed executable that hosts the killable native helper. Apps in
        /// App Sandbox use a separately signed inheriting helper; command-line
        /// clients use their current executable.
        helper_executable: PathBuf,
    }

    impl std::fmt::Debug for BluetoothTransport {
        fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
            formatter
                .debug_struct("BluetoothTransport")
                .field("helper_pid", &self.child.as_ref().map(Child::id))
                .field("helper_healthy", &self.helper_healthy)
                .field("address", &self.address)
                .field("channel", &self.channel)
                .field("helper_executable", &self.helper_executable)
                .finish_non_exhaustive()
        }
    }

    fn validate_helper_executable(path: &Path) -> Result<PathBuf, TransportError> {
        if path.is_absolute() {
            Ok(path.to_path_buf())
        } else {
            Err(bluetooth_helper_error(
                format!("validating executable path {}", path.display()),
                io::Error::new(
                    io::ErrorKind::InvalidInput,
                    "Bluetooth helper executable path must be absolute",
                ),
            ))
        }
    }

    fn new_helper_command(
        helper_executable: &Path,
        device_name: &str,
        service: BluetoothService,
    ) -> Command {
        let channel = match service {
            BluetoothService::FixedChannel(channel) => channel.get().to_string(),
            BluetoothService::SerialPort => "spp".to_owned(),
        };
        let mut command = Command::new(helper_executable);
        let _command = command
            .arg("--kenwood-bluetooth-helper")
            .env(HELPER_SENTINEL_ENV, HELPER_SENTINEL_VALUE)
            .env(HELPER_DEVICE_ENV, device_name)
            .env(HELPER_CHANNEL_ENV, channel)
            .env(HELPER_LIVENESS_FD_ENV, HELPER_LIVENESS_FD.to_string())
            .env_remove(HELPER_CONTROL_ENV)
            .env_remove(HELPER_TEST_ENV)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::inherit());
        command
    }

    fn new_helper_control_command(helper_executable: &Path, mode: &str) -> Command {
        let mut command = Command::new(helper_executable);
        let _command = command
            .arg("--kenwood-bluetooth-helper-control")
            .env(HELPER_SENTINEL_ENV, HELPER_SENTINEL_VALUE)
            .env(HELPER_CONTROL_ENV, mode)
            .env(HELPER_LIVENESS_FD_ENV, HELPER_LIVENESS_FD.to_string())
            .env_remove(HELPER_DEVICE_ENV)
            .env_remove(HELPER_CHANNEL_ENV)
            .env_remove(HELPER_TEST_ENV)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::inherit());
        command
    }

    fn new_helper_test_command(helper_executable: &Path, mode: &str) -> Command {
        let mut command = Command::new(helper_executable);
        let _command = command
            .arg("--kenwood-bluetooth-helper-test")
            .env(HELPER_SENTINEL_ENV, HELPER_SENTINEL_VALUE)
            .env(HELPER_TEST_ENV, mode)
            .env(HELPER_LIVENESS_FD_ENV, HELPER_LIVENESS_FD.to_string())
            .env_remove(HELPER_CONTROL_ENV)
            .env_remove(HELPER_DEVICE_ENV)
            .env_remove(HELPER_CHANNEL_ENV)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::inherit());
        command
    }

    fn bluetooth_helper_error(context: impl Into<String>, source: io::Error) -> TransportError {
        TransportError::BluetoothHelper {
            context: context.into(),
            source,
        }
    }

    impl BluetoothTransport {
        /// Validate one signed helper's launch and bidirectional pipe lifecycle.
        ///
        /// This runs the helper's private sentinel-gated `echo-v1` mode with a
        /// fixed challenge. It verifies constructor dispatch, readiness
        /// framing, both pipe directions, clean exit, and bounded teardown. It
        /// deliberately does not initialize `IOBluetooth`, enumerate paired
        /// devices, or open a radio, so packaging validation is independent of
        /// ambient Bluetooth state.
        /// This synchronous call requires the type's [exclusive helper
        /// lease](BluetoothTransport#process-wide-exclusivity), so it cannot
        /// run while a native connection or another helper operation is held.
        /// Readiness and echo polling use a five-second deadline after launch.
        /// EOF can be followed by a separate 250 ms exit-observation wait;
        /// failure cleanup also has its own bounds. Process launch and the
        /// short blocking challenge write are not preempted by this deadline,
        /// so it is not a five-second whole-call wall-clock guarantee.
        ///
        /// # Errors
        ///
        /// Returns [`TransportError::BluetoothHelper`] if the path is relative,
        /// the helper cannot launch, its readiness or echo is invalid, it does
        /// not exit cleanly, or a readiness/echo/exit observation deadline expires.
        /// A busy process-wide lease is a helper error with source kind
        /// [`io::ErrorKind::WouldBlock`].
        pub fn validate_helper_launch_with_executable(
            helper_executable: impl AsRef<Path>,
        ) -> Result<(), TransportError> {
            let helper_executable = validate_helper_executable(helper_executable.as_ref())?;
            validate_helper_launch(&helper_executable, HELPER_VALIDATION_TIMEOUT)
        }

        /// Enumerate paired Bluetooth devices for later exact qualification.
        ///
        /// Discovery runs in the same isolated native helper used for RFCOMM,
        /// but it performs no radio I/O. Each returned device carries the
        /// exact Bluetooth address required for unambiguous later selection.
        /// The helper invocation, record count, field sizes, and total payload
        /// are independently bounded. Repeated records with the same normalized
        /// address and byte-identical display name coalesce in first-seen order;
        /// conflicting names remain invalid. The record bound applies before
        /// coalescing, including every repeated record.
        /// This synchronous operation has a 22-second discovery deadline plus
        /// bounded cleanup. Use a joined blocking worker in async code. It
        /// requires the type's [exclusive helper lease](BluetoothTransport#process-wide-exclusivity);
        /// perform inventory before opening a connection.
        ///
        /// # Errors
        ///
        /// Returns [`TransportError::BluetoothHelper`] if the current
        /// executable cannot be located, the helper cannot be launched, the
        /// discovery deadline expires, or its framed response is invalid.
        /// A busy process-wide lease uses source kind [`io::ErrorKind::WouldBlock`].
        pub fn paired_devices() -> Result<Vec<PairedBluetoothDevice>, TransportError> {
            let executable = std::env::current_exe().map_err(|source| {
                bluetooth_helper_error("locating the current executable", source)
            })?;
            Self::paired_devices_with_helper_executable(executable)
        }

        /// Enumerate paired devices through a specific signed helper.
        ///
        /// Sandboxed applications should pass their separately signed,
        /// sandbox-inheriting helper executable. The path must be absolute and
        /// the executable must contain this crate's native helper constructor.
        /// This operation does not open RFCOMM or send any bytes to a radio.
        /// It has the same synchronous execution, 22-second discovery deadline
        /// and process-wide exclusivity as [`Self::paired_devices`].
        ///
        /// # Errors
        ///
        /// Returns [`TransportError::BluetoothHelper`] if the path or helper
        /// lifecycle is invalid, discovery exceeds its bound, or the helper's
        /// device framing is malformed.
        /// Overlap with any live helper uses source kind [`io::ErrorKind::WouldBlock`].
        pub fn paired_devices_with_helper_executable(
            helper_executable: impl AsRef<Path>,
        ) -> Result<Vec<PairedBluetoothDevice>, TransportError> {
            Self::paired_devices_with_helper_executable_cancellable(
                helper_executable,
                &BluetoothOpenCancellation::default(),
            )
        }

        /// Enumerate paired devices with synchronous cancellation.
        ///
        /// This has the same bounds and identity semantics as
        /// [`Self::paired_devices_with_helper_executable`]. A sticky
        /// cancellation request terminates an active helper and returns
        /// [`TransportError::BluetoothOpenInterrupted`].
        /// The blocking worker must still be joined. Cancellation requests do
        /// not themselves prove helper release or free its process-wide lease.
        ///
        /// # Errors
        ///
        /// Returns the ordinary discovery errors or
        /// [`TransportError::BluetoothOpenInterrupted`] when cancelled.
        /// A busy process-wide lease remains a helper error with source kind
        /// [`io::ErrorKind::WouldBlock`].
        pub fn paired_devices_with_helper_executable_cancellable(
            helper_executable: impl AsRef<Path>,
            cancellation: &BluetoothOpenCancellation,
        ) -> Result<Vec<PairedBluetoothDevice>, TransportError> {
            let helper_executable = validate_helper_executable(helper_executable.as_ref())?;
            cancellation.check()?;
            enumerate_paired_devices(&helper_executable, cancellation)
        }

        /// Open one explicitly selected paired device through a fresh helper.
        ///
        /// This performs one attempt, never a scan, retry, or radio command.
        /// Native discovery and opening share a 22-second parent deadline.
        /// Call this synchronous function on a joined blocking task when used
        /// by an async application, and retain its sticky cancellation token.
        /// Its [process-wide lease](BluetoothTransport#process-wide-exclusivity)
        /// excludes simultaneous inventory, validation or another open, even
        /// for a different device. Complete those operations before opening.
        ///
        /// # Errors
        ///
        /// Rejects ambiguous or absent devices, failed service resolution,
        /// invalid helper evidence, cancellation, and bounded startup failure.
        /// [`TransportError::BluetoothOpen`] retains the host-observed opening
        /// stage, not a firmware cause. A complete failure record and matching
        /// reaped helper preserve independent cleanup uncertainty as
        /// [`TransportError::BluetoothOpenWithCleanup`]. An unframed cleanup
        /// exit remains [`TransportError::BluetoothClose`]. No failure is retried.
        /// A busy process-wide lease is a helper error with source kind
        /// [`io::ErrorKind::WouldBlock`].
        pub fn open(
            selector: &BluetoothDeviceSelector,
            service: BluetoothService,
            cancellation: &BluetoothOpenCancellation,
        ) -> Result<Self, TransportError> {
            let executable = std::env::current_exe().map_err(|source| {
                bluetooth_helper_error("locating the current executable", source)
            })?;
            Self::open_with_helper_executable(selector, service, executable, cancellation)
        }

        /// Open one selected endpoint through a separately signed helper.
        ///
        /// The absolute executable must link this native constructor and remain
        /// available for the operation. No model default or retry is inferred.
        /// Exact address selection is checked against the helper's successful
        /// endpoint evidence before any radio bytes are exposed.
        /// Cancellation and the original deadline are checked again before
        /// ownership is returned; late helpers undergo bounded retirement.
        /// This is the same synchronous, process-exclusive operation as
        /// [`Self::open`], including its deadline and joined-worker obligations.
        ///
        /// # Errors
        ///
        /// Returns the same bounded open errors as [`Self::open`], or rejects
        /// an invalid helper path before launch.
        /// Overlap uses a helper error with source kind [`io::ErrorKind::WouldBlock`].
        pub fn open_with_helper_executable(
            selector: &BluetoothDeviceSelector,
            service: BluetoothService,
            helper_executable: impl AsRef<Path>,
            cancellation: &BluetoothOpenCancellation,
        ) -> Result<Self, TransportError> {
            let helper_executable = validate_helper_executable(helper_executable.as_ref())?;
            cancellation.check()?;
            let deadline = Instant::now() + HELPER_OPEN_TIMEOUT;
            let mut process_slot = Some(HelperProcessSlot::reserve()?);

            // SAFETY: The inert anchor retains the native constructor from its
            // static archive. It has no arguments, result, or runtime effects.
            unsafe { bt_helper_link_anchor() };
            let (helper_liveness, parent_liveness) = create_liveness_pipe()
                .map_err(|source| bluetooth_helper_error("creating the liveness pipe", source))?;
            let mut command = new_helper_command(&helper_executable, selector.as_str(), service);
            prepare_liveness_fd(&mut command, helper_liveness.as_raw_fd());
            let mut child = command.spawn().map_err(|source| {
                bluetooth_helper_error(format!("launching {}", helper_executable.display()), source)
            })?;
            drop(helper_liveness);
            let Some(helper_stdin) = child.stdin.take() else {
                terminate_child(child, process_slot.take(), Some(parent_liveness), false);
                return Err(helper_readiness_error(io::Error::new(
                    io::ErrorKind::BrokenPipe,
                    "Bluetooth helper has no stdin pipe",
                )));
            };
            let Some(mut helper_stdout) = child.stdout.take() else {
                drop(helper_stdin);
                terminate_child(child, process_slot.take(), Some(parent_liveness), false);
                return Err(helper_readiness_error(io::Error::new(
                    io::ErrorKind::BrokenPipe,
                    "Bluetooth helper has no stdout pipe",
                )));
            };
            let endpoint = set_nonblocking(helper_stdin.as_raw_fd())
                .and_then(|()| set_nonblocking(helper_stdout.as_raw_fd()))
                .map_err(helper_readiness_error)
                .and_then(|()| {
                    await_helper_ready_until(&mut child, &mut helper_stdout, deadline, cancellation)
                })
                .and_then(|()| {
                    read_endpoint_until(
                        &mut helper_stdout,
                        selector,
                        service,
                        deadline,
                        cancellation,
                    )
                });
            let (address, channel) = match endpoint {
                Ok(endpoint) => endpoint,
                Err(error) => {
                    drop(helper_stdin);
                    drop(helper_stdout);
                    terminate_child(child, process_slot.take(), Some(parent_liveness), false);
                    return Err(error);
                }
            };
            tracing::info!(device = %address, channel = channel.get(), pid = child.id(),
                "Bluetooth RFCOMM helper ready");
            Self {
                child: Some(child),
                helper_stdin: Some(helper_stdin),
                helper_stdout: Some(helper_stdout),
                parent_liveness: Some(parent_liveness),
                helper_healthy: true,
                process_slot,
                address,
                channel,
                close_outcome: None,
                helper_executable,
            }
            .admit_open(deadline, cancellation)
        }

        /// Check handoff after all endpoint reads, preparation and tracing.
        fn admit_open(
            mut self,
            deadline: Instant,
            cancellation: &BluetoothOpenCancellation,
        ) -> Result<Self, TransportError> {
            if let Err(error) = check_open_completion(deadline, cancellation) {
                // A late result still owns its helper and lease. Retire them
                // through the existing bounded cleanup path before returning
                // the original cancellation or deadline failure.
                self.terminate_helper(false);
                return Err(error);
            }
            Ok(self)
        }

        /// Exact paired address reported by the successful native open.
        ///
        /// This is endpoint-selection evidence, not a radio model identity.
        #[must_use]
        pub const fn address(&self) -> &BluetoothAddress {
            &self.address
        }

        /// Actual successfully opened RFCOMM channel.
        #[must_use]
        pub const fn channel(&self) -> RfcommChannel {
            self.channel
        }

        fn terminate_helper(&mut self, graceful: bool) {
            self.helper_healthy = false;
            // Stdin EOF asks the helper to exit. Keep its stdout reader alive
            // until reap so in-flight ingress cannot manufacture EPIPE while
            // the native channel is closing. SIGKILL still bounds backpressure.
            drop(self.helper_stdin.take());
            if let Some(child) = self.child.take() {
                self.close_outcome = Some(terminate_child_checked(
                    child,
                    self.process_slot.take(),
                    self.parent_liveness.take(),
                    graceful,
                ));
            } else {
                drop(self.process_slot.take());
                drop(self.parent_liveness.take());
                if self.close_outcome.is_none() {
                    self.close_outcome = Some(Err(BluetoothCloseFailure::ForcedTermination));
                }
            }
            drop(self.helper_stdout.take());
        }
    }

    fn read_endpoint_until(
        stdout: &mut impl io::Read,
        selector: &BluetoothDeviceSelector,
        service: BluetoothService,
        deadline: Instant,
        cancellation: &BluetoothOpenCancellation,
    ) -> Result<(BluetoothAddress, RfcommChannel), TransportError> {
        let mut endpoint = [0_u8; 18];
        let mut read = 0;
        while read < endpoint.len() {
            cancellation.check()?;
            if Instant::now() >= deadline {
                return Err(helper_readiness_error(io::Error::new(
                    io::ErrorKind::TimedOut,
                    "Bluetooth endpoint evidence timed out",
                )));
            }
            let destination = endpoint.get_mut(read..).ok_or_else(|| {
                helper_readiness_error(io::Error::new(
                    io::ErrorKind::InvalidData,
                    "invalid endpoint evidence cursor",
                ))
            })?;
            match stdout.read(destination) {
                Ok(0) => {
                    return Err(helper_readiness_error(io::Error::new(
                        io::ErrorKind::UnexpectedEof,
                        "Bluetooth endpoint evidence was truncated",
                    )));
                }
                Ok(count) => read += count,
                Err(error) if error.kind() == io::ErrorKind::WouldBlock => {
                    std::thread::sleep(PIPE_POLL_INTERVAL);
                }
                Err(error) => return Err(helper_readiness_error(error)),
            }
        }
        let parsed = parse_endpoint(&endpoint, selector, service)?;
        check_open_completion(deadline, cancellation)?;
        Ok(parsed)
    }

    /// Valid complete evidence cannot outlive its original admission window.
    fn check_open_completion(
        deadline: Instant,
        cancellation: &BluetoothOpenCancellation,
    ) -> Result<(), TransportError> {
        cancellation.check()?;
        if Instant::now() >= deadline {
            return Err(helper_readiness_error(io::Error::new(
                io::ErrorKind::TimedOut,
                "Bluetooth helper opening completed after its deadline",
            )));
        }
        Ok(())
    }

    fn parse_endpoint(
        endpoint: &[u8; 18],
        selector: &BluetoothDeviceSelector,
        service: BluetoothService,
    ) -> Result<(BluetoothAddress, RfcommChannel), TransportError> {
        let (raw_address, raw_channel) = endpoint.split_at(17);
        let address = std::str::from_utf8(raw_address)
            .map_err(|error| {
                helper_readiness_error(io::Error::new(io::ErrorKind::InvalidData, error))
            })?
            .parse::<BluetoothAddress>()?;
        let channel = RfcommChannel::new(*raw_channel.first().ok_or_else(|| {
            helper_readiness_error(io::Error::new(
                io::ErrorKind::InvalidData,
                "missing RFCOMM channel",
            ))
        })?)?;
        if matches!(selector, BluetoothDeviceSelector::Address(expected) if expected != &address)
            || matches!(service, BluetoothService::FixedChannel(expected) if expected != channel)
        {
            return Err(helper_readiness_error(io::Error::new(
                io::ErrorKind::InvalidData,
                "Bluetooth endpoint differs from the explicit selection",
            )));
        }
        Ok((address, channel))
    }

    fn validate_helper_launch(
        helper_executable: &Path,
        timeout: Duration,
    ) -> Result<(), TransportError> {
        let mut process_slot = Some(HelperProcessSlot::reserve()?);

        // SAFETY: This no-argument/no-result function has no runtime side
        // effects. The reference retains the native constructor in the signed
        // helper executable selected by the caller.
        unsafe { bt_helper_link_anchor() };

        let (helper_liveness, parent_liveness) = create_liveness_pipe()
            .map_err(|source| bluetooth_helper_error("creating the liveness pipe", source))?;
        let mut command = new_helper_test_command(helper_executable, HELPER_ECHO_TEST_MODE);
        prepare_liveness_fd(&mut command, helper_liveness.as_raw_fd());
        let mut child = command.spawn().map_err(|source| {
            bluetooth_helper_error(format!("launching {}", helper_executable.display()), source)
        })?;
        drop(helper_liveness);

        let Some(helper_stdin) = child.stdin.take() else {
            terminate_child(child, process_slot.take(), Some(parent_liveness), false);
            return Err(helper_readiness_error(io::Error::new(
                io::ErrorKind::BrokenPipe,
                "spawned Bluetooth helper has no stdin pipe",
            )));
        };
        let Some(mut helper_stdout) = child.stdout.take() else {
            drop(helper_stdin);
            terminate_child(child, process_slot.take(), Some(parent_liveness), false);
            return Err(helper_readiness_error(io::Error::new(
                io::ErrorKind::BrokenPipe,
                "spawned Bluetooth helper has no stdout pipe",
            )));
        };
        if let Err(source) = set_nonblocking(helper_stdout.as_raw_fd()) {
            drop(helper_stdin);
            drop(helper_stdout);
            terminate_child(child, process_slot.take(), Some(parent_liveness), false);
            return Err(helper_readiness_error(source));
        }

        let result = validate_helper_echo_until(
            &mut child,
            helper_stdin,
            &mut helper_stdout,
            Instant::now() + timeout,
        );
        drop(helper_stdout);
        match result {
            Ok(()) => {
                drop(parent_liveness);
                drop(process_slot.take());
                Ok(())
            }
            Err(error) => {
                terminate_child(child, process_slot.take(), Some(parent_liveness), false);
                Err(error)
            }
        }
    }

    fn validate_helper_echo_until(
        child: &mut Child,
        mut stdin: ChildStdin,
        stdout: &mut ChildStdout,
        deadline: Instant,
    ) -> Result<(), TransportError> {
        let cancellation = BluetoothOpenCancellation::default();
        await_helper_ready_until(child, stdout, deadline, &cancellation)?;
        stdin
            .write_all(HELPER_VALIDATION_CHALLENGE)
            .map_err(|source| {
                bluetooth_helper_error("writing the helper validation echo", source)
            })?;
        drop(stdin);

        let mut echoed = Vec::with_capacity(HELPER_VALIDATION_CHALLENGE.len());
        let mut buffer = [0_u8; 64];
        loop {
            match stdout.read(&mut buffer) {
                Ok(0) => {
                    let status = await_helper_exit_after_stdout_eof(child, &cancellation)?;
                    if !status.success() {
                        return Err(bluetooth_helper_error(
                            "validating the Bluetooth helper launch",
                            io::Error::new(
                                io::ErrorKind::BrokenPipe,
                                format!("Bluetooth helper exited with {status}"),
                            ),
                        ));
                    }
                    if echoed == HELPER_VALIDATION_CHALLENGE {
                        return Ok(());
                    }
                    return Err(bluetooth_helper_error(
                        "validating the Bluetooth helper echo",
                        io::Error::new(
                            io::ErrorKind::InvalidData,
                            "Bluetooth helper returned an incomplete validation echo",
                        ),
                    ));
                }
                Ok(count) => {
                    let bytes = buffer.get(..count).ok_or_else(|| {
                        bluetooth_helper_error(
                            "validating the Bluetooth helper echo",
                            io::Error::new(
                                io::ErrorKind::InvalidData,
                                "Bluetooth helper returned an invalid echo length",
                            ),
                        )
                    })?;
                    echoed.extend_from_slice(bytes);
                    if echoed.len() > HELPER_VALIDATION_CHALLENGE.len()
                        || HELPER_VALIDATION_CHALLENGE.get(..echoed.len())
                            != Some(echoed.as_slice())
                    {
                        return Err(bluetooth_helper_error(
                            "validating the Bluetooth helper echo",
                            io::Error::new(
                                io::ErrorKind::InvalidData,
                                "Bluetooth helper returned the wrong validation echo",
                            ),
                        ));
                    }
                }
                Err(error) if error.kind() == io::ErrorKind::WouldBlock => {
                    if Instant::now() >= deadline {
                        return Err(bluetooth_helper_error(
                            "validating the Bluetooth helper launch",
                            io::Error::new(
                                io::ErrorKind::TimedOut,
                                "Bluetooth helper validation timed out",
                            ),
                        ));
                    }
                    std::thread::sleep(PIPE_POLL_INTERVAL);
                }
                Err(source) => {
                    return Err(bluetooth_helper_error(
                        "reading the helper validation echo",
                        source,
                    ));
                }
            }
        }
    }

    fn enumerate_paired_devices(
        helper_executable: &Path,
        cancellation: &BluetoothOpenCancellation,
    ) -> Result<Vec<PairedBluetoothDevice>, TransportError> {
        cancellation.check()?;
        let mut process_slot = Some(HelperProcessSlot::reserve()?);

        // SAFETY: This no-argument/no-result function has no runtime side
        // effects. The reference retains the native constructor in the signed
        // helper executable selected by the caller.
        unsafe { bt_helper_link_anchor() };

        let (helper_liveness, parent_liveness) = create_liveness_pipe()
            .map_err(|source| bluetooth_helper_error("creating the liveness pipe", source))?;
        let mut command = new_helper_control_command(helper_executable, HELPER_PAIRED_CONTROL_MODE);
        prepare_liveness_fd(&mut command, helper_liveness.as_raw_fd());
        let mut child = command.spawn().map_err(|source| {
            bluetooth_helper_error(format!("launching {}", helper_executable.display()), source)
        })?;
        drop(helper_liveness);

        let Some(mut helper_stdout) = child.stdout.take() else {
            terminate_child(child, process_slot.take(), Some(parent_liveness), false);
            return Err(bluetooth_helper_error(
                "enumerating paired Bluetooth devices",
                io::Error::new(
                    io::ErrorKind::BrokenPipe,
                    "spawned Bluetooth helper has no stdout pipe",
                ),
            ));
        };
        if let Err(source) = set_nonblocking(helper_stdout.as_raw_fd()) {
            drop(helper_stdout);
            terminate_child(child, process_slot.take(), Some(parent_liveness), false);
            return Err(bluetooth_helper_error(
                "preparing paired-device enumeration",
                source,
            ));
        }

        let deadline = Instant::now() + HELPER_ENUMERATION_TIMEOUT;
        let result =
            await_helper_ready_until(&mut child, &mut helper_stdout, deadline, cancellation)
                .and_then(|()| {
                    collect_paired_device_payload(
                        &mut child,
                        &mut helper_stdout,
                        deadline,
                        cancellation,
                    )
                })
                .and_then(|payload| {
                    parse_paired_device_payload(&payload).map_err(|source| {
                        bluetooth_helper_error("parsing paired Bluetooth devices", source)
                    })
                });
        drop(helper_stdout);

        match result {
            Ok(devices) => {
                drop(parent_liveness);
                drop(process_slot.take());
                Ok(devices)
            }
            Err(error) => {
                terminate_child(child, process_slot.take(), Some(parent_liveness), false);
                Err(error)
            }
        }
    }

    fn collect_paired_device_payload(
        child: &mut Child,
        stdout: &mut ChildStdout,
        deadline: Instant,
        cancellation: &BluetoothOpenCancellation,
    ) -> Result<Vec<u8>, TransportError> {
        let mut payload = Vec::new();
        let mut buffer = [0_u8; 4096];
        loop {
            cancellation.check()?;
            match stdout.read(&mut buffer) {
                Ok(0) => {
                    let status = await_helper_exit_after_stdout_eof(child, cancellation)?;
                    if status.success() {
                        return Ok(payload);
                    }
                    let detail = match status.code() {
                        Some(HELPER_EXIT_TOO_MANY_PAIRED_DEVICES) => format!(
                            "paired-device enumeration exceeded the {MAX_PAIRED_DEVICES}-device safety bound"
                        ),
                        _ => format!("paired-device helper exited with {status}"),
                    };
                    return Err(bluetooth_helper_error(
                        "enumerating paired Bluetooth devices",
                        io::Error::new(io::ErrorKind::InvalidData, detail),
                    ));
                }
                Ok(count) => {
                    let next_length = payload.len().checked_add(count).ok_or_else(|| {
                        bluetooth_helper_error(
                            "enumerating paired Bluetooth devices",
                            io::Error::new(
                                io::ErrorKind::InvalidData,
                                "paired-device helper payload length overflow",
                            ),
                        )
                    })?;
                    if next_length > MAX_PAIRED_PAYLOAD_BYTES {
                        return Err(bluetooth_helper_error(
                            "enumerating paired Bluetooth devices",
                            io::Error::new(
                                io::ErrorKind::InvalidData,
                                format!(
                                    "paired-device helper payload exceeded {MAX_PAIRED_PAYLOAD_BYTES} bytes"
                                ),
                            ),
                        ));
                    }
                    let bytes = buffer.get(..count).ok_or_else(|| {
                        bluetooth_helper_error(
                            "enumerating paired Bluetooth devices",
                            io::Error::new(
                                io::ErrorKind::InvalidData,
                                "paired-device helper returned an invalid read length",
                            ),
                        )
                    })?;
                    payload.extend_from_slice(bytes);
                }
                Err(error) if error.kind() == io::ErrorKind::WouldBlock => {
                    if Instant::now() >= deadline {
                        return Err(bluetooth_helper_error(
                            "enumerating paired Bluetooth devices",
                            io::Error::new(
                                io::ErrorKind::TimedOut,
                                format!(
                                    "paired-device helper exceeded its {}-second deadline; macOS may be waiting for the responsible foreground app to resolve Bluetooth access",
                                    HELPER_ENUMERATION_TIMEOUT.as_secs()
                                ),
                            ),
                        ));
                    }
                    std::thread::sleep(PIPE_POLL_INTERVAL);
                }
                Err(source) => {
                    return Err(bluetooth_helper_error(
                        "reading paired Bluetooth devices",
                        source,
                    ));
                }
            }
        }
    }

    fn parse_paired_device_payload(payload: &[u8]) -> io::Result<Vec<PairedBluetoothDevice>> {
        let mut devices: Vec<PairedBluetoothDevice> = Vec::new();
        let mut offset = 0_usize;
        let mut raw_records = 0_usize;
        loop {
            let (address_length, name_length, record_offset) =
                paired_device_record_lengths(payload, offset)?;
            offset = record_offset;
            if address_length == 0 && name_length == 0 {
                if offset != payload.len() {
                    return Err(io::Error::new(
                        io::ErrorKind::InvalidData,
                        "paired-device payload contains bytes after its terminator",
                    ));
                }
                return Ok(devices);
            }
            if address_length == 0 || name_length == 0 {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidData,
                    "paired-device record contains an empty field",
                ));
            }
            if raw_records >= MAX_PAIRED_DEVICES {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidData,
                    format!("paired-device payload exceeded {MAX_PAIRED_DEVICES} records"),
                ));
            }
            raw_records += 1;
            if address_length != 17 || name_length > MAX_PAIRED_DISPLAY_NAME_BYTES {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidData,
                    "paired-device record exceeds its field bounds",
                ));
            }
            let record_length = address_length.checked_add(name_length).ok_or_else(|| {
                io::Error::new(
                    io::ErrorKind::InvalidData,
                    "paired-device record length overflow",
                )
            })?;
            let record_end = offset.checked_add(record_length).ok_or_else(|| {
                io::Error::new(
                    io::ErrorKind::InvalidData,
                    "paired-device payload offset overflow",
                )
            })?;
            let record = payload.get(offset..record_end).ok_or_else(|| {
                io::Error::new(
                    io::ErrorKind::UnexpectedEof,
                    "paired-device payload ended inside a record",
                )
            })?;
            let (address_bytes, name_bytes) = record.split_at(address_length);
            let raw_address = std::str::from_utf8(address_bytes).map_err(|error| {
                io::Error::new(
                    io::ErrorKind::InvalidData,
                    format!("paired-device address is not UTF-8: {error}"),
                )
            })?;
            let Ok(address) = raw_address.parse::<BluetoothAddress>() else {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidData,
                    format!("paired-device address is not exact: {raw_address:?}"),
                ));
            };
            let display_name = std::str::from_utf8(name_bytes).map_err(|error| {
                io::Error::new(
                    io::ErrorKind::InvalidData,
                    format!("paired-device name is not UTF-8: {error}"),
                )
            })?;
            match devices.iter().find(|device| device.address == address) {
                Some(device) if device.display_name != display_name => {
                    return Err(io::Error::new(
                        io::ErrorKind::InvalidData,
                        format!("paired-device address has conflicting display names: {address}"),
                    ));
                }
                Some(_) => {}
                None => devices.push(PairedBluetoothDevice {
                    address,
                    display_name: display_name.to_owned(),
                }),
            }
            offset = record_end;
        }
    }

    fn paired_device_record_lengths(
        payload: &[u8],
        offset: usize,
    ) -> io::Result<(usize, usize, usize)> {
        let header_end = offset.checked_add(4).ok_or_else(|| {
            io::Error::new(
                io::ErrorKind::InvalidData,
                "paired-device record header length overflow",
            )
        })?;
        let header = payload.get(offset..header_end).ok_or_else(|| {
            io::Error::new(
                io::ErrorKind::UnexpectedEof,
                "paired-device payload ended before its terminator",
            )
        })?;
        let &[address_high, address_low, name_high, name_low] = header else {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "paired-device record header has the wrong size",
            ));
        };
        Ok((
            usize::from(u16::from_be_bytes([address_high, address_low])),
            usize::from(u16::from_be_bytes([name_high, name_low])),
            header_end,
        ))
    }

    impl Transport for BluetoothTransport {
        /// Submit the complete byte slice to helper stdin, not to a peer ACK.
        ///
        /// See the [type's I/O contract](BluetoothTransport#io-completion-and-cancellation)
        /// for pipe backpressure, partial writes, empty input and cancellation.
        async fn write(&mut self, data: &[u8]) -> Result<(), TransportError> {
            if data.is_empty() {
                return Ok(());
            }
            tracing::debug!(bytes = data.len(), "BT helper pipe write");

            if !self.helper_healthy {
                return Err(not_connected_write_error());
            }
            let Self {
                child,
                helper_stdin,
                parent_liveness,
                helper_healthy,
                close_outcome,
                process_slot,
                ..
            } = self;
            if child.is_none() || helper_stdin.is_none() {
                return Err(not_connected_write_error());
            }
            let mut cancellation = HelperWriteCancellation::new(
                child,
                process_slot,
                parent_liveness,
                helper_healthy,
                close_outcome,
            );
            let Some(helper_stdin) = helper_stdin.as_mut() else {
                return Err(not_connected_write_error());
            };
            let deadline = tokio::time::Instant::now() + PIPE_WRITE_TIMEOUT;

            for chunk in data.chunks(MACOS_PIPE_BUF) {
                loop {
                    match helper_stdin.write(chunk) {
                        Ok(count) if count == chunk.len() => break,
                        Ok(0) => {
                            return Err(TransportError::Write(io::Error::new(
                                io::ErrorKind::WriteZero,
                                "Bluetooth helper stdin closed",
                            )));
                        }
                        Ok(count) => {
                            return Err(TransportError::Write(io::Error::new(
                                io::ErrorKind::InvalidData,
                                format!(
                                    "non-atomic Bluetooth helper pipe write: {count}/{} bytes",
                                    chunk.len()
                                ),
                            )));
                        }
                        Err(error) if error.kind() == io::ErrorKind::WouldBlock => {
                            if tokio::time::Instant::now() >= deadline {
                                return Err(TransportError::Write(io::Error::new(
                                    io::ErrorKind::TimedOut,
                                    "Bluetooth helper stdin remained backpressured",
                                )));
                            }
                            tokio::time::sleep(PIPE_POLL_INTERVAL).await;
                        }
                        Err(error) => return Err(TransportError::Write(error)),
                    }
                }
            }

            cancellation.disarm();
            Ok(())
        }

        /// Read helper output without an intrinsic deadline or framing.
        ///
        /// Pending reads are cancellation-safe. For nonempty buffers, pipe
        /// EOF retires the helper and reports a read error with `UnexpectedEof`.
        /// The independent cleanup outcome remains available through close.
        async fn read(&mut self, buffer: &mut [u8]) -> Result<usize, TransportError> {
            if buffer.is_empty() {
                return Ok(0);
            }
            if !self.helper_healthy {
                return Err(not_connected_read_error());
            }
            loop {
                let result = {
                    let Some(helper_stdout) = self.helper_stdout.as_mut() else {
                        return Err(not_connected_read_error());
                    };
                    helper_stdout.read(buffer)
                };
                match result {
                    Ok(0) => {
                        self.terminate_helper(false);
                        return Err(TransportError::Read(io::Error::new(
                            io::ErrorKind::UnexpectedEof,
                            "Bluetooth helper exited",
                        )));
                    }
                    Ok(count) => {
                        tracing::debug!(bytes = count, "BT helper pipe read");
                        return Ok(count);
                    }
                    Err(error) if error.kind() == io::ErrorKind::WouldBlock => {
                        tokio::time::sleep(PIPE_POLL_INTERVAL).await;
                    }
                    Err(error) => {
                        self.terminate_helper(false);
                        return Err(TransportError::Read(error));
                    }
                }
            }
        }

        /// Perform synchronous bounded cleanup and return its retained outcome.
        ///
        /// This future does not yield, so a Tokio timeout cannot preempt its
        /// poll. See the [execution and cleanup contract](BluetoothTransport#execution-and-cleanup)
        /// for wait budgets, deferred reaping and independent failure evidence.
        async fn close(&mut self) -> Result<(), TransportError> {
            tracing::info!(pid = ?self.child.as_ref().map(Child::id), "closing Bluetooth RFCOMM helper");
            self.terminate_helper(true);
            self.close_outcome
                .unwrap_or(Err(BluetoothCloseFailure::ReapPending))
                .map_err(|failure| TransportError::BluetoothClose { failure })
        }
    }

    impl Drop for BluetoothTransport {
        fn drop(&mut self) {
            self.terminate_helper(true);
        }
    }

    /// Exclusive lease for the one live RFCOMM helper this process permits.
    ///
    /// When synchronous reap exceeds its bound, this value moves to the
    /// detached waiter so the slot is not released until the old process is
    /// actually gone.
    struct HelperProcessSlot;

    impl HelperProcessSlot {
        fn reserve() -> Result<Self, TransportError> {
            reap_pending_children();
            let _previously_reserved = HELPER_PROCESS_SLOT_RESERVED
                .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
                .map_err(|_already_reserved| {
                    tracing::warn!("refusing second Bluetooth helper while one is still live");
                    bluetooth_helper_error(
                        "reserving the process slot",
                        io::Error::new(
                            io::ErrorKind::WouldBlock,
                            "another Bluetooth helper is still live",
                        ),
                    )
                })?;
            Ok(Self)
        }
    }

    impl Drop for HelperProcessSlot {
        fn drop(&mut self) {
            HELPER_PROCESS_SLOT_RESERVED.store(false, Ordering::Release);
        }
    }

    /// Kill-on-cancel guard for a potentially partial logical pipe write.
    ///
    /// Tokio timeouts cancel by dropping the transport future. If that occurs
    /// between 512-byte chunks, leaving the helper alive could let it consume
    /// a truncated radio command. Killing the process closes both byte streams
    /// and makes this transport fail closed. A caller must explicitly acquire
    /// a fresh endpoint; the shared transport never reopens itself.
    struct HelperWriteCancellation<'transport> {
        child: &'transport mut Option<Child>,
        process_slot: &'transport mut Option<HelperProcessSlot>,
        parent_liveness: &'transport mut Option<OwnedFd>,
        helper_healthy: &'transport mut bool,
        close_outcome: &'transport mut Option<Result<(), BluetoothCloseFailure>>,
        armed: bool,
    }

    impl<'transport> HelperWriteCancellation<'transport> {
        const fn new(
            child: &'transport mut Option<Child>,
            process_slot: &'transport mut Option<HelperProcessSlot>,
            parent_liveness: &'transport mut Option<OwnedFd>,
            helper_healthy: &'transport mut bool,
            close_outcome: &'transport mut Option<Result<(), BluetoothCloseFailure>>,
        ) -> Self {
            Self {
                child,
                process_slot,
                parent_liveness,
                helper_healthy,
                close_outcome,
                armed: true,
            }
        }

        const fn disarm(&mut self) {
            self.armed = false;
        }
    }

    impl Drop for HelperWriteCancellation<'_> {
        fn drop(&mut self) {
            if self.armed {
                *self.helper_healthy = false;
                if let Some(child) = self.child.take() {
                    let pid = child.id();
                    tracing::warn!(
                        pid,
                        "terminating Bluetooth helper after cancelled/failed pipe write"
                    );
                    *self.close_outcome = Some(terminate_child_checked(
                        child,
                        self.process_slot.take(),
                        self.parent_liveness.take(),
                        false,
                    ));
                } else {
                    drop(self.process_slot.take());
                    drop(self.parent_liveness.take());
                }
            }
        }
    }

    fn create_liveness_pipe() -> io::Result<(OwnedFd, OwnedFd)> {
        let mut read_fd = -1_i32;
        let mut write_fd = -1_i32;
        // SAFETY: Both pointers refer to initialized writable `i32`s. On
        // success the native function returns two new, uniquely owned file
        // descriptors; on failure it closes any descriptor it created.
        if unsafe { bt_liveness_pipe_create(&raw mut read_fd, &raw mut write_fd) } != 0 {
            return Err(io::Error::last_os_error());
        }
        if read_fd < 0 || write_fd < 0 {
            return Err(io::Error::other(
                "Bluetooth helper liveness pipe returned invalid descriptors",
            ));
        }
        // SAFETY: Successful `bt_liveness_pipe_create` transfers one unique
        // ownership unit for each new descriptor to this caller.
        Ok(unsafe {
            (
                OwnedFd::from_raw_fd(read_fd),
                OwnedFd::from_raw_fd(write_fd),
            )
        })
    }

    fn prepare_liveness_fd(command: &mut Command, source_fd: i32) {
        // SAFETY: The closure runs after fork and before exec, and calls only
        // the native async-signal-safe `dup2`/`fcntl` shim. `source_fd` stays
        // open in the parent until `Command::spawn` returns. Returning an OS
        // error aborts exec without entering Rust code in the child.
        unsafe {
            let _command = command.pre_exec(move || {
                if bt_helper_prepare_liveness_fd(source_fd, HELPER_LIVENESS_FD) == 0 {
                    Ok(())
                } else {
                    Err(io::Error::last_os_error())
                }
            });
        }
    }

    fn set_nonblocking(fd: i32) -> io::Result<()> {
        // SAFETY: `fd` comes directly from a live `ChildStdin` or
        // `ChildStdout`. The native function only performs F_GETFL/F_SETFL and
        // neither closes nor retains the descriptor.
        if unsafe { bt_fd_set_nonblocking(fd) } == 0 {
            Ok(())
        } else {
            Err(io::Error::last_os_error())
        }
    }

    fn helper_readiness_error(source: io::Error) -> TransportError {
        bluetooth_helper_error("the readiness handshake", source)
    }

    const fn helper_open_stage(code: i32) -> Option<BluetoothOpenStage> {
        match code {
            100 => Some(BluetoothOpenStage::ContextAllocation),
            101 => Some(BluetoothOpenStage::SdpStart),
            102 => Some(BluetoothOpenStage::SdpCompletion),
            103 => Some(BluetoothOpenStage::SdpDeadline),
            104 => Some(BluetoothOpenStage::ServiceResolution),
            105 => Some(BluetoothOpenStage::RfcommStart),
            106 => Some(BluetoothOpenStage::RfcommCompletion),
            107 => Some(BluetoothOpenStage::RfcommDeadline),
            108 => Some(BluetoothOpenStage::RfcommEndpoint),
            109 => Some(BluetoothOpenStage::StartupDeadline),
            _ => None,
        }
    }

    fn helper_exit_error(status: ExitStatus) -> TransportError {
        if let Some(stage) = status.code().and_then(helper_open_stage) {
            return TransportError::BluetoothOpen { stage };
        }
        match status.code() {
            Some(71) => TransportError::NotFound,
            Some(HELPER_EXIT_AMBIGUOUS_DEVICE_NAME) => TransportError::BluetoothDeviceNameAmbiguous,
            Some(89) => TransportError::BluetoothClose {
                failure: BluetoothCloseFailure::ChannelUnconfirmed,
            },
            _ => helper_readiness_error(io::Error::new(
                io::ErrorKind::BrokenPipe,
                format!("Bluetooth helper exited with {status}"),
            )),
        }
    }

    fn await_helper_exit_after_stdout_eof(
        child: &mut Child,
        cancellation: &BluetoothOpenCancellation,
    ) -> Result<ExitStatus, TransportError> {
        await_helper_exit_until(child, Instant::now() + HELPER_EOF_EXIT_BUDGET, cancellation)
    }

    fn await_helper_exit_until(
        child: &mut Child,
        deadline: Instant,
        cancellation: &BluetoothOpenCancellation,
    ) -> Result<ExitStatus, TransportError> {
        loop {
            cancellation.check()?;
            if let Some(status) = child.try_wait().map_err(helper_readiness_error)? {
                return Ok(status);
            }
            if Instant::now() >= deadline {
                return Err(helper_readiness_error(io::Error::new(
                    io::ErrorKind::UnexpectedEof,
                    "Bluetooth helper closed stdout but did not exit within the bounded reap window",
                )));
            }
            std::thread::sleep(PIPE_POLL_INTERVAL);
        }
    }

    /// Admit failure evidence only after EOF and a matching, reaped child.
    fn await_helper_open_failure_until(
        child: &mut Child,
        stdout: &mut ChildStdout,
        deadline: Instant,
        cancellation: &BluetoothOpenCancellation,
    ) -> Result<TransportError, TransportError> {
        // The extra byte detects any trailing data without unbounded buffering.
        let mut record = [0_u8; 3];
        let mut offset = 0;
        loop {
            check_open_completion(deadline, cancellation)?;
            let destination = record
                .get_mut(offset..)
                .ok_or_else(|| invalid_open_failure("invalid failure record cursor"))?;
            match stdout.read(destination) {
                Ok(0) => break,
                Ok(count) => {
                    offset += count;
                    if offset > 2 {
                        return Err(invalid_open_failure("trailing failed-open evidence"));
                    }
                }
                Err(error) if error.kind() == io::ErrorKind::WouldBlock => {
                    std::thread::sleep(PIPE_POLL_INTERVAL);
                }
                Err(error) => return Err(helper_readiness_error(error)),
            }
        }
        if offset != 2 {
            return Err(invalid_open_failure("truncated failed-open evidence"));
        }
        let [stage_code, cleanup, _] = record;
        let stage = helper_open_stage(i32::from(stage_code))
            .ok_or_else(|| invalid_open_failure("unknown failed-open stage"))?;
        let expected_exit = match cleanup {
            0 => i32::from(stage_code),
            1 => 89,
            _ => return Err(invalid_open_failure("unknown failed-open cleanup evidence")),
        };
        let reap_deadline = deadline.min(Instant::now() + HELPER_EOF_EXIT_BUDGET);
        let status = await_helper_exit_until(child, reap_deadline, cancellation)?;
        check_open_completion(deadline, cancellation)?;
        if status.code() != Some(expected_exit) {
            return Err(invalid_open_failure(
                "failed-open evidence differs from helper exit",
            ));
        }
        Ok(if cleanup == 1 {
            TransportError::BluetoothOpenWithCleanup {
                stage,
                cleanup: BluetoothCloseFailure::ChannelUnconfirmed,
            }
        } else {
            TransportError::BluetoothOpen { stage }
        })
    }

    fn invalid_open_failure(message: &'static str) -> TransportError {
        helper_readiness_error(io::Error::new(io::ErrorKind::InvalidData, message))
    }

    #[cfg(test)]
    fn await_helper_ready(
        child: &mut Child,
        stdout: &mut ChildStdout,
    ) -> Result<(), TransportError> {
        await_helper_ready_cancellable(child, stdout, &BluetoothOpenCancellation::default())
    }

    #[cfg(test)]
    fn await_helper_ready_cancellable(
        child: &mut Child,
        stdout: &mut ChildStdout,
        cancellation: &BluetoothOpenCancellation,
    ) -> Result<(), TransportError> {
        await_helper_ready_until(
            child,
            stdout,
            Instant::now() + HELPER_OPEN_TIMEOUT,
            cancellation,
        )
    }

    fn await_helper_ready_until(
        child: &mut Child,
        stdout: &mut ChildStdout,
        deadline: Instant,
        cancellation: &BluetoothOpenCancellation,
    ) -> Result<(), TransportError> {
        let mut ready = [0_u8; HELPER_READY_MAGIC.len()];
        let mut offset = 0_usize;
        while offset < ready.len() {
            check_open_completion(deadline, cancellation)?;
            let remaining = ready.get_mut(offset..).ok_or_else(|| {
                helper_readiness_error(io::Error::new(
                    io::ErrorKind::InvalidData,
                    "invalid helper readiness offset",
                ))
            })?;
            match stdout.read(remaining) {
                Ok(0) => {
                    if offset != 0 {
                        return Err(invalid_open_failure("truncated helper readiness prefix"));
                    }
                    let reap_deadline = deadline.min(Instant::now() + HELPER_EOF_EXIT_BUDGET);
                    let status = await_helper_exit_until(child, reap_deadline, cancellation)?;
                    check_open_completion(deadline, cancellation)?;
                    return Err(helper_exit_error(status));
                }
                Ok(count) => {
                    offset = offset.checked_add(count).ok_or_else(|| {
                        helper_readiness_error(io::Error::new(
                            io::ErrorKind::InvalidData,
                            "Bluetooth helper readiness length overflow",
                        ))
                    })?;
                    if ready.get(..offset) != HELPER_READY_MAGIC.get(..offset)
                        && ready.get(..offset) != HELPER_OPEN_FAILURE_MAGIC.get(..offset)
                    {
                        return Err(helper_readiness_error(io::Error::new(
                            io::ErrorKind::InvalidData,
                            "Bluetooth helper emitted an invalid readiness prefix",
                        )));
                    }
                }
                Err(error) if error.kind() == io::ErrorKind::WouldBlock => {
                    std::thread::sleep(PIPE_POLL_INTERVAL);
                }
                Err(error) => return Err(helper_readiness_error(error)),
            }
        }

        if &ready == HELPER_READY_MAGIC {
            Ok(())
        } else {
            Err(await_helper_open_failure_until(
                child,
                stdout,
                deadline,
                cancellation,
            )?)
        }
    }

    /// Helpers whose detached waiter could not start remain exclusively owned.
    static PENDING_REAPS: Mutex<Vec<PendingReap>> = Mutex::new(Vec::new());

    struct PendingReap {
        child: Child,
        slot: Option<HelperProcessSlot>,
    }

    fn retain_pending_reap(pending: PendingReap) {
        PENDING_REAPS
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .push(pending);
    }

    fn reap_pending_children() {
        let mut pending = PENDING_REAPS
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let mut index = 0;
        while index < pending.len() {
            let reaped = pending
                .get_mut(index)
                .is_some_and(|entry| matches!(entry.child.try_wait(), Ok(Some(_))));
            if reaped {
                let retired = pending.swap_remove(index);
                drop(retired.slot);
            } else {
                index += 1;
            }
        }
        drop(pending);
    }

    fn close_status(status: ExitStatus) -> Result<(), BluetoothCloseFailure> {
        if status.success() {
            Ok(())
        } else if status.code() == Some(89) {
            Err(BluetoothCloseFailure::ChannelUnconfirmed)
        } else {
            Err(BluetoothCloseFailure::HelperExited {
                code: status.code(),
            })
        }
    }

    /// Best-effort cleanup used when an earlier operation already failed.
    fn terminate_child(
        child: Child,
        process_slot: Option<HelperProcessSlot>,
        parent_liveness: Option<OwnedFd>,
        graceful: bool,
    ) {
        let _outcome = terminate_child_checked(child, process_slot, parent_liveness, graceful);
    }

    /// Bounded caller-side cleanup; only a detached owner may block in wait.
    fn terminate_child_checked(
        mut child: Child,
        process_slot: Option<HelperProcessSlot>,
        mut parent_liveness: Option<OwnedFd>,
        graceful: bool,
    ) -> Result<(), BluetoothCloseFailure> {
        if let Ok(Some(status)) = child.try_wait() {
            return close_status(status);
        }
        if graceful {
            let deadline = Instant::now() + GRACEFUL_EXIT_BUDGET;
            loop {
                match child.try_wait() {
                    Ok(Some(status)) => return close_status(status),
                    Ok(None) if Instant::now() < deadline => {
                        std::thread::sleep(Duration::from_millis(1));
                    }
                    Ok(None) | Err(_) => break,
                }
            }
        }

        drop(parent_liveness.take());
        let _kill = child.kill();
        let deadline = Instant::now() + SYNC_REAP_BUDGET;
        loop {
            match child.try_wait() {
                Ok(Some(_)) => return Err(BluetoothCloseFailure::ForcedTermination),
                Ok(None) if Instant::now() < deadline => {
                    std::thread::sleep(Duration::from_millis(1));
                }
                Ok(None) | Err(_) => break,
            }
        }

        // Spawn before transferring ownership. Every failed transfer is retained
        // for bounded try_wait polling before a later helper reservation.
        let (sender, receiver) = mpsc::sync_channel::<PendingReap>(1);
        let waiter = std::thread::Builder::new()
            .name(format!("kenwood-bt-reaper-{}", child.id()))
            .spawn(move || {
                if let Ok(mut pending) = receiver.recv() {
                    match pending.child.wait() {
                        Ok(_) => drop(pending.slot),
                        Err(_) => retain_pending_reap(pending),
                    }
                }
            });
        let pending = PendingReap {
            child,
            slot: process_slot,
        };
        match waiter {
            Ok(_handle) => {
                if let Err(mpsc::SendError(pending)) = sender.send(pending) {
                    retain_pending_reap(pending);
                }
            }
            Err(_) => retain_pending_reap(pending),
        }
        Err(BluetoothCloseFailure::ReapPending)
    }

    fn not_connected_write_error() -> TransportError {
        TransportError::Write(io::Error::new(
            io::ErrorKind::NotConnected,
            "Bluetooth helper is not running",
        ))
    }

    fn not_connected_read_error() -> TransportError {
        TransportError::Read(io::Error::new(
            io::ErrorKind::NotConnected,
            "Bluetooth helper is not running",
        ))
    }

    #[cfg(test)]
    mod tests {
        use std::error::Error;
        use std::ffi::CString;
        use std::io::{self, Read as _, Write as _};
        use std::os::fd::{AsRawFd as _, OwnedFd};
        use std::os::unix::process::ExitStatusExt as _;
        use std::path::Path;
        use std::process::{Child, ChildStdin, ChildStdout, Command, ExitStatus, Stdio};
        use std::sync::Mutex;
        use std::time::{Duration, Instant};

        use super::{
            BluetoothOpenCancellation, BluetoothTransport, GRACEFUL_EXIT_BUDGET,
            HELPER_CONTROL_ENV, HELPER_EOF_EXIT_BUDGET, HELPER_EXIT_AMBIGUOUS_DEVICE_NAME,
            HELPER_LIVENESS_FD, HELPER_LIVENESS_FD_ENV, HELPER_READY_MAGIC, HELPER_SENTINEL_ENV,
            HELPER_SENTINEL_VALUE, HELPER_TEST_ENV, HELPER_VALIDATION_CHALLENGE, HelperProcessSlot,
            HelperWriteCancellation, SYNC_REAP_BUDGET, TransportError, await_helper_ready,
            await_helper_ready_cancellable, bt_device_identifier_matches_display_name,
            bt_helper_link_anchor, create_liveness_pipe, helper_exit_error, new_helper_command,
            parse_paired_device_payload, prepare_liveness_fd, set_nonblocking, terminate_child,
            validate_helper_echo_until, validate_helper_executable,
        };
        use crate::bluetooth::{
            BluetoothAddress, BluetoothDeviceSelector, BluetoothService, RfcommChannel,
        };

        type TestResult = Result<(), Box<dyn Error>>;

        /// Isolate fixtures that reserve the actual process-global helper lease.
        ///
        /// Hold this guard until the fixture's child is reaped and its lease is
        /// released. Helpers created with no lease remain independently parallel.
        static PROCESS_SLOT_TEST_LOCK: Mutex<()> = Mutex::new(());

        #[test]
        fn native_open_classification_distinguishes_callback_deadline_and_endpoint_failures()
        -> TestResult {
            assert_native_cleanup_fixture("open-stage-classification-v1")
        }

        #[test]
        fn native_connected_start_processes_events_before_protocol_dispatch() -> TestResult {
            assert_native_cleanup_fixture("startup-progress-v1")
        }

        #[test]
        fn native_open_failure_preserves_stage_and_independent_cleanup_after_reap() -> TestResult {
            use crate::error::{BluetoothCloseFailure, BluetoothOpenStage};
            for (mode, expected_exit) in [
                ("open-failure-cleanup-v1", 89),
                ("open-failure-clean-v1", 107),
            ] {
                let (mut child, stdin, mut stdout, liveness) = spawn_native_test_helper(mode)?;
                drop(stdin);
                if let Err(error) = set_nonblocking(stdout.as_raw_fd()) {
                    terminate_child(child, None, Some(liveness), false);
                    return Err(error.into());
                }
                let result = await_helper_ready(&mut child, &mut stdout);
                let reaped = child.try_wait();
                terminate_child(child, None, Some(liveness), false);
                let reaped = reaped?;
                assert_eq!(reaped.and_then(|status| status.code()), Some(expected_exit));
                if expected_exit == 89 {
                    assert!(
                        matches!(
                            result,
                            Err(TransportError::BluetoothOpenWithCleanup {
                                stage: BluetoothOpenStage::RfcommDeadline,
                                cleanup: BluetoothCloseFailure::ChannelUnconfirmed,
                            })
                        ),
                        "original opening stage was lost: {result:?}"
                    );
                } else {
                    assert!(matches!(
                        result,
                        Err(TransportError::BluetoothOpen {
                            stage: BluetoothOpenStage::RfcommDeadline,
                        })
                    ));
                }
            }
            Ok(())
        }

        #[test]
        fn failure_record_requires_exact_payload_and_matching_exit() -> TestResult {
            for script in [
                "printf 'KENWBT-ERROR'; exit 89",
                "printf 'KENWBT-ERROR-v1!'; exit 89",
                "printf 'KENWBT-ERROR-v1!\\153'; exit 89",
                "printf 'KENWBT-ERROR-v1!\\143\\001'; exit 89",
                "printf 'KENWBT-ERROR-v1!\\153\\002'; exit 89",
                "printf 'KENWBT-ERROR-v1!\\153\\001x'; exit 89",
                "printf 'KENWBT-ERROR-v1!\\153\\001'; exit 107",
                "printf 'KENWBT-ERROR-v1!\\153\\000'; exit 89",
                "printf 'KENWBT-ERROR-v1!\\153\\001'; exit 0",
            ] {
                let result = failure_script_result(
                    script,
                    Duration::from_secs(1),
                    &BluetoothOpenCancellation::default(),
                )?;
                assert!(
                    matches!(result.outcome, Err(TransportError::BluetoothHelper {
                    source, ..
                }) if source.kind() == io::ErrorKind::InvalidData),
                    "invalid failure evidence was admitted for {script:?}"
                );
            }
            Ok(())
        }

        #[test]
        fn failure_record_waits_for_matching_delayed_exit() -> TestResult {
            use crate::error::{BluetoothCloseFailure, BluetoothOpenStage};
            let result = failure_script_result(
                "printf 'KENWBT-ERROR-v1!\\153\\001'; exec 1>&-; /bin/sleep 0.02; exit 89",
                Duration::from_secs(1),
                &BluetoothOpenCancellation::default(),
            )?;
            assert_eq!(result.reaped.and_then(|status| status.code()), Some(89));
            assert!(matches!(
                result.outcome,
                Err(TransportError::BluetoothOpenWithCleanup {
                    stage: BluetoothOpenStage::RfcommDeadline,
                    cleanup: BluetoothCloseFailure::ChannelUnconfirmed,
                })
            ));
            Ok(())
        }

        #[test]
        fn failure_record_cannot_admit_an_unretired_or_cancelled_helper() -> TestResult {
            let script = "printf 'KENWBT-ERROR-v1!\\153\\001'; exec 1>&-; read -r gate; exit 89";
            let pending = failure_script_result(
                script,
                Duration::from_millis(60),
                &BluetoothOpenCancellation::default(),
            )?;
            assert!(pending.reaped.is_none());
            assert!(matches!(
                pending.outcome,
                Err(TransportError::BluetoothHelper { .. })
            ));

            let cancellation = BluetoothOpenCancellation::default();
            let trigger = cancellation.clone();
            let worker = std::thread::spawn(move || {
                std::thread::sleep(Duration::from_millis(20));
                trigger.cancel();
            });
            let cancelled = failure_script_result(script, Duration::from_secs(1), &cancellation);
            worker
                .join()
                .map_err(|_panic| "cancellation worker panicked")?;
            let cancelled = cancelled?;
            assert!(cancelled.reaped.is_none());
            assert!(matches!(
                cancelled.outcome,
                Err(TransportError::BluetoothOpenInterrupted)
            ));
            Ok(())
        }

        struct FailureScriptResult {
            outcome: Result<(), TransportError>,
            reaped: Option<ExitStatus>,
        }

        fn failure_script_result(
            script: &str,
            budget: Duration,
            cancellation: &BluetoothOpenCancellation,
        ) -> Result<FailureScriptResult, Box<dyn Error>> {
            let mut child = Command::new("/bin/sh")
                .args(["-c", script])
                .stdin(Stdio::piped())
                .stdout(Stdio::piped())
                .spawn()?;
            let Some(mut stdout) = child.stdout.take() else {
                terminate_child(child, None, None, false);
                return Err("failure fixture has no stdout".into());
            };
            let outcome = set_nonblocking(stdout.as_raw_fd())
                .map_err(super::helper_readiness_error)
                .and_then(|()| {
                    super::await_helper_ready_until(
                        &mut child,
                        &mut stdout,
                        Instant::now() + budget,
                        cancellation,
                    )
                });
            let reaped = child.try_wait();
            terminate_child(child, None, None, false);
            Ok(FailureScriptResult {
                outcome,
                reaped: reaped?,
            })
        }

        #[test]
        fn matched_device_open_failure_codes_retain_typed_human_readable_stages() {
            use crate::error::BluetoothOpenStage;
            for (code, stage) in [
                (100, BluetoothOpenStage::ContextAllocation),
                (101, BluetoothOpenStage::SdpStart),
                (102, BluetoothOpenStage::SdpCompletion),
                (103, BluetoothOpenStage::SdpDeadline),
                (104, BluetoothOpenStage::ServiceResolution),
                (105, BluetoothOpenStage::RfcommStart),
                (106, BluetoothOpenStage::RfcommCompletion),
                (107, BluetoothOpenStage::RfcommDeadline),
                (108, BluetoothOpenStage::RfcommEndpoint),
                (109, BluetoothOpenStage::StartupDeadline),
            ] {
                let error = helper_exit_error(ExitStatus::from_raw(code << 8));
                assert!(
                    matches!(error, TransportError::BluetoothOpen { stage: actual } if actual == stage)
                );
                assert!(error.to_string().contains(&stage.to_string()));
            }
            assert_eq!(
                BluetoothOpenStage::SdpDeadline.to_string(),
                "SDP completion deadline"
            );
            assert_eq!(
                BluetoothOpenStage::RfcommDeadline.to_string(),
                "RFCOMM opening deadline"
            );
        }

        #[test]
        fn pending_native_open_cleanup_preserves_arc_ownership_through_pool_drain() -> TestResult {
            assert_native_cleanup_fixture("pending-open-cleanup-v1")
        }

        #[test]
        fn unconfirmed_native_close_preserves_detached_callback_ownership_until_exit() -> TestResult
        {
            assert_native_cleanup_fixture("unconfirmed-open-cleanup-v1")
        }

        #[test]
        fn failed_native_delegate_detachment_preserves_callback_objects_until_exit() -> TestResult {
            assert_native_cleanup_fixture("failed-detach-cleanup-v1")
        }

        #[test]
        fn late_native_open_completion_cannot_resurrect_a_closed_context() -> TestResult {
            assert_native_cleanup_fixture("closed-before-open-v1")
        }

        fn assert_native_cleanup_fixture(mode: &str) -> TestResult {
            let (mut child, stdin, mut stdout, liveness) = spawn_native_test_helper(mode)?;
            drop(stdin);
            set_nonblocking(stdout.as_raw_fd())?;
            let deadline = Instant::now() + Duration::from_secs(2);
            let status = loop {
                if let Some(status) = child.try_wait()? {
                    break status;
                }
                if Instant::now() >= deadline {
                    terminate_child(child, None, Some(liveness), false);
                    return Err("pending-open cleanup fixture exceeded its deadline".into());
                }
                std::thread::sleep(Duration::from_millis(5));
            };
            assert_eq!(status.code(), Some(0));
            let mut ready = [0_u8; 16];
            stdout.read_exact(&mut ready)?;
            assert_eq!(&ready, HELPER_READY_MAGIC);
            drop(liveness);
            Ok(())
        }

        #[test]
        fn unconfirmed_open_cleanup_is_typed_and_never_retryable_not_found() {
            assert!(matches!(
                helper_exit_error(ExitStatus::from_raw(89 << 8)),
                TransportError::BluetoothClose {
                    failure: super::BluetoothCloseFailure::ChannelUnconfirmed
                }
            ));
        }

        #[test]
        fn duplicate_native_sdp_completion_is_a_terminal_helper_failure() -> TestResult {
            let (mut child, stdin, mut stdout, liveness) =
                spawn_native_test_helper("duplicate-sdp-v1")?;
            drop(stdin);
            set_nonblocking(stdout.as_raw_fd())?;
            let deadline = Instant::now() + Duration::from_secs(1);
            let status = loop {
                if let Some(status) = child.try_wait()? {
                    break status;
                }
                if Instant::now() >= deadline {
                    terminate_child(child, None, Some(liveness), false);
                    return Err("duplicate SDP callback guard did not terminate helper".into());
                }
                std::thread::sleep(Duration::from_millis(5));
            };
            assert_eq!(status.code(), Some(90));
            let mut ready = [0_u8; 16];
            stdout.read_exact(&mut ready)?;
            assert_eq!(&ready, HELPER_READY_MAGIC);
            drop(liveness);
            Ok(())
        }

        #[tokio::test]
        async fn transport_close_preserves_failed_cleanup_and_generic_reopen_is_unsupported()
        -> TestResult {
            use crate::Transport as _;
            for mode in ["echo-v1", "close-output-v1", "hang-v1"] {
                let (mut child, stdin, mut stdout, liveness) = spawn_native_test_helper(mode)?;
                set_nonblocking(stdout.as_raw_fd())?;
                await_helper_ready(&mut child, &mut stdout)?;
                let mut transport = BluetoothTransport {
                    child: Some(child),
                    helper_stdin: Some(stdin),
                    helper_stdout: Some(stdout),
                    parent_liveness: Some(liveness),
                    helper_healthy: true,
                    process_slot: None,
                    address: "00-11-22-33-44-55".parse()?,
                    channel: RfcommChannel::new(2)?,
                    close_outcome: None,
                    helper_executable: std::env::current_exe()?,
                };
                assert!(matches!(
                    transport.reopen().await,
                    Err(TransportError::ReopenUnsupported)
                ));
                let result = transport.close().await;
                if mode == "hang-v1" {
                    assert!(matches!(
                        result,
                        Err(TransportError::BluetoothClose {
                            failure: super::BluetoothCloseFailure::ForcedTermination
                        })
                    ));
                    assert!(matches!(
                        transport.close().await,
                        Err(TransportError::BluetoothClose { .. })
                    ));
                } else {
                    assert!(result.is_ok());
                    transport.close().await?;
                }
            }
            Ok(())
        }

        #[test]
        fn endpoint_evidence_validates_actual_address_and_service_channel() -> TestResult {
            let address: BluetoothAddress = "00-11-22-33-44-55".parse()?;
            let selector = BluetoothDeviceSelector::Address(address.clone());
            let mut bytes = *b"00-11-22-33-44-55\x1b";
            let (actual, channel) =
                super::parse_endpoint(&bytes, &selector, BluetoothService::SerialPort)?;
            assert_eq!(actual, address);
            assert_eq!(channel.get(), 27);
            assert!(
                super::parse_endpoint(
                    &bytes,
                    &selector,
                    BluetoothService::FixedChannel(RfcommChannel::new(2)?)
                )
                .is_err()
            );
            for channel in [0, 31, 255] {
                *bytes.last_mut().ok_or("endpoint channel missing")? = channel;
                assert!(
                    super::parse_endpoint(&bytes, &selector, BluetoothService::SerialPort).is_err()
                );
            }
            *bytes.last_mut().ok_or("endpoint channel missing")? = 2;
            *bytes.first_mut().ok_or("endpoint address missing")? = b'1';
            assert!(
                super::parse_endpoint(&bytes, &selector, BluetoothService::SerialPort).is_err()
            );
            *bytes.first_mut().ok_or("endpoint address missing")? = 0xff;
            assert!(
                super::parse_endpoint(&bytes, &selector, BluetoothService::SerialPort).is_err()
            );
            Ok(())
        }

        #[test]
        fn endpoint_reader_consumes_no_radio_ingress_after_its_exact_record() -> TestResult {
            let selector = BluetoothDeviceSelector::Address("00-11-22-33-44-55".parse()?);
            let mut child = Command::new("/bin/sh")
                .args(["-c", "printf '00-11-22-33-44-55\\033CAT'"])
                .stdout(Stdio::piped())
                .spawn()?;
            let mut stdout = child.stdout.take().ok_or("fixture stdout missing")?;
            set_nonblocking(stdout.as_raw_fd())?;
            let result = super::read_endpoint_until(
                &mut stdout,
                &selector,
                BluetoothService::SerialPort,
                Instant::now() + Duration::from_secs(1),
                &BluetoothOpenCancellation::default(),
            );
            let status = child.wait()?;
            assert!(status.success());
            assert_eq!(result?.1.get(), 27);
            let mut trailing = [0; 3];
            stdout.read_exact(&mut trailing)?;
            assert_eq!(&trailing, b"CAT");
            Ok(())
        }

        /// Deliver complete valid evidence, then change admission before read returns.
        struct CompletingEndpointReader<F> {
            completion: F,
            reads: usize,
        }

        impl<F: FnMut()> io::Read for CompletingEndpointReader<F> {
            fn read(&mut self, buffer: &mut [u8]) -> io::Result<usize> {
                let endpoint = b"00-11-22-33-44-55\x1b";
                let destination = buffer.get_mut(..endpoint.len()).ok_or_else(|| {
                    io::Error::new(io::ErrorKind::InvalidInput, "fixture buffer too short")
                })?;
                destination.copy_from_slice(endpoint);
                self.reads += 1;
                (self.completion)();
                Ok(endpoint.len())
            }
        }

        #[test]
        fn final_endpoint_read_cancellation_cannot_admit_complete_evidence() -> TestResult {
            let selector = BluetoothDeviceSelector::Address("00-11-22-33-44-55".parse()?);
            let cancellation = BluetoothOpenCancellation::default();
            let mut reader = CompletingEndpointReader {
                completion: || cancellation.cancel(),
                reads: 0,
            };
            let result = super::read_endpoint_until(
                &mut reader,
                &selector,
                BluetoothService::SerialPort,
                Instant::now() + Duration::from_secs(1),
                &cancellation,
            );
            assert_eq!(reader.reads, 1);
            assert!(matches!(
                result,
                Err(TransportError::BluetoothOpenInterrupted)
            ));
            Ok(())
        }

        #[test]
        fn final_endpoint_read_deadline_cannot_admit_complete_evidence() -> TestResult {
            let selector = BluetoothDeviceSelector::Address("00-11-22-33-44-55".parse()?);
            let cancellation = BluetoothOpenCancellation::default();
            let deadline = Instant::now() + Duration::from_secs(1);
            let mut reader = CompletingEndpointReader {
                completion: || {
                    while Instant::now() < deadline {
                        std::thread::sleep(deadline.saturating_duration_since(Instant::now()));
                    }
                },
                reads: 0,
            };
            let result = super::read_endpoint_until(
                &mut reader,
                &selector,
                BluetoothService::SerialPort,
                deadline,
                &cancellation,
            );
            assert_eq!(reader.reads, 1);
            assert!(
                matches!(result, Err(TransportError::BluetoothHelper { source, .. })
                    if source.kind() == io::ErrorKind::TimedOut)
            );
            Ok(())
        }

        #[test]
        fn final_open_admission_cancellation_retires_owned_helper() -> TestResult {
            assert_late_open_retires_helper(true)
        }

        #[test]
        fn final_open_admission_deadline_retires_owned_helper() -> TestResult {
            assert_late_open_retires_helper(false)
        }

        #[test]
        fn final_open_admission_preserves_current_helper_ownership() -> TestResult {
            let _isolation = PROCESS_SLOT_TEST_LOCK
                .lock()
                .map_err(|_poison| "process-slot test lock poisoned")?;
            let transport = owned_pending_test_transport()?;
            let expected_pid = transport
                .child
                .as_ref()
                .ok_or("fixture child missing")?
                .id();
            let mut admitted = transport.admit_open(
                Instant::now() + Duration::from_secs(1),
                &BluetoothOpenCancellation::default(),
            )?;
            let actual_pid = admitted.child.as_ref().map(Child::id);
            let still_running = admitted
                .child
                .as_mut()
                .ok_or("admitted child missing")?
                .try_wait();
            admitted.terminate_helper(false);
            assert_eq!(actual_pid, Some(expected_pid));
            assert!(still_running?.is_none());
            Ok(())
        }

        /// The caller holds `PROCESS_SLOT_TEST_LOCK` through admission and reap.
        fn owned_pending_test_transport() -> Result<BluetoothTransport, Box<dyn Error>> {
            let address = "00-11-22-33-44-55".parse()?;
            let channel = RfcommChannel::new(27)?;
            let helper_executable = std::env::current_exe()?;
            let mut process_slot = Some(HelperProcessSlot::reserve()?);
            let (mut child, stdin, mut stdout, liveness) = spawn_native_test_helper("hang-v1")?;
            let readiness = set_nonblocking(stdout.as_raw_fd())
                .map_err(super::helper_readiness_error)
                .and_then(|()| await_helper_ready(&mut child, &mut stdout));
            if let Err(error) = readiness {
                drop(stdin);
                terminate_child(child, process_slot.take(), Some(liveness), false);
                drop(stdout);
                return Err(error.into());
            }
            Ok(BluetoothTransport {
                child: Some(child),
                helper_stdin: Some(stdin),
                helper_stdout: Some(stdout),
                parent_liveness: Some(liveness),
                helper_healthy: true,
                process_slot,
                address,
                channel,
                close_outcome: None,
                helper_executable,
            })
        }

        fn assert_late_open_retires_helper(cancel: bool) -> TestResult {
            let _isolation = PROCESS_SLOT_TEST_LOCK
                .lock()
                .map_err(|_poison| "process-slot test lock poisoned")?;
            let transport = owned_pending_test_transport()?;
            let child_pid = transport
                .child
                .as_ref()
                .ok_or("fixture child missing")?
                .id();
            let cancellation = BluetoothOpenCancellation::default();
            let deadline = if cancel {
                cancellation.cancel();
                Instant::now() + Duration::from_secs(1)
            } else {
                Instant::now()
            };
            let error = match transport.admit_open(deadline, &cancellation) {
                Ok(mut accepted) => {
                    accepted.terminate_helper(false);
                    return Err("late completed open was admitted".into());
                }
                Err(error) => error,
            };
            if cancel {
                assert!(matches!(error, TransportError::BluetoothOpenInterrupted));
            } else {
                assert!(
                    matches!(error, TransportError::BluetoothHelper { source, .. }
                        if source.kind() == io::ErrorKind::TimedOut)
                );
            }
            let reap_deadline = Instant::now() + Duration::from_secs(1);
            loop {
                if let Ok(released) = HelperProcessSlot::reserve() {
                    drop(released);
                    break;
                }
                if Instant::now() >= reap_deadline {
                    return Err("late helper lease was not released after bounded reap".into());
                }
                std::thread::sleep(Duration::from_millis(5));
            }
            let alive = Command::new("/bin/kill")
                .args(["-0", &child_pid.to_string()])
                .output()?;
            assert!(!alive.status.success(), "late helper was not reaped");
            Ok(())
        }

        #[test]
        fn endpoint_evidence_has_one_absolute_deadline_and_sticky_cancellation() -> TestResult {
            for cancel in [false, true] {
                let selector = BluetoothDeviceSelector::Address("00-11-22-33-44-55".parse()?);
                let mut child = Command::new("/bin/sleep")
                    .arg("10")
                    .stdout(Stdio::piped())
                    .spawn()?;
                let mut stdout = child.stdout.take().ok_or("fixture stdout missing")?;
                set_nonblocking(stdout.as_raw_fd())?;
                let cancellation = BluetoothOpenCancellation::default();
                if cancel {
                    cancellation.cancel();
                }
                let start = Instant::now();
                let result = super::read_endpoint_until(
                    &mut stdout,
                    &selector,
                    BluetoothService::SerialPort,
                    start + Duration::from_millis(10),
                    &cancellation,
                );
                drop(stdout);
                terminate_child(child, None, None, false);
                assert!(start.elapsed() < Duration::from_secs(1));
                if cancel {
                    assert!(matches!(
                        result,
                        Err(TransportError::BluetoothOpenInterrupted)
                    ));
                } else {
                    assert!(
                        matches!(result, Err(TransportError::BluetoothHelper { source, .. }) if source.kind() == io::ErrorKind::TimedOut)
                    );
                }
            }
            Ok(())
        }

        #[test]
        fn close_status_never_conflates_forced_failed_and_confirmed_release() -> TestResult {
            use crate::error::BluetoothCloseFailure;
            assert_eq!(super::close_status(ExitStatus::from_raw(0)), Ok(()));
            assert_eq!(
                super::close_status(ExitStatus::from_raw(89 << 8)),
                Err(BluetoothCloseFailure::ChannelUnconfirmed)
            );
            assert_eq!(
                super::close_status(ExitStatus::from_raw(75 << 8)),
                Err(BluetoothCloseFailure::HelperExited { code: Some(75) })
            );
            assert_eq!(
                super::close_status(ExitStatus::from_raw(9)),
                Err(BluetoothCloseFailure::HelperExited { code: None })
            );
            let child = Command::new("/bin/sleep").arg("10").spawn()?;
            let start = Instant::now();
            let result = super::terminate_child_checked(child, None, None, false);
            assert_eq!(result, Err(BluetoothCloseFailure::ForcedTermination));
            assert!(start.elapsed() < Duration::from_secs(1));
            Ok(())
        }

        #[test]
        fn deferred_reaping_retains_exclusive_lease_until_child_is_reaped() -> TestResult {
            let _isolation = PROCESS_SLOT_TEST_LOCK
                .lock()
                .map_err(|_poison| "process-slot test lock poisoned")?;
            let slot = HelperProcessSlot::reserve()?;
            // Keep the child alive until the contention assertion completes.
            // A timed sleep can expire while this test thread is descheduled.
            let mut child = Command::new("/bin/cat")
                .stdin(Stdio::piped())
                .stdout(Stdio::null())
                .spawn()?;
            let stdin = child.stdin.take().ok_or("pending helper stdin missing")?;
            super::retain_pending_reap(super::PendingReap {
                child,
                slot: Some(slot),
            });
            assert!(
                matches!(HelperProcessSlot::reserve(), Err(TransportError::BluetoothHelper { source, .. })
                    if source.kind() == io::ErrorKind::WouldBlock)
            );
            drop(stdin);
            let deadline = Instant::now() + Duration::from_secs(1);
            loop {
                if let Ok(slot) = HelperProcessSlot::reserve() {
                    drop(slot);
                    break;
                }
                if Instant::now() >= deadline {
                    return Err("pending helper lease was not released".into());
                }
                std::thread::sleep(Duration::from_millis(5));
            }
            Ok(())
        }

        #[test]
        fn service_selection_is_explicit_and_native_sdp_admission_is_callback_bound() -> TestResult
        {
            for (service, expected) in [
                (BluetoothService::FixedChannel(RfcommChannel::new(2)?), "2"),
                (
                    BluetoothService::FixedChannel(RfcommChannel::new(27)?),
                    "27",
                ),
                (BluetoothService::SerialPort, "spp"),
            ] {
                let command =
                    new_helper_command(Path::new("/absolute/helper"), "00-11-22-33-44-55", service);
                assert!(
                    command
                        .get_envs()
                        .any(|(name, value)| name == super::HELPER_CHANNEL_ENV
                            && value == Some(std::ffi::OsStr::new(expected)))
                );
            }
            let native = include_str!("bluetooth_mac.m");
            assert!(native.contains("sdp_phase_failure([device isConnected], resolve_serial_port"));
            assert!(native.contains("g_pending_sdp = sdp"));
            assert!(native.contains("atomic_compare_exchange_strong(&state"));
            assert!(native.contains("matches != 1"));
            assert!(native.contains("uint8_t actual_channel = [ctx->channel getChannelID]"));
            assert!(!native.contains("CFBridgingRelease((__bridge CFTypeRef)channel)"));
            assert!(matches!(
                helper_exit_error(ExitStatus::from_raw(HELPER_EXIT_AMBIGUOUS_DEVICE_NAME << 8)),
                TransportError::BluetoothDeviceNameAmbiguous
            ));
            Ok(())
        }

        #[test]
        fn native_iobluetooth_is_confined_to_process_helper() {
            let shim = include_str!("bluetooth_mac.m");
            let rust = include_str!("bluetooth.rs")
                .split_once("    #[cfg(test)]\n    mod tests")
                .map_or(include_str!("bluetooth.rs"), |(production, _tests)| {
                    production
                });
            let detached_blocking_task = ["spawn", "_blocking"].concat();
            let in_process_write_ffi = ["bt_rfcomm", "_write"].concat();

            assert!(shim.contains("__attribute__((constructor))"));
            assert!(shim.contains("KENWOOD_BT_HELPER_PROCESS_V2"));
            assert!(shim.contains("KENWOOD_BT_HELPER_TEST_MODE"));
            assert!(shim.contains("signal(SIGINT, SIG_IGN)"));
            assert!(shim.contains("signal(SIGQUIT, SIG_IGN)"));
            assert!(!shim.contains("signal(SIGTERM, SIG_IGN)"));
            assert!(shim.contains("strcmp(mode, \"paired\")"));
            assert!(!shim.contains("paired-v"));
            assert!(shim.contains("parent_liveness_watchdog"));
            assert!(shim.contains("pre_ready"));
            assert!(shim.contains("monotonic_seconds() + 20.0"));
            assert!(shim.contains("[ctx->channel writeSync:bytes"));
            assert!(!shim.contains("[ctx->channel writeAsync:"));
            assert!(!shim.contains("sleep:NO]"));
            assert!(!shim.contains("[device closeConnection]"));
            assert!(rust.contains("std::env::current_exe()"));
            assert!(rust.contains("open_with_helper_executable"));
            assert!(rust.contains("validate_helper_executable"));
            assert!(!rust.contains(&in_process_write_ffi));
            assert!(!rust.contains(&detached_blocking_task));
        }

        #[test]
        fn native_device_selection_keeps_exact_addresses_strict() {
            let shim = include_str!("bluetooth_mac.m");
            let address_match = shim.find("caseInsensitiveCompare:identifier]");
            let name_match = shim.find("name_match_count++");

            assert!(matches!(
                (address_match, name_match),
                (Some(address), Some(name)) if address < name
            ));
            assert!(shim.contains("device_identifier_is_exact_address"));
            assert!(shim.contains("bt_device_identifier_matches_display_name("));
            assert!(shim.contains("if (!device && exact_address_selector) return NULL;"));
            assert!(shim.contains("if (!device && name_match_count > 1)"));
            assert!(shim.contains("BT_HELPER_EXIT_AMBIGUOUS_DEVICE_NAME 87"));
            assert!(shim.contains("*failure = BT_HELPER_EXIT_AMBIGUOUS_DEVICE_NAME"));
        }

        #[test]
        fn absent_exact_address_cannot_match_device_named_like_address() -> TestResult {
            let exact_address = CString::new("AA-BB-CC-DD-EE-FF")?;
            let same_display_name = CString::new("AA-BB-CC-DD-EE-FF")?;
            let ordinary_name = CString::new("Field Radio")?;

            // SAFETY: Every pointer comes from a live `CString` and remains
            // valid for the duration of each read-only native predicate call.
            let exact_match = unsafe {
                bt_device_identifier_matches_display_name(
                    exact_address.as_ptr(),
                    same_display_name.as_ptr(),
                )
            };
            // SAFETY: Both inputs are live, NUL-terminated `CString` values.
            let ordinary_match = unsafe {
                bt_device_identifier_matches_display_name(
                    ordinary_name.as_ptr(),
                    ordinary_name.as_ptr(),
                )
            };

            assert_eq!(exact_match, 0);
            assert_eq!(ordinary_match, 1);
            Ok(())
        }

        #[test]
        fn paired_device_inventory_uses_only_address_and_name_metadata() {
            let shim = include_str!("bluetooth_mac.m");
            let inventory = shim
                .split_once("static int run_control_helper")
                .and_then(|(_, remainder)| remainder.split_once("static int run_test_helper"))
                .map_or("", |(inventory, _)| inventory);

            assert!(inventory.contains("[IOBluetoothDevice pairedDevices]"));
            assert!(inventory.contains("write_paired_device_record(device)"));
            assert!(!inventory.contains("device_name_looks_like_d75"));
            assert!(!inventory.contains("tier"));
            assert!(!inventory.contains("device_has_cached_spp_channel"));
            assert!(!inventory.contains("performSDPQuery"));
            assert!(!inventory.contains("getServiceRecordForUUID"));
            assert!(!inventory.contains(".services"));
            assert!(!inventory.contains("[device services]"));
            assert!(!inventory.contains("getServices"));
            assert!(!inventory.contains("openConnection"));
            assert!(!inventory.contains("openRFCOMMChannel"));
        }

        #[test]
        fn tui_reconnect_has_no_main_thread_response_bridge() {
            let main = include_str!("../../thd75-tui/src/main.rs");
            let radio_task = include_str!("../../thd75-tui/src/radio_task.rs");

            assert!(!main.contains("CFRunLoopRunInMode"));
            assert!(!main.contains("bt_req_rx"));
            assert!(!radio_task.contains("recv_timeout"));
            assert!(!radio_task.contains("BT requires main thread"));
            assert!(radio_task.contains("tokio::task::spawn_blocking"));
        }

        #[test]
        fn lodestar_uses_the_same_process_isolation_boundary() {
            let swift = include_str!("../../lodestar/Shared/Transport/IOBluetoothTransport.swift");
            let native = include_str!("../../lodestar/Shared/Transport/IOBluetoothHelper.m");

            assert!(!swift.contains("closeConnection()"));
            assert!(!swift.contains("import IOBluetooth"));
            assert!(!swift.contains("IOBluetoothRFCOMMChannel"));
            assert!(swift.contains("lodestar_bt_helper_spawn"));
            assert!(swift.contains("lodestar_bt_helper_terminate"));
            assert!(swift.contains("BluetoothHelperPipeReader"));
            assert!(native.contains("#include \"../../../kenwood-transport/src/bluetooth_mac.m\""));
            assert!(native.contains("F_DUPFD_CLOEXEC"));
            assert!(native.contains("child_liveness_text"));
            assert!(native.contains("posix_spawn("));
            assert!(native.contains("waitpid("));
            assert!(!native.contains("closeConnection"));
        }

        #[test]
        fn process_slot_rejects_concurrent_helpers_and_releases_on_drop() -> TestResult {
            let _isolation = PROCESS_SLOT_TEST_LOCK
                .lock()
                .map_err(|_poison| "process-slot test lock poisoned")?;
            let first = HelperProcessSlot::reserve()?;
            assert!(matches!(
                HelperProcessSlot::reserve(),
                Err(TransportError::BluetoothHelper { .. })
            ));
            drop(first);
            let after_drop = HelperProcessSlot::reserve()?;
            drop(after_drop);
            Ok(())
        }

        #[test]
        fn relative_custom_helper_path_is_rejected_before_launch() -> TestResult {
            let relative = Path::new("AzimuthBluetoothHelper");
            let Err(TransportError::BluetoothHelper { context, source }) =
                BluetoothTransport::open_with_helper_executable(
                    &BluetoothDeviceSelector::Address("00-11-22-33-44-55".parse()?),
                    BluetoothService::SerialPort,
                    relative,
                    &BluetoothOpenCancellation::default(),
                )
            else {
                return Err("relative Bluetooth helper path was accepted".into());
            };

            assert!(context.contains("AzimuthBluetoothHelper"));
            assert_eq!(source.kind(), io::ErrorKind::InvalidInput);
            assert_eq!(
                source.to_string(),
                "Bluetooth helper executable path must be absolute"
            );
            Ok(())
        }

        #[test]
        fn custom_helper_command_uses_exact_validated_executable() -> TestResult {
            let helper =
                Path::new("/Applications/Azimuth.app/Contents/MacOS/AzimuthBluetoothHelper");
            let validated = validate_helper_executable(helper)?;
            let command = new_helper_command(
                &validated,
                "Custom TH-D75",
                BluetoothService::FixedChannel(RfcommChannel::new(2)?),
            );

            assert_eq!(validated, helper);
            assert_eq!(command.get_program(), helper.as_os_str());
            assert!(
                command
                    .get_args()
                    .any(|argument| argument == "--kenwood-bluetooth-helper")
            );
            Ok(())
        }

        #[test]
        fn paired_device_parser_accepts_exact_addresses_and_arbitrary_display_names() -> TestResult
        {
            let payload = paired_device_payload(&[
                ("00-11-22-33-44-55", "TH-D75"),
                ("AA:BB:CC:DD:EE:FF", "Field Radio One"),
            ])?;

            let devices = parse_paired_device_payload(&payload)?;

            assert_eq!(devices.len(), 2);
            assert_eq!(
                devices.first().map(|device| device.address().as_str()),
                Some("00-11-22-33-44-55")
            );
            assert_eq!(
                devices.get(1).map(|device| device.address().as_str()),
                Some("AA-BB-CC-DD-EE-FF")
            );
            assert_eq!(
                devices
                    .get(1)
                    .map(super::PairedBluetoothDevice::display_name),
                Some("Field Radio One")
            );
            Ok(())
        }

        #[test]
        fn paired_device_parser_coalesces_identical_records_after_address_normalization()
        -> TestResult {
            let payload = paired_device_payload(&[
                ("00-11-22-33-44-55", "First"),
                ("aa:bb:cc:dd:ee:ff", "stm32mp1-ex5240"),
                ("11-22-33-44-55-66", "Last"),
                ("AA-BB-CC-DD-EE-FF", "stm32mp1-ex5240"),
            ])?;
            let devices = parse_paired_device_payload(&payload)?;
            let observed: Vec<_> = devices
                .iter()
                .map(|device| (device.address().as_str(), device.display_name()))
                .collect();
            assert_eq!(
                observed,
                [
                    ("00-11-22-33-44-55", "First"),
                    ("AA-BB-CC-DD-EE-FF", "stm32mp1-ex5240"),
                    ("11-22-33-44-55-66", "Last"),
                ]
            );
            Ok(())
        }

        #[test]
        fn paired_device_parser_counts_raw_records_before_deduplication() -> TestResult {
            let records = vec![("00-11-22-33-44-55", "Radio"); super::MAX_PAIRED_DEVICES];
            let payload = paired_device_payload(&records)?;
            assert_eq!(parse_paired_device_payload(&payload)?.len(), 1);

            let records = vec![("00-11-22-33-44-55", "Radio"); super::MAX_PAIRED_DEVICES + 1];
            let payload = paired_device_payload(&records)?;
            let error = parse_paired_device_payload(&payload)
                .err()
                .ok_or("duplicate records bypassed the inventory bound")?;
            assert_eq!(error.kind(), io::ErrorKind::InvalidData);
            assert!(error.to_string().contains("exceeded"));
            Ok(())
        }

        #[test]
        fn paired_device_parser_rejects_conflicting_names_for_one_address() -> TestResult {
            let payload = paired_device_payload(&[
                ("00-11-22-33-44-55", "First"),
                ("00:11:22:33:44:55", "Second"),
            ])?;

            let Err(error) = parse_paired_device_payload(&payload) else {
                return Err("conflicting names for one Bluetooth address were accepted".into());
            };

            assert_eq!(error.kind(), io::ErrorKind::InvalidData);
            assert!(error.to_string().contains("conflicting display names"));
            Ok(())
        }

        #[test]
        fn paired_device_parser_validates_records_after_identical_duplicates() -> TestResult {
            let records = [
                ("00-11-22-33-44-55", "Radio"),
                ("00:11:22:33:44:55", "Radio"),
            ];
            let mut truncated = paired_device_payload(&records)?;
            let _removed = truncated.pop();
            let mut trailing = paired_device_payload(&records)?;
            trailing.push(0x41);
            let mut invalid_name =
                paired_device_payload(&[records[0], records[1], ("00-11-22-33-44-55", "X")])?;
            let name_index = invalid_name.len().checked_sub(5).ok_or("short fixture")?;
            *invalid_name
                .get_mut(name_index)
                .ok_or("missing name byte")? = 0xFF;
            for (payload, kind) in [
                (truncated, io::ErrorKind::UnexpectedEof),
                (trailing, io::ErrorKind::InvalidData),
                (invalid_name, io::ErrorKind::InvalidData),
            ] {
                let error = parse_paired_device_payload(&payload)
                    .err()
                    .ok_or("deduplication bypassed malformed remaining bytes")?;
                assert_eq!(error.kind(), kind);
            }
            Ok(())
        }

        #[test]
        fn paired_device_parser_rejects_name_like_or_truncated_selectors() -> TestResult {
            for address in ["TH-D75", "00-11-22-33-44", "00-11-22-33-44-GG"] {
                let payload = paired_device_payload(&[(address, "Radio")])?;
                let Err(error) = parse_paired_device_payload(&payload) else {
                    return Err("non-address Bluetooth selector was accepted".into());
                };
                assert_eq!(error.kind(), io::ErrorKind::InvalidData);
                assert!(
                    error.to_string().contains("not exact")
                        || error.to_string().contains("field bounds")
                );
            }
            Ok(())
        }

        #[test]
        fn paired_device_parser_requires_one_final_terminator() -> TestResult {
            let mut truncated = paired_device_payload(&[("00-11-22-33-44-55", "Radio")])?;
            truncated.truncate(truncated.len().saturating_sub(2));
            let Err(truncated_error) = parse_paired_device_payload(&truncated) else {
                return Err("truncated paired-device payload was accepted".into());
            };
            assert_eq!(truncated_error.kind(), io::ErrorKind::UnexpectedEof);

            let mut trailing = paired_device_payload(&[("00-11-22-33-44-55", "Radio")])?;
            trailing.push(0x41);
            let Err(trailing_error) = parse_paired_device_payload(&trailing) else {
                return Err("bytes after paired-device terminator were accepted".into());
            };
            assert_eq!(trailing_error.kind(), io::ErrorKind::InvalidData);
            Ok(())
        }

        #[test]
        fn pre_cancelled_open_stops_before_helper_launch() -> TestResult {
            let cancellation = BluetoothOpenCancellation::default();
            cancellation.cancel();

            let result = BluetoothTransport::open_with_helper_executable(
                &BluetoothDeviceSelector::Address("00-11-22-33-44-55".parse()?),
                BluetoothService::SerialPort,
                "/helper-does-not-need-to-exist",
                &cancellation,
            );

            assert!(matches!(
                result,
                Err(TransportError::BluetoothOpenInterrupted)
            ));
            Ok(())
        }

        #[test]
        fn cancellation_interrupts_blocked_helper_readiness() -> TestResult {
            let mut child = Command::new("/bin/sleep")
                .arg("30")
                .stdout(Stdio::piped())
                .spawn()?;
            let mut stdout = child.stdout.take().ok_or("blocked helper has no stdout")?;
            set_nonblocking(stdout.as_raw_fd())?;
            let cancellation = BluetoothOpenCancellation::default();
            let cancellation_signal = cancellation.clone();
            let requester = std::thread::spawn(move || {
                std::thread::sleep(Duration::from_millis(20));
                cancellation_signal.cancel();
            });

            let started = Instant::now();
            let result = await_helper_ready_cancellable(&mut child, &mut stdout, &cancellation);
            drop(stdout);
            terminate_child(child, None, None, false);
            requester
                .join()
                .map_err(|_panic| "cancellation requester panicked")?;

            assert!(matches!(
                result,
                Err(TransportError::BluetoothOpenInterrupted)
            ));
            assert!(
                started.elapsed() < Duration::from_secs(1),
                "blocked helper cancellation was not prompt"
            );
            Ok(())
        }

        #[test]
        fn helper_validation_accepts_exact_echo_and_clean_exit() -> TestResult {
            let mut child = Command::new("/bin/sh")
                .args(["-c", "printf 'KENWBT-READY-v2!'; /bin/cat"])
                .stdin(Stdio::piped())
                .stdout(Stdio::piped())
                .spawn()?;
            let stdin = child.stdin.take().ok_or("echo helper has no stdin")?;
            let mut stdout = child.stdout.take().ok_or("echo helper has no stdout")?;
            set_nonblocking(stdout.as_raw_fd())?;

            validate_helper_echo_until(
                &mut child,
                stdin,
                &mut stdout,
                Instant::now() + Duration::from_secs(1),
            )?;
            assert_eq!(HELPER_VALIDATION_CHALLENGE, b"AZIMUTH-BT-HELPER-v1");
            assert!(child.try_wait()?.is_some());
            Ok(())
        }

        #[test]
        fn helper_validation_timeout_is_bounded() -> TestResult {
            let mut child = Command::new("/bin/sleep")
                .arg("30")
                .stdin(Stdio::piped())
                .stdout(Stdio::piped())
                .spawn()?;
            let stdin = child.stdin.take().ok_or("blocked helper has no stdin")?;
            let mut stdout = child.stdout.take().ok_or("blocked helper has no stdout")?;
            set_nonblocking(stdout.as_raw_fd())?;

            let started = Instant::now();
            let result = validate_helper_echo_until(
                &mut child,
                stdin,
                &mut stdout,
                Instant::now() + Duration::from_millis(20),
            );
            drop(stdout);
            terminate_child(child, None, None, false);

            let Err(error) = result else {
                return Err("blocked validation helper did not time out".into());
            };
            assert!(
                matches!(error, TransportError::BluetoothHelper { source, .. }
                if source.kind() == io::ErrorKind::TimedOut)
            );
            assert!(
                started.elapsed() < Duration::from_secs(1),
                "helper validation timeout exceeded its test bound"
            );
            Ok(())
        }

        #[test]
        fn only_helper_exit_code_71_reports_absent_paired_device() {
            let retryable = helper_exit_error(ExitStatus::from_raw(71 << 8));
            assert!(matches!(retryable, TransportError::NotFound));

            for raw_status in [0, 72 << 8, 74 << 8, 9] {
                let non_retryable = helper_exit_error(ExitStatus::from_raw(raw_status));
                assert!(
                    matches!(non_retryable, TransportError::BluetoothHelper { .. }),
                    "raw wait status {raw_status} was unexpectedly retryable"
                );
            }
        }

        #[test]
        fn legacy_empty_stdout_cannot_authorize_retry_after_the_original_deadline() -> TestResult {
            let mut child = Command::new("/bin/sh")
                .args(["-c", "exec 1>&-; read -r gate; exit 71"])
                .stdin(Stdio::piped())
                .stdout(Stdio::piped())
                .spawn()?;
            let Some(mut stdout) = child.stdout.take() else {
                terminate_child(child, None, None, false);
                return Err("legacy deadline fixture has no stdout".into());
            };
            let Some(mut stdin) = child.stdin.take() else {
                terminate_child(child, None, None, false);
                return Err("legacy deadline fixture has no stdin".into());
            };
            // Observe EOF before starting the deadline, independent of child
            // startup scheduling. Keep its exit blocked on our owned stdin.
            let eof = stdout.read(&mut [0_u8; 1]);
            if !matches!(eof, Ok(0)) {
                terminate_child(child, None, None, false);
                return Err("legacy fixture did not close stdout".into());
            }
            let releaser = std::thread::spawn(move || {
                std::thread::sleep(Duration::from_millis(80));
                stdin.write_all(b"\n")
            });
            let result = super::await_helper_ready_until(
                &mut child,
                &mut stdout,
                Instant::now() + Duration::from_millis(40),
                &BluetoothOpenCancellation::default(),
            );
            let release = releaser.join();
            terminate_child(child, None, None, false);
            release.map_err(|_panic| "legacy fixture release worker panicked")??;
            assert!(
                matches!(result, Err(TransportError::BluetoothHelper { .. })),
                "late legacy helper was retry-eligible: {result:?}"
            );
            Ok(())
        }

        #[test]
        fn stdout_eof_waits_boundedly_for_delayed_exit_71() -> TestResult {
            let mut child = Command::new("/bin/sh")
                .args(["-c", "exec 1>&-; /bin/sleep 0.02; exit 71"])
                .stdout(Stdio::piped())
                .spawn()?;
            let mut stdout = child
                .stdout
                .take()
                .ok_or("delayed-exit helper has no stdout")?;
            let started = Instant::now();
            let Err(error) = await_helper_ready(&mut child, &mut stdout) else {
                return Err("helper unexpectedly reported readiness".into());
            };

            assert!(matches!(error, TransportError::NotFound));
            assert!(
                started.elapsed() < HELPER_EOF_EXIT_BUDGET,
                "delayed helper exit exceeded EOF reap budget"
            );
            Ok(())
        }

        #[test]
        fn partial_invalid_readiness_is_open_even_when_helper_exits_71() -> TestResult {
            let mut child = Command::new("/bin/sh")
                .args(["-c", "printf BAD; exit 71"])
                .stdout(Stdio::piped())
                .spawn()?;
            let mut stdout = child
                .stdout
                .take()
                .ok_or("invalid-prefix helper has no stdout")?;
            let Err(error) = await_helper_ready(&mut child, &mut stdout) else {
                return Err("helper unexpectedly accepted an invalid prefix".into());
            };
            let _status = child.wait()?;

            assert!(matches!(error, TransportError::BluetoothHelper { .. }));
            Ok(())
        }

        #[test]
        fn current_executable_helper_constructor_is_raw_echo_stream() -> TestResult {
            let (mut child, mut stdin, mut stdout, parent_liveness) =
                spawn_native_test_helper("echo-v1")?;
            let mut ready = [0_u8; HELPER_READY_MAGIC.len()];
            stdout.read_exact(&mut ready)?;
            assert_eq!(&ready, HELPER_READY_MAGIC);

            let payload = b"ID\rW 0000\r\0binary";
            stdin.write_all(payload)?;
            drop(stdin);
            let mut echoed = Vec::new();
            let _ = stdout.read_to_end(&mut echoed)?;
            let status = child.wait()?;
            drop(parent_liveness);
            assert!(status.success());
            assert_eq!(&echoed, payload);
            Ok(())
        }

        #[test]
        fn interactive_sigint_does_not_kill_the_radio_helper() -> TestResult {
            let (mut child, mut stdin, mut stdout, parent_liveness) =
                spawn_native_test_helper("echo-v1")?;
            let mut ready = [0_u8; HELPER_READY_MAGIC.len()];
            stdout.read_exact(&mut ready)?;
            assert_eq!(&ready, HELPER_READY_MAGIC);

            let signal = Command::new("/bin/kill")
                .args(["-INT", &child.id().to_string()])
                .output()?;
            assert!(
                signal.status.success(),
                "failed to signal helper: {signal:?}"
            );
            assert!(
                child.try_wait()?.is_none(),
                "the helper inherited the terminal's default SIGINT action"
            );

            let payload = b"still-connected";
            stdin.write_all(payload)?;
            drop(stdin);
            let mut echoed = Vec::new();
            let _ = stdout.read_to_end(&mut echoed)?;
            let status = child.wait()?;
            drop(parent_liveness);
            assert!(status.success());
            assert_eq!(echoed, payload);
            Ok(())
        }

        #[test]
        fn wedged_current_executable_helper_is_bounded_and_reaped() -> TestResult {
            let (child, stdin, mut stdout, parent_liveness) = spawn_native_test_helper("hang-v1")?;
            let pid = child.id();
            let mut ready = [0_u8; HELPER_READY_MAGIC.len()];
            stdout.read_exact(&mut ready)?;
            assert_eq!(&ready, HELPER_READY_MAGIC);
            drop(stdin);
            drop(stdout);

            let started = Instant::now();
            terminate_child(child, None, Some(parent_liveness), true);
            let elapsed = started.elapsed();
            let bounded_teardown =
                GRACEFUL_EXIT_BUDGET + SYNC_REAP_BUDGET + Duration::from_millis(250);
            assert!(
                elapsed < bounded_teardown,
                "wedged helper teardown took {elapsed:?}, expected less than {bounded_teardown:?}"
            );

            let probe = Command::new("/bin/kill")
                .args(["-0", &pid.to_string()])
                .output()?;
            assert!(!probe.status.success());
            Ok(())
        }

        #[test]
        fn parent_liveness_eof_exits_even_wedged_helper() -> TestResult {
            let (mut child, stdin, mut stdout, parent_liveness) =
                spawn_native_test_helper("hang-v1")?;
            let mut ready = [0_u8; HELPER_READY_MAGIC.len()];
            stdout.read_exact(&mut ready)?;
            assert_eq!(&ready, HELPER_READY_MAGIC);

            drop(parent_liveness);
            let deadline = Instant::now() + Duration::from_secs(1);
            loop {
                if child.try_wait()?.is_some() {
                    drop(stdin);
                    drop(stdout);
                    return Ok(());
                }
                if Instant::now() >= deadline {
                    drop(stdin);
                    drop(stdout);
                    drop(child.kill());
                    drop(child.wait());
                    return Err("helper watchdog did not observe parent liveness EOF".into());
                }
                std::thread::sleep(Duration::from_millis(5));
            }
        }

        #[test]
        fn cancelled_write_guard_kills_helper_process() -> TestResult {
            let child = Command::new("/bin/sleep").arg("30").spawn()?;
            let pid = child.id();
            let mut child = Some(child);
            let mut process_slot = None;
            let mut parent_liveness = None;
            let mut helper_healthy = true;
            let mut close_outcome = None;
            {
                let _guard = HelperWriteCancellation::new(
                    &mut child,
                    &mut process_slot,
                    &mut parent_liveness,
                    &mut helper_healthy,
                    &mut close_outcome,
                );
            }
            assert!(!helper_healthy);
            assert!(child.is_none());
            assert!(matches!(
                close_outcome,
                Some(Err(super::BluetoothCloseFailure::ForcedTermination))
            ));

            let deadline = Instant::now() + Duration::from_secs(1);
            loop {
                let probe = Command::new("/bin/kill")
                    .args(["-0", &pid.to_string()])
                    .output()?;
                if !probe.status.success() {
                    return Ok(());
                }
                if Instant::now() >= deadline {
                    return Err("cancelled write guard did not kill helper".into());
                }
                std::thread::sleep(Duration::from_millis(5));
            }
        }

        #[test]
        fn completed_write_guard_leaves_helper_running() -> TestResult {
            let child = Command::new("/bin/sleep").arg("30").spawn()?;
            let mut child = Some(child);
            let mut process_slot = None;
            let mut parent_liveness = None;
            let mut helper_healthy = true;
            let mut close_outcome = None;
            {
                let mut guard = HelperWriteCancellation::new(
                    &mut child,
                    &mut process_slot,
                    &mut parent_liveness,
                    &mut helper_healthy,
                    &mut close_outcome,
                );
                guard.disarm();
            }
            assert!(helper_healthy);
            assert_eq!(close_outcome, None);
            let mut child = child.ok_or("completed write guard lost helper")?;
            assert!(child.try_wait()?.is_none());
            child.kill()?;
            let _ = child.wait()?;
            Ok(())
        }

        fn spawn_native_test_helper(
            mode: &str,
        ) -> Result<(Child, ChildStdin, ChildStdout, OwnedFd), Box<dyn Error>> {
            // SAFETY: No arguments or runtime behavior; this only anchors the
            // Objective-C constructor's object file in the test executable.
            unsafe { bt_helper_link_anchor() };
            let executable = std::env::current_exe()?;
            let (helper_liveness, parent_liveness) = create_liveness_pipe()?;
            let mut command = Command::new(executable);
            let _command = command
                .arg("--kenwood-bluetooth-helper-test")
                .env(HELPER_SENTINEL_ENV, HELPER_SENTINEL_VALUE)
                .env(HELPER_TEST_ENV, mode)
                .env(HELPER_LIVENESS_FD_ENV, HELPER_LIVENESS_FD.to_string())
                .env_remove(HELPER_CONTROL_ENV)
                .stdin(Stdio::piped())
                .stdout(Stdio::piped())
                .stderr(Stdio::inherit());
            prepare_liveness_fd(&mut command, helper_liveness.as_raw_fd());
            let mut child = command.spawn()?;
            drop(helper_liveness);
            let stdin = child
                .stdin
                .take()
                .ok_or_else(|| io::Error::other("native Bluetooth test helper has no stdin"))?;
            let stdout = child
                .stdout
                .take()
                .ok_or_else(|| io::Error::other("native Bluetooth test helper has no stdout"))?;
            Ok((child, stdin, stdout, parent_liveness))
        }

        fn paired_device_payload(records: &[(&str, &str)]) -> Result<Vec<u8>, Box<dyn Error>> {
            let mut payload = Vec::new();
            for (address, name) in records {
                let address_length = u16::try_from(address.len())?;
                let name_length = u16::try_from(name.len())?;
                payload.extend_from_slice(&address_length.to_be_bytes());
                payload.extend_from_slice(&name_length.to_be_bytes());
                payload.extend_from_slice(address.as_bytes());
                payload.extend_from_slice(name.as_bytes());
            }
            payload.extend_from_slice(&[0, 0, 0, 0]);
            Ok(payload)
        }
    }
}

#[cfg(target_os = "macos")]
pub use inner::BluetoothTransport;
