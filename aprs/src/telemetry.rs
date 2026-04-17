//! APRS telemetry (APRS 1.0.1 ch. 13).

use crate::error::AprsError;

/// Parsed APRS telemetry report.
///
/// Format: `T#seq,val1,val2,val3,val4,val5,dddddddd`
/// where vals are 0-999 analog values and d's are binary digits (8 bits).
///
/// Per APRS 1.0.1 Chapter 13, telemetry is used to transmit analog and
/// digital sensor readings. Up to 5 analog channels are supported; each
/// channel is stored as `Option<u16>` so callers can distinguish
/// "channel not reported" from "channel reported as 0".
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct AprsTelemetry {
    /// Telemetry sequence number (0-999 or "MIC").
    pub sequence: String,
    /// Analog values — exactly 5 channels per APRS 1.0.1 §13.1.
    /// Channels omitted from the wire frame are `None`.
    pub analog: [Option<u16>; 5],
    /// Digital value (8 bits).
    pub digital: u8,
}

/// Parse an APRS telemetry report (`T#seq,v1,v2,v3,v4,v5,dddddddd`).
///
/// Per APRS 1.0.1 §13.1 a telemetry frame has exactly 5 analog channels
/// and 1 digital channel. We tolerate fewer analog channels (missing
/// channels become `None`) but reject frames with more fields than the
/// spec allows — those are almost certainly malformed.
///
/// # Errors
///
/// Returns [`AprsError::InvalidFormat`] for malformed input: missing
/// `T#` prefix, non-integer analog values, non-binary digital digits,
/// or more than 7 comma-separated fields.
pub fn parse_aprs_telemetry(info: &[u8]) -> Result<AprsTelemetry, AprsError> {
    // Minimum: T#seq,v (at least 5 bytes)
    if info.first() != Some(&b'T') || info.get(1) != Some(&b'#') {
        return Err(AprsError::InvalidFormat);
    }

    let body_bytes = info.get(2..).unwrap_or(&[]);
    let body = String::from_utf8_lossy(body_bytes);
    let parts: Vec<&str> = body.split(',').collect();
    // Spec limit: sequence + 5 analog + 1 digital = 7 fields max.
    if parts.is_empty() || parts.len() > 7 {
        return Err(AprsError::InvalidFormat);
    }

    let sequence = parts.first().ok_or(AprsError::InvalidFormat)?.to_string();

    // Parse analog values into a fixed-size [Option<u16>; 5].
    let mut analog: [Option<u16>; 5] = [None, None, None, None, None];
    let analog_end = std::cmp::min(parts.len(), 6); // indices 1..=5
    let analog_parts = parts.get(1..analog_end).unwrap_or(&[]);
    for (i, part) in analog_parts.iter().enumerate() {
        let trimmed = part.trim();
        if trimmed.is_empty() {
            continue;
        }
        let val: u16 = trimmed.parse().map_err(|_| AprsError::InvalidFormat)?;
        if let Some(slot) = analog.get_mut(i) {
            *slot = Some(val);
        }
    }

    // Parse digital value (8 binary digits) if present. Per APRS 1.0.1
    // §13.1, the field is exactly 8 binary digits; malformed input is a
    // parse error, not a silent zero.
    let digital = if let Some(digi_raw) = parts.get(6) {
        let digi_str = digi_raw.trim();
        let digi_bits = digi_str.get(..8).unwrap_or(digi_str);
        u8::from_str_radix(digi_bits, 2).map_err(|_| AprsError::InvalidFormat)?
    } else {
        0
    };

    Ok(AprsTelemetry {
        sequence,
        analog,
        digital,
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
    fn parse_telemetry_full() -> TestResult {
        let info = b"T#123,100,200,300,400,500,10101010";
        let t = parse_aprs_telemetry(info)?;
        assert_eq!(t.sequence, "123");
        assert_eq!(
            t.analog,
            [Some(100), Some(200), Some(300), Some(400), Some(500)]
        );
        assert_eq!(t.digital, 0b1010_1010);
        Ok(())
    }

    #[test]
    fn parse_telemetry_mic_sequence() -> TestResult {
        let info = b"T#MIC,001,002,003,004,005,11111111";
        let t = parse_aprs_telemetry(info)?;
        assert_eq!(t.sequence, "MIC");
        assert_eq!(t.analog, [Some(1), Some(2), Some(3), Some(4), Some(5)]);
        assert_eq!(t.digital, 0xFF);
        Ok(())
    }

    #[test]
    fn parse_telemetry_partial_analog() -> TestResult {
        // Only 3 analog values, no digital.
        let info = b"T#001,10,20,30";
        let t = parse_aprs_telemetry(info)?;
        assert_eq!(t.sequence, "001");
        assert_eq!(t.analog, [Some(10), Some(20), Some(30), None, None]);
        assert_eq!(t.digital, 0);
        Ok(())
    }

    #[test]
    fn parse_telemetry_zero_values() -> TestResult {
        let info = b"T#000,0,0,0,0,0,00000000";
        let t = parse_aprs_telemetry(info)?;
        assert_eq!(t.sequence, "000");
        assert_eq!(t.analog, [Some(0), Some(0), Some(0), Some(0), Some(0)]);
        assert_eq!(t.digital, 0);
        Ok(())
    }

    #[test]
    fn parse_telemetry_rejects_too_many_fields() {
        // 6 analog + 1 digital = 7 after the sequence = 8 fields total.
        let info = b"T#001,1,2,3,4,5,6,00000000";
        assert!(
            matches!(parse_aprs_telemetry(info), Err(AprsError::InvalidFormat)),
            "expected InvalidFormat for 8-field input",
        );
    }

    #[test]
    fn parse_telemetry_invalid_no_hash() {
        let info = b"T123,1,2,3,4,5,00000000";
        assert!(parse_aprs_telemetry(info).is_err(), "missing # rejected");
    }

    #[test]
    fn parse_telemetry_invalid_digital_field_is_error() {
        // Digital field must be exactly 8 binary digits — non-binary
        // characters must fail parsing, not silently return 0.
        let info = b"T#123,1,2,3,4,5,XXXXXXXX";
        assert!(
            matches!(parse_aprs_telemetry(info), Err(AprsError::InvalidFormat)),
            "non-binary digital field must error",
        );
    }
}
