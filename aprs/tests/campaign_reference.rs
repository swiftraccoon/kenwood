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
    AprsData, AprsFrameHeader, AprsSymbol, CompressedPositionText, Course, Latitude, Longitude,
    MiceMessage, MiceReport, MiceSpeed, MiceStatusText, PositionReportText, Speed, StatusText,
    Tocall,
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
    let pkt = AprsFrameHeader::new(source()?, Tocall::th_d75(), DigipeaterPath::empty())
        .position_report_packet(
            Latitude::new(LAT)?,
            Longitude::new(LON)?,
            AprsSymbol::CAR,
            &reference_text()?,
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
    let pkt = AprsFrameHeader::new(source()?, Tocall::th_d75(), DigipeaterPath::empty())
        .position_compressed_packet(
            Latitude::new(LAT)?,
            Longitude::new(LON)?,
            AprsSymbol::CAR,
            &compressed_reference_text()?,
        );
    assert_eq!(
        pkt.information().first().copied().ok_or("empty info")?,
        b'!'
    );
    let parsed = AprsData::from_bytes(pkt.information())?;
    assert!(
        matches!(&parsed, AprsData::Position(p)
            if (p.latitude().as_degrees() - LAT).abs() < 0.001 && (p.longitude().as_degrees() - LON).abs() < 0.001),
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
    let report = MiceReport::new(
        Latitude::new(LAT)?,
        Longitude::new(LON)?,
        MiceSpeed::new(25)?,
        Course::new(90)?,
        AprsSymbol::CAR,
        mice_reference_text()?,
    )?
    .with_message(MiceMessage::OffDuty);
    let pkt = report.to_packet(source()?, DigipeaterPath::empty());
    let dest = pkt.header.destination.to_string();
    let parsed = AprsData::from_bytes_with_destination(pkt.information(), &dest)?;
    assert!(
        matches!(&parsed, AprsData::Position(p)
            if (p.latitude().as_degrees() - LAT).abs() < 0.01
                && (p.longitude().as_degrees() - LON).abs() < 0.01
                && p.speed().map(Speed::as_knots) == Some(25.0)
                && p.course().map(Course::as_degrees) == Some(90)),
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
    let pkt = AprsFrameHeader::new(source()?, Tocall::th_d75(), DigipeaterPath::empty())
        .status_packet(&text);
    assert_eq!(pkt.information(), b">ref\r".to_vec());
    println!(
        "status info: {}",
        String::from_utf8_lossy(pkt.information())
    );
    Ok(())
}
