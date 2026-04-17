//! APRS status report (APRS 1.0.1 ch. 16).

use crate::error::AprsError;

/// An APRS status report (data type `>`).
///
/// Contains free-form text, optionally prefixed with a Maidenhead
/// grid locator (6 chars) or a timestamp (7 chars DHM/HMS).
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct AprsStatus {
    /// Status text.
    pub text: String,
}

/// Parse an APRS status report (`>text`).
///
/// # Errors
///
/// Returns [`AprsError::InvalidFormat`] if the info field does not begin
/// with `>`.
pub fn parse_aprs_status(info: &[u8]) -> Result<AprsStatus, AprsError> {
    if info.first() != Some(&b'>') {
        return Err(AprsError::InvalidFormat);
    }
    let body = info.get(1..).unwrap_or(&[]);
    let text = String::from_utf8_lossy(body).trim().to_string();
    Ok(AprsStatus { text })
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
        Ok(())
    }

    #[test]
    fn parse_status_empty() -> TestResult {
        let info = b">";
        let status = parse_aprs_status(info)?;
        assert_eq!(status.text, "");
        Ok(())
    }
}
