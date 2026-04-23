//! Parser for D-STAR repeater list `.tsv` files.
//!
//! The repeater list is a UTF-16LE encoded, tab-separated file with a
//! BOM (`FF FE`). It contains the D-STAR repeater directory used for
//! DR (D-STAR Repeater) mode operation.
//!
//! # Location
//!
//! `/KENWOOD/TH-D75/SETTINGS/RPT_LIST/*.tsv`
//!
//! # Capacity
//!
//! Up to 1500 repeater entries.

use super::SdCardError;

/// Number of expected columns in the repeater list TSV.
const EXPECTED_COLUMNS: usize = 8;

/// A single D-STAR repeater directory entry.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RepeaterEntry {
    /// Repeater group or region name (e.g., `"Japan"`).
    pub group_name: String,
    /// Repeater name or description.
    pub name: String,
    /// Sub-name or area description.
    pub sub_name: String,
    /// RPT1 callsign (D-STAR 8-char, space-padded, e.g., `"JR6YPR A"`).
    pub callsign_rpt1: String,
    /// RPT2/gateway callsign (D-STAR 8-char, space-padded, e.g., `"JR6YPR G"`).
    pub callsign_rpt2: String,
    /// Operating frequency in Hz.
    pub frequency: u32,
    /// Duplex direction (`"+"`, `"-"`, or empty for simplex).
    pub duplex: String,
    /// TX offset frequency in Hz.
    pub offset: u32,
}

/// Parses a repeater list TSV file from raw bytes.
///
/// Expects UTF-16LE encoding with a BOM prefix (`FF FE`). The first
/// line is treated as a column header and is skipped.
///
/// # Errors
///
/// Returns an [`SdCardError`] if the encoding is invalid or any data
/// row has an unexpected column count.
pub fn parse_repeater_list(data: &[u8]) -> Result<Vec<RepeaterEntry>, SdCardError> {
    let text = decode_utf16le_bom(data)?;
    let mut entries = Vec::new();

    for (line_idx, line) in text.lines().enumerate() {
        // Skip header row and blank lines.
        if line_idx == 0 || line.trim().is_empty() {
            continue;
        }

        let line_num = line_idx + 1;
        let cols: Vec<&str> = line.split('\t').collect();
        let actual = cols.len();
        // Slice-pattern destructure: requires >= EXPECTED_COLUMNS; `..` ignores extras.
        let &[
            group_name,
            name,
            sub_name,
            callsign_rpt1,
            callsign_rpt2,
            freq_s,
            duplex,
            offset_s,
            ..,
        ] = cols.as_slice()
        else {
            return Err(SdCardError::ColumnCount {
                line: line_num,
                expected: EXPECTED_COLUMNS,
                actual,
            });
        };

        let frequency = parse_frequency_mhz(freq_s, line_num, "Frequency")?;
        let offset = parse_frequency_mhz(offset_s, line_num, "Offset")?;

        entries.push(RepeaterEntry {
            group_name: group_name.to_owned(),
            name: name.to_owned(),
            sub_name: sub_name.to_owned(),
            callsign_rpt1: callsign_rpt1.to_owned(),
            callsign_rpt2: callsign_rpt2.to_owned(),
            frequency,
            duplex: duplex.to_owned(),
            offset,
        });
    }

    Ok(entries)
}

/// Generates a repeater list TSV file as UTF-16LE bytes with BOM.
///
/// The output includes a header row followed by one row per entry.
#[must_use]
pub fn write_repeater_list(entries: &[RepeaterEntry]) -> Vec<u8> {
    let mut text = String::new();

    // Header row
    text.push_str(
        "Group Name\tName\tSub Name\tRepeater Call Sign\t\
         Gateway Call Sign\tFrequency\tDup\tOffset\r\n",
    );

    // Data rows
    for entry in entries {
        text.push_str(&entry.group_name);
        text.push('\t');
        text.push_str(&entry.name);
        text.push('\t');
        text.push_str(&entry.sub_name);
        text.push('\t');
        text.push_str(&entry.callsign_rpt1);
        text.push('\t');
        text.push_str(&entry.callsign_rpt2);
        text.push('\t');
        text.push_str(&format_frequency_mhz(entry.frequency));
        text.push('\t');
        text.push_str(&entry.duplex);
        text.push('\t');
        text.push_str(&format_frequency_mhz(entry.offset));
        text.push_str("\r\n");
    }

    encode_utf16le_bom(&text)
}

/// Parses a MHz frequency string (e.g., `"145.000000"`) into Hz.
fn parse_frequency_mhz(s: &str, line: usize, column: &str) -> Result<u32, SdCardError> {
    let trimmed = s.trim();
    if trimmed.is_empty() {
        return Ok(0);
    }
    let mhz: f64 = trimmed.parse().map_err(|_| SdCardError::InvalidField {
        line,
        column: column.to_owned(),
        detail: format!("invalid frequency: {trimmed:?}"),
    })?;
    // Convert MHz to Hz, rounding to nearest integer.
    #[expect(
        clippy::cast_possible_truncation,
        clippy::cast_sign_loss,
        reason = "Parses MHz frequencies from D75 SD-card .tsv repeater lists. The D75 tunes \
                  0.5-1300 MHz, so `mhz * 1_000_000` stays well within u32 range and is always \
                  non-negative. `.round()` produces an integral f64 whose truncation to u32 is \
                  the intended conversion."
    )]
    let hz = (mhz * 1_000_000.0).round() as u32;
    Ok(hz)
}

/// Formats a frequency in Hz as a MHz string with 6 decimal places.
fn format_frequency_mhz(hz: u32) -> String {
    let mhz = f64::from(hz) / 1_000_000.0;
    format!("{mhz:.6}")
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

    // `chunks_exact(2)` yields exactly-2-byte slices; `try_into` is infallible
    // for a slice of the known length, so this is `.unwrap_or_default`-safe.
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
    fn decode_utf16le_bom_basic() -> TestResult {
        let text = "hello";
        let encoded = encode_utf16le_bom(text);
        let decoded = decode_utf16le_bom(&encoded)?;
        assert_eq!(decoded, "hello");
        Ok(())
    }

    #[test]
    fn decode_utf16le_missing_bom() {
        let result = decode_utf16le_bom(&[0x00, 0x00]);
        assert!(
            matches!(result, Err(SdCardError::MissingBom)),
            "expected MissingBom, got {result:?}"
        );
    }

    #[test]
    fn decode_utf16le_odd_length() {
        let result = decode_utf16le_bom(&[0xFF, 0xFE, 0x41]);
        assert!(
            matches!(result, Err(SdCardError::InvalidUtf16Length { .. })),
            "expected InvalidUtf16Length, got {result:?}"
        );
    }

    #[test]
    fn format_frequency_round_trip() -> TestResult {
        let hz = 145_000_000u32;
        let s = format_frequency_mhz(hz);
        assert_eq!(s, "145.000000");
        let back = parse_frequency_mhz(&s, 1, "test")?;
        assert_eq!(back, hz);
        Ok(())
    }

    #[test]
    fn parse_frequency_empty() -> TestResult {
        let hz = parse_frequency_mhz("", 1, "test")?;
        assert_eq!(hz, 0);
        Ok(())
    }
}
