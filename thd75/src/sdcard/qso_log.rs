//! Parser for QSO log `.tsv` files.
//!
//! The QSO log records communication history, primarily for D-STAR
//! contacts. Each entry contains the direction (TX/RX), timestamp,
//! frequency, mode, GPS position, signal report, and D-STAR routing
//! information.
//!
//! # Location
//!
//! `/KENWOOD/TH-D75/QSO_LOG/*.tsv`
//!
//! # Format
//!
//! Tab-separated values, plain text (ASCII/UTF-8). The first line is
//! a header row with 24 column names.

use super::SdCardError;

/// Number of expected columns in the QSO log TSV.
const EXPECTED_COLUMNS: usize = 24;

/// A single QSO log entry.
///
/// All fields are stored as strings to preserve the exact firmware
/// output format. Callers can parse individual fields as needed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct QsoEntry {
    /// Direction: `"TX"` or `"RX"`.
    pub tx_rx: String,
    /// Date and time of the contact (e.g., `"2026/03/28 14:30"`).
    pub date: String,
    /// Operating frequency (e.g., `"145.000.000"` or Hz string).
    pub frequency: String,
    /// Operating mode: `"FM"`, `"DV"`, `"NFM"`, or `"AM"`.
    pub mode: String,
    /// Own GPS latitude at time of QSO.
    pub my_latitude: String,
    /// Own GPS longitude at time of QSO.
    pub my_longitude: String,
    /// Own GPS altitude at time of QSO.
    pub my_altitude: String,
    /// Transmit power level.
    pub rf_power: String,
    /// Signal strength reading.
    pub s_meter: String,
    /// Source callsign (own callsign for TX, remote for RX).
    pub caller: String,
    /// User memo/notes field.
    pub memo: String,
    /// Destination callsign (URCALL).
    pub called: String,
    /// D-STAR RPT1 (link source repeater).
    pub rpt1: String,
    /// D-STAR RPT2 (link destination repeater).
    pub rpt2: String,
    /// D-STAR slow-data message.
    pub message: String,
    /// Repeater control flags.
    pub repeater_control: String,
    /// Break (BK) flag.
    pub bk: String,
    /// Emergency (EMR) flag.
    pub emr: String,
    /// D-STAR fast data flag.
    pub fast_data: String,
    /// Remote station latitude (from D-STAR GPS data).
    pub latitude: String,
    /// Remote station longitude (from D-STAR GPS data).
    pub longitude: String,
    /// Remote station altitude.
    pub altitude: String,
    /// Remote station course/heading.
    pub course: String,
    /// Remote station speed.
    pub speed: String,
}

/// Parses a QSO log TSV file from raw bytes.
///
/// Expects plain ASCII/UTF-8 text with tab-separated columns. The
/// first line is treated as a header row and is skipped. Each data
/// row must have exactly 24 columns.
///
/// # Errors
///
/// Returns an [`SdCardError`] if a data row has an unexpected column
/// count.
#[expect(
    clippy::similar_names,
    reason = "TSV columns use the firmware's naming convention where `my_latitude` / \
              `my_longitude` / `my_altitude` are the station's own GPS snapshot and the \
              unprefixed `latitude` / `longitude` / `altitude` are the remote station's \
              D-STAR-embedded position. Renaming would diverge from the wire format \
              (see struct docs above)."
)]
pub fn parse_qso_log(data: &[u8]) -> Result<Vec<QsoEntry>, SdCardError> {
    let text = String::from_utf8_lossy(data);
    let mut entries = Vec::new();

    for (line_idx, line) in text.lines().enumerate() {
        // Skip header row and blank lines.
        if line_idx == 0 || line.trim().is_empty() {
            continue;
        }

        let line_num = line_idx + 1;
        let cols: Vec<&str> = line.split('\t').collect();
        let actual = cols.len();
        // Slice-pattern destructure: requires >= EXPECTED_COLUMNS elements, tail `..`
        // allows (and ignores) extras. Binds each column to a named local — no indexing.
        let &[
            tx_rx,
            date,
            frequency,
            mode,
            my_latitude,
            my_longitude,
            my_altitude,
            rf_power,
            s_meter,
            caller,
            memo,
            called,
            rpt1,
            rpt2,
            message,
            repeater_control,
            bk,
            emr,
            fast_data,
            latitude,
            longitude,
            altitude,
            course,
            speed,
            ..,
        ] = cols.as_slice()
        else {
            return Err(SdCardError::ColumnCount {
                line: line_num,
                expected: EXPECTED_COLUMNS,
                actual,
            });
        };

        entries.push(QsoEntry {
            tx_rx: tx_rx.to_owned(),
            date: date.to_owned(),
            frequency: frequency.to_owned(),
            mode: mode.to_owned(),
            my_latitude: my_latitude.to_owned(),
            my_longitude: my_longitude.to_owned(),
            my_altitude: my_altitude.to_owned(),
            rf_power: rf_power.to_owned(),
            s_meter: s_meter.to_owned(),
            caller: caller.to_owned(),
            memo: memo.to_owned(),
            called: called.to_owned(),
            rpt1: rpt1.to_owned(),
            rpt2: rpt2.to_owned(),
            message: message.to_owned(),
            repeater_control: repeater_control.to_owned(),
            bk: bk.to_owned(),
            emr: emr.to_owned(),
            fast_data: fast_data.to_owned(),
            latitude: latitude.to_owned(),
            longitude: longitude.to_owned(),
            altitude: altitude.to_owned(),
            course: course.to_owned(),
            speed: speed.to_owned(),
        });
    }

    Ok(entries)
}

/// Generates a QSO log TSV file as UTF-8 bytes.
///
/// The output includes the 24-column header row followed by one row
/// per entry.
#[must_use]
pub fn write_qso_log(entries: &[QsoEntry]) -> Vec<u8> {
    let mut text = String::new();

    // Header row (matching firmware column names)
    text.push_str(
        "TX/RX\tDate\tFrequency\tMode\t\
         My Latitude\tMy Longitude\tMy Altitude\t\
         RF Power\tS Meter\tCaller\tMemo\tCalled\t\
         RPT1\tRPT2\tMessage\tRepeater Control\t\
         BK\tEMR\tFast Data\t\
         Latitude\tLongitude\tAltitude\tCourse\tSpeed\r\n",
    );

    // Data rows
    for e in entries {
        text.push_str(&e.tx_rx);
        text.push('\t');
        text.push_str(&e.date);
        text.push('\t');
        text.push_str(&e.frequency);
        text.push('\t');
        text.push_str(&e.mode);
        text.push('\t');
        text.push_str(&e.my_latitude);
        text.push('\t');
        text.push_str(&e.my_longitude);
        text.push('\t');
        text.push_str(&e.my_altitude);
        text.push('\t');
        text.push_str(&e.rf_power);
        text.push('\t');
        text.push_str(&e.s_meter);
        text.push('\t');
        text.push_str(&e.caller);
        text.push('\t');
        text.push_str(&e.memo);
        text.push('\t');
        text.push_str(&e.called);
        text.push('\t');
        text.push_str(&e.rpt1);
        text.push('\t');
        text.push_str(&e.rpt2);
        text.push('\t');
        text.push_str(&e.message);
        text.push('\t');
        text.push_str(&e.repeater_control);
        text.push('\t');
        text.push_str(&e.bk);
        text.push('\t');
        text.push_str(&e.emr);
        text.push('\t');
        text.push_str(&e.fast_data);
        text.push('\t');
        text.push_str(&e.latitude);
        text.push('\t');
        text.push_str(&e.longitude);
        text.push('\t');
        text.push_str(&e.altitude);
        text.push('\t');
        text.push_str(&e.course);
        text.push('\t');
        text.push_str(&e.speed);
        text.push_str("\r\n");
    }

    text.into_bytes()
}

#[cfg(test)]
mod tests {
    use super::*;

    type TestResult = Result<(), Box<dyn std::error::Error>>;

    #[test]
    fn parse_empty_log() -> TestResult {
        let data = b"TX/RX\tDate\tFrequency\tMode\t\
            My Latitude\tMy Longitude\tMy Altitude\t\
            RF Power\tS Meter\tCaller\tMemo\tCalled\t\
            RPT1\tRPT2\tMessage\tRepeater Control\t\
            BK\tEMR\tFast Data\t\
            Latitude\tLongitude\tAltitude\tCourse\tSpeed\r\n";
        let entries = parse_qso_log(data)?;
        assert!(entries.is_empty());
        Ok(())
    }

    #[test]
    fn parse_too_few_columns() {
        let data = b"TX/RX\tDate\tFrequency\n\
            TX\t2026/03/28\n";
        let result = parse_qso_log(data);
        assert!(
            matches!(result, Err(SdCardError::ColumnCount { line: 2, .. })),
            "expected ColumnCount error for line 2, got {result:?}"
        );
    }
}
