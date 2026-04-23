//! Parser for D-STAR callsign list `.tsv` files.
//!
//! The callsign list is a UTF-16LE encoded, tab-separated file with a
//! BOM (`FF FE`). It stores D-STAR destination callsigns (URCALL
//! addresses) used for direct calling.
//!
//! # Location
//!
//! `/KENWOOD/TH-D75/SETTINGS/CALLSIGN_LIST/*.tsv`
//!
//! # Capacity
//!
//! Up to 120 entries.

use super::SdCardError;

/// A single D-STAR destination callsign entry.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CallsignEntry {
    /// D-STAR destination callsign (URCALL), 8 chars, space-padded.
    pub callsign: String,
}

/// Parses a callsign list TSV file from raw bytes.
///
/// Expects UTF-16LE encoding with a BOM prefix (`FF FE`). The first
/// line is treated as a column header and is skipped. Each subsequent
/// line contains at least one column (the callsign).
///
/// # Errors
///
/// Returns an [`SdCardError`] if the encoding is invalid or a data
/// row has no columns.
pub fn parse_callsign_list(data: &[u8]) -> Result<Vec<CallsignEntry>, SdCardError> {
    let text = decode_utf16le_bom(data)?;
    let mut entries = Vec::new();

    for (line_idx, line) in text.lines().enumerate() {
        // Skip header row and blank lines.
        if line_idx == 0 || line.trim().is_empty() {
            continue;
        }

        let line_num = line_idx + 1;
        let callsign = line
            .split('\t')
            .next()
            .ok_or(SdCardError::ColumnCount {
                line: line_num,
                expected: 1,
                actual: 0,
            })?
            .to_owned();

        // Skip the D-STAR broadcast CQ address — it is always implicit.
        if callsign.trim() == "CQCQCQ" {
            continue;
        }

        entries.push(CallsignEntry { callsign });
    }

    Ok(entries)
}

/// Generates a callsign list TSV file as UTF-16LE bytes with BOM.
///
/// The output includes a header row followed by one row per entry.
#[must_use]
pub fn write_callsign_list(entries: &[CallsignEntry]) -> Vec<u8> {
    let mut text = String::new();

    // Header row
    text.push_str("Callsign\r\n");

    // Data rows
    for entry in entries {
        text.push_str(&entry.callsign);
        text.push_str("\r\n");
    }

    encode_utf16le_bom(&text)
}

/// Decodes a UTF-16LE byte sequence with a leading BOM into a `String`.
fn decode_utf16le_bom(data: &[u8]) -> Result<String, SdCardError> {
    let Some((bom, payload)) = data.split_first_chunk::<2>() else {
        return Err(SdCardError::MissingBom);
    };
    if *bom != [0xFF, 0xFE] {
        return Err(SdCardError::MissingBom);
    }

    if !payload.len().is_multiple_of(2) {
        return Err(SdCardError::InvalidUtf16Length { len: payload.len() });
    }

    let code_units: Vec<u16> = payload
        .chunks_exact(2)
        .map(|pair| u16::from_le_bytes(pair.try_into().unwrap_or([0, 0])))
        .collect();

    String::from_utf16(&code_units).map_err(|e| SdCardError::Utf16Decode {
        detail: e.to_string(),
    })
}

/// Encodes a string as UTF-16LE bytes with a leading BOM.
fn encode_utf16le_bom(text: &str) -> Vec<u8> {
    let mut out = Vec::with_capacity(2 + text.len() * 2);
    // BOM
    out.push(0xFF);
    out.push(0xFE);
    // UTF-16LE payload
    for unit in text.encode_utf16() {
        out.extend_from_slice(&unit.to_le_bytes());
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    type TestResult = Result<(), Box<dyn std::error::Error>>;

    #[test]
    fn parse_empty_list() -> TestResult {
        let data = encode_utf16le_bom("Callsign\r\n");
        let entries = parse_callsign_list(&data)?;
        assert!(entries.is_empty());
        Ok(())
    }

    #[test]
    fn parse_filters_cqcqcq() -> TestResult {
        let data = encode_utf16le_bom("Callsign\r\nCQCQCQ  \r\nW4CDR   \r\n");
        let entries = parse_callsign_list(&data)?;
        // CQCQCQ (with trailing spaces trimmed) should be filtered out.
        assert_eq!(entries.len(), 1);
        let first = entries.first().ok_or("expected one entry")?;
        assert_eq!(first.callsign, "W4CDR   ");
        Ok(())
    }
}
