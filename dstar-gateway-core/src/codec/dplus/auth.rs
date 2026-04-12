//! `DPlus` auth chunk parser.
//!
//! Parses the framed TCP response from `auth.dstargateway.org:20001`
//! into a list of known reflector callsigns and IP addresses.
//!
//! Reference: `ircDDBGateway/Common/DPlusAuthenticator.cpp:151-192`.

use std::net::IpAddr;

use crate::validator::{AuthHostSkipReason, Diagnostic, DiagnosticSink};

use super::error::DPlusError;

const CHUNK_HEADER_SIZE: usize = 8;
const RECORD_SIZE: usize = 26;
const IP_FIELD_SIZE: usize = 16;
const CALLSIGN_FIELD_SIZE: usize = 8;

/// A single entry from the `DPlus` auth server's host list response.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DPlusHost {
    /// Reflector callsign (e.g. `"REF030"`), trimmed of trailing spaces.
    pub callsign: String,
    /// Reflector IPv4 address.
    pub address: IpAddr,
}

/// Parsed host list from the `DPlus` auth TCP response.
#[derive(Debug, Clone, Default)]
pub struct HostList {
    hosts: Vec<DPlusHost>,
}

impl HostList {
    /// Create an empty host list.
    #[must_use]
    pub const fn new() -> Self {
        Self { hosts: Vec::new() }
    }

    /// Return a slice of all parsed hosts.
    #[must_use]
    pub fn hosts(&self) -> &[DPlusHost] {
        &self.hosts
    }

    /// Number of hosts in the list.
    #[must_use]
    pub const fn len(&self) -> usize {
        self.hosts.len()
    }

    /// True if the list is empty.
    #[must_use]
    pub const fn is_empty(&self) -> bool {
        self.hosts.is_empty()
    }

    /// Look up a host by callsign (case-insensitive).
    #[must_use]
    pub fn find(&self, callsign: &str) -> Option<&DPlusHost> {
        self.hosts
            .iter()
            .find(|h| h.callsign.eq_ignore_ascii_case(callsign))
    }
}

/// Parse a `DPlus` auth TCP response into a [`HostList`].
///
/// Lenient parser: malformed records are skipped with a diagnostic.
/// Fatal errors (truncated chunk header, invalid flags, invalid type
/// byte, undersized chunk length) return `Err`.
///
/// # Errors
///
/// Returns `DPlusError::AuthChunk*` variants for fatal format errors.
pub fn parse_auth_response(
    data: &[u8],
    sink: &mut dyn DiagnosticSink,
) -> Result<HostList, DPlusError> {
    let mut hosts = Vec::new();
    let mut cursor = 0usize;

    while cursor < data.len() {
        let chunk_len = validate_chunk_header(data, cursor)?;
        let chunk_end = cursor + chunk_len;
        let chunk = data.get(cursor..chunk_end).unwrap_or(&[]);
        parse_chunk_records(chunk, cursor, &mut hosts, sink);
        cursor = chunk_end;
    }

    Ok(HostList { hosts })
}

/// Validate a chunk header at `cursor` and return the declared chunk length.
fn validate_chunk_header(data: &[u8], cursor: usize) -> Result<usize, DPlusError> {
    let remaining = data.len() - cursor;
    if remaining < 3 {
        return Err(DPlusError::AuthChunkTruncated {
            offset: cursor,
            need: 3,
            have: remaining,
        });
    }

    let b0 = *data.get(cursor).unwrap_or(&0);
    let b1 = *data.get(cursor + 1).unwrap_or(&0);
    let b2 = *data.get(cursor + 2).unwrap_or(&0);

    let chunk_len = (usize::from(b1 & 0x0F) * 256) + usize::from(b0);

    if (b1 & 0xC0) != 0xC0 {
        return Err(DPlusError::AuthChunkFlagsInvalid {
            offset: cursor,
            byte: b1,
        });
    }
    if b2 != 0x01 {
        return Err(DPlusError::AuthChunkTypeInvalid {
            offset: cursor,
            byte: b2,
        });
    }
    if chunk_len < CHUNK_HEADER_SIZE {
        return Err(DPlusError::AuthChunkUndersized {
            offset: cursor,
            claimed: chunk_len,
        });
    }
    if cursor + chunk_len > data.len() {
        return Err(DPlusError::AuthChunkTruncated {
            offset: cursor,
            need: chunk_len,
            have: data.len() - cursor,
        });
    }
    Ok(chunk_len)
}

/// Walk records in a single chunk (skipping the 8-byte header), filtering
/// and appending to `hosts`. Malformed records emit a diagnostic and are
/// skipped. Trailing non-record bytes also emit a diagnostic.
fn parse_chunk_records(
    chunk: &[u8],
    chunk_offset: usize,
    hosts: &mut Vec<DPlusHost>,
    sink: &mut dyn DiagnosticSink,
) {
    let mut i = CHUNK_HEADER_SIZE;
    while i + RECORD_SIZE <= chunk.len() {
        let record_offset = chunk_offset + i;
        let record = chunk.get(i..i + RECORD_SIZE).unwrap_or(&[]);
        i += RECORD_SIZE;

        if let Some(host) = parse_record(record, record_offset, sink) {
            hosts.push(host);
        }
    }

    // Trailing bytes inside this chunk that didn't form a complete record.
    let leftover = chunk.len() - i;
    if leftover > 0 {
        sink.record(Diagnostic::AuthChunkTrailingBytes {
            offset: chunk_offset + i,
            bytes: leftover,
        });
    }
}

/// Parse a single 26-byte record into a [`DPlusHost`] or skip it via a
/// diagnostic.
fn parse_record(
    record: &[u8],
    record_offset: usize,
    sink: &mut dyn DiagnosticSink,
) -> Option<DPlusHost> {
    let ip_bytes = record.get(..IP_FIELD_SIZE).unwrap_or(&[]);
    let callsign_bytes = record
        .get(IP_FIELD_SIZE..IP_FIELD_SIZE + CALLSIGN_FIELD_SIZE)
        .unwrap_or(&[]);
    let active_byte = record.get(25).copied().unwrap_or(0);
    let active = (active_byte & 0x80) == 0x80;

    let ip_str = std::str::from_utf8(ip_bytes)
        .unwrap_or("")
        .trim_matches(['\0', ' ']);
    let callsign_str = std::str::from_utf8(callsign_bytes)
        .unwrap_or("")
        .trim_matches(['\0', ' ']);

    if !active {
        sink.record(Diagnostic::AuthHostSkipped {
            offset: record_offset,
            reason: AuthHostSkipReason::Inactive,
        });
        return None;
    }
    if ip_str.is_empty() {
        sink.record(Diagnostic::AuthHostSkipped {
            offset: record_offset,
            reason: AuthHostSkipReason::EmptyIp,
        });
        return None;
    }
    if callsign_str.is_empty() {
        sink.record(Diagnostic::AuthHostSkipped {
            offset: record_offset,
            reason: AuthHostSkipReason::EmptyCallsign,
        });
        return None;
    }
    if callsign_str.starts_with("XRF") {
        sink.record(Diagnostic::AuthHostSkipped {
            offset: record_offset,
            reason: AuthHostSkipReason::XrfPrefix,
        });
        return None;
    }

    let Ok(address) = ip_str.parse::<IpAddr>() else {
        sink.record(Diagnostic::AuthHostSkipped {
            offset: record_offset,
            reason: AuthHostSkipReason::MalformedIp,
        });
        return None;
    };

    Some(DPlusHost {
        callsign: callsign_str.to_owned(),
        address,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::validator::VecSink;

    /// Build one chunk wrapping the given host records.
    fn build_chunk(records: &[[u8; 26]]) -> Vec<u8> {
        let body_len = 8 + records.len() * 26;
        assert!(body_len <= 0x0FFF);
        let mut chunk = Vec::with_capacity(body_len);
        #[expect(clippy::cast_possible_truncation, reason = "mask guarantees 0..=255")]
        let lo = (body_len & 0xFF) as u8;
        #[expect(clippy::cast_possible_truncation, reason = "mask guarantees 0..=15")]
        let hi = ((body_len >> 8) & 0x0F) as u8;
        chunk.push(lo);
        chunk.push(0xC0 | hi);
        chunk.push(0x01);
        chunk.extend_from_slice(&[0u8; 5]);
        for r in records {
            chunk.extend_from_slice(r);
        }
        assert_eq!(chunk.len(), body_len);
        chunk
    }

    /// Build one 26-byte host record: space-padded ASCII IP, space-padded
    /// callsign, module byte 0, active flag set.
    fn build_record(ip: &str, call: &str) -> Result<[u8; 26], Box<dyn std::error::Error>> {
        let mut rec = [b' '; 26];
        let ip_bytes = ip.as_bytes();
        assert!(ip_bytes.len() <= 16);
        rec.get_mut(..ip_bytes.len())
            .ok_or("ip_bytes within rec")?
            .copy_from_slice(ip_bytes);
        let cs_bytes = call.as_bytes();
        assert!(cs_bytes.len() <= 8);
        rec.get_mut(16..16 + cs_bytes.len())
            .ok_or("cs_bytes within rec")?
            .copy_from_slice(cs_bytes);
        rec[24] = 0;
        rec[25] = 0x80; // active
        Ok(rec)
    }

    type TestResult = Result<(), Box<dyn std::error::Error>>;

    #[test]
    fn empty_input_returns_empty_list() -> TestResult {
        let mut sink = VecSink::default();
        let list = parse_auth_response(&[], &mut sink)?;
        assert_eq!(list.len(), 0);
        Ok(())
    }

    #[test]
    fn single_record_parses() -> TestResult {
        let rec = build_record("192.168.1.1", "REF030")?;
        let data = build_chunk(&[rec]);
        let mut sink = VecSink::default();
        let list = parse_auth_response(&data, &mut sink)?;
        assert_eq!(list.len(), 1);
        assert_eq!(
            list.hosts()
                .first()
                .ok_or("expected at least one host")?
                .callsign,
            "REF030"
        );
        Ok(())
    }

    #[test]
    fn inactive_record_is_skipped_with_diagnostic() -> TestResult {
        let mut inactive = build_record("192.168.1.1", "REF030")?;
        inactive[25] = 0x00; // clear active bit
        let data = build_chunk(&[inactive]);
        let mut sink = VecSink::default();
        let list = parse_auth_response(&data, &mut sink)?;
        assert_eq!(list.len(), 0);
        assert_eq!(sink.len(), 1);
        Ok(())
    }

    #[test]
    fn xrf_prefix_is_skipped() -> TestResult {
        let rec = build_record("192.168.1.1", "XRF030")?;
        let data = build_chunk(&[rec]);
        let mut sink = VecSink::default();
        let list = parse_auth_response(&data, &mut sink)?;
        assert_eq!(list.len(), 0);
        assert_eq!(sink.len(), 1);
        Ok(())
    }

    #[test]
    fn malformed_ip_is_skipped_with_diagnostic() -> TestResult {
        let rec = build_record("notanipaddr", "REF030")?;
        let data = build_chunk(&[rec]);
        let mut sink = VecSink::default();
        let list = parse_auth_response(&data, &mut sink)?;
        assert_eq!(list.len(), 0);
        assert_eq!(sink.len(), 1);
        Ok(())
    }

    #[test]
    fn invalid_flags_returns_error() -> TestResult {
        let mut data = build_chunk(&[build_record("10.0.0.1", "REF001")?]);
        *data.get_mut(1).ok_or("index 1 within data")? = 0x80; // corrupt flags (must have top two bits set)
        let mut sink = VecSink::default();
        let Err(err) = parse_auth_response(&data, &mut sink) else {
            return Err("expected error for invalid flags".into());
        };
        assert!(matches!(err, DPlusError::AuthChunkFlagsInvalid { .. }));
        Ok(())
    }

    #[test]
    fn invalid_type_byte_returns_error() -> TestResult {
        let mut data = build_chunk(&[build_record("10.0.0.1", "REF001")?]);
        *data.get_mut(2).ok_or("index 2 within data")? = 0x02;
        let mut sink = VecSink::default();
        let Err(err) = parse_auth_response(&data, &mut sink) else {
            return Err("expected error for invalid type".into());
        };
        assert!(matches!(err, DPlusError::AuthChunkTypeInvalid { .. }));
        Ok(())
    }

    #[test]
    fn truncated_chunk_returns_error() -> TestResult {
        let full = build_chunk(&[build_record("10.0.0.1", "REF001")?]);
        let truncated = full
            .get(..full.len() - 1)
            .ok_or("truncated slice within full")?;
        let mut sink = VecSink::default();
        let Err(err) = parse_auth_response(truncated, &mut sink) else {
            return Err("expected error for truncated chunk".into());
        };
        assert!(matches!(err, DPlusError::AuthChunkTruncated { .. }));
        Ok(())
    }

    #[test]
    fn case_insensitive_lookup() -> TestResult {
        let rec = build_record("10.0.0.1", "REF030")?;
        let data = build_chunk(&[rec]);
        let mut sink = VecSink::default();
        let list = parse_auth_response(&data, &mut sink)?;
        assert!(list.find("ref030").is_some());
        assert!(list.find("Ref030").is_some());
        assert!(list.find("XYZ999").is_none());
        Ok(())
    }
}
