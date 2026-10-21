//! APRS-IS login string + passcode computation.
//!
//! # Passcode
//!
//! The APRS-IS passcode is computed from the callsign (without SSID)
//! using a well-known hash algorithm. Use [`aprs_is_passcode`] to
//! compute it.

use crate::error::AprsIsError;
use crate::{LoginField, LoginFieldError};
use std::num::NonZeroU16;

/// A passcode in the APRS-IS 15-bit wire domain, including zero.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct VerifiedPasscode(u16);

impl VerifiedPasscode {
    /// Returns `None` when `value` exceeds 32767.
    #[must_use]
    pub const fn new(value: u16) -> Option<Self> {
        if value <= 0x7fff {
            Some(Self(value))
        } else {
            None
        }
    }

    /// Returns the validated wire value.
    #[must_use]
    pub const fn value(self) -> u16 {
        self.0
    }
}

impl std::fmt::Display for VerifiedPasscode {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        self.0.fmt(formatter)
    }
}

impl TryFrom<u16> for VerifiedPasscode {
    type Error = std::num::TryFromIntError;

    fn try_from(value: u16) -> Result<Self, Self::Error> {
        i16::try_from(value).map(|_| Self(value))
    }
}

impl From<VerifiedPasscode> for u16 {
    fn from(value: VerifiedPasscode) -> Self {
        value.value()
    }
}

/// APRS-IS authentication passcode.
///
/// Per the APRS-IS authentication spec, a valid passcode is a 15-bit
/// nonnegative integer computed from the callsign via the public
/// [`aprs_is_passcode`] hash. Clients that don't hold a verified passcode
/// (e.g. read-only monitors) may authenticate as "receive-only" which
/// maps to the wire value `-1`.
///
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Passcode {
    /// A 15-bit credential awaiting the server's verification.
    Verified(VerifiedPasscode),
    /// Receive-only connection; the server delivers its feed and rejects
    /// packets submitted by this client.
    ReceiveOnly,
}

impl Passcode {
    /// Compute the passcode for a callsign using the public APRS-IS
    /// hash algorithm.
    #[must_use]
    pub fn for_callsign(callsign: &str) -> Self {
        let value = aprs_is_passcode(callsign);
        let value = u16::try_from(value).unwrap_or_else(|_| unreachable!("hash is 15-bit"));
        Self::Verified(VerifiedPasscode(value))
    }

    /// Convert to the wire representation used in the APRS-IS login
    /// string. `Verified(n)` → `n`, `ReceiveOnly` → `-1`.
    #[must_use]
    pub const fn as_wire(self) -> i32 {
        match self {
            Self::Verified(n) => n.value() as i32,
            Self::ReceiveOnly => -1,
        }
    }
}

impl std::fmt::Display for Passcode {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        self.as_wire().fmt(f)
    }
}

/// APRS-IS (Internet Service) client configuration.
///
/// Connects to an APRS-IS server via TCP, authenticates with callsign
/// and passcode, and allows sending/receiving APRS packets over the
/// internet backbone.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AprsIsConfig {
    /// Callsign with optional SSID (e.g., "N0CALL-10").
    pub(crate) callsign: String,
    /// APRS-IS passcode (computed from callsign, or `ReceiveOnly`).
    pub(crate) passcode: Passcode,
    /// Server hostname (e.g., "rotate.aprs2.net").
    pub(crate) server: String,
    /// Server port (default 14580).
    pub(crate) port: u16,
    /// APRS-IS filter string (e.g., "r/35.25/-97.75/100" for 100km radius).
    pub(crate) filter: String,
    /// Software name for login.
    pub(crate) software_name: String,
    /// Software version for login.
    pub(crate) software_version: String,
}

impl AprsIsConfig {
    /// Creates a checked APRS-IS login with the callsign-derived passcode.
    ///
    /// Computes the passcode automatically from the callsign. Defaults to
    /// `rotate.aprs2.net:14580` with no filter.
    ///
    /// # Errors
    ///
    /// Returns [`AprsIsError::InvalidLoginField`] for an invalid login identity.
    pub fn new(callsign: &str) -> Result<Self, AprsIsError> {
        validate_login_callsign(callsign)?;
        Ok(Self {
            callsign: callsign.to_owned(),
            passcode: Passcode::for_callsign(callsign),
            server: "rotate.aprs2.net".to_owned(),
            port: 14580,
            filter: String::new(),
            software_name: "aprs-is".to_owned(),
            software_version: env!("CARGO_PKG_VERSION").to_owned(),
        })
    }

    /// Create a receive-only APRS-IS configuration for the given
    /// callsign. The server rejects packets submitted by this login.
    ///
    /// # Errors
    ///
    /// Returns [`AprsIsError::InvalidLoginField`] for an invalid login identity.
    pub fn receive_only(callsign: &str) -> Result<Self, AprsIsError> {
        Self::new(callsign)?.with_passcode(Passcode::ReceiveOnly)
    }

    /// Selects a DNS hostname or IP address and a nonzero TCP port.
    ///
    /// # Errors
    ///
    /// Returns [`AprsIsError::InvalidLoginField`] for an invalid hostname.
    pub fn with_endpoint(
        mut self,
        server: impl Into<String>,
        port: NonZeroU16,
    ) -> Result<Self, AprsIsError> {
        let server = server.into();
        let host = server.strip_suffix('.').unwrap_or(&server);
        let valid_dns = !host.is_empty()
            && host.len() <= 253
            && host.split('.').all(|label| {
                !label.is_empty()
                    && label.len() <= 63
                    && label
                        .bytes()
                        .all(|byte| byte.is_ascii_alphanumeric() || byte == b'-')
                    && !label.starts_with('-')
                    && !label.ends_with('-')
            });
        if server.parse::<std::net::IpAddr>().is_err() && !valid_dns {
            return Err(AprsIsError::InvalidLoginField {
                field: LoginField::Server,
                reason: LoginFieldError::InvalidEndpoint,
            });
        }
        self.server = server;
        self.port = port.get();
        Ok(self)
    }

    /// Selects authentication while preserving the complete login-line bound.
    ///
    /// # Errors
    ///
    /// Returns [`AprsIsError::InvalidUplinkLine`] if the login exceeds 512 bytes.
    pub fn with_passcode(mut self, passcode: Passcode) -> Result<Self, AprsIsError> {
        self.passcode = passcode;
        self.checked()
    }

    /// Selects nonempty ASCII software-name and version words.
    ///
    /// # Errors
    ///
    /// Returns [`AprsIsError::InvalidLoginField`] for invalid words, or
    /// [`AprsIsError::InvalidUplinkLine`] if the login exceeds 512 bytes.
    pub fn with_software(
        mut self,
        name: impl Into<String>,
        version: impl Into<String>,
    ) -> Result<Self, AprsIsError> {
        self.software_name = name.into();
        self.software_version = version.into();
        self.checked()
    }

    /// Selects a raw server filter extension without interpreting its grammar.
    ///
    /// # Errors
    ///
    /// Returns [`AprsIsError::InvalidLoginField`] for non-ASCII or control bytes,
    /// or [`AprsIsError::InvalidUplinkLine`] if the login exceeds 512 bytes.
    pub fn with_raw_filter(mut self, filter: impl Into<String>) -> Result<Self, AprsIsError> {
        self.filter = filter.into();
        self.checked()
    }

    /// Selects streamed clauses with checked order and spatial limits.
    ///
    /// # Errors
    ///
    /// Returns [`AprsIsError::InvalidFilter`] for an invalid combination, or
    /// [`AprsIsError::InvalidUplinkLine`] when the complete login exceeds 512 bytes.
    pub fn with_filter<'filters, I>(self, filters: I) -> Result<Self, AprsIsError>
    where
        I: IntoIterator<Item = &'filters crate::AprsIsFilter>,
    {
        self.with_raw_filter(crate::AprsIsFilter::join(filters)?)
    }

    fn checked(self) -> Result<Self, AprsIsError> {
        let _line =
            crate::AprsIsUplinkLine::from_wire_bytes(build_login_string(&self)?.as_bytes())?;
        Ok(self)
    }

    /// Returns the case-preserved checked login identity.
    #[must_use]
    pub fn callsign(&self) -> &str {
        &self.callsign
    }

    /// Returns the selected authentication mode.
    #[must_use]
    pub const fn passcode(&self) -> Passcode {
        self.passcode
    }

    /// Returns the server hostname or IP address without a port.
    #[must_use]
    pub fn server(&self) -> &str {
        &self.server
    }

    /// Returns the nonzero TCP port.
    #[must_use]
    pub const fn port(&self) -> NonZeroU16 {
        match NonZeroU16::new(self.port) {
            Some(port) => port,
            None => unreachable!(),
        }
    }

    /// Returns the unprefixed filter expression, or an empty string.
    #[must_use]
    pub fn filter(&self) -> &str {
        &self.filter
    }

    /// Returns the software name and version as validated login words.
    #[must_use]
    pub fn software(&self) -> (&str, &str) {
        (&self.software_name, &self.software_version)
    }
}

/// Compute the APRS-IS passcode from a callsign.
///
/// The algorithm is a simple hash of the callsign characters (without SSID).
/// This is NOT cryptographic -- it is a well-known public algorithm used
/// by all APRS software.
///
/// # Algorithm
///
/// 1. Strip SSID (everything from `-` onward).
/// 2. Uppercase the base callsign.
/// 3. Starting with hash = 0x73E2, XOR each pair of bytes: first byte
///    shifted left 8 bits, second byte as-is. If the callsign has an odd
///    number of characters, the last byte is XOR'd shifted left 8 bits.
/// 4. Mask with 0x7FFF to produce a nonnegative 15-bit value.
#[must_use]
pub fn aprs_is_passcode(callsign: &str) -> i32 {
    // Strip SSID.
    let base = callsign
        .split('-')
        .next()
        .unwrap_or(callsign)
        .to_uppercase();

    let bytes = base.as_bytes();
    let mut hash: u16 = 0x73E2;

    let mut i = 0;
    while i < bytes.len() {
        let Some(first) = bytes.get(i) else {
            break;
        };
        hash ^= u16::from(*first) << 8;
        if let Some(second) = bytes.get(i + 1) {
            hash ^= u16::from(*second);
        }
        i += 2;
    }

    i32::from(hash & 0x7FFF)
}

/// Validate an APRS-IS login identity against the published wire grammar.
fn validate_login_callsign(value: &str) -> Result<(), AprsIsError> {
    let invalid = |reason| AprsIsError::InvalidLoginField {
        field: LoginField::Callsign,
        reason,
    };

    if !value.is_ascii() {
        return Err(invalid(LoginFieldError::NonAscii));
    }
    if value.len() > 9 {
        return Err(invalid(LoginFieldError::IdentityTooLong));
    }
    if value.len() == 8 && value.bytes().all(|byte| byte.is_ascii_hexdigit()) {
        return Err(invalid(LoginFieldError::ReservedServerIdentity));
    }
    if value.matches('-').count() > 1 {
        return Err(invalid(LoginFieldError::MultipleSsidSeparators));
    }

    let (base, suffix) = value
        .split_once('-')
        .map_or((value, None), |(base, suffix)| (base, Some(suffix)));
    if base.len() < 3 {
        return Err(invalid(LoginFieldError::BaseTooShort));
    }
    if !base.bytes().all(|byte| byte.is_ascii_alphanumeric()) {
        return Err(invalid(LoginFieldError::InvalidBase));
    }
    if let Some(suffix) = suffix {
        if !(1..=2).contains(&suffix.len()) {
            return Err(invalid(LoginFieldError::InvalidSsidLength));
        }
        if !suffix.bytes().all(|byte| byte.is_ascii_alphanumeric()) {
            return Err(invalid(LoginFieldError::InvalidSsid));
        }
        if suffix == "0" {
            return Err(invalid(LoginFieldError::ExplicitZeroSsid));
        }
    }
    Ok(())
}

/// Validate a non-callsign login field that must be one non-empty word.
///
/// Per <https://www.aprs-is.net/Connecting.aspx>, the software name and
/// version must each be one word ("Do not use spaces"). The login line is
/// space-delimited, so any embedded whitespace or control character would
/// shift the keyword parsing or inject another handshake line.
fn validate_login_word(field: LoginField, value: &str) -> Result<(), AprsIsError> {
    if value.is_empty() {
        return Err(AprsIsError::InvalidLoginField {
            field,
            reason: LoginFieldError::Empty,
        });
    }
    if !value.is_ascii() {
        return Err(AprsIsError::InvalidLoginField {
            field,
            reason: LoginFieldError::NonAscii,
        });
    }
    if value.chars().any(|c| c.is_whitespace() || c.is_control()) {
        return Err(AprsIsError::InvalidLoginField {
            field,
            reason: LoginFieldError::NotSingleWord,
        });
    }
    Ok(())
}

/// Validate the filter clause, which legitimately contains spaces
/// between sub-clauses but must not carry a `CR`/`LF`.
///
/// Per <https://www.aprs-is.net/Connecting.aspx>, the server command permits
/// spaces. This implementation accepts ASCII and rejects every control byte.
fn validate_filter(value: &str) -> Result<(), AprsIsError> {
    if !value.is_ascii() {
        return Err(AprsIsError::InvalidLoginField {
            field: LoginField::Filter,
            reason: LoginFieldError::NonAscii,
        });
    }
    if value.bytes().any(|byte| byte.is_ascii_control()) {
        return Err(AprsIsError::InvalidLoginField {
            field: LoginField::Filter,
            reason: LoginFieldError::ControlCharacter,
        });
    }
    Ok(())
}

/// Build the APRS-IS login string.
///
/// Format: `user CALL pass PASSCODE vers SOFTNAME SOFTVER filter FILTER\r\n`
///
/// If the filter is empty, the `filter` clause is omitted.
///
/// The callsign is validated against APRS-IS's ASCII-alphanumeric,
/// optional-SSID, and nine-byte identity grammar. Software name/version are
/// validated as single whitespace-free words, and the filter is checked for
/// embedded `CRLF` before the line is assembled. This is the choke point for
/// the handshake: an unvalidated value could otherwise corrupt the
/// space-delimited login line or inject a second handshake line onto the
/// stream. See <https://www.aprs-is.net/Connecting.aspx> ("Login Format
/// Rules"; software name/version "Do not use spaces").
///
/// # Errors
///
/// Returns [`AprsIsError::InvalidLoginField`] if the callsign violates the
/// APRS-IS identity grammar, the software name/version is empty or contains
/// whitespace/control characters, or the filter contains a `\r`/`\n`.
pub fn build_login_string(config: &AprsIsConfig) -> Result<String, AprsIsError> {
    validate_login_callsign(&config.callsign)?;
    validate_login_word(LoginField::SoftwareName, &config.software_name)?;
    validate_login_word(LoginField::SoftwareVersion, &config.software_version)?;
    validate_filter(&config.filter)?;

    let mut login = format!(
        "user {} pass {} vers {} {}",
        config.callsign, config.passcode, config.software_name, config.software_version,
    );

    if !config.filter.is_empty() {
        login.push_str(" filter ");
        login.push_str(&config.filter);
    }

    login.push_str("\r\n");
    Ok(login)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn passcode_n0call() {
        // Well-known test vector: N0CALL -> 13023
        assert_eq!(aprs_is_passcode("N0CALL"), 13023);
    }

    #[test]
    fn passcode_strips_ssid() {
        assert_eq!(aprs_is_passcode("N0CALL-10"), aprs_is_passcode("N0CALL"));
    }

    #[test]
    fn passcode_case_insensitive() {
        assert_eq!(aprs_is_passcode("n0call"), aprs_is_passcode("N0CALL"));
    }

    #[test]
    fn passcode_is_positive() {
        // The result must always be a nonnegative 15-bit value.
        let code = aprs_is_passcode("W1AW");
        assert!(
            (0..=0x7FFF).contains(&code),
            "passcode out of 15-bit range: {code}"
        );
    }

    #[test]
    fn passcode_odd_length_callsign() {
        // 5-character callsign (odd length).
        let code = aprs_is_passcode("K1ABC");
        assert!(
            (0..=0x7FFF).contains(&code),
            "passcode out of 15-bit range: {code}"
        );
    }

    #[test]
    fn passcode_w1aw() {
        // W1AW -> computed via standard APRS-IS algorithm.
        assert_eq!(aprs_is_passcode("W1AW"), 25988);
    }

    #[test]
    fn config_defaults() -> Result<(), Box<dyn std::error::Error>> {
        let config = AprsIsConfig::new("N0CALL-10")?;
        assert_eq!(config.callsign, "N0CALL-10");
        assert_eq!(config.passcode.as_wire(), 13023);
        assert_eq!(config.server, "rotate.aprs2.net");
        assert_eq!(config.port, 14580);
        assert!(config.filter.is_empty());
        assert_eq!(config.software_name, "aprs-is");
        Ok(())
    }

    #[test]
    fn receive_only_config() -> Result<(), Box<dyn std::error::Error>> {
        let config = AprsIsConfig::receive_only("N0CALL")?;
        assert_eq!(config.passcode, Passcode::ReceiveOnly);
        assert_eq!(config.passcode.as_wire(), -1);
        Ok(())
    }

    #[test]
    fn passcode_for_callsign_matches_hash() {
        assert_eq!(Passcode::for_callsign("N0CALL").as_wire(), 13023);
        assert_eq!(Passcode::for_callsign("W1AW").as_wire(), 25988);
    }

    #[test]
    fn passcode_receive_only_as_wire() {
        assert_eq!(Passcode::ReceiveOnly.as_wire(), -1);
        assert_eq!(
            VerifiedPasscode::new(0).map(VerifiedPasscode::value),
            Some(0)
        );
        assert_eq!(
            VerifiedPasscode::new(32767).map(VerifiedPasscode::value),
            Some(32767)
        );
        assert_eq!(VerifiedPasscode::new(32768), None);
        assert_eq!(VerifiedPasscode::new(u16::MAX), None);
    }

    #[test]
    fn login_string_no_filter() -> Result<(), Box<dyn std::error::Error>> {
        let config = AprsIsConfig::new("N0CALL")?;
        let login = build_login_string(&config)?;
        assert!(
            login.starts_with("user N0CALL pass 13023 vers aprs-is "),
            "unexpected login prefix: {login:?}"
        );
        assert!(login.ends_with("\r\n"), "missing CRLF: {login:?}");
        assert!(
            !login.contains("filter"),
            "filter clause unexpectedly present: {login:?}"
        );
        Ok(())
    }

    #[test]
    fn login_string_with_filter() -> Result<(), Box<dyn std::error::Error>> {
        let config = AprsIsConfig::new("N0CALL")?.with_raw_filter("r/35.25/-97.75/100")?;
        let login = build_login_string(&config)?;
        assert!(
            login.contains("filter r/35.25/-97.75/100"),
            "filter not in login string: {login:?}"
        );
        assert!(login.ends_with("\r\n"), "missing CRLF: {login:?}");
        Ok(())
    }

    #[test]
    fn login_string_rejects_software_name_with_space() -> Result<(), Box<dyn std::error::Error>> {
        // Connecting.html: "softwarename=one word name of the client
        // software. Do not use spaces." A space would shift the
        // space-delimited keyword parsing.
        let result = AprsIsConfig::new("N0CALL")?.with_software("my app", "1");
        assert!(
            matches!(
                result,
                Err(AprsIsError::InvalidLoginField {
                    field: LoginField::SoftwareName,
                    ..
                })
            ),
            "expected InvalidLoginField(software_name), got {result:?}"
        );
        Ok(())
    }

    #[test]
    fn login_string_rejects_software_version_with_space() -> Result<(), Box<dyn std::error::Error>>
    {
        // Connecting.html: "softwarevers=version number of the client
        // software. Do not use spaces."
        let result = AprsIsConfig::new("N0CALL")?.with_software("test", "1 0");
        assert!(
            matches!(
                result,
                Err(AprsIsError::InvalidLoginField {
                    field: LoginField::SoftwareVersion,
                    ..
                })
            ),
            "expected InvalidLoginField(software_version), got {result:?}"
        );
        Ok(())
    }

    #[test]
    fn login_string_rejects_filter_with_crlf() -> Result<(), Box<dyn std::error::Error>> {
        // A `\r\n` in the filter would inject a second handshake line
        // onto the stream after the login line.
        let result = AprsIsConfig::new("N0CALL")?.with_raw_filter("r/1/2/3\r\ninject");
        assert!(
            matches!(
                result,
                Err(AprsIsError::InvalidLoginField {
                    field: LoginField::Filter,
                    ..
                })
            ),
            "expected InvalidLoginField(filter), got {result:?}"
        );
        Ok(())
    }

    #[test]
    fn login_string_rejects_callsign_with_whitespace() {
        // A space/CRLF in the callsign would corrupt the `user CALL`
        // keyword pair.
        let result = AprsIsConfig::new("N0 CALL");
        assert!(
            matches!(
                result,
                Err(AprsIsError::InvalidLoginField {
                    field: LoginField::Callsign,
                    ..
                })
            ),
            "expected InvalidLoginField(callsign), got {result:?}"
        );
    }

    #[test]
    fn login_string_enforces_complete_aprs_is_identity_grammar() {
        for invalid in [
            "",
            "NO",
            "N0_CALL",
            "N0CALL-0",
            "N0CALL-",
            "N0CALL-ABC",
            "N0CALL-A-",
            "N0CALL-1-2",
            "N0CALL1234",
            "NØCALL",
            "N0CALL*",
            "DEADBEEF",
            "deadbeef",
        ] {
            assert!(
                matches!(
                    AprsIsConfig::new(invalid),
                    Err(AprsIsError::InvalidLoginField {
                        field: LoginField::Callsign,
                        ..
                    })
                ),
                "accepted invalid login identity {invalid:?}"
            );
        }
    }

    #[test]
    fn login_string_accepts_case_preserved_two_character_ssid()
    -> Result<(), Box<dyn std::error::Error>> {
        let config = AprsIsConfig::new("ae5pl-TS")?;
        let login = build_login_string(&config)?;
        assert!(login.starts_with("user ae5pl-TS pass "));
        Ok(())
    }

    #[test]
    fn login_string_allows_filter_with_internal_spaces() -> Result<(), Box<dyn std::error::Error>> {
        // Spaces separate clauses in the raw-expression extension.
        let config = AprsIsConfig::new("N0CALL")?.with_raw_filter("m/200 -p/CW")?;
        let login = build_login_string(&config)?;
        assert!(
            login.contains("filter m/200 -p/CW"),
            "multi-clause filter not preserved: {login:?}"
        );
        Ok(())
    }

    #[test]
    fn config_builders_reject_invalid_endpoints_and_complete_login_overflow()
    -> Result<(), Box<dyn std::error::Error>> {
        for invalid in [
            "",
            "host:14580",
            "host/name",
            "bad host",
            "-host",
            "host..name",
            "host\n",
        ] {
            assert!(
                AprsIsConfig::new("N0CALL")?
                    .with_endpoint(invalid, NonZeroU16::MIN)
                    .is_err(),
                "{invalid:?}"
            );
        }
        let config = AprsIsConfig::new("N0CALL")?.with_endpoint("::1", NonZeroU16::MIN)?;
        assert_eq!(config.server(), "::1");
        assert_eq!(config.port(), NonZeroU16::MIN);
        for invalid in ["a\tfilter", "a\0filter", "é"] {
            assert!(config.clone().with_raw_filter(invalid).is_err());
        }
        let prefix_len = build_login_string(&config)?.len() + " filter ".len();
        let maximum = config
            .clone()
            .with_raw_filter("x".repeat(crate::MAX_IS_LINE_BYTES - prefix_len))?;
        assert_eq!(
            build_login_string(&maximum)?.len(),
            crate::MAX_IS_LINE_BYTES
        );
        let result = config.with_raw_filter("x".repeat(crate::MAX_IS_LINE_BYTES - prefix_len + 1));
        assert!(matches!(
            result,
            Err(AprsIsError::InvalidUplinkLine(
                crate::AprsIsUplinkLineError::TooLong { .. }
            ))
        ));
        Ok(())
    }
}
