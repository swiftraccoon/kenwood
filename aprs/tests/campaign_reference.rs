//! Reference payloads for APRS on-air validation.
//!
//! Builds each transmit format an on-air validation session exercises
//! with canonical inputs, pins the encodings (exact bytes where the
//! format is deterministic, parse-roundtrip where not), and prints the
//! payloads for byte-level comparison against what APRS-IS receives.
//!
//! Show the reference strings with:
//! `cargo nextest run -p aprs --test campaign_reference --no-capture`

use aprs::{
    AprsData, AprsSymbol, CompressedPositionText, Course, Latitude, Longitude, MiceMessage,
    MiceSpeed, MiceStatusText, PositionReportText, StatusText, build_aprs_mice_with_message_packet,
    build_aprs_position_compressed_packet, build_aprs_position_report_packet,
    build_aprs_status_packet, parse_aprs_data, parse_aprs_data_full,
};
use ax25_codec::{Ax25Address, DigipeaterPath};
use kiss_tnc as _;
use proptest as _;
use thiserror as _;

type TestResult = Result<(), Box<dyn std::error::Error>>;

const LAT: f64 = 35.30;
const LON: f64 = -82.46;

fn source() -> Result<Ax25Address, Box<dyn std::error::Error>> {
    Ok(Ax25Address::new("N0CALL", 7)?)
}

fn reference_text() -> Result<PositionReportText, Box<dyn std::error::Error>> {
    Ok(PositionReportText::new("ref")?)
}

fn compressed_reference_text() -> Result<CompressedPositionText, Box<dyn std::error::Error>> {
    Ok(CompressedPositionText::new("ref")?)
}

fn mice_reference_text() -> Result<MiceStatusText, Box<dyn std::error::Error>> {
    Ok(MiceStatusText::new("ref")?)
}

#[test]
fn uncompressed_reference_is_exact() -> TestResult {
    let pkt = build_aprs_position_report_packet(
        &source()?,
        Latitude::new(LAT)?,
        Longitude::new(LON)?,
        AprsSymbol::CAR,
        &reference_text()?,
        &DigipeaterPath::empty(),
    );
    assert_eq!(pkt.information(), b"!3518.00N/08227.60W>ref".to_vec());
    println!(
        "uncompressed info: {}",
        String::from_utf8_lossy(pkt.information())
    );
    Ok(())
}

#[test]
fn compressed_reference_roundtrips() -> TestResult {
    let pkt = build_aprs_position_compressed_packet(
        &source()?,
        Latitude::new(LAT)?,
        Longitude::new(LON)?,
        AprsSymbol::CAR,
        &compressed_reference_text()?,
        &DigipeaterPath::empty(),
    );
    assert_eq!(
        pkt.information().first().copied().ok_or("empty info")?,
        b'!'
    );
    let parsed = parse_aprs_data(pkt.information())?;
    assert!(
        matches!(&parsed, AprsData::Position(p)
            if (p.latitude - LAT).abs() < 0.001 && (p.longitude - LON).abs() < 0.001),
        "compressed roundtrip drifted: {parsed:?}"
    );
    println!(
        "compressed info: {}",
        String::from_utf8_lossy(pkt.information())
    );
    Ok(())
}

#[test]
fn mice_reference_roundtrips() -> TestResult {
    let pkt = build_aprs_mice_with_message_packet(
        &source()?,
        Latitude::new(LAT)?,
        Longitude::new(LON)?,
        MiceSpeed::new(25)?,
        Course::new(90)?,
        MiceMessage::OffDuty,
        AprsSymbol::CAR,
        &mice_reference_text()?,
        &DigipeaterPath::empty(),
    );
    let dest = pkt.destination.to_string();
    let parsed = parse_aprs_data_full(pkt.information(), &dest)?;
    assert!(
        matches!(&parsed, AprsData::Position(p)
            if (p.latitude - LAT).abs() < 0.01
                && (p.longitude - LON).abs() < 0.01
                && p.speed_knots == Some(25)
                && p.course_degrees == Some(90)),
        "Mic-E roundtrip drifted: {parsed:?}"
    );
    println!("mice destination: {dest}");
    println!("mice info: {}", String::from_utf8_lossy(pkt.information()));
    Ok(())
}

#[test]
fn status_reference_is_exact() -> TestResult {
    // The trailing carriage return is part of the documented status
    // wire format (`>text\r`), so the on-air comparison must expect it.
    let text = StatusText::new("ref")?;
    let pkt = build_aprs_status_packet(&source()?, &text, &DigipeaterPath::empty());
    assert_eq!(pkt.information(), b">ref\r".to_vec());
    println!(
        "status info: {}",
        String::from_utf8_lossy(pkt.information())
    );
    Ok(())
}
