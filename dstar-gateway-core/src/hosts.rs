//! D-STAR reflector host file parser.
//!
//! Host files map reflector names (e.g. "REF030", "XRF012", "DCS003")
//! to their network addresses and ports. The format is one entry per
//! line: `name address port` or `name address` (Pi-Star format).
//!
//! Lines starting with `#` are comments. Empty lines are skipped.
//!
//! Host lists come from the XLX self-registration registry
//! (`xlxapi.rlx.lu`) and the `DPlus` auth server; see
//! [`parse_xlx_directory`] for the registry format.

use std::collections::HashMap;
use std::num::{NonZeroU16, ParseIntError};

use crate::types::ProtocolKind;

pub use address::{HostAddress, HostAddressError};
pub use name::{HostName, HostNameError};

mod address;
mod name;

/// Validated catalog name, unresolved network address, and nonzero UDP port.
///
/// Construction and host-file parsing perform no resolution or I/O.
/// See the [host-file example](HostFile).
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct HostEntry {
    name: HostName,
    address: HostAddress,
    port: NonZeroU16,
}

impl HostEntry {
    /// Combine independently validated host-file fields.
    #[must_use]
    pub const fn new(name: HostName, address: HostAddress, port: NonZeroU16) -> Self {
        Self {
            name,
            address,
            port,
        }
    }

    /// Canonical case-insensitive catalog name.
    #[must_use]
    pub const fn name(&self) -> &HostName {
        &self.name
    }

    /// Validated DNS hostname or unscoped IP literal.
    #[must_use]
    pub const fn address(&self) -> &HostAddress {
        &self.address
    }

    /// Nonzero destination UDP port.
    #[must_use]
    pub const fn port(&self) -> NonZeroU16 {
        self.port
    }
}

/// Collection of host file entries keyed by reflector name.
///
/// Lookups are case-insensitive: the query is upper-cased before
/// the `HashMap` lookup. Every insertion likewise upper-cases the name.
/// Duplicate names use last-wins semantics.
///
/// ```
/// use std::num::NonZeroU16;
/// use dstar_gateway_core::hosts::{HostAddress, HostEntry, HostFile, HostName};
///
/// # fn example() -> Result<(), Box<dyn std::error::Error>> {
/// let port = NonZeroU16::new(20001).ok_or("nonzero protocol port")?;
/// let mut hosts = HostFile::new();
/// hosts.parse("REF030 reflector.example 20001\n", port)?;
/// let replacement = HostEntry::new(
///     HostName::new("ref030")?, HostAddress::new("[2001:db8::1]")?, port,
/// );
/// hosts.extend([replacement]);
/// assert_eq!(hosts.lookup("Ref030").ok_or("configured host")?.address().to_string(), "2001:db8::1");
/// assert_eq!((&hosts).into_iter().count(), 1);
/// assert!(hosts.parse("REF031 other.example invalid\n", port).is_err());
/// assert_eq!(hosts.len(), 1); // A malformed file changes no existing entry.
/// # Ok(())
/// # }
/// ```
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct HostFile {
    entries: HashMap<String, HostEntry>,
}

impl HostFile {
    /// Create an empty host file.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Parse host entries from text content.
    ///
    /// Supports two formats:
    /// - 3 columns: `name address port`
    /// - 2 columns: `name address` (port from `default_port`)
    ///
    /// `#` begins a comment; empty lines are skipped. Every nonempty line must
    /// have exactly two or three fields, with a decimal nonzero port when
    /// supplied. All lines validate before insertion, so an invalid file
    /// preserves the entire previous collection. A valid file replaces matching
    /// names and keeps names absent from the file.
    ///
    /// # Errors
    ///
    /// Returns [`HostFileError`] with the one-based line number and typed field
    /// cause. Malformed fields are never replaced by a default or skipped.
    pub fn parse(&mut self, content: &str, default_port: NonZeroU16) -> Result<(), HostFileError> {
        let mut parsed = Self::new();
        for (index, line) in content.lines().enumerate() {
            if let Some(entry) = parse_line(line, default_port).map_err(|error| HostFileError {
                line: index + 1,
                error,
            })? {
                parsed.insert(entry);
            }
        }
        self.extend(parsed);
        Ok(())
    }

    /// Look up an entry by name (case-insensitive).
    #[must_use]
    pub fn lookup(&self, name: &str) -> Option<&HostEntry> {
        self.entries.get(&name.to_ascii_uppercase())
    }

    /// Insert an entry whose name is already canonicalized at construction.
    ///
    /// Replaces any entry with the same case-insensitive name.
    pub fn insert(&mut self, entry: HostEntry) {
        drop(self.entries.insert(entry.name.as_str().to_owned(), entry));
    }

    /// Number of entries in the host file.
    #[must_use]
    pub fn len(&self) -> usize {
        self.entries.len()
    }

    /// True if the host file has zero entries.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    /// Iterate all entries.
    pub fn iter(&self) -> impl Iterator<Item = &HostEntry> + '_ {
        self.entries.values()
    }
}

impl<'a> IntoIterator for &'a HostFile {
    type Item = &'a HostEntry;
    type IntoIter = std::collections::hash_map::Values<'a, String, HostEntry>;

    fn into_iter(self) -> Self::IntoIter {
        self.entries.values()
    }
}

impl IntoIterator for HostFile {
    type Item = HostEntry;
    type IntoIter = std::collections::hash_map::IntoValues<String, HostEntry>;

    fn into_iter(self) -> Self::IntoIter {
        self.entries.into_values()
    }
}

impl Extend<HostEntry> for HostFile {
    fn extend<T: IntoIterator<Item = HostEntry>>(&mut self, entries: T) {
        for entry in entries {
            self.insert(entry);
        }
    }
}

impl FromIterator<HostEntry> for HostFile {
    fn from_iter<T: IntoIterator<Item = HostEntry>>(entries: T) -> Self {
        let mut hosts = Self::new();
        hosts.extend(entries);
        hosts
    }
}

/// Default UDP port for `DPlus` (`REF`) reflectors.
const XLX_DPLUS_PORT: NonZeroU16 = NonZeroU16::new(20001).unwrap();
/// Default UDP port for `DExtra` (`XRF`) reflectors.
const XLX_DEXTRA_PORT: NonZeroU16 = NonZeroU16::new(30001).unwrap();
/// Default UDP port for `DCS` reflectors.
const XLX_DCS_PORT: NonZeroU16 = NonZeroU16::new(30051).unwrap();

/// Parse the XLX reflector directory into protocol-tagged host entries.
///
/// This is the `xlxapi.rlx.lu/api.php?do=GetReflectorHostname` format:
/// one `name<whitespace>address` entry per line, grouped under `#`
/// comment headers. The same reflector appears once per protocol
/// prefix (`REF`, `XRF`, `DCS`), all pointing at one address; those
/// prefixes are protocols, not separate networks.
///
/// Protocol family and UDP port are derived from the name prefix.
/// Lines with an unrecognised prefix, comments (`#`), and blank lines are
/// skipped. A recognized prefix requires a complete valid host entry.
///
/// # Errors
///
/// Returns [`HostFileError`] for a malformed recognized entry, discarding the
/// partial result and preserving the source line number.
pub fn parse_xlx_directory(content: &str) -> Result<Vec<(ProtocolKind, HostEntry)>, HostFileError> {
    let mut out = Vec::new();
    for (index, line) in content.lines().enumerate() {
        let line = line.split_once('#').map_or(line, |(data, _)| data).trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let Some(name_raw) = line.split_whitespace().next() else {
            continue;
        };
        let name = name_raw.to_ascii_uppercase();
        let (protocol, port) = if name.starts_with("REF") {
            (ProtocolKind::DPlus, XLX_DPLUS_PORT)
        } else if name.starts_with("XRF") {
            (ProtocolKind::DExtra, XLX_DEXTRA_PORT)
        } else if name.starts_with("DCS") {
            (ProtocolKind::Dcs, XLX_DCS_PORT)
        } else {
            continue;
        };
        if let Some(entry) = parse_line(line, port).map_err(|error| HostFileError {
            line: index + 1,
            error,
        })? {
            out.push((protocol, entry));
        }
    }
    Ok(out)
}

/// Parse one complete line, retaining comments and blank lines as no entry.
fn parse_line(line: &str, default_port: NonZeroU16) -> Result<Option<HostEntry>, HostEntryError> {
    let line = line.split_once('#').map_or(line, |(data, _)| data).trim();
    if line.is_empty() {
        return Ok(None);
    }
    let mut parts = line.split_whitespace();
    let (Some(name), Some(address)) = (parts.next(), parts.next()) else {
        return Err(HostEntryError::InvalidFieldCount {
            got: line.split_whitespace().count(),
        });
    };
    let port = parts.next().map_or(Ok(default_port), |port| {
        if !port.bytes().all(|byte| byte.is_ascii_digit()) {
            return Err(HostEntryError::InvalidPortToken {
                token: port.to_owned(),
            });
        }
        port.parse::<NonZeroU16>().map_err(HostEntryError::Port)
    })?;
    if parts.next().is_some() {
        return Err(HostEntryError::InvalidFieldCount {
            got: line.split_whitespace().count(),
        });
    }
    Ok(Some(HostEntry::new(
        HostName::new(name)?,
        HostAddress::new(address)?,
        port,
    )))
}

/// A host-file line that cannot produce a validated entry.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("host-file line {line}: {error}")]
pub struct HostFileError {
    line: usize,
    #[source]
    error: HostEntryError,
}

impl HostFileError {
    /// One-based source line number.
    #[must_use]
    pub const fn line(&self) -> usize {
        self.line
    }

    /// Typed field or line-grammar failure.
    #[must_use]
    pub const fn error(&self) -> &HostEntryError {
        &self.error
    }
}

/// Field and grammar failures in a recognized host-file entry.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[non_exhaustive]
pub enum HostEntryError {
    /// A nonempty line has a field count other than two or three.
    #[error("expected name, address and optional port; found {got} fields")]
    InvalidFieldCount {
        /// Number of supplied whitespace-separated fields.
        got: usize,
    },
    /// A name fails the catalog-token grammar.
    #[error(transparent)]
    Name(#[from] HostNameError),
    /// An address fails DNS hostname or IP-literal syntax.
    #[error(transparent)]
    Address(#[from] HostAddressError),
    /// A port token contains a non-decimal byte.
    #[error("port is not an unsigned decimal integer: {token:?}")]
    InvalidPortToken {
        /// Original supplied token.
        token: String,
    },
    /// A decimal port is zero or exceeds the sixteen-bit UDP port domain.
    #[error("invalid nonzero UDP port: {0}")]
    Port(#[source] ParseIntError),
}

#[cfg(test)]
mod tests {
    use super::*;

    type TestResult = Result<(), Box<dyn std::error::Error>>;

    fn entry(name: &str, address: &str, port: NonZeroU16) -> Result<HostEntry, HostEntryError> {
        Ok(HostEntry::new(
            HostName::new(name)?,
            HostAddress::new(address)?,
            port,
        ))
    }

    #[test]
    fn direct_and_collection_insertion_share_case_insensitive_replacement() -> TestResult {
        let mut hosts: HostFile =
            std::iter::once(entry("ref030", "first.example", XLX_DPLUS_PORT)?).collect();
        hosts.extend([entry(
            "Ref030",
            "second.example",
            NonZeroU16::new(20002).ok_or("port")?,
        )?]);
        assert_eq!(hosts.len(), 1, "case variants replace the same entry");
        let entry = hosts.lookup("ref030").ok_or("normalized entry missing")?;
        assert_eq!(entry.name().as_str(), "REF030");
        assert_eq!(entry.address().dns_name(), Some("second.example"));
        assert_eq!((&hosts).into_iter().count(), 1);
        assert_eq!(hosts.into_iter().count(), 1);
        Ok(())
    }

    #[test]
    fn parse_three_column_format() -> TestResult {
        let mut hf = HostFile::new();
        hf.parse("REF001 ref001.dstargateway.org 20001\n", XLX_DPLUS_PORT)?;
        let entry = hf.lookup("REF001").ok_or("REF001 present")?;
        assert_eq!(entry.name().as_str(), "REF001");
        assert_eq!(entry.address().dns_name(), Some("ref001.dstargateway.org"));
        assert_eq!(entry.port().get(), 20001);
        Ok(())
    }

    #[test]
    fn parse_two_column_format_uses_default_port() -> TestResult {
        let mut hf = HostFile::new();
        hf.parse("XRF012 xrf012.dstar.su\n", XLX_DEXTRA_PORT)?;
        let entry = hf.lookup("XRF012").ok_or("XRF012 present")?;
        assert_eq!(entry.port().get(), 30001);
        Ok(())
    }

    #[test]
    fn parse_skips_comments() -> TestResult {
        let mut hf = HostFile::new();
        hf.parse(
            "# this is a comment\nREF001 a 20001 # trailing comment\n",
            XLX_DPLUS_PORT,
        )?;
        assert!(hf.lookup("REF001").is_some());
        assert_eq!(hf.len(), 1);
        Ok(())
    }

    #[test]
    fn parse_skips_empty_lines() -> TestResult {
        let mut hf = HostFile::new();
        hf.parse("\n\nREF001 a 20001\n\n", XLX_DPLUS_PORT)?;
        assert_eq!(hf.len(), 1);
        Ok(())
    }

    #[test]
    fn lookup_is_case_insensitive() -> TestResult {
        let mut hf = HostFile::new();
        hf.parse("REF001 a 20001\n", XLX_DPLUS_PORT)?;
        assert!(hf.lookup("ref001").is_some());
        assert!(hf.lookup("Ref001").is_some());
        Ok(())
    }

    #[test]
    fn parse_duplicates_last_wins() -> TestResult {
        let mut hf = HostFile::new();
        hf.parse("REF001 first 20001\nREF001 second 20002\n", XLX_DPLUS_PORT)?;
        let entry = hf.lookup("REF001").ok_or("REF001 present")?;
        assert_eq!(entry.address().dns_name(), Some("second"));
        Ok(())
    }

    #[test]
    fn malformed_file_preserves_previous_entries_and_rejects_port_fallback() -> TestResult {
        let mut hf = HostFile::new();
        hf.parse("REF001 original.example 20001\n", XLX_DPLUS_PORT)?;
        let result = hf.parse(
            "REF001 replacement.example 20002\nREF002 other.example notaport\n",
            XLX_DPLUS_PORT,
        );
        assert!(
            matches!(result, Err(ref error) if error.line() == 2
            && matches!(error.error(), HostEntryError::InvalidPortToken { token } if token == "notaport")),
            "malformed ports must identify their line instead of choosing a different endpoint: {result:?}"
        );
        let entry = hf.lookup("REF001").ok_or("REF001 present")?;
        assert_eq!(
            entry.address().dns_name(),
            Some("original.example"),
            "a valid earlier replacement in the same invalid file must not commit"
        );
        assert!(
            hf.lookup("REF002").is_none(),
            "an invalid new entry must not appear"
        );
        for invalid in [
            "REF001",
            "REF001 host 0",
            "REF001 host 65536",
            "REF001 host 20001 extra",
            "REF001 host:20001 20001",
        ] {
            assert!(
                hf.parse(invalid, XLX_DPLUS_PORT).is_err(),
                "invalid file accepted: {invalid:?}"
            );
            assert_eq!(
                hf.lookup("REF001")
                    .ok_or("original entry")?
                    .address()
                    .dns_name(),
                Some("original.example")
            );
        }
        Ok(())
    }

    #[test]
    fn parse_short_line_is_an_explicit_grammar_error() {
        let mut hf = HostFile::new();
        let result = hf.parse("REF001\n", XLX_DPLUS_PORT);
        assert!(
            matches!(result, Err(ref error) if error.line() == 1
            && matches!(error.error(), HostEntryError::InvalidFieldCount { got: 1 })),
            "a recognized incomplete configuration must fail: {result:?}"
        );
        assert_eq!(hf.len(), 0);
    }

    #[test]
    fn xlx_directory_routes_prefix_to_protocol_and_port() -> TestResult {
        let sample = "\
# XLX live Reflector host file.
#
# XLX-DCS Reflectors
#
DCS030\t185.230.132.103

# XLX-XRF Reflectors
XRF030\t185.230.132.103

# XLX-REF Reflectors
REF030\t185.230.132.103
";
        let parsed = parse_xlx_directory(sample)?;
        assert_eq!(parsed.len(), 3, "three reflectors parsed, got {parsed:?}");
        assert!(parsed.contains(&(
            ProtocolKind::Dcs,
            entry("DCS030", "185.230.132.103", XLX_DCS_PORT)?,
        )));
        assert!(parsed.contains(&(
            ProtocolKind::DExtra,
            entry("XRF030", "185.230.132.103", XLX_DEXTRA_PORT)?,
        )));
        assert!(parsed.contains(&(
            ProtocolKind::DPlus,
            entry("REF030", "185.230.132.103", XLX_DPLUS_PORT)?,
        )));
        Ok(())
    }

    #[test]
    fn xlx_directory_skips_unknown_prefix_but_rejects_incomplete_known_entries() -> TestResult {
        let sample = "XLX307\t1.2.3.4\nDCS001\t9.9.9.9\n# trailer\n";
        let parsed = parse_xlx_directory(sample)?;
        assert_eq!(
            parsed.len(),
            1,
            "only DCS001 is a valid 2-field known-prefix line"
        );
        let entry = parsed.first().ok_or("one entry parsed")?;
        assert_eq!(entry.1.name().as_str(), "DCS001");
        let invalid = parse_xlx_directory("DCS001 example\nREF001\n");
        assert!(
            matches!(invalid, Err(ref error) if error.line() == 2
            && matches!(error.error(), HostEntryError::InvalidFieldCount { got: 1 })),
            "a recognized incomplete registry line cannot be silently dropped: {invalid:?}"
        );
        Ok(())
    }
}
