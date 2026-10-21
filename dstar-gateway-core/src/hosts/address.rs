//! Validated DNS hostnames and unscoped IP literals, without a port or URI scheme.

use std::fmt;
use std::net::IpAddr;
use std::str::FromStr;

/// A DNS hostname or an IPv4/IPv6 literal from a host-file address column.
///
/// DNS labels follow RFC 1035 section 2.3.1 with RFC 1123 section 2.1's
/// leading-digit allowance: 1..=63 ASCII letters, digits or hyphens per label,
/// with alphanumeric edges and an ASCII letter in the final label. The name has at most
/// 253 bytes before an optional
/// root dot. Single-label names and ASCII IDNA labels are supported. IP literals
/// have no scope identifier; bracketed IPv6 is accepted and stored as an IP.
/// Construction performs no DNS or other I/O. See [host loading](super::HostFile).
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct HostAddress(Address);

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
enum Address {
    Ip(IpAddr),
    Dns(String),
}

impl HostAddress {
    /// Validate an entire hostname or IP literal without trimming or repair.
    ///
    /// # Errors
    ///
    /// Returns a typed [`HostAddressError`] for an empty address, oversized
    /// name/label, empty label, non-hostname byte, or label-edge hyphen.
    pub fn new(value: &str) -> Result<Self, HostAddressError> {
        let literal = value
            .strip_prefix('[')
            .and_then(|value| value.strip_suffix(']'));
        if let Ok(ip) = literal.unwrap_or(value).parse::<IpAddr>() {
            return Ok(Self(Address::Ip(ip)));
        }
        let name = value.strip_suffix('.').unwrap_or(value);
        if name.is_empty() {
            return Err(HostAddressError::Empty);
        }
        if name.len() > 253 {
            return Err(HostAddressError::NameTooLong { bytes: name.len() });
        }
        for (index, label) in name.split('.').enumerate() {
            if label.is_empty() {
                return Err(HostAddressError::EmptyLabel { index });
            }
            if label.len() > 63 {
                return Err(HostAddressError::LabelTooLong {
                    index,
                    bytes: label.len(),
                });
            }
            if let Some((offset, byte)) = label
                .bytes()
                .enumerate()
                .find(|(_, byte)| !byte.is_ascii_alphanumeric() && *byte != b'-')
            {
                return Err(HostAddressError::InvalidByte {
                    index,
                    offset,
                    byte,
                });
            }
            if label.starts_with('-') || label.ends_with('-') {
                return Err(HostAddressError::HyphenAtEdge { index });
            }
        }
        if name
            .rsplit('.')
            .next()
            .is_some_and(|label| !label.bytes().any(|byte| byte.is_ascii_alphabetic()))
        {
            return Err(HostAddressError::NonalphabeticTopLabel);
        }
        Ok(Self(Address::Dns(value.to_owned())))
    }

    /// IP address for a literal, or `None` for a DNS hostname.
    #[must_use]
    pub const fn ip(&self) -> Option<IpAddr> {
        match self.0 {
            Address::Ip(ip) => Some(ip),
            Address::Dns(_) => None,
        }
    }

    /// Borrow the original validated DNS hostname, including an optional root dot.
    #[must_use]
    pub fn dns_name(&self) -> Option<&str> {
        match &self.0 {
            Address::Dns(name) => Some(name),
            Address::Ip(_) => None,
        }
    }
}

impl From<IpAddr> for HostAddress {
    fn from(value: IpAddr) -> Self {
        Self(Address::Ip(value))
    }
}

impl FromStr for HostAddress {
    type Err = HostAddressError;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        Self::new(value)
    }
}

impl TryFrom<&str> for HostAddress {
    type Error = HostAddressError;

    fn try_from(value: &str) -> Result<Self, Self::Error> {
        Self::new(value)
    }
}

impl fmt::Display for HostAddress {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match &self.0 {
            Address::Ip(ip) => fmt::Display::fmt(ip, formatter),
            Address::Dns(name) => formatter.write_str(name),
        }
    }
}

/// Hostname syntax rejected without resolving or opening an endpoint.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
#[non_exhaustive]
pub enum HostAddressError {
    /// No address or only the DNS root was supplied.
    #[error("host address is empty")]
    Empty,
    /// The hostname before an optional root dot exceeds 253 bytes.
    #[error("hostname has {bytes} bytes; at most 253 are supported")]
    NameTooLong {
        /// Observed byte length.
        bytes: usize,
    },
    /// Two dots or an initial dot produced an empty label.
    #[error("hostname label {index} is empty")]
    EmptyLabel {
        /// Zero-based label index.
        index: usize,
    },
    /// A hostname label exceeds the DNS 63-byte limit.
    #[error("hostname label {index} has {bytes} bytes; at most 63 are supported")]
    LabelTooLong {
        /// Zero-based label index.
        index: usize,
        /// Observed label byte length.
        bytes: usize,
    },
    /// A label contains a byte outside ASCII letters, digits and hyphen.
    #[error("hostname label {index} byte {offset} is invalid: 0x{byte:02X}")]
    InvalidByte {
        /// Zero-based label index.
        index: usize,
        /// Zero-based byte offset within that label.
        offset: usize,
        /// Rejected original byte.
        byte: u8,
    },
    /// A label starts or ends with a hyphen.
    #[error("hostname label {index} starts or ends with a hyphen")]
    HyphenAtEdge {
        /// Zero-based label index.
        index: usize,
    },
    /// A non-IP hostname's final label contains no ASCII letter.
    #[error("a hostname's final label must contain an ASCII letter")]
    NonalphabeticTopLabel,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn address_boundary_distinguishes_literals_names_and_endpoint_syntax()
    -> Result<(), Box<dyn std::error::Error>> {
        let ipv6 = HostAddress::new("[2001:db8::1]")?;
        assert_eq!(ipv6.ip(), Some("2001:db8::1".parse()?));
        assert_eq!(ipv6.to_string(), "2001:db8::1");
        assert_eq!(
            HostAddress::new("9-radio.example.")?.dns_name(),
            Some("9-radio.example.")
        );
        for invalid in [
            "",
            ".",
            "bad..example",
            "-bad.example",
            "bad-.example",
            "https://example",
            "example:30001",
            " example",
            "example\n",
            "fe80::1%2",
            "a_b",
            "999.1.1.1",
            "host.123",
            "host.1-2",
        ] {
            assert!(
                HostAddress::new(invalid).is_err(),
                "unexpected accepted address {invalid:?}"
            );
        }
        let label = "a".repeat(63);
        let maximum = format!("{label}.{label}.{label}.{}", "b".repeat(61));
        assert_eq!(maximum.len(), 253);
        assert!(
            HostAddress::new(&maximum).is_ok(),
            "the maximum DNS name remains representable"
        );
        assert!(
            HostAddress::new(&format!("{maximum}b")).is_err(),
            "DNS name length is checked before use"
        );
        assert!(
            HostAddress::new(&"a".repeat(64)).is_err(),
            "individual labels are bounded"
        );
        Ok(())
    }
}
