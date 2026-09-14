//! Platform-independent validated Bluetooth selection and service values.

use std::fmt;
use std::str::FromStr;
use std::sync::{
    Arc,
    atomic::{AtomicBool, Ordering},
};

use crate::TransportError;

/// Sticky thread-safe cancellation for one bounded native helper operation.
///
/// Available on every platform with `native-bluetooth`. All clones share one
/// flag, initially clear. A canceled token cannot be reset or reused for a new
/// successful operation. It cancels discovery/opening, not an already returned
/// connection; explicitly close that transport. Canceling a token does not join
/// a blocking worker or prove helper retirement. Retain and join the worker.
#[derive(Debug, Clone, Default)]
pub struct BluetoothOpenCancellation {
    requested: Arc<AtomicBool>,
}

impl BluetoothOpenCancellation {
    /// Request cancellation, including before an operation starts.
    pub fn cancel(&self) {
        self.requested.store(true, Ordering::Release);
    }

    /// Whether cancellation has been requested; requests are never cleared.
    #[must_use]
    pub fn is_cancelled(&self) -> bool {
        self.requested.load(Ordering::Acquire)
    }

    #[cfg(target_os = "macos")]
    pub(super) fn check(&self) -> Result<(), TransportError> {
        if self.is_cancelled() {
            Err(TransportError::BluetoothOpenInterrupted)
        } else {
            Ok(())
        }
    }
}

/// Exact Bluetooth address, stored in uppercase hyphen-separated form.
///
/// Available on every platform with `native-bluetooth`. Parsing accepts exactly
/// six two-digit ASCII hexadecimal octets, separated consistently by either
/// colons or hyphens. Hexadecimal letters may have either case. Mixed separators,
/// whitespace, omitted zeroes and non-ASCII digits are rejected. Parsing checks
/// syntax only, not pairing, device presence, identity or reachability.
///
/// # Examples
///
/// Validation and normalization are entirely offline, including on non-macOS
/// hosts. No native helper is started:
///
/// ```rust
/// use kenwood_transport::TransportError;
/// use kenwood_transport::bluetooth::{BluetoothAddress, BluetoothDeviceName, RfcommChannel};
///
/// fn main() -> Result<(), TransportError> {
///     let address: BluetoothAddress = "aa:bb:cc:dd:ee:ff".parse()?;
///     assert_eq!(address.as_str(), "AA-BB-CC-DD-EE-FF");
///     assert!("AA:BB-CC-DD-EE-FF".parse::<BluetoothAddress>().is_err());
///     assert!(BluetoothDeviceName::new(address.as_str()).is_err());
///     let name = BluetoothDeviceName::new("Bench radio")?;
///     assert_eq!(name.as_str(), "Bench radio");
///     assert_eq!(RfcommChannel::new(1)?.get(), 1);
///     assert!(RfcommChannel::new(31).is_err());
///     Ok(())
/// }
/// ```
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct BluetoothAddress(String);

impl BluetoothAddress {
    /// Canonical address text; this never names a display-name selector.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl FromStr for BluetoothAddress {
    type Err = TransportError;

    /// Parse the complete address grammar documented on [`BluetoothAddress`].
    ///
    /// # Errors
    ///
    /// Returns [`TransportError::BluetoothParameter`] for any invalid address.
    fn from_str(value: &str) -> Result<Self, Self::Err> {
        let separator = value.as_bytes().get(2).copied();
        if value.len() != 17
            || !matches!(separator, Some(b':' | b'-'))
            || !value.bytes().enumerate().all(|(index, byte)| {
                if index % 3 == 2 {
                    Some(byte) == separator
                } else {
                    byte.is_ascii_hexdigit()
                }
            })
        {
            return Err(invalid("address", value));
        }
        Ok(Self(
            value
                .chars()
                .map(|character| match character {
                    ':' => '-',
                    other => other.to_ascii_uppercase(),
                })
                .collect(),
        ))
    }
}

impl fmt::Display for BluetoothAddress {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.as_str())
    }
}

/// Nonempty Bluetooth display name; ambiguity is refused during native selection.
///
/// Available on every platform with `native-bluetooth`. Names contain 1 through
/// 1,024 UTF-8 bytes and no Unicode control characters. Text that parses as a
/// [`BluetoothAddress`] is rejected. Case, spaces and other non-control Unicode
/// characters are retained exactly; there is no trimming or normalization.
/// A valid name is only a selector, not evidence of a paired or reachable radio.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BluetoothDeviceName(String);

impl BluetoothDeviceName {
    /// Validate a display name without consulting paired devices.
    ///
    /// # Errors
    ///
    /// Returns [`TransportError::BluetoothParameter`] for an empty name, more
    /// than 1,024 UTF-8 bytes, a Unicode control character, or text accepted by
    /// [`BluetoothAddress`]. An address must use an exact-address selector.
    pub fn new(value: &str) -> Result<Self, TransportError> {
        if value.is_empty()
            || value.len() > 1024
            || value.chars().any(char::is_control)
            || value.parse::<BluetoothAddress>().is_ok()
        {
            Err(invalid("display name", value))
        } else {
            Ok(Self(value.to_owned()))
        }
    }

    /// Validated display-name text, not physical identity evidence.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

/// Explicit exact-address or display-name selection for one paired device.
///
/// Available on every platform with `native-bluetooth`. Construction performs
/// no enumeration. Native opening applies the selection without fallback.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BluetoothDeviceSelector {
    /// An exact address never falls back to name matching.
    Address(BluetoothAddress),
    /// A name must match exactly one paired record.
    Name(BluetoothDeviceName),
}

impl BluetoothDeviceSelector {
    /// Exact selector text passed to the isolated helper.
    #[must_use]
    pub fn as_str(&self) -> &str {
        match self {
            Self::Address(value) => value.as_str(),
            Self::Name(value) => value.as_str(),
        }
    }
}

/// Validated RFCOMM server channel in the Bluetooth domain 1 through 30.
///
/// Available on every platform with `native-bluetooth`. A valid number is not
/// evidence that a device offers a service on that channel.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct RfcommChannel(u8);

impl RfcommChannel {
    /// Validate one RFCOMM server channel without opening a connection.
    ///
    /// # Errors
    ///
    /// Returns [`TransportError::BluetoothParameter`] outside 1 through 30.
    pub fn new(value: u8) -> Result<Self, TransportError> {
        if (1..=30).contains(&value) {
            Ok(Self(value))
        } else {
            Err(invalid("RFCOMM channel", &value.to_string()))
        }
    }

    /// Raw validated server-channel number.
    #[must_use]
    pub const fn get(self) -> u8 {
        self.0
    }
}

/// Service resolution policy for a single bounded native open.
///
/// Available on every platform with `native-bluetooth`; the native backend
/// that applies this policy is macOS-only. Neither policy retries an open.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BluetoothService {
    /// Use an explicitly selected channel and retain the baseband wakeup.
    ///
    /// The caller supplies channel provenance, such as a model-qualified value
    /// or a channel returned by its previous successful opening of this device.
    /// This policy does not rediscover or substitute a channel.
    FixedChannel(RfcommChannel),
    /// Await a fresh SDP callback and resolve the Serial Port service UUID 0x1101.
    SerialPort,
}

/// One paired device, identified by its exact address rather than its name.
///
/// Available on every platform with `native-bluetooth`. Inventory is cached
/// host pairing metadata, not evidence of current connection or protocol
/// readiness. Display names are observations and need not satisfy the grammar
/// of a caller-created [`BluetoothDeviceName`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PairedBluetoothDevice {
    pub(super) address: BluetoothAddress,
    pub(super) display_name: String,
}

impl PairedBluetoothDevice {
    /// Canonical exact address suitable for an unambiguous selector.
    #[must_use]
    pub const fn address(&self) -> &BluetoothAddress {
        &self.address
    }

    /// Human-readable paired-device name for diagnostics only.
    #[must_use]
    pub fn display_name(&self) -> &str {
        &self.display_name
    }
}

fn invalid(parameter: &'static str, value: &str) -> TransportError {
    TransportError::BluetoothParameter {
        parameter,
        value: value.to_owned(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    type TestResult = Result<(), Box<dyn std::error::Error>>;

    #[test]
    fn exact_addresses_normalize_only_valid_complete_hexadecimal_octets() -> TestResult {
        let expected: BluetoothAddress = "AA-BB-CC-DD-EE-FF".parse()?;
        for value in ["aa:bb:cc:dd:ee:ff", "AA-BB-CC-DD-EE-FF"] {
            assert_eq!(value.parse::<BluetoothAddress>()?, expected);
        }
        for value in [
            "",
            "TH-D75",
            "AA-BB-CC-DD-EE",
            "AA:BB-CC-DD-EE-FF",
            "GG-BB-CC-DD-EE-FF",
            " AA-BB-CC-DD-EE-FF",
        ] {
            assert!(matches!(
                value.parse::<BluetoothAddress>(),
                Err(TransportError::BluetoothParameter {
                    parameter: "address",
                    ..
                })
            ));
        }
        assert_eq!(expected.to_string(), "AA-BB-CC-DD-EE-FF");
        Ok(())
    }

    #[test]
    fn channels_cover_the_entire_byte_domain_and_names_cannot_hide_addresses() -> TestResult {
        for value in u8::MIN..=u8::MAX {
            let channel = RfcommChannel::new(value);
            if (1..=30).contains(&value) {
                assert_eq!(channel?.get(), value);
            } else {
                assert!(matches!(
                    channel,
                    Err(TransportError::BluetoothParameter {
                        parameter: "RFCOMM channel",
                        ..
                    })
                ));
            }
        }
        for value in ["", "AA-BB-CC-DD-EE-FF", "Radio\0name", "Radio\nname"] {
            assert!(BluetoothDeviceName::new(value).is_err());
        }
        assert!(BluetoothDeviceName::new(&"a".repeat(1025)).is_err());
        assert_eq!(
            BluetoothDeviceName::new("Field 日本")?.as_str(),
            "Field 日本"
        );
        Ok(())
    }

    #[test]
    fn cancellation_is_sticky_and_shared_by_clones() {
        let signal = BluetoothOpenCancellation::default();
        let other = signal.clone();
        assert!(!other.is_cancelled());
        signal.cancel();
        other.cancel();
        assert!(signal.is_cancelled());
        assert!(other.is_cancelled());
    }
}
