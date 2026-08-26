//! Swift-facing macOS Bluetooth Classic SPP byte transport.

use std::sync::{Arc, Mutex as StandardMutex};

use kenwood_thd75::types::SerialNumber;

#[cfg(target_os = "macos")]
use kenwood_thd75::{
    Radio,
    error::TransportError,
    radio::raw_protocol_session::RawProtocolSession,
    transport::{BluetoothTransport, Transport},
};
#[cfg(target_os = "macos")]
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
#[cfg(target_os = "macos")]
use tokio::sync::{Mutex, Notify};

use crate::terminal_mode::canonicalize_bluetooth_address;
#[cfg(test)]
use crate::terminal_mode::is_exact_bluetooth_address;
#[cfg(target_os = "macos")]
use crate::terminal_mode::{
    RecoveryCancellation, bundled_bluetooth_helper_executable, enumerate_paired_bluetooth_devices,
    open_selected_bluetooth_transport, transport_error_detail,
};

/// Largest byte vector accepted by one Swift-facing link operation.
///
/// CAT, MCP, KISS, and MMDVM frames used by Azimuth fit comfortably inside
/// this bound. Limiting each foreign call also prevents an accidental UI-side
/// allocation from becoming an unbounded helper-pipe operation.
const MAXIMUM_BLUETOOTH_TRANSFER_BYTES: u32 = 4_096;

/// A paired macOS Bluetooth Classic device.
///
/// `display_name` is presentation only. `address` is the exact stable selector
/// that must be passed back when constructing an address-bound link.
#[derive(Debug, Clone, PartialEq, Eq, uniffi::Record)]
pub struct BluetoothPairedDevice {
    /// Exact Bluetooth address returned by `IOBluetooth`.
    pub address: String,
    /// Human-readable paired-device name.
    pub display_name: String,
}

/// One bounded paired-device discovery snapshot for the connection picker.
///
/// Every record is an unqualified paired device. Its exact address is stable
/// selection identity; its display name is presentation only. Opening a record
/// still requires strict CAT identity qualification before Azimuth treats it
/// as a TH-D75.
#[derive(Debug, Clone, PartialEq, Eq, uniffi::Record)]
pub struct BluetoothDeviceDiscovery {
    /// Every paired device from the bounded native snapshot.
    pub devices: Vec<BluetoothPairedDevice>,
}

/// How a normal Azimuth Bluetooth link chooses one paired radio.
#[derive(Debug, Clone, PartialEq, Eq, uniffi::Enum)]
pub enum BluetoothLinkTarget {
    /// Open one paired SPP device by its exact Bluetooth address.
    ExactAddress {
        /// Exact 17-character address returned by
        /// [`discover_paired_bluetooth_devices`].
        address: String,
    },
    /// Enumerate paired devices and select only the radio whose CAT serial
    /// exactly matches the stable serial previously learned from USB.
    ExpectedUsbSerial {
        /// Exact validated eight-character TH-D75 serial number.
        serial_number: String,
    },
    /// Open one already resolved address and re-prove the expected CAT serial.
    ///
    /// This avoids another paired-device scan without weakening the physical
    /// radio identity gate. It is the durable form of a successful
    /// [`Self::ExpectedUsbSerial`] selection.
    ExactAddressExpectedUsbSerial {
        /// Exact 17-character paired Bluetooth address.
        address: String,
        /// Exact validated eight-character TH-D75 serial number.
        serial_number: String,
    },
}

/// Failure from paired-device discovery or a normal Bluetooth byte link.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error, uniffi::Error)]
pub enum BluetoothLinkError {
    /// Native `IOBluetooth` SPP is available only in the macOS build.
    #[error("native TH-D75 Bluetooth control is available only on macOS")]
    UnsupportedPlatform,
    /// An address selector was not an exact Bluetooth address.
    #[error("Bluetooth address must contain six two-digit hexadecimal octets: {address}")]
    InvalidAddress {
        /// Rejected address.
        address: String,
    },
    /// The expected USB serial did not satisfy the radio's exact serial type.
    #[error("expected USB radio serial is invalid: {detail}")]
    InvalidExpectedUsbSerial {
        /// Validation detail.
        detail: String,
    },
    /// The embedded signed helper or paired SPP endpoint was unavailable.
    #[error("Bluetooth {operation} failed: {detail}")]
    BluetoothUnavailable {
        /// Bounded operation which failed.
        operation: String,
        /// Native/helper error chain.
        detail: String,
    },
    /// The opened SPP endpoint did not provide a valid bounded CAT identity.
    #[error("could not prove the Bluetooth radio CAT serial: {detail}")]
    CatIdentityUnavailable {
        /// Protocol, timeout, or transport detail.
        detail: String,
    },
    /// Serial-based selection reopened a different radio than the one requested.
    #[error("Bluetooth radio serial {actual} does not match expected USB radio serial {expected}")]
    SerialMismatch {
        /// USB serial requested by the caller.
        expected: String,
        /// CAT serial returned after the selected SPP endpoint opened.
        actual: String,
    },
    /// A byte operation was requested without an open SPP link.
    #[error("the Bluetooth radio link is not open")]
    NotOpen,
    /// A close, reopen, write, or explicit cancellation interrupted a pending
    /// blocking RFCOMM read before it returned any bytes.
    #[error("the pending Bluetooth read was interrupted")]
    ReadInterrupted,
    /// Explicit cancellation interrupted a pending Bluetooth helper open.
    #[error("the pending Bluetooth open was interrupted")]
    OpenInterrupted,
    /// A read or write exceeded the bounded foreign-call transfer size.
    #[error("Bluetooth transfer length {requested} is outside 1..={maximum} bytes")]
    InvalidTransferLength {
        /// Requested byte count.
        requested: u32,
        /// Largest accepted byte count.
        maximum: u32,
    },
    /// Internal link state could not be read safely.
    #[error("Bluetooth link state is unavailable: {detail}")]
    StateUnavailable {
        /// Lock failure detail.
        detail: String,
    },
}

#[cfg(target_os = "macos")]
#[derive(Debug, Clone)]
enum ValidatedBluetoothLinkTarget {
    ExactAddress(String),
    ExpectedUsbSerial(SerialNumber),
    ExactAddressExpectedUsbSerial {
        address: String,
        serial_number: SerialNumber,
    },
}

#[derive(Debug, Clone, Default)]
struct MatchedBluetoothIdentity {
    address: Option<String>,
    serial_number: Option<String>,
}

#[cfg(target_os = "macos")]
#[derive(Debug)]
enum OpenBluetoothTransport {
    Raw(BluetoothTransport),
    SerialQualified(RawProtocolSession<BluetoothTransport>),
}

#[cfg(target_os = "macos")]
impl Transport for OpenBluetoothTransport {
    async fn write(&mut self, data: &[u8]) -> Result<(), TransportError> {
        match self {
            Self::Raw(transport) => transport.write(data).await,
            Self::SerialQualified(transport) => transport.write(data).await,
        }
    }

    async fn read(&mut self, buffer: &mut [u8]) -> Result<usize, TransportError> {
        match self {
            Self::Raw(transport) => transport.read(buffer).await,
            Self::SerialQualified(transport) => transport.read(buffer).await,
        }
    }

    async fn close(&mut self) -> Result<(), TransportError> {
        match self {
            Self::Raw(transport) => transport.close().await,
            Self::SerialQualified(transport) => transport.close().await,
        }
    }

    async fn reopen(&mut self) -> Result<(), TransportError> {
        match self {
            Self::Raw(transport) => transport.reopen().await,
            Self::SerialQualified(transport) => transport.reopen().await,
        }
    }
}

/// One normal, selectable macOS Bluetooth Classic SPP byte link.
///
/// The object is permanently bound either to an exact paired-device address or
/// to an exact USB radio serial. An exact-address target opens raw SPP so
/// Azimuth's mode preflight can identify and recover CAT, KISS, or MMDVM state.
/// A serial-qualified target first exits transient packet modes, then performs
/// a fresh bounded CAT `AE` proof and exposes that matched serial through
/// [`Self::matched_cat_serial`]. The recovered stream remains owned as a raw
/// protocol session so the caller's normal mode preflight sees a clean link.
/// The underlying TH-D75 transport retains its process-wide one-helper lease,
/// so a second live Bluetooth owner fails instead of competing for the radio's
/// single SPP channel.
#[derive(Debug, uniffi::Object)]
pub struct BluetoothByteTransport {
    #[cfg(target_os = "macos")]
    target: ValidatedBluetoothLinkTarget,
    #[cfg(target_os = "macos")]
    transport: Mutex<Option<OpenBluetoothTransport>>,
    #[cfg(target_os = "macos")]
    read_interrupt_generation: AtomicU64,
    #[cfg(target_os = "macos")]
    pending_read_interrupt: AtomicBool,
    #[cfg(target_os = "macos")]
    read_interrupt_notification: Notify,
    #[cfg(target_os = "macos")]
    pending_open_interrupt: AtomicBool,
    #[cfg(target_os = "macos")]
    active_open_cancellation: StandardMutex<Option<Arc<RecoveryCancellation>>>,
    #[cfg(target_os = "macos")]
    cached_expected_serial_address: StandardMutex<Option<String>>,
    matched_identity: StandardMutex<MatchedBluetoothIdentity>,
}

#[uniffi::export(async_runtime = "tokio")]
impl BluetoothByteTransport {
    /// Construct a closed link for an exact address or USB-serial target.
    ///
    /// Construction performs syntax validation only and never launches the
    /// helper or opens a radio. Call [`Self::open`] for the bounded native work.
    ///
    /// # Errors
    ///
    /// Returns [`BluetoothLinkError::InvalidAddress`] or
    /// [`BluetoothLinkError::InvalidExpectedUsbSerial`] for an invalid target.
    #[uniffi::constructor]
    pub fn new(target: BluetoothLinkTarget) -> Result<Arc<Self>, BluetoothLinkError> {
        #[cfg(target_os = "macos")]
        let target = match target {
            BluetoothLinkTarget::ExactAddress { address } => {
                let canonical = canonicalize_bluetooth_address(&address)
                    .ok_or(BluetoothLinkError::InvalidAddress { address })?;
                ValidatedBluetoothLinkTarget::ExactAddress(canonical)
            }
            BluetoothLinkTarget::ExpectedUsbSerial { serial_number } => {
                let serial = SerialNumber::new(&serial_number).map_err(|error| {
                    BluetoothLinkError::InvalidExpectedUsbSerial {
                        detail: error.to_string(),
                    }
                })?;
                ValidatedBluetoothLinkTarget::ExpectedUsbSerial(serial)
            }
            BluetoothLinkTarget::ExactAddressExpectedUsbSerial {
                address,
                serial_number,
            } => {
                let canonical = canonicalize_bluetooth_address(&address)
                    .ok_or(BluetoothLinkError::InvalidAddress { address })?;
                let serial = SerialNumber::new(&serial_number).map_err(|error| {
                    BluetoothLinkError::InvalidExpectedUsbSerial {
                        detail: error.to_string(),
                    }
                })?;
                ValidatedBluetoothLinkTarget::ExactAddressExpectedUsbSerial {
                    address: canonical,
                    serial_number: serial,
                }
            }
        };

        #[cfg(not(target_os = "macos"))]
        validate_target_without_opening(target)?;

        Ok(Arc::new(Self {
            #[cfg(target_os = "macos")]
            target,
            #[cfg(target_os = "macos")]
            transport: Mutex::new(None),
            #[cfg(target_os = "macos")]
            read_interrupt_generation: AtomicU64::new(0),
            #[cfg(target_os = "macos")]
            pending_read_interrupt: AtomicBool::new(false),
            #[cfg(target_os = "macos")]
            read_interrupt_notification: Notify::new(),
            #[cfg(target_os = "macos")]
            pending_open_interrupt: AtomicBool::new(false),
            #[cfg(target_os = "macos")]
            active_open_cancellation: StandardMutex::new(None),
            #[cfg(target_os = "macos")]
            cached_expected_serial_address: StandardMutex::new(None),
            matched_identity: StandardMutex::new(MatchedBluetoothIdentity::default()),
        }))
    }

    /// Open the bound SPP endpoint.
    ///
    /// Address targets open that address as an unqualified byte stream so the
    /// caller can run Azimuth's packet-mode preflight. USB-serial targets use
    /// the signed helper's bounded paired-device snapshot and identity probes
    /// to auto-match the same physical radio. Both serial-qualified target
    /// forms recover transient packet mode and query the final opened endpoint
    /// again before exposing it to byte operations.
    ///
    /// # Errors
    ///
    /// Returns a typed platform, helper, selection, identity, or mismatch error.
    #[cfg_attr(
        not(target_os = "macos"),
        expect(
            clippy::unused_async,
            reason = "UniFFI keeps byte-link opening asynchronous on every target, while unsupported platforms return immediately"
        )
    )]
    pub async fn open(&self) -> Result<(), BluetoothLinkError> {
        #[cfg(target_os = "macos")]
        {
            let mut slot = self.transport.lock().await;
            if slot.is_some() {
                return Ok(());
            }
            let active_open = self.begin_open_cancellation()?;
            self.replace_matched_identity(None, None)?;
            let cached_address = self.cached_expected_serial_address()?;
            let opened = open_native_transport(
                &self.target,
                cached_address.as_deref(),
                active_open.cancellation(),
            )
            .await?;
            let OpenedNativeBluetoothTransport {
                transport,
                exact_address,
            } = opened;
            let (opened_transport, matched_serial) =
                if let Some(expected) = self.target.expected_serial() {
                    let qualified = recover_and_qualify_serial_cancellable(
                        transport,
                        expected,
                        active_open.cancellation(),
                    )
                    .await?;
                    (
                        OpenBluetoothTransport::SerialQualified(qualified.transport),
                        Some(qualified.serial_number),
                    )
                } else {
                    (OpenBluetoothTransport::Raw(transport), None)
                };

            if active_open.cancellation().check().is_err() {
                return Err(BluetoothLinkError::OpenInterrupted);
            }
            if self.target.caches_resolved_address() {
                self.replace_cached_expected_serial_address(exact_address.clone())?;
            }
            self.replace_matched_identity(exact_address, matched_serial)?;
            *slot = Some(opened_transport);
            drop(slot);
            drop(active_open);
            Ok(())
        }

        #[cfg(not(target_os = "macos"))]
        {
            Err(BluetoothLinkError::UnsupportedPlatform)
        }
    }

    /// Write one bounded byte vector to the open RFCOMM stream.
    ///
    /// # Errors
    ///
    /// Returns [`BluetoothLinkError::NotOpen`], a transfer-bound error, a typed
    /// unsupported-platform error, or the complete native transport error.
    #[cfg_attr(
        not(target_os = "macos"),
        expect(
            clippy::unused_async,
            reason = "UniFFI keeps byte-link writes asynchronous on every target, while unsupported platforms return immediately"
        )
    )]
    pub async fn write(&self, bytes: Vec<u8>) -> Result<(), BluetoothLinkError> {
        validate_transfer_length(bytes.len())?;

        #[cfg(target_os = "macos")]
        {
            self.interrupt_pending_read();
            let mut slot = self.transport.lock().await;
            let transport = slot.as_mut().ok_or(BluetoothLinkError::NotOpen)?;
            let result = transport.write(&bytes).await.map_err(|error| {
                BluetoothLinkError::BluetoothUnavailable {
                    operation: "write".to_owned(),
                    detail: transport_error_detail(&error),
                }
            });
            drop(slot);
            result
        }

        #[cfg(not(target_os = "macos"))]
        {
            drop(bytes);
            Err(BluetoothLinkError::UnsupportedPlatform)
        }
    }

    /// Wait for and return no more than `max_length` RFCOMM bytes.
    ///
    /// An empty vector is never used as an idle tick. Native EOF is returned as
    /// an error, matching Azimuth's existing blocking byte-transport contract.
    ///
    /// # Errors
    ///
    /// Returns [`BluetoothLinkError::NotOpen`], a transfer-bound error, a typed
    /// unsupported-platform error, or the complete native transport error.
    #[cfg_attr(
        not(target_os = "macos"),
        expect(
            clippy::unused_async,
            reason = "UniFFI keeps byte-link reads asynchronous on every target, while unsupported platforms return immediately"
        )
    )]
    pub async fn read(&self, max_length: u32) -> Result<Vec<u8>, BluetoothLinkError> {
        validate_transfer_length_u32(max_length)?;

        #[cfg(target_os = "macos")]
        {
            if self.consume_pending_read_interrupt() {
                return Err(BluetoothLinkError::ReadInterrupted);
            }
            let generation = self.read_interrupt_generation.load(Ordering::Acquire);
            if self.consume_pending_read_interrupt() {
                return Err(BluetoothLinkError::ReadInterrupted);
            }
            let mut slot = self.transport.lock().await;
            let transport = slot.as_mut().ok_or(BluetoothLinkError::NotOpen)?;
            let length = usize::try_from(max_length).map_err(|error| {
                BluetoothLinkError::StateUnavailable {
                    detail: error.to_string(),
                }
            })?;
            let mut bytes = vec![0_u8; length];
            let count = tokio::select! {
                biased;
                () = self.wait_for_read_interrupt(generation) => {
                    let _consumed = self.consume_pending_read_interrupt();
                    Err(BluetoothLinkError::ReadInterrupted)
                }
                result = transport.read(&mut bytes) => {
                    result.map_err(|error| BluetoothLinkError::BluetoothUnavailable {
                        operation: "read".to_owned(),
                        detail: transport_error_detail(&error),
                    })
                }
            }?;
            bytes.truncate(count);
            drop(slot);
            Ok(bytes)
        }

        #[cfg(not(target_os = "macos"))]
        {
            Err(BluetoothLinkError::UnsupportedPlatform)
        }
    }

    /// Close the current helper and clear the matched CAT identity.
    ///
    /// Closing an already closed object is a successful no-op.
    ///
    /// # Errors
    ///
    /// Returns the native close error on macOS. Other platforms have no native
    /// resource to close and also return success.
    #[cfg_attr(
        not(target_os = "macos"),
        expect(
            clippy::unused_async,
            reason = "UniFFI keeps byte-link closure asynchronous on every target, while unsupported platforms only clear local state"
        )
    )]
    pub async fn close(&self) -> Result<(), BluetoothLinkError> {
        #[cfg(target_os = "macos")]
        {
            self.interrupt_pending_read();
            let mut slot = self.transport.lock().await;
            let result = if let Some(transport) = slot.as_mut() {
                transport
                    .close()
                    .await
                    .map_err(|error| BluetoothLinkError::BluetoothUnavailable {
                        operation: "close".to_owned(),
                        detail: transport_error_detail(&error),
                    })
            } else {
                Ok(())
            };
            *slot = None;
            drop(slot);
            self.pending_open_interrupt.store(false, Ordering::Release);
            self.replace_matched_identity(None, None)?;
            result
        }

        #[cfg(not(target_os = "macos"))]
        {
            self.replace_matched_identity(None, None)
        }
    }

    /// Reopen the same target using its original qualification policy.
    ///
    /// This operation is valid both after [`Self::close`] and while the link is
    /// open. The prior helper is fully closed before a new one is requested.
    ///
    /// # Errors
    ///
    /// Returns the same typed failures as [`Self::open`], plus any native close
    /// failure from the previous helper.
    pub async fn reopen(&self) -> Result<(), BluetoothLinkError> {
        self.close().await?;
        self.open().await
    }

    /// Apply a serial baud request.
    ///
    /// Bluetooth Classic RFCOMM has no host line-coding baud control. The
    /// TH-D75's SPP link uses its fixed radio-side rate, so this method is an
    /// intentional no-op for every baud value and does not require an open link.
    pub fn set_baud_rate(&self, baud: u32) {
        accept_fixed_rfcomm_baud(baud);
    }

    /// Interrupt one pending blocking read without closing the RFCOMM link.
    ///
    /// Swift must call this from its task-cancellation handler because dropping
    /// a Swift `async` wrapper does not implicitly cancel the underlying `UniFFI`
    /// Rust future. The interrupted [`Self::read`] returns
    /// [`BluetoothLinkError::ReadInterrupted`] without consuming bytes. Calling
    /// this method while no read is active leaves one sticky interrupt for the
    /// next read, closing the cancellation-before-future-registration race.
    pub fn cancel_pending_read(&self) {
        #[cfg(target_os = "macos")]
        self.interrupt_pending_read();
    }

    /// Interrupt one pending helper open without waiting for its full bound.
    ///
    /// The request is sticky across future registration. If no open is active,
    /// the next [`Self::open`] returns [`BluetoothLinkError::OpenInterrupted`]
    /// before launching a helper.
    pub fn cancel_pending_open(&self) {
        #[cfg(target_os = "macos")]
        self.interrupt_pending_open();
    }

    /// Return the exact CAT serial proved during the latest successful open.
    ///
    /// Returns `None` for a raw exact-address link, before open, and after close.
    ///
    /// # Errors
    ///
    /// Returns [`BluetoothLinkError::StateUnavailable`] if a prior panic
    /// poisoned the small identity lock.
    pub fn matched_cat_serial(&self) -> Result<Option<String>, BluetoothLinkError> {
        self.matched_identity
            .lock()
            .map(|identity| identity.serial_number.clone())
            .map_err(|error| BluetoothLinkError::StateUnavailable {
                detail: error.to_string(),
            })
    }

    /// Return the canonical exact address reached by the latest open.
    ///
    /// Returns `None` before open, after close, or after a failed open. A
    /// serial-qualified result is published only after its final CAT proof.
    ///
    /// # Errors
    ///
    /// Returns [`BluetoothLinkError::StateUnavailable`] if a prior panic
    /// poisoned the small identity lock.
    pub fn matched_address(&self) -> Result<Option<String>, BluetoothLinkError> {
        self.matched_identity
            .lock()
            .map(|identity| identity.address.clone())
            .map_err(|error| BluetoothLinkError::StateUnavailable {
                detail: error.to_string(),
            })
    }
}

impl BluetoothByteTransport {
    #[cfg(target_os = "macos")]
    fn interrupt_pending_open(&self) {
        self.pending_open_interrupt.store(true, Ordering::Release);
        let active = match self.active_open_cancellation.lock() {
            Ok(slot) => slot.clone(),
            Err(poisoned) => poisoned.into_inner().clone(),
        };
        if let Some(cancellation) = active {
            cancellation.request();
            self.pending_open_interrupt.store(false, Ordering::Release);
        }
    }

    #[cfg(target_os = "macos")]
    fn begin_open_cancellation(&self) -> Result<ActiveOpenCancellation<'_>, BluetoothLinkError> {
        if self.pending_open_interrupt.swap(false, Ordering::AcqRel) {
            return Err(BluetoothLinkError::OpenInterrupted);
        }
        let cancellation = Arc::new(RecoveryCancellation::default());
        match self.active_open_cancellation.lock() {
            Ok(mut slot) => *slot = Some(Arc::clone(&cancellation)),
            Err(poisoned) => *poisoned.into_inner() = Some(Arc::clone(&cancellation)),
        }
        if self.pending_open_interrupt.swap(false, Ordering::AcqRel) {
            cancellation.request();
            self.clear_active_open_cancellation(&cancellation);
            return Err(BluetoothLinkError::OpenInterrupted);
        }
        Ok(ActiveOpenCancellation {
            owner: self,
            cancellation,
        })
    }

    #[cfg(target_os = "macos")]
    fn clear_active_open_cancellation(&self, cancellation: &Arc<RecoveryCancellation>) {
        let mut slot = match self.active_open_cancellation.lock() {
            Ok(slot) => slot,
            Err(poisoned) => poisoned.into_inner(),
        };
        if slot
            .as_ref()
            .is_some_and(|active| Arc::ptr_eq(active, cancellation))
        {
            *slot = None;
        }
    }

    #[cfg(target_os = "macos")]
    fn interrupt_pending_read(&self) {
        self.pending_read_interrupt.store(true, Ordering::Release);
        let _previous_generation = self
            .read_interrupt_generation
            .fetch_add(1, Ordering::AcqRel);
        self.read_interrupt_notification.notify_one();
    }

    #[cfg(target_os = "macos")]
    fn consume_pending_read_interrupt(&self) -> bool {
        self.pending_read_interrupt.swap(false, Ordering::AcqRel)
    }

    #[cfg(target_os = "macos")]
    async fn wait_for_read_interrupt(&self, generation: u64) {
        loop {
            if self.read_interrupt_generation.load(Ordering::Acquire) != generation {
                return;
            }
            self.read_interrupt_notification.notified().await;
        }
    }

    fn replace_matched_identity(
        &self,
        address: Option<String>,
        serial_number: Option<String>,
    ) -> Result<(), BluetoothLinkError> {
        let mut matched =
            self.matched_identity
                .lock()
                .map_err(|error| BluetoothLinkError::StateUnavailable {
                    detail: error.to_string(),
                })?;
        *matched = MatchedBluetoothIdentity {
            address,
            serial_number,
        };
        drop(matched);
        Ok(())
    }

    #[cfg(target_os = "macos")]
    fn cached_expected_serial_address(&self) -> Result<Option<String>, BluetoothLinkError> {
        self.cached_expected_serial_address
            .lock()
            .map(|address| address.clone())
            .map_err(|error| BluetoothLinkError::StateUnavailable {
                detail: error.to_string(),
            })
    }

    #[cfg(target_os = "macos")]
    fn replace_cached_expected_serial_address(
        &self,
        address: Option<String>,
    ) -> Result<(), BluetoothLinkError> {
        let mut cached = self
            .cached_expected_serial_address
            .lock()
            .map_err(|error| BluetoothLinkError::StateUnavailable {
                detail: error.to_string(),
            })?;
        *cached = address;
        drop(cached);
        Ok(())
    }
}

#[cfg(target_os = "macos")]
#[derive(Debug)]
struct ActiveOpenCancellation<'transport> {
    owner: &'transport BluetoothByteTransport,
    cancellation: Arc<RecoveryCancellation>,
}

#[cfg(target_os = "macos")]
impl ActiveOpenCancellation<'_> {
    fn cancellation(&self) -> &RecoveryCancellation {
        &self.cancellation
    }
}

#[cfg(target_os = "macos")]
impl Drop for ActiveOpenCancellation<'_> {
    fn drop(&mut self) {
        self.owner
            .clear_active_open_cancellation(&self.cancellation);
    }
}

#[cfg(target_os = "macos")]
impl ValidatedBluetoothLinkTarget {
    fn expected_serial(&self) -> Option<&SerialNumber> {
        match self {
            Self::ExactAddress(_address) => None,
            Self::ExpectedUsbSerial(serial_number)
            | Self::ExactAddressExpectedUsbSerial { serial_number, .. } => Some(serial_number),
        }
    }

    const fn caches_resolved_address(&self) -> bool {
        matches!(self, Self::ExpectedUsbSerial(_serial_number))
    }
}

#[cfg(not(target_os = "macos"))]
fn validate_target_without_opening(target: BluetoothLinkTarget) -> Result<(), BluetoothLinkError> {
    match target {
        BluetoothLinkTarget::ExactAddress { address } => {
            if canonicalize_bluetooth_address(&address).is_some() {
                Ok(())
            } else {
                Err(BluetoothLinkError::InvalidAddress { address })
            }
        }
        BluetoothLinkTarget::ExpectedUsbSerial { serial_number } => {
            SerialNumber::new(&serial_number)
                .map(|_serial| ())
                .map_err(|error| BluetoothLinkError::InvalidExpectedUsbSerial {
                    detail: error.to_string(),
                })
        }
        BluetoothLinkTarget::ExactAddressExpectedUsbSerial {
            address,
            serial_number,
        } => {
            if canonicalize_bluetooth_address(&address).is_none() {
                return Err(BluetoothLinkError::InvalidAddress { address });
            }
            SerialNumber::new(&serial_number)
                .map(|_serial| ())
                .map_err(|error| BluetoothLinkError::InvalidExpectedUsbSerial {
                    detail: error.to_string(),
                })
        }
    }
}

/// Enumerate the bounded paired-device snapshot through Azimuth's signed helper.
///
/// Every bounded device is returned with its exact canonical address and
/// display name. Discovery performs no radio I/O and makes no TH-D75 identity
/// claim. Exact selection is followed by CAT qualification during connection.
///
/// # Errors
///
/// Returns [`BluetoothLinkError::UnsupportedPlatform`] outside macOS, or a
/// bounded helper/discovery failure on macOS.
#[uniffi::export(async_runtime = "tokio")]
pub async fn discover_paired_bluetooth_devices()
-> Result<BluetoothDeviceDiscovery, BluetoothLinkError> {
    #[cfg(target_os = "macos")]
    {
        let helper = bundled_bluetooth_helper_executable().map_err(|error| {
            BluetoothLinkError::BluetoothUnavailable {
                operation: "helper resolution".to_owned(),
                detail: error.to_string(),
            }
        })?;
        let native = enumerate_paired_bluetooth_devices(helper)
            .await
            .map_err(|error| BluetoothLinkError::BluetoothUnavailable {
                operation: "paired-device enumeration".to_owned(),
                detail: error.to_string(),
            })?;
        bluetooth_device_discovery_from_native(&native)
    }

    #[cfg(not(target_os = "macos"))]
    {
        Err(BluetoothLinkError::UnsupportedPlatform)
    }
}

#[cfg(target_os = "macos")]
fn bluetooth_device_discovery_from_native(
    native: &[kenwood_thd75::PairedBluetoothDevice],
) -> Result<BluetoothDeviceDiscovery, BluetoothLinkError> {
    let mut devices = native
        .iter()
        .map(|device| {
            let address = canonicalize_bluetooth_address(device.address()).ok_or_else(|| {
                BluetoothLinkError::BluetoothUnavailable {
                    operation: "paired-device enumeration".to_owned(),
                    detail: format!(
                        "native helper returned a non-canonical Bluetooth address: {}",
                        device.address()
                    ),
                }
            })?;
            Ok(BluetoothPairedDevice {
                address,
                display_name: device.display_name().to_owned(),
            })
        })
        .collect::<Result<Vec<_>, _>>()?;
    devices.sort_by(|left, right| {
        left.display_name
            .to_lowercase()
            .cmp(&right.display_name.to_lowercase())
            .then_with(|| left.address.cmp(&right.address))
    });
    let mut addresses = devices
        .iter()
        .map(|device| device.address.as_str())
        .collect::<Vec<_>>();
    addresses.sort_unstable();
    if addresses
        .windows(2)
        .any(|pair| matches!(pair, [left, right] if left == right))
    {
        return Err(BluetoothLinkError::BluetoothUnavailable {
            operation: "paired-device enumeration".to_owned(),
            detail: "native helper returned duplicate paired-device addresses".to_owned(),
        });
    }
    Ok(BluetoothDeviceDiscovery { devices })
}

fn validate_transfer_length(length: usize) -> Result<(), BluetoothLinkError> {
    let requested = u32::try_from(length).unwrap_or(u32::MAX);
    validate_transfer_length_u32(requested)
}

const fn accept_fixed_rfcomm_baud(_baud: u32) {}

const fn validate_transfer_length_u32(length: u32) -> Result<(), BluetoothLinkError> {
    if length == 0 || length > MAXIMUM_BLUETOOTH_TRANSFER_BYTES {
        Err(BluetoothLinkError::InvalidTransferLength {
            requested: length,
            maximum: MAXIMUM_BLUETOOTH_TRANSFER_BYTES,
        })
    } else {
        Ok(())
    }
}

#[cfg(target_os = "macos")]
#[derive(Debug)]
struct OpenedNativeBluetoothTransport {
    transport: BluetoothTransport,
    exact_address: Option<String>,
}

#[cfg(target_os = "macos")]
async fn open_native_transport(
    target: &ValidatedBluetoothLinkTarget,
    cached_expected_serial_address: Option<&str>,
    cancellation: &RecoveryCancellation,
) -> Result<OpenedNativeBluetoothTransport, BluetoothLinkError> {
    let helper = bundled_bluetooth_helper_executable().map_err(|error| {
        BluetoothLinkError::BluetoothUnavailable {
            operation: "helper resolution".to_owned(),
            detail: error.to_string(),
        }
    })?;
    match target {
        ValidatedBluetoothLinkTarget::ExactAddress(address)
        | ValidatedBluetoothLinkTarget::ExactAddressExpectedUsbSerial { address, .. } => {
            let transport = open_exact_address_transport(address, helper, cancellation).await?;
            Ok(OpenedNativeBluetoothTransport {
                transport,
                exact_address: Some(address.clone()),
            })
        }
        ValidatedBluetoothLinkTarget::ExpectedUsbSerial(expected) => {
            if let Some(address) = cached_expected_serial_address {
                let transport = open_exact_address_transport(address, helper, cancellation).await?;
                return Ok(OpenedNativeBluetoothTransport {
                    transport,
                    exact_address: Some(address.to_owned()),
                });
            }
            let selection = open_selected_bluetooth_transport(helper, None, expected, cancellation)
                .await
                .map_err(map_recovery_open_error)?;
            Ok(OpenedNativeBluetoothTransport {
                transport: selection.transport,
                exact_address: selection.exact_address,
            })
        }
    }
}

#[cfg(target_os = "macos")]
async fn open_exact_address_transport(
    address: &str,
    helper: std::path::PathBuf,
    cancellation: &RecoveryCancellation,
) -> Result<BluetoothTransport, BluetoothLinkError> {
    cancellation
        .check()
        .map_err(|_cancelled| BluetoothLinkError::OpenInterrupted)?;
    let address = address.to_owned();
    let open_cancellation = cancellation.bluetooth_open_cancellation();
    tokio::task::spawn_blocking(move || {
        BluetoothTransport::open_with_helper_executable_cancellable(
            Some(&address),
            helper,
            &open_cancellation,
        )
    })
    .await
    .map_err(|error| BluetoothLinkError::BluetoothUnavailable {
        operation: "helper task".to_owned(),
        detail: error.to_string(),
    })?
    .map_err(map_open_transport_error)
}

#[cfg(target_os = "macos")]
fn map_open_transport_error(error: TransportError) -> BluetoothLinkError {
    match error {
        TransportError::BluetoothOpenInterrupted => BluetoothLinkError::OpenInterrupted,
        other => BluetoothLinkError::BluetoothUnavailable {
            operation: "exact-address open".to_owned(),
            detail: transport_error_detail(&other),
        },
    }
}

#[cfg(target_os = "macos")]
fn map_recovery_open_error(
    error: crate::terminal_mode::DvGatewayRecoveryError,
) -> BluetoothLinkError {
    match error {
        crate::terminal_mode::DvGatewayRecoveryError::Cancelled => {
            BluetoothLinkError::OpenInterrupted
        }
        other => BluetoothLinkError::BluetoothUnavailable {
            operation: "USB-serial auto-match".to_owned(),
            detail: other.to_string(),
        },
    }
}

#[cfg(target_os = "macos")]
#[derive(Debug)]
struct QualifiedBluetoothTransport<T: Transport> {
    transport: RawProtocolSession<T>,
    serial_number: String,
}

#[cfg(target_os = "macos")]
async fn recover_and_qualify_serial_cancellable<T: Transport>(
    transport: T,
    expected: &SerialNumber,
    cancellation: &RecoveryCancellation,
) -> Result<QualifiedBluetoothTransport<T>, BluetoothLinkError> {
    let qualification = recover_and_qualify_serial(transport, expected);
    tokio::pin!(qualification);
    tokio::select! {
        biased;
        () = cancellation.cancelled() => Err(BluetoothLinkError::OpenInterrupted),
        result = &mut qualification => result,
    }
}

#[cfg(target_os = "macos")]
async fn recover_and_qualify_serial<T: Transport>(
    transport: T,
    expected: &SerialNumber,
) -> Result<QualifiedBluetoothTransport<T>, BluetoothLinkError> {
    let mut radio = Radio::connect_with_tnc_exit(transport)
        .await
        .map_err(|error| BluetoothLinkError::CatIdentityUnavailable {
            detail: format!("packet-mode recovery before CAT identity failed: {error}"),
        })?;
    let actual = match radio.get_serial_information().await {
        Ok(information) => information.into_parts().0,
        Err(error) => {
            drop(radio.disconnect().await);
            return Err(BluetoothLinkError::CatIdentityUnavailable {
                detail: error.to_string(),
            });
        }
    };
    if &actual != expected {
        drop(radio.disconnect().await);
        return Err(BluetoothLinkError::SerialMismatch {
            expected: expected.to_string(),
            actual: actual.to_string(),
        });
    }
    let transport = match radio.into_raw_protocol_session() {
        Ok(transport) => transport,
        Err((radio, error)) => {
            drop(radio.disconnect().await);
            return Err(BluetoothLinkError::CatIdentityUnavailable {
                detail: format!("CAT identity left an unsafe raw boundary: {error}"),
            });
        }
    };
    Ok(QualifiedBluetoothTransport {
        transport,
        serial_number: actual.to_string(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    type TestResult = Result<(), Box<dyn std::error::Error>>;

    #[test]
    fn exact_address_validation_accepts_one_consistent_separator() {
        assert!(is_exact_bluetooth_address("00-1A-7D-DA-71-13"));
        assert!(is_exact_bluetooth_address("00:1a:7d:da:71:13"));
        assert!(!is_exact_bluetooth_address("TH-D75"));
        assert!(!is_exact_bluetooth_address("00-1A:7D-DA-71-13"));
        assert!(!is_exact_bluetooth_address("00-1A-7D-DA-71-ZZ"));
    }

    #[test]
    fn exact_address_canonicalization_uses_native_hyphen_form() {
        assert_eq!(
            canonicalize_bluetooth_address("40:f3:b0:ae:1c:95").as_deref(),
            Some("40-F3-B0-AE-1C-95")
        );
        assert_eq!(
            canonicalize_bluetooth_address("40-f3-b0-ae-1c-95").as_deref(),
            Some("40-F3-B0-AE-1C-95")
        );
        assert_eq!(canonicalize_bluetooth_address("TH-D75"), None);
    }

    #[test]
    fn constructor_validates_all_target_dialects() -> TestResult {
        let exact = BluetoothByteTransport::new(BluetoothLinkTarget::ExactAddress {
            address: "00-1A-7D-DA-71-13".to_owned(),
        })?;
        assert_eq!(exact.matched_cat_serial()?, None);

        let expected = BluetoothByteTransport::new(BluetoothLinkTarget::ExpectedUsbSerial {
            serial_number: "C3C10368".to_owned(),
        })?;
        assert_eq!(expected.matched_cat_serial()?, None);

        let resolved =
            BluetoothByteTransport::new(BluetoothLinkTarget::ExactAddressExpectedUsbSerial {
                address: "40:f3:b0:ae:1c:95".to_owned(),
                serial_number: "C3C10368".to_owned(),
            })?;
        assert_eq!(resolved.matched_address()?, None);

        let invalid = BluetoothByteTransport::new(BluetoothLinkTarget::ExactAddress {
            address: "TH-D75".to_owned(),
        });
        assert!(matches!(
            invalid,
            Err(BluetoothLinkError::InvalidAddress { .. })
        ));
        Ok(())
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn constructor_stores_native_exact_address_form() -> TestResult {
        let link = BluetoothByteTransport::new(BluetoothLinkTarget::ExactAddress {
            address: "40:f3:b0:ae:1c:95".to_owned(),
        })?;
        let ValidatedBluetoothLinkTarget::ExactAddress(address) = &link.target else {
            return Err("constructor did not retain an exact-address target".into());
        };
        assert_eq!(address, "40-F3-B0-AE-1C-95");

        let resolved =
            BluetoothByteTransport::new(BluetoothLinkTarget::ExactAddressExpectedUsbSerial {
                address: "40:f3:b0:ae:1c:95".to_owned(),
                serial_number: "C3C10368".to_owned(),
            })?;
        let ValidatedBluetoothLinkTarget::ExactAddressExpectedUsbSerial {
            address,
            serial_number,
        } = &resolved.target
        else {
            return Err("constructor did not retain an address-and-serial target".into());
        };
        assert_eq!(address, "40-F3-B0-AE-1C-95");
        assert_eq!(serial_number.as_str(), "C3C10368");
        Ok(())
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn only_raw_exact_address_skips_cat_identity_during_open() -> TestResult {
        let exact = ValidatedBluetoothLinkTarget::ExactAddress("00-1A-7D-DA-71-13".to_owned());
        assert_eq!(exact.expected_serial(), None);

        let expected = SerialNumber::new("C3C10368")?;
        let serial_target = ValidatedBluetoothLinkTarget::ExpectedUsbSerial(expected.clone());
        assert_eq!(serial_target.expected_serial(), Some(&expected));

        let resolved = ValidatedBluetoothLinkTarget::ExactAddressExpectedUsbSerial {
            address: "40-F3-B0-AE-1C-95".to_owned(),
            serial_number: expected.clone(),
        };
        assert_eq!(resolved.expected_serial(), Some(&expected));
        Ok(())
    }

    #[test]
    fn transfer_bound_refuses_empty_and_oversized_calls() {
        assert!(matches!(
            validate_transfer_length_u32(0),
            Err(BluetoothLinkError::InvalidTransferLength { .. })
        ));
        assert!(validate_transfer_length_u32(1).is_ok());
        assert!(validate_transfer_length_u32(MAXIMUM_BLUETOOTH_TRANSFER_BYTES).is_ok());
        assert!(matches!(
            validate_transfer_length_u32(MAXIMUM_BLUETOOTH_TRANSFER_BYTES + 1),
            Err(BluetoothLinkError::InvalidTransferLength { .. })
        ));
    }

    #[tokio::test]
    async fn fixed_rfcomm_baud_is_a_no_op_without_an_open_link() -> TestResult {
        let link = BluetoothByteTransport::new(BluetoothLinkTarget::ExactAddress {
            address: "00-1A-7D-DA-71-13".to_owned(),
        })?;
        link.set_baud_rate(9_600);
        link.set_baud_rate(115_200);
        assert_eq!(link.matched_cat_serial()?, None);
        link.close().await?;
        Ok(())
    }

    #[cfg(target_os = "macos")]
    #[tokio::test]
    async fn read_interrupt_generation_closes_the_registration_race() -> TestResult {
        let link = BluetoothByteTransport::new(BluetoothLinkTarget::ExactAddress {
            address: "00-1A-7D-DA-71-13".to_owned(),
        })?;
        let generation = link.read_interrupt_generation.load(Ordering::Acquire);

        // Interrupt before the waiter registers. The generation check must
        // still complete immediately instead of depending on notification
        // timing.
        link.cancel_pending_read();
        tokio::time::timeout(
            std::time::Duration::from_millis(50),
            link.wait_for_read_interrupt(generation),
        )
        .await
        .map_err(|_elapsed| "read interrupt notification was lost")?;
        Ok(())
    }

    #[cfg(target_os = "macos")]
    #[tokio::test]
    async fn interrupt_before_read_is_sticky_until_the_read_consumes_it() -> TestResult {
        let link = BluetoothByteTransport::new(BluetoothLinkTarget::ExactAddress {
            address: "00-1A-7D-DA-71-13".to_owned(),
        })?;

        link.cancel_pending_read();
        assert_eq!(link.read(1).await, Err(BluetoothLinkError::ReadInterrupted));
        assert_eq!(link.read(1).await, Err(BluetoothLinkError::NotOpen));
        Ok(())
    }

    #[cfg(target_os = "macos")]
    #[tokio::test]
    async fn cancel_before_open_registration_is_sticky() -> TestResult {
        let link = BluetoothByteTransport::new(BluetoothLinkTarget::ExactAddress {
            address: "40-F3-B0-AE-1C-95".to_owned(),
        })?;

        link.cancel_pending_open();
        assert_eq!(link.open().await, Err(BluetoothLinkError::OpenInterrupted));
        Ok(())
    }

    #[cfg(target_os = "macos")]
    #[tokio::test]
    async fn close_consumes_a_late_open_cancellation_before_reopen() -> TestResult {
        let link = BluetoothByteTransport::new(BluetoothLinkTarget::ExactAddress {
            address: "40-F3-B0-AE-1C-95".to_owned(),
        })?;

        link.cancel_pending_open();
        link.close().await?;
        let registration = link.begin_open_cancellation()?;
        assert!(registration.cancellation().check().is_ok());
        drop(registration);
        Ok(())
    }

    #[cfg(target_os = "macos")]
    #[tokio::test]
    async fn expected_serial_cache_survives_close_but_published_identity_does_not() -> TestResult {
        let link = BluetoothByteTransport::new(BluetoothLinkTarget::ExpectedUsbSerial {
            serial_number: "C3C10368".to_owned(),
        })?;
        link.replace_cached_expected_serial_address(Some("40-F3-B0-AE-1C-95".to_owned()))?;
        link.replace_matched_identity(
            Some("40-F3-B0-AE-1C-95".to_owned()),
            Some("C3C10368".to_owned()),
        )?;

        link.close().await?;

        assert_eq!(link.matched_address()?, None);
        assert_eq!(link.matched_cat_serial()?, None);
        assert_eq!(
            link.cached_expected_serial_address()?.as_deref(),
            Some("40-F3-B0-AE-1C-95")
        );
        Ok(())
    }

    #[cfg(target_os = "macos")]
    #[tokio::test]
    async fn serial_qualification_recovers_kiss_before_ae_and_preserves_raw_link() -> TestResult {
        let mut transport = serial_qualification_transport(b"\xC0\x00\xC0", "C3C10368");
        transport.expect(b"raw probe", b"raw reply");
        let expected = SerialNumber::new("C3C10368")?;

        let mut qualified = recover_and_qualify_serial(transport, &expected).await?;
        qualified.transport.write(b"raw probe").await?;
        let mut buffer = [0_u8; 16];
        let count = qualified.transport.read(&mut buffer).await?;

        assert_eq!(qualified.serial_number, "C3C10368");
        assert_eq!(buffer.get(..count), Some(b"raw reply".as_slice()));
        Ok(())
    }

    #[cfg(target_os = "macos")]
    #[tokio::test]
    async fn serial_qualification_recovers_transient_mmdvm_before_ae() -> TestResult {
        let transport = serial_qualification_transport(&[0xE0, 0x03, 0x00], "C3C10368");
        let expected = SerialNumber::new("C3C10368")?;

        let qualified = recover_and_qualify_serial(transport, &expected).await?;

        assert_eq!(qualified.serial_number, "C3C10368");
        Ok(())
    }

    #[cfg(target_os = "macos")]
    #[tokio::test]
    async fn serial_qualification_rejects_a_different_radio_after_recovery() -> TestResult {
        let transport = serial_qualification_transport(b"TN 0,0\r", "C5310165");
        let expected = SerialNumber::new("C3C10368")?;

        let Err(error) = recover_and_qualify_serial(transport, &expected).await else {
            return Err("a different physical radio passed serial qualification".into());
        };
        assert!(matches!(
            error,
            BluetoothLinkError::SerialMismatch { expected, actual }
                if expected == "C3C10368" && actual == "C5310165"
        ));
        Ok(())
    }

    #[cfg(target_os = "macos")]
    #[tokio::test]
    async fn cancellation_interrupts_the_owned_recovery_and_identity_future() -> TestResult {
        let transport = serial_qualification_transport(b"TN 0,0\r", "C3C10368");
        let expected = SerialNumber::new("C3C10368")?;
        let cancellation = Arc::new(RecoveryCancellation::default());
        let requester = Arc::clone(&cancellation);
        let cancel = tokio::spawn(async move {
            tokio::time::sleep(std::time::Duration::from_millis(20)).await;
            requester.request();
        });

        let started = std::time::Instant::now();
        let result =
            recover_and_qualify_serial_cancellable(transport, &expected, cancellation.as_ref())
                .await;
        cancel.await?;

        assert!(matches!(result, Err(BluetoothLinkError::OpenInterrupted)));
        assert!(
            started.elapsed() < std::time::Duration::from_millis(250),
            "cancellation waited for the recovery preamble instead of dropping its owned transport"
        );
        Ok(())
    }

    #[cfg(target_os = "macos")]
    fn serial_qualification_transport(
        recovery_residue: &[u8],
        serial_number: &str,
    ) -> kenwood_thd75::transport::MockTransport {
        use kenwood_thd75::transport::MockTransport;

        let mut transport = MockTransport::new();
        transport.expect(b"ID\r", b"N\r");
        transport.expect(b"\r", b"");
        transport.expect(b"\r", b"");
        transport.expect(&[0x03], b"");
        transport.expect(&[0xC0, 0xFF, 0xC0], b"");
        transport.expect(b"\rTC 1\r", b"");
        transport.expect(b"TN 0,0\r", recovery_residue);
        transport.expect(b"ID\r", b"ID TH-D75\r");
        transport.expect(b"AE\r", format!("AE {serial_number},K01\r").as_bytes());
        transport.pend_when_empty();
        transport
    }
}
