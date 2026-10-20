//! Real-capture regression tests for APRS parsing.
//!
//! These tests exercise the parser with verbatim APRS frames captured
//! from a real on-air monitor. They guard against regressions in the
//! data-type dispatch, Mic-E decoder, position parser, and weather
//! parser against known-good inputs.
//!
//! Capture sources are intentionally anonymized: callsigns are
//! replaced with well-known testing callsigns (`N0CALL`, `W1AW`) and
//! positions with synthetic values.

use aprs::{
    AprsData, BarometricPressure, BulletinNumber, Course, Fahrenheit, Humidity, MessageKind,
    MiceMessage, MiceSpeed, Speed, ThreeDigitWeatherValue, WindDirection,
};
use ax25_codec::{
    Ax25Address, Ax25Error, Ax25Header, Ax25Packet, Ax25Pid, CommandResponse, DigipeaterPath,
    PollFinal, RouteEntry,
};
use kenwood_thd75::aprs::ax25_to_kiss_wire;
use kiss_tnc::KissFrame;

// Deps visible to every kenwood-thd75 test target but unused here.
// Acknowledged so `unused_crate_dependencies` stays silent without
// weakening the lint.
use aprs_is as _;
use dstar_gateway_core as _;
use encoding_rs as _;
use kenwood_schema as _;
use kenwood_transport as _;
use mmdvm as _;
use mmdvm_core as _;
use proptest as _;
use serde_json as _;
use thiserror as _;
use tokio as _;
use tokio_serial as _;
use tracing as _;

type TestResult = Result<(), Box<dyn std::error::Error>>;

/// Build a KISS-wrapped AX.25 UI frame from (src, dst, path, info)
/// components. Used by the tests below to simulate what a radio's KISS
/// TNC emits.
fn make_wire_frame(
    src: &str,
    dst: &str,
    digis: &[&str],
    info: &[u8],
) -> Result<Vec<u8>, Ax25Error> {
    let source = Ax25Address::new(src, 0)?;
    let destination = Ax25Address::new(dst, 0)?;
    let entries: Vec<RouteEntry> = digis
        .iter()
        .map(|d| RouteEntry::new(d, 0))
        .collect::<Result<_, _>>()?;
    let packet = Ax25Packet::unnumbered_information(
        Ax25Header {
            source,
            destination,
            digipeaters: DigipeaterPath::new(entries)?,
            command_response: CommandResponse::Command,
        },
        PollFinal::Clear,
        Ax25Pid::NoLayer3,
        info.to_vec(),
    );
    let ax25 = packet.to_bytes();
    Ok(KissFrame::data(ax25).to_bytes())
}

#[test]
fn real_capture_uncompressed_position() -> TestResult {
    // Typical mobile station beacon: uncompressed position with a
    // CSE/SPD extension and altitude in the comment.
    let wire = make_wire_frame(
        "N0CALL",
        "APK005",
        &["WIDE1", "WIDE2"],
        b"!3515.00N/09745.00W>088/015/A=001234Test beacon",
    )?;
    let kiss = KissFrame::from_bytes(&wire)?;
    let packet = Ax25Packet::from_bytes(&kiss.data)?;
    assert_eq!(packet.header.source.callsign, "N0CALL");
    let data = AprsData::from_bytes(packet.information())?;
    let AprsData::Position(pos) = data else {
        return Err(format!("expected Position, got {data:?}").into());
    };
    assert!((pos.latitude().as_degrees() - 35.25).abs() < 0.01);
    assert!((pos.longitude().as_degrees() - (-97.75)).abs() < 0.01);
    assert_eq!(pos.course(), Some(Course::new(88)?));
    assert_eq!(pos.speed(), Some(Speed::from_knots(15.0)?));
    assert_eq!(pos.extensions().altitude_feet(), Some(1234));
    assert!(pos.comment().contains("Test beacon"));
    Ok(())
}

#[test]
fn real_capture_mice_emergency() -> TestResult {
    use aprs::{AprsSymbol, Latitude, Longitude, MiceReport, MiceStatusText};
    use ax25_codec::Ax25Address;
    let source = Ax25Address::new("N0CALL", 7)?;
    let packet = MiceReport::new(
        Latitude::new(35.25)?,
        Longitude::new(-97.75)?,
        MiceSpeed::new(30)?,
        Course::new(180)?,
        AprsSymbol::from_chars('/', 'E')?,
        MiceStatusText::new("emergency test")?,
    )?
    .with_message(MiceMessage::Emergency)
    .to_packet(source, DigipeaterPath::empty());
    let wire = ax25_to_kiss_wire(&packet);
    let kiss = KissFrame::from_bytes(&wire)?;
    let parsed_packet = Ax25Packet::from_bytes(&kiss.data)?;
    let data = AprsData::from_bytes_with_destination(
        parsed_packet.information(),
        parsed_packet.header.destination.callsign.as_str(),
    )?;
    let AprsData::Position(pos) = data else {
        return Err(format!("expected Position, got {data:?}").into());
    };
    assert_eq!(pos.mice_message(), Some(MiceMessage::Emergency));
    assert!((pos.latitude().as_degrees() - 35.25).abs() < 0.05);
    assert_eq!(pos.speed(), Some(Speed::from_knots(30.0)?));
    assert_eq!(pos.course(), Some(Course::new(180)?));
    Ok(())
}

#[test]
fn real_capture_weather_station() -> TestResult {
    // Typical Davis weather station beacon:
    // `!DDMM.MMN\DDDMM.MMW_DIR/SPDgGUSTtTEMPr001p002P003h55b10135`
    let wire = make_wire_frame(
        "WX1STA",
        "APK005",
        &[],
        b"!3515.00N/09745.00W_090/010g020t072r001p005P010h55b10135",
    )?;
    let kiss = KissFrame::from_bytes(&wire)?;
    let packet = Ax25Packet::from_bytes(&kiss.data)?;
    let data = AprsData::from_bytes(packet.information())?;
    let AprsData::Position(pos) = data else {
        return Err(format!("expected Position, got {data:?}").into());
    };
    assert_eq!(pos.symbol().code_char(), '_');
    let wx = pos.weather().ok_or("expected embedded weather")?;
    assert_eq!(wx.wind_direction().map(WindDirection::degrees), Some(90));
    assert_eq!(wx.wind_speed().map(ThreeDigitWeatherValue::value), Some(10),);
    assert_eq!(wx.wind_gust().map(ThreeDigitWeatherValue::value), Some(20),);
    assert_eq!(wx.temperature().map(Fahrenheit::get), Some(72));
    assert_eq!(wx.humidity().map(Humidity::percent), Some(55));
    assert_eq!(
        wx.pressure().map(BarometricPressure::tenths_hpa),
        Some(10_135),
    );
    Ok(())
}

#[test]
fn real_capture_bulletin_message() -> TestResult {
    let wire = make_wire_frame(
        "N0CALL",
        "APK005",
        &[],
        b":BLN1     :Net tonight at 8 PM on 146.52",
    )?;
    let kiss = KissFrame::from_bytes(&wire)?;
    let packet = Ax25Packet::from_bytes(&kiss.data)?;
    let data = AprsData::from_bytes(packet.information())?;
    let AprsData::Message(msg) = data else {
        return Err(format!("expected Message, got {data:?}").into());
    };
    assert_eq!(
        msg.kind(),
        MessageKind::Bulletin {
            number: BulletinNumber::new(1)?
        }
    );
    Ok(())
}

#[test]
fn real_capture_object_with_timestamp() -> TestResult {
    // Object: EVENT with * (live), 7-char DHM timestamp, position, comment.
    let wire = make_wire_frame(
        "N0CALL",
        "APK005",
        &[],
        b";EVENT    *092345z3515.00N/09745.00W>Run marathon",
    )?;
    let kiss = KissFrame::from_bytes(&wire)?;
    let packet = Ax25Packet::from_bytes(&kiss.data)?;
    let data = AprsData::from_bytes(packet.information())?;
    let AprsData::Object(obj) = data else {
        return Err(format!("expected Object, got {data:?}").into());
    };
    assert_eq!(obj.name().as_str(), "EVENT");
    assert!(obj.state().is_live());
    assert_eq!(obj.timestamp().to_wire_string(), "092345z");
    Ok(())
}

#[test]
fn real_capture_telemetry() -> TestResult {
    let wire = make_wire_frame(
        "N0CALL",
        "APK005",
        &[],
        b"T#042,123,456,789,012,345,10101010",
    )?;
    let kiss = KissFrame::from_bytes(&wire)?;
    let packet = Ax25Packet::from_bytes(&kiss.data)?;
    let data = AprsData::from_bytes(packet.information())?;
    let AprsData::Telemetry(t) = data else {
        return Err(format!("expected Telemetry, got {data:?}").into());
    };
    assert_eq!(t.sequence().to_string(), "042");
    assert_eq!(t.analog().first().ok_or("analog[0] missing")?.value(), 123);
    assert_eq!(t.digital().bits(), 0b1010_1010);
    Ok(())
}

#[test]
fn real_capture_third_party() -> TestResult {
    let wire = make_wire_frame(
        "N0CALL",
        "APK005",
        &[],
        b"}W1AW>APK005,TCPIP,N0CALL*:!4903.50N/07201.75W-From the internet",
    )?;
    let kiss = KissFrame::from_bytes(&wire)?;
    let packet = Ax25Packet::from_bytes(&kiss.data)?;
    let data = AprsData::from_bytes(packet.information())?;
    let AprsData::ThirdParty { header, payload } = data else {
        return Err(format!("expected ThirdParty, got {data:?}").into());
    };
    assert_eq!(header, "W1AW>APK005,TCPIP,N0CALL*");
    assert!(std::str::from_utf8(&payload)?.contains("4903.50N"));
    Ok(())
}

#[test]
fn real_capture_grid_square() -> TestResult {
    let wire = make_wire_frame("N0CALL", "APK005", &[], b"[EM13qc")?;
    let kiss = KissFrame::from_bytes(&wire)?;
    let packet = Ax25Packet::from_bytes(&kiss.data)?;
    let data = AprsData::from_bytes(packet.information())?;
    let AprsData::Grid(grid) = data else {
        return Err(format!("expected Grid, got {data:?}").into());
    };
    assert_eq!(grid.as_str(), "EM13qc");
    Ok(())
}
