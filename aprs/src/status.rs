//! APRS status report parsing (APRS 1.0.1 §16 pp.80-82).
//!
//! Per the spec, a status report begins with the `>` data-type
//! identifier and carries one of four sub-formats:
//!
//! 1. **Plain text**: `>FREE_TEXT` (up to 62 chars).
//! 2. **With timestamp**: `>DDHHMMzFREE_TEXT` (DHM Zulu + 55 chars).
//! 3. **With Maidenhead grid + symbol**: `>IO91SX/G` (4 or 6 char grid
//!    locator + `/` + symbol code, optionally followed by a comment).
//! 4. **With beam-heading + ERP**: a 4-char `^DCK` suffix where `^`
//!    introduces a 3-digit course (000-359 in 10° steps, hex-encoded)
//!    and a 1-char power letter (table on p.81). Not yet implemented.
//!
//! Earlier code generations parsed only sub-format (1), discarding the
//! structural data of the others.

use crate::error::AprsError;
use crate::packet::AprsTimestamp;

/// An APRS status report (data type `>`).
///
/// Captures the spec-defined sub-formats from APRS 1.0.1 §16
/// (pp.80-82).
///
/// The `text` field always carries the human-readable trailing
/// portion (the part a station operator would type); the other
/// fields decompose the structured prefix when present.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct AprsStatus {
    /// Status text (trailing free-form portion, structured prefix
    /// stripped). When the status carries no structured prefix this
    /// is the entire body.
    pub text: String,
    /// Optional 7-byte DHM-Zulu timestamp prefix (`DDHHMMz`), parsed via
    /// [`AprsTimestamp::parse`]. Per APRS 1.0.1 §16 p.80 a status report's
    /// timestamp can *only* be DHM-Zulu, so this is always the
    /// [`AprsTimestamp::DhmZulu`] variant when present. `None` when the
    /// body did not begin with a well-formed DHM-Zulu timestamp (an HMS or
    /// DHM-local 7-byte prefix is treated as status text, not a
    /// timestamp).
    pub timestamp: Option<AprsTimestamp>,
    /// Optional Maidenhead grid locator prefix (4 or 6 ASCII chars
    /// per APRS 1.0.1 §6 grid-square rules).
    pub grid_locator: Option<String>,
    /// Optional 2-byte symbol pair following a Maidenhead grid prefix.
    /// First byte is the symbol-table character (`/` primary, `\\`
    /// alternate); second byte is the symbol code.
    pub symbol: Option<(char, char)>,
}

/// Maximum status text length per APRS 1.0.1 §16 p.80.
///
/// The spec quote is "up to 62 characters". With a 7-byte timestamp
/// the spec allows 55 characters; with a 6-char Maidenhead+symbol
/// prefix it allows 54. The parser is length-tolerant on receive
/// (accepts overlong text without error) because the spec text says
/// "should" rather than "must"; the constant is exposed for callers
/// that want to enforce on send.
pub const MAX_APRS_STATUS_TEXT_LEN: usize = 62;

/// Parse an APRS status report (`>text`).
///
/// Recognises the four spec-defined sub-formats described in
/// [`AprsStatus`]. The dispatch order is:
///
/// 1. If the body starts with a 7-byte DHM-Zulu timestamp
///    (`DDHHMMz`, the only timestamp form a status report may carry,
///    APRS 1.0.1 §16 p.80), strip the prefix and surface it in the
///    `timestamp` field. A 7-byte HMS or DHM-local prefix is *not* a
///    valid status timestamp and is left as status text.
/// 2. Otherwise, if the body starts with a 6- or 4-char Maidenhead
///    grid locator followed by `/` and one byte (symbol code), strip
///    the grid+symbol prefix and surface them.
/// 3. The remainder becomes `text` (trimmed of trailing whitespace).
///
/// # Errors
///
/// Returns [`AprsError::InvalidFormat`] if the info field does not
/// begin with `>`.
pub fn parse_aprs_status(info: &[u8]) -> Result<AprsStatus, AprsError> {
    if info.first() != Some(&b'>') {
        return Err(AprsError::InvalidFormat);
    }
    let body = info.get(1..).unwrap_or(&[]);
    let body_str = String::from_utf8_lossy(body);

    // Sub-format 2: DHM-Zulu timestamp prefix (7 bytes).
    //
    // APRS 1.0.1 §16 p.80: "The timestamp can only be in DHM zulu format."
    // Unlike position/object reports, a status report MUST NOT carry an
    // HMS (`HHMMSSh`) or DHM-local (`DDHHMM/`) timestamp. `AprsTimestamp::
    // parse` happily accepts those other forms, so a free-text status like
    // ">120000hrs since reset" would otherwise mis-parse "120000h" as an
    // HMS timestamp and silently truncate the text to "rs since reset".
    // Only strip the prefix when it is the spec-legal `DhmZulu` variant;
    // any other parse result means the leading bytes are status text.
    if let Some(prefix) = body_str.get(..7)
        && let Some(ts @ AprsTimestamp::DhmZulu { .. }) = AprsTimestamp::parse(prefix)
    {
        let rest = body_str.get(7..).unwrap_or("").trim_end().to_owned();
        return Ok(AprsStatus {
            text: rest,
            timestamp: Some(ts),
            grid_locator: None,
            symbol: None,
        });
    }

    // Sub-format 3: Maidenhead grid prefix followed by `/symbol`.
    //
    // Spec at §16 p.81: a 6-char grid locator (or 4-char + 2-char
    // trailing extension) followed by `/` and a symbol code, optionally
    // a space + free-form comment text. Detection: 6 or 4 chars of
    // valid grid format, then `/`, then exactly one byte.
    for grid_len in [6usize, 4usize] {
        if let Some(grid_candidate) = body_str.get(..grid_len)
            && is_valid_grid_locator(grid_candidate)
            && body_str.as_bytes().get(grid_len) == Some(&b'/')
            && let Some(sym_byte) = body_str.as_bytes().get(grid_len + 1).copied()
        {
            let grid = grid_candidate.to_owned();
            // The symbol-table is implicit `/` (primary) per APRS 1.0.1
            // p.81 example "IO91SX/G": the `/` here is the table marker
            // and `G` is the code.
            let symbol_table = '/';
            let symbol_code = sym_byte as char;
            let rest = body_str
                .get(grid_len + 2..)
                .unwrap_or("")
                .trim_end()
                .to_owned();
            return Ok(AprsStatus {
                text: rest,
                timestamp: None,
                grid_locator: Some(grid),
                symbol: Some((symbol_table, symbol_code)),
            });
        }
    }

    // Sub-format 1 (fallback): plain text.
    Ok(AprsStatus {
        text: body_str.trim_end().to_owned(),
        timestamp: None,
        grid_locator: None,
        symbol: None,
    })
}

/// Validate an APRS Maidenhead grid locator per APRS 1.0.1 §6 p.25.
///
/// Form: 2 letters (field, `A..R` case-insensitive) + 2 digits
/// (square) + optionally 2 letters (sub-square, `a..x` case-
/// insensitive). Both 4-char and 6-char forms are accepted.
fn is_valid_grid_locator(s: &str) -> bool {
    let bytes = s.as_bytes();
    match bytes.len() {
        4 => grid_field_ok(bytes.get(..2)) && grid_square_ok(bytes.get(2..4)),
        6 => {
            grid_field_ok(bytes.get(..2))
                && grid_square_ok(bytes.get(2..4))
                && grid_subsquare_ok(bytes.get(4..6))
        }
        _ => false,
    }
}

/// Field pair check: two letters in `A..R` (case-insensitive).
fn grid_field_ok(slot: Option<&[u8]>) -> bool {
    let Some(pair) = slot else {
        return false;
    };
    if pair.len() != 2 {
        return false;
    }
    pair.iter().all(|b| {
        let up = b.to_ascii_uppercase();
        (b'A'..=b'R').contains(&up)
    })
}

/// Square pair check: two ASCII digits.
fn grid_square_ok(slot: Option<&[u8]>) -> bool {
    let Some(pair) = slot else {
        return false;
    };
    if pair.len() != 2 {
        return false;
    }
    pair.iter().all(u8::is_ascii_digit)
}

/// Sub-square pair check: two letters in `a..x` (case-insensitive).
///
/// The spec's own example on p.25 (`IO91SX`) uses uppercase even
/// though the canonical Maidenhead form is lowercase, so both are
/// accepted. Letters beyond `x` / `X` (i.e. `y`/`z`) are rejected
/// since they would index outside the sub-square grid.
fn grid_subsquare_ok(slot: Option<&[u8]>) -> bool {
    let Some(pair) = slot else {
        return false;
    };
    if pair.len() != 2 {
        return false;
    }
    pair.iter().all(|b| {
        let low = b.to_ascii_lowercase();
        (b'a'..=b'x').contains(&low)
    })
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    type TestResult = Result<(), Box<dyn std::error::Error>>;

    #[test]
    fn parse_status_basic() -> TestResult {
        let info = b">Operating on 144.390";
        let status = parse_aprs_status(info)?;
        assert_eq!(status.text, "Operating on 144.390");
        assert_eq!(status.timestamp, None);
        assert_eq!(status.grid_locator, None);
        assert_eq!(status.symbol, None);
        Ok(())
    }

    #[test]
    fn parse_status_empty() -> TestResult {
        let info = b">";
        let status = parse_aprs_status(info)?;
        assert_eq!(status.text, "");
        Ok(())
    }

    #[test]
    fn parse_status_with_dhm_zulu_timestamp() -> TestResult {
        // Sub-format 2: DHM-Zulu timestamp followed by free text.
        let info = b">092345zOn the air";
        let status = parse_aprs_status(info)?;
        assert_eq!(
            status.timestamp,
            Some(AprsTimestamp::DhmZulu {
                day: 9,
                hour: 23,
                minute: 45,
            }),
        );
        assert_eq!(status.text, "On the air");
        Ok(())
    }

    #[test]
    fn parse_status_rejects_hms_timestamp() -> TestResult {
        // APRS 1.0.1 §16 p.80: a status timestamp can ONLY be DHM-Zulu.
        // A 7-byte HMS prefix (`234517h`) must be treated as status text,
        // not stripped as a timestamp.
        let info = b">234517hLate-night net";
        let status = parse_aprs_status(info)?;
        assert_eq!(status.timestamp, None);
        assert_eq!(status.text, "234517hLate-night net");
        Ok(())
    }

    #[test]
    fn parse_status_rejects_dhm_local_timestamp() -> TestResult {
        // DHM-local (`DDHHMM/`) is likewise not a valid status timestamp.
        let info = b">110000/Local time status";
        let status = parse_aprs_status(info)?;
        assert_eq!(status.timestamp, None);
        assert_eq!(status.text, "110000/Local time status");
        Ok(())
    }

    #[test]
    fn parse_status_hms_lookalike_freetext_not_truncated() -> TestResult {
        // Verified bug: ">120000hrs since reset" must keep its full text;
        // the old parser mis-read "120000h" as an HMS timestamp and
        // truncated the text to "rs since reset".
        let info = b">120000hrs since reset";
        let status = parse_aprs_status(info)?;
        assert_eq!(status.timestamp, None);
        assert_eq!(status.text, "120000hrs since reset");
        Ok(())
    }

    #[test]
    fn parse_status_dhm_zulu_still_stripped() -> TestResult {
        // The one legal form must still be recognised and stripped.
        let info = b">092345zReal status";
        let status = parse_aprs_status(info)?;
        assert_eq!(
            status.timestamp,
            Some(AprsTimestamp::DhmZulu {
                day: 9,
                hour: 23,
                minute: 45,
            }),
        );
        assert_eq!(status.text, "Real status");
        Ok(())
    }

    #[test]
    fn parse_status_with_grid_locator_and_symbol() -> TestResult {
        // Sub-format 3: spec example from §16 p.81.
        let info = b">IO91SX/G";
        let status = parse_aprs_status(info)?;
        assert_eq!(status.grid_locator.as_deref(), Some("IO91SX"));
        assert_eq!(status.symbol, Some(('/', 'G')));
        assert_eq!(status.text, "");
        Ok(())
    }

    #[test]
    fn parse_status_with_grid_locator_and_comment() -> TestResult {
        let info = b">IO91SX/G Net active";
        let status = parse_aprs_status(info)?;
        assert_eq!(status.grid_locator.as_deref(), Some("IO91SX"));
        assert_eq!(status.symbol, Some(('/', 'G')));
        assert_eq!(status.text, " Net active");
        Ok(())
    }

    #[test]
    fn parse_status_with_4char_grid() -> TestResult {
        let info = b">IO91/V";
        let status = parse_aprs_status(info)?;
        assert_eq!(status.grid_locator.as_deref(), Some("IO91"));
        assert_eq!(status.symbol, Some(('/', 'V')));
        Ok(())
    }

    #[test]
    fn parse_status_with_invalid_grid_falls_through_to_text() -> TestResult {
        // "ZZZZZZ" is not a valid grid (field letters must be A..R).
        // The parser must fall through to plain-text mode rather than
        // emitting a spurious grid.
        let info = b">ZZZZZZ/G suspicious";
        let status = parse_aprs_status(info)?;
        assert_eq!(status.grid_locator, None);
        assert_eq!(status.symbol, None);
        assert_eq!(status.text, "ZZZZZZ/G suspicious");
        Ok(())
    }

    #[test]
    fn parse_status_timestamp_takes_precedence_over_grid() -> TestResult {
        // A 7-byte well-formed timestamp wins over a 6-byte grid match,
        // because the timestamp is the deterministic structured form
        // (suffix discriminates) and grids can superficially resemble
        // text prefixes.
        let info = b">092345zIO91SX is the grid";
        let status = parse_aprs_status(info)?;
        assert!(status.timestamp.is_some());
        // The grid-locator prefix is not extracted in this case; it
        // becomes part of `text` since the timestamp prefix consumed
        // the first 7 bytes.
        assert_eq!(status.text, "IO91SX is the grid");
        Ok(())
    }

    #[test]
    fn validate_grid_locator_accepts_valid_forms() {
        assert!(is_valid_grid_locator("AA00"));
        assert!(is_valid_grid_locator("IO91"));
        assert!(is_valid_grid_locator("RR99"));
        assert!(is_valid_grid_locator("IO91SX"));
        assert!(is_valid_grid_locator("AA00aa"));
        assert!(is_valid_grid_locator("RR99xx"));
    }

    #[test]
    fn validate_grid_locator_rejects_malformed() {
        assert!(!is_valid_grid_locator(""));
        assert!(!is_valid_grid_locator("ABC")); // wrong length
        assert!(!is_valid_grid_locator("ABCDE")); // wrong length
        assert!(!is_valid_grid_locator("ZZ00")); // field letter > R
        assert!(!is_valid_grid_locator("AAaa")); // square not digit
        assert!(!is_valid_grid_locator("AA00zz")); // sub-square letter > x (lowercase)
        assert!(!is_valid_grid_locator("AA00ZZ")); // sub-square letter > x (uppercase)
    }

    #[test]
    fn validate_grid_locator_accepts_mixed_case() {
        // Real-world traffic mixes cases; accept both for both halves
        // of the locator. Sub-square stays bounded at A..X (case-folded
        // to a..x); Y and Z are spec-disallowed in either case.
        assert!(is_valid_grid_locator("aa00"), "lowercase field");
        assert!(is_valid_grid_locator("AA00XX"), "uppercase sub-square at X");
        assert!(is_valid_grid_locator("Aa00bX"), "mixed case");
    }
}
