//! Parser for Kenwood D-STAR callsign-list `.tsv` files.
//!
//! The exact table has three columns, in order: `Name`, `Callsign`, and
//! `Memo`. MCP-D75 can write either UTF-16LE with a BOM or Shift-JIS without a
//! BOM. Both forms are decoded strictly; replacement characters are never
//! introduced.
//!
//! Empty flash slots do not appear in the TSV. Rows therefore retain their
//! order but carry no persistent slot number.
//!
//! # Location
//!
//! The TH-D75 user manual documents the singular directory
//! `/KENWOOD/TH-D75/SETTING/CALLSIGN_LIST/*.tsv`.
//!
//! # Capacity
//!
//! Up to 300 entries. Callsigns must be unique within one list.

use std::borrow::Cow;
use std::collections::HashMap;

use encoding_rs::SHIFT_JIS;

use super::{SdCardError, decode_utf16le_bom, encode_utf16le_bom};
use crate::types::DstarCallsign;
pub use crate::types::{
    CallsignEntry, CallsignEntryError, CallsignListMemo, CallsignListName, CallsignListTextError,
};

/// Exact header used by the Kenwood callsign-list TSV.
pub const CALLSIGN_LIST_HEADER: &str = "Name\tCallsign\tMemo";

/// Maximum number of direct-call destinations stored by the TH-D75.
pub const MAX_CALLSIGN_ENTRIES: usize = 300;

const FILE_TYPE: &str = "D-STAR callsign list";
const SUPPORTED_ENCODINGS: &str = "UTF-16LE with BOM or Shift-JIS without BOM";
const COLUMN_COUNT: usize = 3;

/// Text encoding used when writing a callsign-list TSV.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub enum CallsignListEncoding {
    /// UTF-16 little-endian with the `FF FE` byte-order mark.
    #[default]
    Utf16Le,
    /// Shift-JIS without a byte-order mark.
    ShiftJis,
}

/// Parses a callsign-list TSV file from raw bytes.
///
/// The parser accepts the two encodings emitted by MCP-D75, requires the exact
/// `Name\tCallsign\tMemo` header, preserves all three fields, and rejects
/// duplicate destination callsigns.
///
/// # Errors
///
/// Returns an [`SdCardError`] for an unsupported encoding, a mismatched
/// header, malformed TSV quoting, an invalid field, a duplicate callsign, or
/// more than [`MAX_CALLSIGN_ENTRIES`] rows.
pub fn parse_callsign_list(data: &[u8]) -> Result<Vec<CallsignEntry>, SdCardError> {
    let text = decode_callsign_list(data)?;
    let mut lines = text.lines();
    let actual_header = lines.next().unwrap_or_default();
    if actual_header != CALLSIGN_LIST_HEADER {
        return Err(SdCardError::HeaderMismatch {
            file_type: FILE_TYPE,
            expected: CALLSIGN_LIST_HEADER.to_owned(),
            actual: actual_header.to_owned(),
        });
    }

    let mut entries = Vec::new();
    let mut callsign_lines = HashMap::new();
    for (line_index, line) in lines.enumerate() {
        let line_number = line_index + 2;
        let [name, callsign, memo] = parse_tsv_row(line, line_number)?;
        let entry = CallsignEntry::new(&name, &callsign, &memo).map_err(|error| {
            let column = match error {
                CallsignEntryError::InvalidName(_) => "Name",
                CallsignEntryError::InvalidCallsign(_) | CallsignEntryError::EmptyCallsign => {
                    "Callsign"
                }
                CallsignEntryError::InvalidMemo(_) => "Memo",
            };
            SdCardError::InvalidField {
                line: line_number,
                column: column.to_owned(),
                detail: error.to_string(),
            }
        })?;

        if let Some(first_line) = callsign_lines.insert(entry.callsign().clone(), line_number) {
            return Err(SdCardError::InvalidField {
                line: line_number,
                column: "Callsign".to_owned(),
                detail: format!("duplicate callsign; first appears on line {first_line}"),
            });
        }

        entries.push(entry);
        if entries.len() > MAX_CALLSIGN_ENTRIES {
            return Err(SdCardError::EntryCount {
                file_type: FILE_TYPE,
                maximum: MAX_CALLSIGN_ENTRIES,
                actual: entries.len(),
            });
        }
    }

    Ok(entries)
}

/// Generates a callsign-list TSV as UTF-16LE with a BOM.
///
/// This is the default MCP-D75-compatible representation. Use
/// [`write_callsign_list_with_encoding`] when Shift-JIS output is required.
///
/// # Errors
///
/// Returns [`SdCardError::EntryCount`] for more than 300 rows, or
/// [`SdCardError::InvalidField`] for a duplicate callsign.
pub fn write_callsign_list(entries: &[CallsignEntry]) -> Result<Vec<u8>, SdCardError> {
    write_callsign_list_with_encoding(entries, CallsignListEncoding::Utf16Le)
}

/// Generates a callsign-list TSV in one encoding emitted by MCP-D75.
///
/// Fields containing a tab or double quote use Kenwood's quoted-field form;
/// embedded double quotes are doubled. Rows use CRLF terminators.
///
/// # Errors
///
/// Returns [`SdCardError::EntryCount`] for more than 300 rows, or
/// [`SdCardError::InvalidField`] for a duplicate callsign or an unexpected
/// encoding failure.
pub fn write_callsign_list_with_encoding(
    entries: &[CallsignEntry],
    encoding: CallsignListEncoding,
) -> Result<Vec<u8>, SdCardError> {
    validate_list(entries)?;

    let mut text = String::from(CALLSIGN_LIST_HEADER);
    text.push_str("\r\n");
    for entry in entries {
        push_tsv_field(&mut text, entry.name().as_str());
        text.push('\t');
        push_tsv_field(&mut text, entry.callsign().as_str());
        text.push('\t');
        push_tsv_field(&mut text, entry.memo().as_str());
        text.push_str("\r\n");
    }

    match encoding {
        CallsignListEncoding::Utf16Le => Ok(encode_utf16le_bom(&text)),
        CallsignListEncoding::ShiftJis => {
            let (bytes, _, had_errors) = SHIFT_JIS.encode(&text);
            if had_errors {
                return Err(SdCardError::InvalidField {
                    line: 0,
                    column: "Encoding".to_owned(),
                    detail: "validated callsign-list text was not representable in Shift-JIS"
                        .to_owned(),
                });
            }
            Ok(bytes.into_owned())
        }
    }
}

fn decode_callsign_list(data: &[u8]) -> Result<String, SdCardError> {
    if data.starts_with(&[0xFF, 0xFE]) {
        return decode_utf16le_bom(data);
    }
    if data.starts_with(&[0xFE, 0xFF]) {
        return Err(SdCardError::UnsupportedTextEncoding {
            file_type: FILE_TYPE,
            expected: SUPPORTED_ENCODINGS,
        });
    }

    SHIFT_JIS
        .decode_without_bom_handling_and_without_replacement(data)
        .map(Cow::into_owned)
        .ok_or(SdCardError::UnsupportedTextEncoding {
            file_type: FILE_TYPE,
            expected: SUPPORTED_ENCODINGS,
        })
}

fn validate_list(entries: &[CallsignEntry]) -> Result<(), SdCardError> {
    if entries.len() > MAX_CALLSIGN_ENTRIES {
        return Err(SdCardError::EntryCount {
            file_type: FILE_TYPE,
            maximum: MAX_CALLSIGN_ENTRIES,
            actual: entries.len(),
        });
    }

    let mut callsign_lines: HashMap<&DstarCallsign, usize> = HashMap::new();
    for (index, entry) in entries.iter().enumerate() {
        let line_number = index + 2;
        if let Some(first_line) = callsign_lines.insert(entry.callsign(), line_number) {
            return Err(SdCardError::InvalidField {
                line: line_number,
                column: "Callsign".to_owned(),
                detail: format!("duplicate callsign; first appears on line {first_line}"),
            });
        }
    }
    Ok(())
}

fn push_tsv_field(output: &mut String, field: &str) {
    if field.contains(['\t', '"']) {
        output.push('"');
        for character in field.chars() {
            if character == '"' {
                output.push('"');
            }
            output.push(character);
        }
        output.push('"');
    } else {
        output.push_str(field);
    }
}

fn parse_tsv_row(line: &str, line_number: usize) -> Result<[String; COLUMN_COUNT], SdCardError> {
    let mut fields = Vec::with_capacity(COLUMN_COUNT);
    let bytes = line.as_bytes();
    let mut cursor = 0;

    loop {
        let mut field = String::new();
        if bytes.get(cursor) == Some(&b'"') {
            cursor += 1;
            loop {
                let remaining = bytes.get(cursor..).unwrap_or_default();
                let Some(relative_quote) = remaining.iter().position(|&byte| byte == b'"') else {
                    return Err(malformed_row(line_number, "unterminated quoted field"));
                };
                let quote = cursor + relative_quote;
                let segment = line
                    .get(cursor..quote)
                    .ok_or_else(|| malformed_row(line_number, "quote split a UTF-8 character"))?;
                field.push_str(segment);
                cursor = quote + 1;

                if bytes.get(cursor) == Some(&b'"') {
                    field.push('"');
                    cursor += 1;
                    continue;
                }
                if cursor < bytes.len() && bytes.get(cursor) != Some(&b'\t') {
                    return Err(malformed_row(
                        line_number,
                        "characters follow a quoted field before the next tab",
                    ));
                }
                break;
            }
        } else {
            let remaining = bytes.get(cursor..).unwrap_or_default();
            let relative_end = remaining
                .iter()
                .position(|&byte| byte == b'\t')
                .unwrap_or(remaining.len());
            let end = cursor + relative_end;
            let value = line
                .get(cursor..end)
                .ok_or_else(|| malformed_row(line_number, "tab split a UTF-8 character"))?;
            if value.contains('"') {
                return Err(malformed_row(
                    line_number,
                    "an unquoted field contains a double quote",
                ));
            }
            field.push_str(value);
            cursor = end;
        }

        fields.push(field);
        if cursor == bytes.len() {
            break;
        }
        cursor += 1;
        if cursor > bytes.len() {
            return Err(malformed_row(
                line_number,
                "row ended after an invalid delimiter",
            ));
        }
    }

    let actual = fields.len();
    fields.try_into().map_err(|_| SdCardError::ColumnCount {
        line: line_number,
        expected: COLUMN_COUNT,
        actual,
    })
}

fn malformed_row(line: usize, detail: &str) -> SdCardError {
    SdCardError::InvalidField {
        line,
        column: "TSV row".to_owned(),
        detail: detail.to_owned(),
    }
}

#[cfg(test)]
mod tests {
    use std::fmt::Write as _;

    use super::*;

    type TestResult = Result<(), Box<dyn std::error::Error>>;

    fn entry(name: &str, callsign: &str, memo: &str) -> Result<CallsignEntry, CallsignEntryError> {
        CallsignEntry::new(name, callsign, memo)
    }

    #[test]
    fn parses_empty_list() -> TestResult {
        let data = encode_utf16le_bom("Name\tCallsign\tMemo\r\n");
        assert!(parse_callsign_list(&data)?.is_empty());
        Ok(())
    }

    #[test]
    fn preserves_all_three_columns() -> TestResult {
        let data = encode_utf16le_bom(
            "Name\tCallsign\tMemo\r\nAlice\tW4CDR\tfriend\r\nCQ\tCQCQCQ\tbroadcast\r\n",
        );
        let entries = parse_callsign_list(&data)?;
        let first = entries.first().ok_or("missing first entry")?;
        assert_eq!(first.name().as_str(), "Alice");
        assert_eq!(first.callsign().as_str(), "W4CDR");
        assert_eq!(first.memo().as_str(), "friend");
        assert_eq!(
            entries
                .get(1)
                .ok_or("missing second entry")?
                .callsign()
                .as_str(),
            "CQCQCQ"
        );
        Ok(())
    }

    #[test]
    fn quoted_tabs_and_quotes_roundtrip() -> TestResult {
        let entries = vec![entry("A\tB", "W4CDR", "said \"hello\"")?];
        let bytes = write_callsign_list(&entries)?;
        let text = decode_utf16le_bom(&bytes)?;
        assert_eq!(
            text,
            "Name\tCallsign\tMemo\r\n\"A\tB\"\tW4CDR\t\"said \"\"hello\"\"\"\r\n"
        );
        assert_eq!(parse_callsign_list(&bytes)?, entries);
        Ok(())
    }

    #[test]
    fn shift_jis_roundtrip_is_strict() -> TestResult {
        let entries = vec![entry("東京", "JP1YLA", "友人")?];
        let bytes = write_callsign_list_with_encoding(&entries, CallsignListEncoding::ShiftJis)?;
        assert!(!bytes.starts_with(&[0xFF, 0xFE]));
        assert_eq!(parse_callsign_list(&bytes)?, entries);
        assert!(matches!(
            parse_callsign_list(&[0x81]),
            Err(SdCardError::UnsupportedTextEncoding {
                file_type: FILE_TYPE,
                ..
            })
        ));
        Ok(())
    }

    #[test]
    fn text_fields_enforce_lossless_storage_widths() {
        assert!(matches!(
            CallsignListName::new("12345678901234567"),
            Err(CallsignListTextError::TooLong {
                encoded_len: 17,
                maximum: 16,
            })
        ));
        assert!(matches!(
            CallsignListName::new("日本語日本語日本語"),
            Err(CallsignListTextError::TooLong {
                encoded_len: 18,
                maximum: 16,
            })
        ));
        assert!(matches!(
            CallsignListMemo::new("emoji 📻"),
            Err(CallsignListTextError::UnrepresentableShiftJis)
        ));
        assert!(matches!(
            CallsignListMemo::new("bad\0memo"),
            Err(CallsignListTextError::Nul { offset: 3 })
        ));
        assert!(matches!(
            CallsignListMemo::new("bad\nmemo"),
            Err(CallsignListTextError::LineTerminator {
                offset: 3,
                character: '\n',
            })
        ));
    }

    #[test]
    fn parser_and_writer_reject_duplicate_callsigns() -> TestResult {
        let data = encode_utf16le_bom(
            "Name\tCallsign\tMemo\r\nAlice\tW4CDR\tfirst\r\nBob\tW4CDR\tsecond\r\n",
        );
        assert!(matches!(
            parse_callsign_list(&data),
            Err(SdCardError::InvalidField { line: 3, column, detail })
                if column == "Callsign" && detail.contains("line 2")
        ));

        let entries = vec![
            entry("Alice", "W4CDR", "first")?,
            entry("Bob", "W4CDR", "second")?,
        ];
        assert!(matches!(
            write_callsign_list(&entries),
            Err(SdCardError::InvalidField { line: 3, column, detail })
                if column == "Callsign" && detail.contains("line 2")
        ));
        Ok(())
    }

    #[test]
    fn parser_rejects_more_entries_than_the_radio_can_store() -> TestResult {
        let mut text = String::from("Name\tCallsign\tMemo\r\n");
        for index in 0..=MAX_CALLSIGN_ENTRIES {
            write!(&mut text, "\tA{index:07}\t\r\n")?;
        }
        assert!(matches!(
            parse_callsign_list(&encode_utf16le_bom(&text)),
            Err(SdCardError::EntryCount {
                file_type: FILE_TYPE,
                maximum: MAX_CALLSIGN_ENTRIES,
                actual,
            }) if actual == MAX_CALLSIGN_ENTRIES + 1
        ));
        Ok(())
    }

    #[test]
    fn writer_rejects_more_entries_than_the_radio_can_store() -> TestResult {
        let entry = entry("", "W4CDR", "")?;
        let entries = vec![entry; MAX_CALLSIGN_ENTRIES + 1];
        assert!(matches!(
            write_callsign_list(&entries),
            Err(SdCardError::EntryCount {
                file_type: FILE_TYPE,
                maximum: MAX_CALLSIGN_ENTRIES,
                actual,
            }) if actual == MAX_CALLSIGN_ENTRIES + 1
        ));
        Ok(())
    }

    #[test]
    fn parser_requires_exact_header_and_column_count() {
        assert!(matches!(
            parse_callsign_list(&encode_utf16le_bom("Callsign\r\nW4CDR\r\n")),
            Err(SdCardError::HeaderMismatch { .. })
        ));
        assert!(matches!(
            parse_callsign_list(&encode_utf16le_bom(
                "Name\tCallsign\tMemo\r\nAlice\tW4CDR\r\n"
            )),
            Err(SdCardError::ColumnCount {
                line: 2,
                expected: COLUMN_COUNT,
                actual: 2,
            })
        ));
    }

    #[test]
    fn parser_rejects_malformed_quoting() {
        let unterminated = encode_utf16le_bom("Name\tCallsign\tMemo\r\n\"Alice\tW4CDR\tmemo\r\n");
        assert!(matches!(
            parse_callsign_list(&unterminated),
            Err(SdCardError::InvalidField { line: 2, column, .. }) if column == "TSV row"
        ));

        let unquoted = encode_utf16le_bom("Name\tCallsign\tMemo\r\nAl\"ice\tW4CDR\tmemo\r\n");
        assert!(matches!(
            parse_callsign_list(&unquoted),
            Err(SdCardError::InvalidField { line: 2, column, .. }) if column == "TSV row"
        ));
    }
}
