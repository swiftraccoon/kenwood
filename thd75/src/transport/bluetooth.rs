//! Native macOS Bluetooth RFCOMM transport.
//!
//! Bypasses the broken `IOUserBluetoothSerialDriver` serial port driver and
//! talks directly to the radio through `IOBluetoothRFCOMMChannel`.
//!
//! `IOBluetooth` writes can block forever when RFCOMM flow-control credit
//! stalls, including its nominally asynchronous API (which blocks the main
//! dispatch queue later). The framework therefore runs in a killable helper
//! process. The parent communicates with it through non-blocking raw byte
//! pipes and never calls an `IOBluetooth` write or close routine itself.
//!
//! A newly launched `IOBluetooth` shim can briefly report an already-connected
//! baseband before its process-local Classic manager is ready to open RFCOMM.
//! A native open that reaches that state is bounded and reported as
//! [`TransportError::NotFound`](crate::error::TransportError::NotFound);
//! construction retries that failure exactly once in a fresh helper after a
//! short delay. Neither the radio's baseband nor any system Bluetooth process
//! is torn down as part of open or recovery.
//!
//! This module is only available on macOS (`cfg(target_os = "macos")`).

#[cfg(any(target_os = "macos", all(doc, unix)))]
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
        atomic::{AtomicBool, Ordering},
        mpsc,
    };
    use std::time::{Duration, Instant};

    use crate::error::TransportError;
    use crate::transport::Transport;

    unsafe extern "C" {
        fn bt_helper_link_anchor();
        fn bt_fd_set_nonblocking(fd: i32) -> i32;
        fn bt_liveness_pipe_create(read_fd: *mut i32, write_fd: *mut i32) -> i32;
        fn bt_helper_prepare_liveness_fd(source_fd: i32, target_fd: i32) -> i32;
    }

    /// The RFCOMM channel for the TH-D75's SPP (Serial Port) service.
    const SPP_CHANNEL: u8 = 2;

    /// Default device name for Bluetooth discovery.
    const DEFAULT_DEVICE_NAME: &str = "TH-D75";

    /// Private launch sentinel recognized by the Objective-C constructor
    /// before the selected helper reaches ordinary `main`.
    const HELPER_SENTINEL_ENV: &str = "THD75_BT_HELPER_PROCESS_V1";
    const HELPER_SENTINEL_VALUE: &str = "4d7f29c8b35a";
    const HELPER_DEVICE_ENV: &str = "THD75_BT_HELPER_DEVICE";
    const HELPER_CHANNEL_ENV: &str = "THD75_BT_HELPER_CHANNEL";
    const HELPER_CONTROL_ENV: &str = "THD75_BT_HELPER_CONTROL_MODE";
    const HELPER_PAIRED_CONTROL_MODE: &str = "paired-v2";
    const HELPER_TEST_ENV: &str = "THD75_BT_HELPER_TEST_MODE";
    const HELPER_LIVENESS_FD_ENV: &str = "THD75_BT_HELPER_LIVENESS_FD";
    const HELPER_LIVENESS_FD: i32 = 3;

    /// Prefix emitted by the helper after RFCOMM is open and before it
    /// enables radio ingress on stdout.
    const HELPER_READY_MAGIC: &[u8; 16] = b"THD75BT-READY-v1";

    /// Maximum time one helper attempt waits for RFCOMM open.
    /// Two seconds of scheduling margin sit above the native single
    /// 20-second SDP/baseband/channel-open deadline.
    const HELPER_OPEN_TIMEOUT: Duration = Duration::from_secs(22);

    /// Delay before the one fresh-helper retry after a native open failure.
    const HELPER_OPEN_RETRY_DELAY: Duration = Duration::from_secs(1);

    /// Public construction performs at most two independent helper attempts.
    const HELPER_OPEN_MAX_ATTEMPTS: u8 = 2;

    /// Cold App Sandbox initialization of `IOBluetooth` can exceed five
    /// seconds before `pairedDevices` returns. Keep the signed helper's whole
    /// ready/list/exit cycle under the same hard ceiling as one RFCOMM open.
    const HELPER_ENUMERATION_TIMEOUT: Duration = Duration::from_secs(22);

    /// Maximum paired records accepted from one signed helper invocation.
    const MAX_PAIRED_CANDIDATES: usize = 64;

    /// Bluetooth names are normally limited to 248 bytes. This larger bound
    /// tolerates framework formatting while keeping the helper payload finite.
    const MAX_PAIRED_DISPLAY_NAME_BYTES: usize = 1024;

    /// One hint byte, four length bytes, one exact address, and one bounded
    /// display name per record, followed by the five-byte terminator.
    const MAX_PAIRED_PAYLOAD_BYTES: usize =
        MAX_PAIRED_CANDIDATES * (5 + 17 + MAX_PAIRED_DISPLAY_NAME_BYTES) + 5;

    /// Enumeration metadata that justifies retrying a transient probe open.
    const PAIRED_CANDIDATE_HINT_CACHED_SPP: u8 = 1 << 0;
    const PAIRED_CANDIDATE_HINT_D75_NAME: u8 = 1 << 1;
    const PAIRED_CANDIDATE_KNOWN_HINTS: u8 =
        PAIRED_CANDIDATE_HINT_CACHED_SPP | PAIRED_CANDIDATE_HINT_D75_NAME;

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

    /// Hard ceiling for direct transport writes when no outer radio timeout
    /// is present. Radio operations normally cancel sooner using their own
    /// configured command timeout.
    const PIPE_WRITE_TIMEOUT: Duration = Duration::from_secs(5);

    /// POSIX guarantees atomic non-blocking pipe writes through `PIPE_BUF`;
    /// macOS reports 512 bytes. Every TH-D75 command frame is at most 261
    /// bytes, but chunking keeps the transport correct for arbitrary callers.
    const MACOS_PIPE_BUF: usize = 512;

    /// Healthy helpers get a short EOF-driven graceful-close opportunity
    /// after the parent drops both pipes.
    const GRACEFUL_EXIT_BUDGET: Duration = Duration::from_millis(600);

    /// Maximum synchronous time spent checking that SIGKILL reaped the
    /// helper. A detached waiter owns the child after this additional bound.
    const SYNC_REAP_BUDGET: Duration = Duration::from_millis(100);

    /// The radio supports one SPP connection. Preserve the prior native
    /// transport's one-handle-per-process invariant across helper processes.
    static HELPER_PROCESS_SLOT_RESERVED: AtomicBool = AtomicBool::new(false);

    /// One paired Bluetooth device that can be tried by exact address.
    #[derive(Debug, Clone, PartialEq, Eq)]
    pub struct PairedBluetoothCandidate {
        address: String,
        display_name: String,
        /// Native cached-SPP or D75-name evidence. This is deliberately not a
        /// public identity claim; it controls only a bounded transient retry.
        retry_transient_probe_not_found: bool,
    }

    impl PairedBluetoothCandidate {
        /// Exact address returned by `IOBluetooth` for unambiguous selection.
        #[must_use]
        pub fn address(&self) -> &str {
            &self.address
        }

        /// Human-readable paired-device name for diagnostics only.
        #[must_use]
        pub fn display_name(&self) -> &str {
            &self.display_name
        }
    }

    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    enum PairedCandidateOpenPurpose {
        Probe,
        Selected,
    }

    impl PairedCandidateOpenPurpose {
        const fn retries_transient_not_found(self, candidate: &PairedBluetoothCandidate) -> bool {
            match self {
                Self::Probe => candidate.retry_transient_probe_not_found,
                Self::Selected => true,
            }
        }
    }

    /// Native macOS Bluetooth transport using an isolated `IOBluetooth` helper.
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
        /// The device name or address this transport was opened with (`None`
        /// used the default name); reopen reuses it.
        device_name: Option<String>,
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
                .field("device_name", &self.device_name)
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

    fn new_helper_command(helper_executable: &Path, device_name: &str) -> Command {
        let mut command = Command::new(helper_executable);
        let _command = command
            .arg("--thd75-bluetooth-helper")
            .env(HELPER_SENTINEL_ENV, HELPER_SENTINEL_VALUE)
            .env(HELPER_DEVICE_ENV, device_name)
            .env(HELPER_CHANNEL_ENV, SPP_CHANNEL.to_string())
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
            .arg("--thd75-bluetooth-helper-control")
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

    fn bluetooth_helper_error(context: impl Into<String>, source: io::Error) -> TransportError {
        TransportError::BluetoothHelper {
            context: context.into(),
            source,
        }
    }

    impl BluetoothTransport {
        /// Enumerate paired devices that can be tried as SPP candidates.
        ///
        /// Discovery runs in the same isolated native helper used for RFCOMM,
        /// but it performs no radio I/O. Each returned candidate carries the
        /// exact Bluetooth address required for unambiguous later selection.
        /// The helper invocation, record count, field sizes, and total payload
        /// are independently bounded.
        ///
        /// # Errors
        ///
        /// Returns [`TransportError::BluetoothHelper`] if the current
        /// executable cannot be located, the helper cannot be launched, the
        /// discovery deadline expires, or its framed response is invalid.
        pub fn paired_spp_candidates() -> Result<Vec<PairedBluetoothCandidate>, TransportError> {
            let executable = std::env::current_exe().map_err(|source| {
                bluetooth_helper_error("locating the current executable", source)
            })?;
            Self::paired_spp_candidates_with_helper_executable(executable)
        }

        /// Enumerate paired SPP candidates through a specific signed helper.
        ///
        /// Sandboxed applications should pass their separately signed,
        /// sandbox-inheriting helper executable. The path must be absolute and
        /// the executable must contain this crate's native helper constructor.
        /// This operation does not open RFCOMM or send any bytes to a radio.
        ///
        /// # Errors
        ///
        /// Returns [`TransportError::BluetoothHelper`] if the path or helper
        /// lifecycle is invalid, discovery exceeds its bound, or the helper's
        /// candidate framing is malformed.
        pub fn paired_spp_candidates_with_helper_executable(
            helper_executable: impl AsRef<Path>,
        ) -> Result<Vec<PairedBluetoothCandidate>, TransportError> {
            let helper_executable = validate_helper_executable(helper_executable.as_ref())?;
            enumerate_paired_candidates(&helper_executable)
        }

        /// Probe one enumerated candidate by its exact address.
        ///
        /// Candidate enumeration carries native cached-SPP and D75-name hints.
        /// A hinted candidate gets the transport's single transient
        /// [`TransportError::NotFound`] retry; an unhinted candidate gets one
        /// attempt so a scan does not repeatedly wake unrelated paired phones.
        /// Each attempt remains independently bounded.
        ///
        /// # Errors
        ///
        /// Returns [`TransportError::BluetoothHelper`] if the signed helper
        /// cannot be launched or prepared, and [`TransportError::NotFound`] if
        /// this exact candidate does not expose the TH-D75 SPP channel.
        pub fn probe_paired_candidate_with_helper_executable(
            candidate: &PairedBluetoothCandidate,
            helper_executable: impl AsRef<Path>,
        ) -> Result<Self, TransportError> {
            let helper_executable = validate_helper_executable(helper_executable.as_ref())?;
            Self::open_exact_paired_candidate(
                candidate,
                &helper_executable,
                PairedCandidateOpenPurpose::Probe,
            )
        }

        /// Open one selected candidate by its exact address.
        ///
        /// This uses the same single transient [`TransportError::NotFound`]
        /// retry as [`Self::open_with_helper_executable`]. Callers that are
        /// still scanning an unqualified candidate set should use
        /// [`Self::probe_paired_candidate_with_helper_executable`] instead.
        ///
        /// # Errors
        ///
        /// Returns [`TransportError::BluetoothHelper`] if the signed helper
        /// cannot be launched or prepared, and [`TransportError::NotFound`] if
        /// this exact candidate cannot be opened in either bounded attempt.
        pub fn open_paired_candidate_with_helper_executable(
            candidate: &PairedBluetoothCandidate,
            helper_executable: impl AsRef<Path>,
        ) -> Result<Self, TransportError> {
            let helper_executable = validate_helper_executable(helper_executable.as_ref())?;
            Self::open_exact_paired_candidate(
                candidate,
                &helper_executable,
                PairedCandidateOpenPurpose::Selected,
            )
        }

        fn open_exact_paired_candidate(
            candidate: &PairedBluetoothCandidate,
            helper_executable: &Path,
            purpose: PairedCandidateOpenPurpose,
        ) -> Result<Self, TransportError> {
            let retry_not_found = purpose.retries_transient_not_found(candidate);
            let max_attempts = if retry_not_found {
                HELPER_OPEN_MAX_ATTEMPTS
            } else {
                1
            };
            open_with_not_found_retry_policy(
                retry_not_found,
                |attempt| {
                    Self::open_once(
                        Some(candidate.address()),
                        helper_executable,
                        attempt,
                        max_attempts,
                    )
                },
                |delay| {
                    tracing::warn!(
                        device = %candidate.address(),
                        failed_attempt = 1,
                        next_attempt = 2,
                        delay_ms = delay.as_millis(),
                        "exact-address Bluetooth RFCOMM helper open returned NotFound; retrying once"
                    );
                    std::thread::sleep(delay);
                },
            )
        }

        /// Connect to a TH-D75 radio through a killable Bluetooth helper.
        ///
        /// The helper is the current signed executable re-launched with a
        /// private environment sentinel. An Objective-C constructor takes over
        /// before Rust `main`, opens RFCOMM on its own main run loop, emits a
        /// fixed readiness prefix, and then treats stdin/stdout as raw serial
        /// byte streams. If that native attempt reports
        /// [`TransportError::NotFound`], construction waits one second and
        /// tries once more in a new helper. Other errors are returned without
        /// retry. Each attempt is bounded independently, so the two-attempt
        /// path can take about 45 seconds.
        ///
        /// `device_name` can be either a paired device's exact display name or
        /// its exact Bluetooth address. Address matching takes precedence. A
        /// display name shared by multiple paired devices fails closed; pass
        /// the exact address to select one of those radios.
        ///
        /// # Errors
        ///
        /// Returns [`TransportError::BluetoothHelper`] when the current
        /// executable cannot be located or launched as a compatible helper,
        /// [`TransportError::BluetoothDeviceNameAmbiguous`] when multiple
        /// paired devices share the requested name, or
        /// [`TransportError::NotFound`] when the paired device or RFCOMM
        /// channel cannot be opened in either bounded helper attempt.
        pub fn open(device_name: Option<&str>) -> Result<Self, TransportError> {
            let executable = std::env::current_exe().map_err(|source| {
                bluetooth_helper_error("locating the current executable", source)
            })?;
            Self::open_with_helper_executable(device_name, executable)
        }

        /// Connect using a compatible executable as the native helper.
        ///
        /// Sandboxed applications cannot safely re-execute their main app as
        /// a child because the child must carry the sandbox-inheritance
        /// entitlement and no other App Sandbox capabilities. They should
        /// embed a minimal signed helper tool with those entitlements.
        ///
        /// This method does not turn an arbitrary executable into a Bluetooth
        /// helper. The selected executable must link this crate's native
        /// macOS helper implementation and reference `bt_helper_link_anchor`.
        /// That reference retains the Objective-C constructor that recognizes
        /// the private parent launch sentinel and takes control before the
        /// helper's ordinary `main`. The executable must also support the
        /// host's architecture and remain runnable at the same location for
        /// the transport's lifetime.
        ///
        /// `helper_executable` must be absolute. Reopen operations preserve
        /// and reuse the validated path.
        /// Device selection follows [`Self::open`]: an exact Bluetooth address
        /// takes precedence, and a non-unique display name is rejected.
        ///
        /// # Errors
        ///
        /// Returns [`TransportError::BluetoothHelper`] when the path is
        /// relative, the helper cannot be started, or its readiness handshake
        /// fails. Returns [`TransportError::BluetoothDeviceNameAmbiguous`]
        /// when multiple paired devices share the requested name, or
        /// [`TransportError::NotFound`] when the paired device or RFCOMM
        /// channel cannot be opened in either bounded helper attempt.
        pub fn open_with_helper_executable(
            device_name: Option<&str>,
            helper_executable: impl AsRef<Path>,
        ) -> Result<Self, TransportError> {
            let name = device_name.unwrap_or(DEFAULT_DEVICE_NAME);
            let helper_executable = validate_helper_executable(helper_executable.as_ref())?;
            open_with_not_found_retry_policy(
                true,
                |attempt| {
                    Self::open_once(
                        device_name,
                        &helper_executable,
                        attempt,
                        HELPER_OPEN_MAX_ATTEMPTS,
                    )
                },
                |delay| {
                    tracing::warn!(
                        device = %name,
                        failed_attempt = 1,
                        next_attempt = 2,
                        delay_ms = delay.as_millis(),
                        "Bluetooth RFCOMM helper open returned NotFound; retrying once"
                    );
                    std::thread::sleep(delay);
                },
            )
        }

        /// Perform one independently bounded helper/RFCOMM open attempt.
        fn open_once(
            device_name: Option<&str>,
            helper_executable: &Path,
            attempt: u8,
            max_attempts: u8,
        ) -> Result<Self, TransportError> {
            let name = device_name.unwrap_or(DEFAULT_DEVICE_NAME);
            tracing::info!(
                device = %name,
                channel = SPP_CHANNEL,
                attempt,
                max_attempts,
                "spawning Bluetooth RFCOMM helper"
            );
            let mut process_slot = Some(HelperProcessSlot::reserve()?);

            // SAFETY: This no-argument/no-result function has no runtime side
            // effects. The reference forces the Objective-C object containing
            // the early helper constructor out of its static archive.
            unsafe { bt_helper_link_anchor() };

            let (helper_liveness, parent_liveness) = create_liveness_pipe()
                .map_err(|source| bluetooth_helper_error("creating the liveness pipe", source))?;
            let mut command = new_helper_command(helper_executable, name);
            prepare_liveness_fd(&mut command, helper_liveness.as_raw_fd());
            let mut child = command.spawn().map_err(|source| {
                bluetooth_helper_error(format!("launching {}", helper_executable.display()), source)
            })?;
            // The child has duplicated this endpoint onto its fixed inherited
            // descriptor. Keeping another parent-side read end would prevent
            // EOF from proving parent death.
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

            if let Err(source) = set_nonblocking(helper_stdin.as_raw_fd())
                .and_then(|()| set_nonblocking(helper_stdout.as_raw_fd()))
            {
                tracing::warn!(
                    device = %name,
                    attempt,
                    max_attempts,
                    error = %source,
                    "Bluetooth helper pipe setup failed"
                );
                drop(helper_stdin);
                drop(helper_stdout);
                terminate_child(child, process_slot.take(), Some(parent_liveness), false);
                return Err(helper_readiness_error(source));
            }
            if let Err(error) = await_helper_ready(&mut child, &mut helper_stdout) {
                tracing::warn!(
                    device = %name,
                    attempt,
                    max_attempts,
                    error = %error,
                    "Bluetooth helper failed to become ready"
                );
                drop(helper_stdin);
                drop(helper_stdout);
                terminate_child(child, process_slot.take(), Some(parent_liveness), false);
                return Err(error);
            }

            tracing::info!(
                device = %name,
                pid = child.id(),
                attempt,
                max_attempts,
                "Bluetooth RFCOMM helper ready"
            );
            Ok(Self {
                child: Some(child),
                helper_stdin: Some(helper_stdin),
                helper_stdout: Some(helper_stdout),
                parent_liveness: Some(parent_liveness),
                helper_healthy: true,
                process_slot,
                device_name: device_name.map(str::to_owned),
                helper_executable: helper_executable.to_owned(),
            })
        }

        fn terminate_helper(&mut self, graceful: bool) {
            self.helper_healthy = false;
            // Closing the pipes first tells a healthy helper to exit; SIGKILL
            // below is what bounds cleanup if it is stuck inside IOBluetooth.
            drop(self.helper_stdin.take());
            drop(self.helper_stdout.take());
            if let Some(child) = self.child.take() {
                terminate_child(
                    child,
                    self.process_slot.take(),
                    self.parent_liveness.take(),
                    graceful,
                );
            } else {
                drop(self.process_slot.take());
                drop(self.parent_liveness.take());
            }
        }
    }

    fn enumerate_paired_candidates(
        helper_executable: &Path,
    ) -> Result<Vec<PairedBluetoothCandidate>, TransportError> {
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
        let result = await_helper_ready_until(&mut child, &mut helper_stdout, deadline)
            .and_then(|()| {
                collect_paired_candidate_payload(&mut child, &mut helper_stdout, deadline)
            })
            .and_then(|payload| {
                parse_paired_candidate_payload(&payload).map_err(|source| {
                    bluetooth_helper_error("parsing paired Bluetooth candidates", source)
                })
            });
        drop(helper_stdout);

        match result {
            Ok(candidates) => {
                drop(parent_liveness);
                drop(process_slot.take());
                Ok(candidates)
            }
            Err(error) => {
                terminate_child(child, process_slot.take(), Some(parent_liveness), false);
                Err(error)
            }
        }
    }

    fn collect_paired_candidate_payload(
        child: &mut Child,
        stdout: &mut ChildStdout,
        deadline: Instant,
    ) -> Result<Vec<u8>, TransportError> {
        let mut payload = Vec::new();
        let mut buffer = [0_u8; 4096];
        loop {
            match stdout.read(&mut buffer) {
                Ok(0) => {
                    let status = await_helper_exit_after_stdout_eof(child)?;
                    if status.success() {
                        return Ok(payload);
                    }
                    let detail = match status.code() {
                        Some(HELPER_EXIT_TOO_MANY_PAIRED_DEVICES) => format!(
                            "paired-device enumeration exceeded the {MAX_PAIRED_CANDIDATES}-candidate safety bound"
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
                                    "paired-device helper exceeded its {}-second deadline",
                                    HELPER_ENUMERATION_TIMEOUT.as_secs()
                                ),
                            ),
                        ));
                    }
                    std::thread::sleep(PIPE_POLL_INTERVAL);
                }
                Err(source) => {
                    return Err(bluetooth_helper_error(
                        "reading paired Bluetooth candidates",
                        source,
                    ));
                }
            }
        }
    }

    fn parse_paired_candidate_payload(payload: &[u8]) -> io::Result<Vec<PairedBluetoothCandidate>> {
        let mut candidates: Vec<PairedBluetoothCandidate> = Vec::new();
        let mut offset = 0_usize;
        loop {
            let (hints, address_length, name_length, record_offset) =
                paired_candidate_record_lengths(payload, offset)?;
            offset = record_offset;
            if hints & !PAIRED_CANDIDATE_KNOWN_HINTS != 0 {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidData,
                    format!("paired-device record contains unknown hints: 0x{hints:02x}"),
                ));
            }
            if address_length == 0 && name_length == 0 {
                if hints != 0 {
                    return Err(io::Error::new(
                        io::ErrorKind::InvalidData,
                        "paired-device terminator contains candidate hints",
                    ));
                }
                if offset != payload.len() {
                    return Err(io::Error::new(
                        io::ErrorKind::InvalidData,
                        "paired-device payload contains bytes after its terminator",
                    ));
                }
                return Ok(candidates);
            }
            if address_length == 0 || name_length == 0 {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidData,
                    "paired-device record contains an empty field",
                ));
            }
            if candidates.len() >= MAX_PAIRED_CANDIDATES {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidData,
                    format!("paired-device payload exceeded {MAX_PAIRED_CANDIDATES} candidates"),
                ));
            }
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
            let address = std::str::from_utf8(address_bytes).map_err(|error| {
                io::Error::new(
                    io::ErrorKind::InvalidData,
                    format!("paired-device address is not UTF-8: {error}"),
                )
            })?;
            if !is_exact_bluetooth_address(address) {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidData,
                    format!("paired-device address is not exact: {address:?}"),
                ));
            }
            if candidates
                .iter()
                .any(|candidate| candidate.address.eq_ignore_ascii_case(address))
            {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidData,
                    format!("paired-device address is duplicated: {address}"),
                ));
            }
            let display_name = std::str::from_utf8(name_bytes).map_err(|error| {
                io::Error::new(
                    io::ErrorKind::InvalidData,
                    format!("paired-device name is not UTF-8: {error}"),
                )
            })?;
            candidates.push(PairedBluetoothCandidate {
                address: address.to_owned(),
                display_name: display_name.to_owned(),
                retry_transient_probe_not_found: hints != 0,
            });
            offset = record_end;
        }
    }

    fn paired_candidate_record_lengths(
        payload: &[u8],
        offset: usize,
    ) -> io::Result<(u8, usize, usize, usize)> {
        let header_end = offset.checked_add(5).ok_or_else(|| {
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
        let &[hints, address_high, address_low, name_high, name_low] = header else {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "paired-device record header has the wrong size",
            ));
        };
        Ok((
            hints,
            usize::from(u16::from_be_bytes([address_high, address_low])),
            usize::from(u16::from_be_bytes([name_high, name_low])),
            header_end,
        ))
    }

    fn is_exact_bluetooth_address(address: &str) -> bool {
        let bytes = address.as_bytes();
        if bytes.len() != 17 {
            return false;
        }
        let mut separator = None;
        for (index, byte) in bytes.iter().copied().enumerate() {
            if matches!(index, 2 | 5 | 8 | 11 | 14) {
                if !matches!(byte, b'-' | b':') {
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

    impl Transport for BluetoothTransport {
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
                process_slot,
                ..
            } = self;
            if child.is_none() || helper_stdin.is_none() {
                return Err(not_connected_write_error());
            }
            let mut cancellation =
                HelperWriteCancellation::new(child, process_slot, parent_liveness, helper_healthy);
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

        async fn close(&mut self) -> Result<(), TransportError> {
            tracing::info!(pid = ?self.child.as_ref().map(Child::id), "closing Bluetooth RFCOMM helper");
            self.terminate_helper(true);
            Ok(())
        }

        async fn reopen(&mut self) -> Result<(), TransportError> {
            let name = self.device_name.clone();
            let helper_executable = self.helper_executable.clone();
            tracing::info!(
                device = ?name,
                max_open_attempts = HELPER_OPEN_MAX_ATTEMPTS,
                "reopening Bluetooth RFCOMM helper"
            );
            self.close().await?;
            // Public `open` owns the one-retry policy. Calling it once here
            // gives reopen the same two-attempt ceiling without nesting loops.
            *self = Self::open_with_helper_executable(name.as_deref(), helper_executable)?;
            Ok(())
        }
    }

    impl Drop for BluetoothTransport {
        fn drop(&mut self) {
            self.terminate_helper(true);
        }
    }

    /// Run one open attempt and optionally retry exactly one `NotFound` result.
    ///
    /// The attempt callback owns all helper cleanup before it returns. Keeping
    /// the retry policy outside `open_once` ensures a reopen invokes the same
    /// two-attempt bound without recursively multiplying attempts.
    fn open_with_not_found_retry_policy<T>(
        retry_not_found: bool,
        mut open_attempt: impl FnMut(u8) -> Result<T, TransportError>,
        mut wait: impl FnMut(Duration),
    ) -> Result<T, TransportError> {
        match open_attempt(1) {
            Err(TransportError::NotFound) if retry_not_found => {
                wait(HELPER_OPEN_RETRY_DELAY);
                open_attempt(HELPER_OPEN_MAX_ATTEMPTS)
            }
            result => result,
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
    /// and makes the transport fail closed until `reopen` installs a new one.
    struct HelperWriteCancellation<'transport> {
        child: &'transport mut Option<Child>,
        process_slot: &'transport mut Option<HelperProcessSlot>,
        parent_liveness: &'transport mut Option<OwnedFd>,
        helper_healthy: &'transport mut bool,
        armed: bool,
    }

    impl<'transport> HelperWriteCancellation<'transport> {
        const fn new(
            child: &'transport mut Option<Child>,
            process_slot: &'transport mut Option<HelperProcessSlot>,
            parent_liveness: &'transport mut Option<OwnedFd>,
            helper_healthy: &'transport mut bool,
        ) -> Self {
            Self {
                child,
                process_slot,
                parent_liveness,
                helper_healthy,
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
                    terminate_child(
                        child,
                        self.process_slot.take(),
                        self.parent_liveness.take(),
                        false,
                    );
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

    fn helper_exit_error(status: ExitStatus) -> TransportError {
        match status.code() {
            Some(71) => TransportError::NotFound,
            Some(HELPER_EXIT_AMBIGUOUS_DEVICE_NAME) => TransportError::BluetoothDeviceNameAmbiguous,
            _ => helper_readiness_error(io::Error::new(
                io::ErrorKind::BrokenPipe,
                format!("Bluetooth helper exited with {status}"),
            )),
        }
    }

    fn await_helper_exit_after_stdout_eof(child: &mut Child) -> Result<ExitStatus, TransportError> {
        let deadline = Instant::now() + HELPER_EOF_EXIT_BUDGET;
        loop {
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

    fn await_helper_ready(
        child: &mut Child,
        stdout: &mut ChildStdout,
    ) -> Result<(), TransportError> {
        await_helper_ready_until(child, stdout, Instant::now() + HELPER_OPEN_TIMEOUT)
    }

    fn await_helper_ready_until(
        child: &mut Child,
        stdout: &mut ChildStdout,
        deadline: Instant,
    ) -> Result<(), TransportError> {
        let mut ready = [0_u8; HELPER_READY_MAGIC.len()];
        let mut offset = 0_usize;
        while offset < ready.len() {
            let remaining = ready.get_mut(offset..).ok_or_else(|| {
                helper_readiness_error(io::Error::new(
                    io::ErrorKind::InvalidData,
                    "invalid helper readiness offset",
                ))
            })?;
            match stdout.read(remaining) {
                Ok(0) => {
                    let status = await_helper_exit_after_stdout_eof(child)?;
                    return Err(helper_exit_error(status));
                }
                Ok(count) => {
                    offset = offset.checked_add(count).ok_or_else(|| {
                        helper_readiness_error(io::Error::new(
                            io::ErrorKind::InvalidData,
                            "Bluetooth helper readiness length overflow",
                        ))
                    })?;
                    if ready.get(..offset) != HELPER_READY_MAGIC.get(..offset) {
                        return Err(helper_readiness_error(io::Error::new(
                            io::ErrorKind::InvalidData,
                            "Bluetooth helper emitted an invalid readiness prefix",
                        )));
                    }
                }
                Err(error) if error.kind() == io::ErrorKind::WouldBlock => {
                    if let Some(status) = child.try_wait().map_err(helper_readiness_error)? {
                        return Err(helper_exit_error(status));
                    }
                    if Instant::now() >= deadline {
                        return Err(helper_readiness_error(io::Error::new(
                            io::ErrorKind::TimedOut,
                            "Bluetooth helper readiness timed out",
                        )));
                    }
                    std::thread::sleep(PIPE_POLL_INTERVAL);
                }
                Err(error) => return Err(helper_readiness_error(error)),
            }
        }

        if &ready == HELPER_READY_MAGIC {
            Ok(())
        } else {
            Err(helper_readiness_error(io::Error::new(
                io::ErrorKind::InvalidData,
                "Bluetooth helper emitted an invalid readiness prefix",
            )))
        }
    }

    fn terminate_child(
        mut child: Child,
        process_slot: Option<HelperProcessSlot>,
        mut parent_liveness: Option<OwnedFd>,
        graceful: bool,
    ) {
        let pid = child.id();
        match child.try_wait() {
            Ok(Some(status)) => {
                tracing::debug!(pid, %status, "Bluetooth helper already exited");
                return;
            }
            Ok(None) => {}
            Err(error) => {
                tracing::debug!(pid, error = %error, "Bluetooth helper initial reap failed");
            }
        }

        if graceful {
            let graceful_deadline = Instant::now() + GRACEFUL_EXIT_BUDGET;
            loop {
                match child.try_wait() {
                    Ok(Some(status)) => {
                        tracing::debug!(pid, %status, "Bluetooth helper exited gracefully");
                        return;
                    }
                    Ok(None) if Instant::now() < graceful_deadline => {
                        std::thread::sleep(Duration::from_millis(1));
                    }
                    Ok(None) => break,
                    Err(error) => {
                        tracing::debug!(pid, error = %error, "Bluetooth helper graceful reap failed");
                        break;
                    }
                }
            }
        }

        // Closing this endpoint makes the child's watchdog `_exit` even if
        // its main thread is wedged in IOBluetooth. SIGKILL below provides an
        // independent hard stop and covers helpers without a live watchdog.
        drop(parent_liveness.take());
        if let Err(error) = child.kill()
            && error.kind() != io::ErrorKind::InvalidInput
        {
            tracing::debug!(pid, error = %error, "Bluetooth helper kill returned an error");
        }

        let deadline = Instant::now() + SYNC_REAP_BUDGET;
        loop {
            match child.try_wait() {
                Ok(Some(status)) => {
                    tracing::debug!(pid, %status, "Bluetooth helper reaped");
                    return;
                }
                Ok(None) if Instant::now() < deadline => {
                    std::thread::sleep(Duration::from_millis(1));
                }
                Ok(None) => break,
                Err(error) => {
                    tracing::debug!(pid, error = %error, "Bluetooth helper synchronous reap failed");
                    break;
                }
            }
        }

        // Start the waiter before transferring `Child` into it. A failed
        // thread spawn therefore cannot silently drop the process handle or
        // release the one-helper slot while the process may still exist.
        let (reaper_tx, reaper_rx) = mpsc::sync_channel::<(Child, Option<HelperProcessSlot>)>(1);
        let reaper = std::thread::Builder::new()
            .name(format!("thd75-bt-reaper-{pid}"))
            .spawn(move || {
                if let Ok((mut child, process_slot)) = reaper_rx.recv() {
                    if let Err(error) = child.wait() {
                        tracing::debug!(pid, error = %error, "Bluetooth helper detached reap failed");
                    }
                    drop(process_slot);
                }
            });
        match reaper {
            Ok(_handle) => {
                if let Err(mpsc::SendError((mut child, process_slot))) =
                    reaper_tx.send((child, process_slot))
                {
                    tracing::warn!(pid, "Bluetooth helper reaper exited before accepting child");
                    if let Err(error) = child.wait() {
                        tracing::debug!(pid, error = %error, "Bluetooth helper fallback reap failed");
                    }
                    drop(process_slot);
                }
            }
            Err(error) => {
                tracing::warn!(pid, error = %error, "could not start Bluetooth helper reaper");
                if let Err(wait_error) = child.wait() {
                    tracing::debug!(pid, error = %wait_error, "Bluetooth helper fallback reap failed");
                }
                drop(process_slot);
            }
        }
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
        use std::io::{self, Read as _, Write as _};
        use std::os::fd::{AsRawFd as _, OwnedFd};
        use std::os::unix::process::ExitStatusExt as _;
        use std::path::Path;
        use std::process::{Child, ChildStdin, ChildStdout, Command, ExitStatus, Stdio};
        use std::time::{Duration, Instant};

        use super::{
            BluetoothTransport, GRACEFUL_EXIT_BUDGET, HELPER_CONTROL_ENV, HELPER_EOF_EXIT_BUDGET,
            HELPER_EXIT_AMBIGUOUS_DEVICE_NAME, HELPER_LIVENESS_FD, HELPER_LIVENESS_FD_ENV,
            HELPER_OPEN_MAX_ATTEMPTS, HELPER_OPEN_RETRY_DELAY, HELPER_READY_MAGIC,
            HELPER_SENTINEL_ENV, HELPER_SENTINEL_VALUE, HELPER_TEST_ENV, HelperProcessSlot,
            HelperWriteCancellation, PAIRED_CANDIDATE_HINT_CACHED_SPP,
            PAIRED_CANDIDATE_HINT_D75_NAME, PairedCandidateOpenPurpose, SYNC_REAP_BUDGET,
            TransportError, await_helper_ready, bt_helper_link_anchor, create_liveness_pipe,
            helper_exit_error, new_helper_command, open_with_not_found_retry_policy,
            parse_paired_candidate_payload, prepare_liveness_fd, terminate_child,
            validate_helper_executable,
        };

        type TestResult = Result<(), Box<dyn Error>>;

        #[test]
        fn native_iobluetooth_is_confined_to_process_helper() {
            let shim = include_str!("bluetooth_mac.m");
            let rust = include_str!("bluetooth.rs")
                .split_once("    #[cfg(test)]")
                .map_or(include_str!("bluetooth.rs"), |(production, _tests)| {
                    production
                });
            let detached_blocking_task = ["spawn", "_blocking"].concat();
            let in_process_write_ffi = ["bt_rfcomm", "_write"].concat();

            assert!(shim.contains("__attribute__((constructor))"));
            assert!(shim.contains("THD75_BT_HELPER_PROCESS_V1"));
            assert!(shim.contains("THD75_BT_HELPER_TEST_MODE"));
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
        fn native_device_selection_prefers_address_and_rejects_duplicate_names() {
            let shim = include_str!("bluetooth_mac.m");
            let address_match = shim.find("caseInsensitiveCompare:identifier]");
            let name_match = shim.find("name_match_count++");

            assert!(matches!(
                (address_match, name_match),
                (Some(address), Some(name)) if address < name
            ));
            assert!(shim.contains("if (!device && name_match_count > 1)"));
            assert!(shim.contains("BT_HELPER_EXIT_AMBIGUOUS_DEVICE_NAME 87"));
            assert!(shim.contains("return BT_HELPER_EXIT_AMBIGUOUS_DEVICE_NAME"));
        }

        #[test]
        fn tui_reconnect_has_no_main_thread_response_bridge() {
            let main = include_str!("../../../thd75-tui/src/main.rs");
            let radio_task = include_str!("../../../thd75-tui/src/radio_task.rs");

            assert!(!main.contains("CFRunLoopRunInMode"));
            assert!(!main.contains("bt_req_rx"));
            assert!(!radio_task.contains("recv_timeout"));
            assert!(!radio_task.contains("BT requires main thread"));
            assert!(radio_task.contains("tokio::task::spawn_blocking"));
        }

        #[test]
        fn lodestar_uses_the_same_process_isolation_boundary() {
            let swift =
                include_str!("../../../lodestar/Shared/Transport/IOBluetoothTransport.swift");
            let native = include_str!("../../../lodestar/Shared/Transport/IOBluetoothHelper.m");

            assert!(!swift.contains("closeConnection()"));
            assert!(!swift.contains("import IOBluetooth"));
            assert!(!swift.contains("IOBluetoothRFCOMMChannel"));
            assert!(swift.contains("lodestar_bt_helper_spawn"));
            assert!(swift.contains("lodestar_bt_helper_terminate"));
            assert!(swift.contains("BluetoothHelperPipeReader"));
            assert!(native.contains("#include \"../../../thd75/src/transport/bluetooth_mac.m\""));
            assert!(native.contains("F_DUPFD_CLOEXEC"));
            assert!(native.contains("child_liveness_text"));
            assert!(native.contains("posix_spawn("));
            assert!(native.contains("waitpid("));
            assert!(!native.contains("closeConnection"));
        }

        #[test]
        fn process_slot_rejects_concurrent_helpers_and_releases_on_drop() -> TestResult {
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
                BluetoothTransport::open_with_helper_executable(None, relative)
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
            let command = new_helper_command(&validated, "Custom TH-D75");

            assert_eq!(validated, helper);
            assert_eq!(command.get_program(), helper.as_os_str());
            assert!(
                command
                    .get_args()
                    .any(|argument| argument == "--thd75-bluetooth-helper")
            );
            Ok(())
        }

        #[test]
        fn paired_candidate_parser_accepts_exact_addresses_and_custom_names() -> TestResult {
            let payload = paired_candidate_payload(&[
                ("00-11-22-33-44-55", "TH-D75"),
                ("AA:BB:CC:DD:EE:FF", "Field Radio One"),
            ])?;

            let candidates = parse_paired_candidate_payload(&payload)?;

            assert_eq!(candidates.len(), 2);
            assert_eq!(
                candidates
                    .first()
                    .map(super::PairedBluetoothCandidate::address),
                Some("00-11-22-33-44-55")
            );
            assert_eq!(
                candidates
                    .get(1)
                    .map(super::PairedBluetoothCandidate::display_name),
                Some("Field Radio One")
            );
            Ok(())
        }

        #[test]
        fn paired_candidate_parser_preserves_only_known_probe_retry_hints() -> TestResult {
            let payload = paired_candidate_payload_with_hints(&[
                (
                    PAIRED_CANDIDATE_HINT_CACHED_SPP,
                    "00-11-22-33-44-55",
                    "Field Radio",
                ),
                (
                    PAIRED_CANDIDATE_HINT_D75_NAME,
                    "AA-BB-CC-DD-EE-FF",
                    "TH-D75",
                ),
                (0, "12-34-56-78-9A-BC", "Phone"),
            ])?;

            let candidates = parse_paired_candidate_payload(&payload)?;

            let retry_hints = candidates
                .iter()
                .map(|candidate| {
                    PairedCandidateOpenPurpose::Probe.retries_transient_not_found(candidate)
                })
                .collect::<Vec<_>>();
            assert_eq!(retry_hints, [true, true, false]);

            let unknown_hint_payload =
                paired_candidate_payload_with_hints(&[(0x80, "00-11-22-33-44-55", "Radio")])?;
            let Err(error) = parse_paired_candidate_payload(&unknown_hint_payload) else {
                return Err("unknown candidate hint was accepted".into());
            };
            assert_eq!(error.kind(), io::ErrorKind::InvalidData);
            assert!(error.to_string().contains("unknown hints"));
            Ok(())
        }

        #[test]
        fn paired_candidate_parser_rejects_duplicate_exact_addresses() -> TestResult {
            let payload = paired_candidate_payload(&[
                ("00-11-22-33-44-55", "First"),
                ("00-11-22-33-44-55", "Second"),
            ])?;

            let Err(error) = parse_paired_candidate_payload(&payload) else {
                return Err("duplicate Bluetooth address was accepted".into());
            };

            assert_eq!(error.kind(), io::ErrorKind::InvalidData);
            assert!(error.to_string().contains("duplicated"));
            Ok(())
        }

        #[test]
        fn paired_candidate_parser_rejects_name_like_or_truncated_selectors() -> TestResult {
            for address in ["TH-D75", "00-11-22-33-44", "00-11-22-33-44-GG"] {
                let payload = paired_candidate_payload(&[(address, "Radio")])?;
                let Err(error) = parse_paired_candidate_payload(&payload) else {
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
        fn paired_candidate_parser_requires_one_final_terminator() -> TestResult {
            let mut truncated = paired_candidate_payload(&[("00-11-22-33-44-55", "Radio")])?;
            truncated.truncate(truncated.len().saturating_sub(2));
            let Err(truncated_error) = parse_paired_candidate_payload(&truncated) else {
                return Err("truncated candidate payload was accepted".into());
            };
            assert_eq!(truncated_error.kind(), io::ErrorKind::UnexpectedEof);

            let mut trailing = paired_candidate_payload(&[("00-11-22-33-44-55", "Radio")])?;
            trailing.push(0x41);
            let Err(trailing_error) = parse_paired_candidate_payload(&trailing) else {
                return Err("bytes after candidate terminator were accepted".into());
            };
            assert_eq!(trailing_error.kind(), io::ErrorKind::InvalidData);
            Ok(())
        }

        #[test]
        fn helper_open_success_is_not_retried() -> TestResult {
            let mut attempts = Vec::new();
            let mut delays = Vec::new();
            let opened = open_with_not_found_retry_policy(
                true,
                |attempt| {
                    attempts.push(attempt);
                    Ok("ready")
                },
                |delay| delays.push(delay),
            )?;

            assert_eq!(opened, "ready");
            assert_eq!(attempts, [1]);
            assert!(delays.is_empty());
            Ok(())
        }

        #[test]
        fn helper_open_retries_not_found_once_after_exact_delay() -> TestResult {
            assert_eq!(HELPER_OPEN_MAX_ATTEMPTS, 2);
            let mut attempts = Vec::new();
            let mut delays = Vec::new();
            let opened = open_with_not_found_retry_policy(
                true,
                |attempt| {
                    attempts.push(attempt);
                    if attempt == 1 {
                        Err(TransportError::NotFound)
                    } else {
                        Ok("ready")
                    }
                },
                |delay| delays.push(delay),
            )?;

            assert_eq!(opened, "ready");
            assert_eq!(attempts, [1, HELPER_OPEN_MAX_ATTEMPTS]);
            assert_eq!(delays, [HELPER_OPEN_RETRY_DELAY]);
            Ok(())
        }

        #[test]
        fn selected_exact_address_recovers_from_one_transient_not_found() -> TestResult {
            let candidate = parse_paired_candidate_payload(&paired_candidate_payload(&[(
                "00-11-22-33-44-55",
                "Custom Name",
            )])?)?
            .into_iter()
            .next()
            .ok_or("candidate payload was unexpectedly empty")?;
            let retry_not_found =
                PairedCandidateOpenPurpose::Selected.retries_transient_not_found(&candidate);
            let mut attempts = Vec::new();
            let mut delays = Vec::new();

            let opened = open_with_not_found_retry_policy(
                retry_not_found,
                |attempt| {
                    attempts.push(attempt);
                    if attempt == 1 {
                        Err(TransportError::NotFound)
                    } else {
                        Ok(candidate.address())
                    }
                },
                |delay| delays.push(delay),
            )?;

            assert_eq!(opened, "00-11-22-33-44-55");
            assert_eq!(attempts, [1, HELPER_OPEN_MAX_ATTEMPTS]);
            assert_eq!(delays, [HELPER_OPEN_RETRY_DELAY]);
            Ok(())
        }

        #[test]
        fn helper_probe_policy_does_not_retry_unhinted_not_found() {
            let mut attempts = Vec::new();
            let mut delays = Vec::new();
            let result = open_with_not_found_retry_policy::<()>(
                false,
                |attempt| {
                    attempts.push(attempt);
                    Err(TransportError::NotFound)
                },
                |delay| delays.push(delay),
            );

            assert!(matches!(result, Err(TransportError::NotFound)));
            assert_eq!(attempts, [1]);
            assert!(delays.is_empty());
        }

        #[test]
        fn helper_open_does_not_retry_non_71_helper_exit() {
            let mut attempts = Vec::new();
            let mut delays = Vec::new();
            let result = open_with_not_found_retry_policy::<()>(
                true,
                |attempt| {
                    attempts.push(attempt);
                    Err(helper_exit_error(ExitStatus::from_raw(72 << 8)))
                },
                |delay| delays.push(delay),
            );

            assert!(matches!(
                result,
                Err(TransportError::BluetoothHelper { .. })
            ));
            assert_eq!(attempts, [1]);
            assert!(delays.is_empty());
        }

        #[test]
        fn helper_open_returns_second_not_found_without_a_third_attempt() {
            let mut attempts = Vec::new();
            let mut delays = Vec::new();
            let result = open_with_not_found_retry_policy::<()>(
                true,
                |attempt| {
                    attempts.push(attempt);
                    Err(TransportError::NotFound)
                },
                |delay| delays.push(delay),
            );

            assert!(matches!(result, Err(TransportError::NotFound)));
            assert_eq!(attempts, [1, HELPER_OPEN_MAX_ATTEMPTS]);
            assert_eq!(delays, [HELPER_OPEN_RETRY_DELAY]);
        }

        #[test]
        fn only_helper_exit_code_71_is_retryable_not_found() {
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
        fn ambiguous_device_name_exit_is_actionable_and_not_retried() {
            let raw_status = HELPER_EXIT_AMBIGUOUS_DEVICE_NAME << 8;
            let error = helper_exit_error(ExitStatus::from_raw(raw_status));
            let message = error.to_string();
            assert!(matches!(
                error,
                TransportError::BluetoothDeviceNameAmbiguous
            ));
            assert!(message.contains("exact Bluetooth address"));

            let mut attempts = Vec::new();
            let mut delays = Vec::new();
            let result = open_with_not_found_retry_policy::<()>(
                true,
                |attempt| {
                    attempts.push(attempt);
                    Err(helper_exit_error(ExitStatus::from_raw(raw_status)))
                },
                |delay| delays.push(delay),
            );
            assert!(matches!(
                result,
                Err(TransportError::BluetoothDeviceNameAmbiguous)
            ));
            assert_eq!(attempts, [1]);
            assert!(delays.is_empty());
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
            {
                let _guard = HelperWriteCancellation::new(
                    &mut child,
                    &mut process_slot,
                    &mut parent_liveness,
                    &mut helper_healthy,
                );
            }
            assert!(!helper_healthy);
            assert!(child.is_none());

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
            {
                let mut guard = HelperWriteCancellation::new(
                    &mut child,
                    &mut process_slot,
                    &mut parent_liveness,
                    &mut helper_healthy,
                );
                guard.disarm();
            }
            assert!(helper_healthy);
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
                .arg("--thd75-bluetooth-helper-test")
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

        fn paired_candidate_payload(records: &[(&str, &str)]) -> Result<Vec<u8>, Box<dyn Error>> {
            let mut payload = Vec::new();
            for (address, name) in records {
                let address_length = u16::try_from(address.len())?;
                let name_length = u16::try_from(name.len())?;
                payload.push(0);
                payload.extend_from_slice(&address_length.to_be_bytes());
                payload.extend_from_slice(&name_length.to_be_bytes());
                payload.extend_from_slice(address.as_bytes());
                payload.extend_from_slice(name.as_bytes());
            }
            payload.extend_from_slice(&[0, 0, 0, 0, 0]);
            Ok(payload)
        }

        fn paired_candidate_payload_with_hints(
            records: &[(u8, &str, &str)],
        ) -> Result<Vec<u8>, Box<dyn Error>> {
            let mut payload = Vec::new();
            for (hints, address, name) in records {
                let address_length = u16::try_from(address.len())?;
                let name_length = u16::try_from(name.len())?;
                payload.push(*hints);
                payload.extend_from_slice(&address_length.to_be_bytes());
                payload.extend_from_slice(&name_length.to_be_bytes());
                payload.extend_from_slice(address.as_bytes());
                payload.extend_from_slice(name.as_bytes());
            }
            payload.extend_from_slice(&[0, 0, 0, 0, 0]);
            Ok(payload)
        }
    }
}

#[cfg(any(target_os = "macos", all(doc, unix)))]
pub use inner::{BluetoothTransport, PairedBluetoothCandidate};
