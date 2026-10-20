//! Property-based round-trip tests for the AX.25 / APRS codec.
//!
//! Every parser the library exposes is paired with a builder. This file
//! generates arbitrary well-formed inputs and checks `parse(build(x)) ==
//! x` for each layer.
//!
//! These properties also exercise the composition of AX.25 and APRS
//! codecs with KISS framing through the radio integration helpers.

use proptest::prelude::*;
use proptest::test_runner::TestCaseError;

use aprs::{
    AprsData, AprsFrameHeader, AprsPosition, AprsPositionlessWeatherReport, AprsSymbol,
    AprsWeather, AprsWeatherTimestamp, BarometricPressure, CompressedPositionText, Course,
    DirectMessageAddressee, Fahrenheit, Humidity, Latitude, Longitude, Luminosity, MessageId,
    MessageText, MiceMessage, MiceReport, MiceSpeed, MiceStatusText, PositionReportText,
    StatusText, ThreeDigitWeatherValue, Tocall, WeatherComment, WindDirection,
};
use ax25_codec::{
    Ax25Address, Ax25Header, Ax25Packet, Ax25Pid, CommandResponse, DigipeaterPath, PollFinal,
    RouteEntry,
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
use serde_json as _;
use thiserror as _;
use tokio as _;
use tokio_serial as _;
use tracing as _;

/// Convert any debug-printable error into a `TestCaseError` so `?` can be used
/// in proptest blocks without violating workspace `unwrap_used` policy.
fn to_test_err<E: std::fmt::Debug>(e: E) -> TestCaseError {
    TestCaseError::fail(format!("{e:?}"))
}

// ---------------------------------------------------------------------------
// Strategies
// ---------------------------------------------------------------------------

fn arb_callsign() -> impl Strategy<Value = String> {
    prop::collection::vec(
        prop::sample::select(vec![
            b'A', b'B', b'C', b'D', b'E', b'F', b'G', b'H', b'I', b'J', b'K', b'L', b'M', b'N',
            b'O', b'P', b'Q', b'R', b'S', b'T', b'U', b'V', b'W', b'X', b'Y', b'Z', b'0', b'1',
            b'2', b'3', b'4', b'5', b'6', b'7', b'8', b'9',
        ]),
        1..=6,
    )
    .prop_filter_map("invalid utf-8 callsign", |bytes| {
        String::from_utf8(bytes).ok()
    })
}

fn arb_ssid() -> impl Strategy<Value = u8> {
    0u8..=15
}

fn arb_ax25_address() -> impl Strategy<Value = Ax25Address> {
    (arb_callsign(), arb_ssid())
        .prop_filter_map("Ax25Address::new", |(c, s)| Ax25Address::new(&c, s).ok())
}

fn arb_route_entry() -> impl Strategy<Value = RouteEntry> {
    (arb_callsign(), arb_ssid(), any::<bool>()).prop_filter_map(
        "RouteEntry::new",
        |(c, s, has_repeated)| {
            let mut r = RouteEntry::new(&c, s).ok()?;
            r.has_repeated = has_repeated;
            Some(r)
        },
    )
}

fn arb_digi_path() -> impl Strategy<Value = DigipeaterPath> {
    prop::collection::vec(arb_route_entry(), 0..=4)
        .prop_filter_map("valid digipeater path", |path| {
            DigipeaterPath::new(path).ok()
        })
}

fn arb_latitude() -> impl Strategy<Value = f64> {
    -89.9f64..=89.9
}

fn arb_longitude() -> impl Strategy<Value = f64> {
    -179.9f64..=179.9
}

fn arb_message_text() -> impl Strategy<Value = MessageText> {
    "[ -~]{0,40}".prop_filter_map("valid APRS message text", |text: String| {
        MessageText::new(&text).ok()
    })
}

fn arb_message_addressee() -> impl Strategy<Value = DirectMessageAddressee> {
    "[A-Z0-9-]{3,9}".prop_filter_map("valid APRS addressee", |value: String| {
        DirectMessageAddressee::new(&value).ok()
    })
}

fn arb_message_id() -> impl Strategy<Value = MessageId> {
    "[A-Za-z0-9]{1,5}".prop_filter_map("valid APRS message ID", |value: String| {
        MessageId::new(&value).ok()
    })
}

fn arb_status_text() -> impl Strategy<Value = StatusText> {
    "[ -~]{0,40}".prop_filter_map("valid APRS status text", |text: String| {
        StatusText::new(&text).ok()
    })
}

fn arb_wind_direction() -> impl Strategy<Value = WindDirection> {
    (0u16..=WindDirection::MAX).prop_filter_map("valid wind direction", |value| {
        WindDirection::new(value).ok()
    })
}

fn arb_three_digit_weather_value() -> impl Strategy<Value = ThreeDigitWeatherValue> {
    (0u16..=ThreeDigitWeatherValue::MAX)
        .prop_filter_map("valid three-digit weather value", |value| {
            ThreeDigitWeatherValue::new(value).ok()
        })
}

fn arb_fahrenheit() -> impl Strategy<Value = Fahrenheit> {
    (Fahrenheit::MIN..=Fahrenheit::MAX).prop_filter_map("valid Fahrenheit temperature", |value| {
        Fahrenheit::new(value).ok()
    })
}

fn arb_humidity() -> impl Strategy<Value = Humidity> {
    (Humidity::MIN..=Humidity::MAX)
        .prop_filter_map("valid humidity", |value| Humidity::new(value).ok())
}

fn arb_pressure() -> impl Strategy<Value = BarometricPressure> {
    (0u32..=BarometricPressure::MAX).prop_filter_map("valid pressure", |value| {
        BarometricPressure::new(value).ok()
    })
}

fn arb_luminosity() -> impl Strategy<Value = Luminosity> {
    (0u16..=Luminosity::MAX)
        .prop_filter_map("valid luminosity", |value| Luminosity::new(value).ok())
}

fn arb_weather_comment() -> impl Strategy<Value = WeatherComment> {
    let safe_leading = (b' '..=b'~')
        .filter(|byte| {
            !matches!(
                byte,
                b'c' | b's' | b'g' | b't' | b'r' | b'p' | b'P' | b'h' | b'b' | b'L' | b'l'
            )
        })
        .map(char::from)
        .collect::<Vec<_>>();
    prop::option::of((prop::sample::select(safe_leading), "[ -~]{0,40}")).prop_filter_map(
        "valid APRS weather comment",
        |comment| {
            let value = comment.map_or_else(String::new, |(first, tail)| format!("{first}{tail}"));
            WeatherComment::new(&value).ok()
        },
    )
}

fn arb_weather() -> impl Strategy<Value = AprsWeather> {
    (
        prop::option::of(arb_wind_direction()),
        prop::option::of(arb_three_digit_weather_value()),
        prop::option::of(arb_three_digit_weather_value()),
        prop::option::of(arb_fahrenheit()),
        prop::option::of(arb_three_digit_weather_value()),
        prop::option::of(arb_three_digit_weather_value()),
        prop::option::of(arb_three_digit_weather_value()),
        prop::option::of(arb_humidity()),
        prop::option::of(arb_pressure()),
        prop::option::of(arb_luminosity()),
    )
        .prop_map(|(wd, ws, g, t, r1, r24, rm, h, b, luminosity)| {
            let mut weather = AprsWeather::new();
            weather.set_wind_direction(wd);
            weather.set_wind_speed(ws);
            weather.set_wind_gust(g);
            weather.set_temperature(t);
            weather.set_rain_1h(r1);
            weather.set_rain_24h(r24);
            weather.set_rain_since_midnight(rm);
            weather.set_humidity(h);
            weather.set_pressure(b);
            weather.set_luminosity(luminosity);
            weather
        })
}

// ---------------------------------------------------------------------------
// AX.25 encode/parse round-trip
// ---------------------------------------------------------------------------

proptest! {
    #[test]
    fn ax25_packet_roundtrip(
        source in arb_ax25_address(),
        dest in arb_ax25_address(),
        digis in arb_digi_path(),
        info in prop::collection::vec(any::<u8>(), 1..100),
    ) {
        let packet = Ax25Packet::unnumbered_information(
            Ax25Header {
                source: source.clone(),
                destination: dest.clone(),
                digipeaters: digis.clone(),
                command_response: CommandResponse::Command,
            },
            PollFinal::Clear,
            Ax25Pid::NoLayer3,
            info.clone(),
        );
        let bytes = packet.to_bytes();
        let parsed = Ax25Packet::from_bytes(&bytes).map_err(to_test_err)?;
        prop_assert_eq!(parsed.header.source.callsign.as_str(), source.callsign.as_str());
        prop_assert_eq!(parsed.header.source.ssid, source.ssid);
        prop_assert_eq!(
            parsed.header.destination.callsign.as_str(),
            dest.callsign.as_str()
        );
        prop_assert_eq!(parsed.header.destination.ssid, dest.ssid);
        prop_assert_eq!(parsed.header.digipeaters.len(), digis.len());
        prop_assert_eq!(parsed.information(), info);
    }
}

// ---------------------------------------------------------------------------
// Uncompressed position round-trip
// ---------------------------------------------------------------------------

proptest! {
    #[test]
    fn position_uncompressed_roundtrip(
        source in arb_ax25_address(),
        lat in arb_latitude(),
        lon in arb_longitude(),
    ) {
        let latitude = Latitude::new(lat).map_err(to_test_err)?;
        let longitude = Longitude::new(lon).map_err(to_test_err)?;
        let packet = AprsFrameHeader::new(source, Tocall::th_d75(), DigipeaterPath::empty()).position_report_packet(
            latitude,
            longitude,
            AprsSymbol::CAR,
            &PositionReportText::default(),
        );
        let wire = ax25_to_kiss_wire(&packet);
        let kiss = KissFrame::from_bytes(&wire).map_err(to_test_err)?;
        let parsed_packet = Ax25Packet::from_bytes(&kiss.data).map_err(to_test_err)?;
        let parsed = AprsPosition::from_bytes(parsed_packet.information()).map_err(to_test_err)?;
        prop_assert!((parsed.latitude().as_degrees() - lat).abs() < 0.02,
            "lat roundtrip failed: in {lat}, out {}", parsed.latitude());
        prop_assert!((parsed.longitude().as_degrees() - lon).abs() < 0.02,
            "lon roundtrip failed: in {lon}, out {}", parsed.longitude());
    }
}

// ---------------------------------------------------------------------------
// Compressed position round-trip
// ---------------------------------------------------------------------------

proptest! {
    #[test]
    fn position_compressed_roundtrip(
        source in arb_ax25_address(),
        lat in arb_latitude(),
        lon in arb_longitude(),
    ) {
        let latitude = Latitude::new(lat).map_err(to_test_err)?;
        let longitude = Longitude::new(lon).map_err(to_test_err)?;
        let packet = AprsFrameHeader::new(source, Tocall::th_d75(), DigipeaterPath::empty()).position_compressed_packet(
            latitude,
            longitude,
            AprsSymbol::CAR,
            &CompressedPositionText::default(),
        );
        let wire = ax25_to_kiss_wire(&packet);
        let kiss = KissFrame::from_bytes(&wire).map_err(to_test_err)?;
        let parsed_packet = Ax25Packet::from_bytes(&kiss.data).map_err(to_test_err)?;
        let parsed = AprsPosition::from_bytes(parsed_packet.information()).map_err(to_test_err)?;
        prop_assert!((parsed.latitude().as_degrees() - lat).abs() < 0.1);
        prop_assert!((parsed.longitude().as_degrees() - lon).abs() < 0.1);
    }
}

// ---------------------------------------------------------------------------
// Mic-E round-trip (message bits + position)
// ---------------------------------------------------------------------------

proptest! {
    #[test]
    fn mice_roundtrip(
        source in arb_ax25_address(),
        lat in arb_latitude(),
        lon in arb_longitude(),
        message in prop::sample::select(vec![
            MiceMessage::OffDuty,
            MiceMessage::EnRoute,
            MiceMessage::InService,
            MiceMessage::Returning,
            MiceMessage::Committed,
            MiceMessage::Special,
            MiceMessage::Priority,
            MiceMessage::Emergency,
        ]),
    ) {
        let latitude = Latitude::new(lat).map_err(to_test_err)?;
        let longitude = Longitude::new(lon).map_err(to_test_err)?;
        let report = MiceReport::new(
            latitude,
            longitude,
            MiceSpeed::new(0).map_err(to_test_err)?,
            Course::new(0).map_err(to_test_err)?,
            AprsSymbol::CAR,
            MiceStatusText::default(),
        ).map_err(to_test_err)?.with_message(message);
        let packet = report.to_packet(source, DigipeaterPath::empty());
        let wire = ax25_to_kiss_wire(&packet);
        let kiss = KissFrame::from_bytes(&wire).map_err(to_test_err)?;
        let parsed_packet = Ax25Packet::from_bytes(&kiss.data).map_err(to_test_err)?;
        let parsed = AprsPosition::from_mice_bytes(
            parsed_packet.header.destination.callsign.as_str(),
            parsed_packet.information(),
        ).map_err(to_test_err)?;
        prop_assert_eq!(parsed.mice_message(), Some(message));
        // Mic-E encodes position to 0.01 minute → ~18 metre precision.
        prop_assert!((parsed.latitude().as_degrees() - lat).abs() < 0.02);
        prop_assert!((parsed.longitude().as_degrees() - lon).abs() < 0.02);
    }
}

// ---------------------------------------------------------------------------
// Weather (positionless) round-trip
// ---------------------------------------------------------------------------

proptest! {
    #[test]
    fn weather_positionless_roundtrip(
        source in arb_ax25_address(),
        wx in arb_weather(),
        comment in arb_weather_comment(),
    ) {
        let timestamp = AprsWeatherTimestamp::month_day_hour_minute_utc(10, 9, 23, 45)
            .map_err(to_test_err)?;
        let report = AprsPositionlessWeatherReport::with_comment(
            timestamp,
            wx.clone(),
            comment,
        );
        let packet = AprsFrameHeader::new(source, Tocall::th_d75(), DigipeaterPath::empty()).weather_packet(&report);
        let wire = ax25_to_kiss_wire(&packet);
        let kiss = KissFrame::from_bytes(&wire).map_err(to_test_err)?;
        let parsed_packet = Ax25Packet::from_bytes(&kiss.data).map_err(to_test_err)?;
        let data = AprsData::from_bytes(parsed_packet.information()).map_err(to_test_err)?;
        let AprsData::PositionlessWeather(parsed) = data else {
            prop_assert!(false, "expected positionless weather variant");
            return Ok(());
        };
        prop_assert_eq!(parsed.timestamp(), report.timestamp());
        prop_assert_eq!(parsed.comment(), report.comment());
        prop_assert_eq!(parsed.weather(), &wx);
    }
}

// ---------------------------------------------------------------------------
// Message round-trip
// ---------------------------------------------------------------------------

proptest! {
    #[test]
    fn message_roundtrip(
        source in arb_ax25_address(),
        addressee in arb_message_addressee(),
        text in arb_message_text(),
        msg_id in prop::option::of(arb_message_id()),
    ) {
        let packet = AprsFrameHeader::new(source, Tocall::th_d75(), DigipeaterPath::empty()).message_packet(
            &addressee,
            &text,
            msg_id.as_ref(),
        );
        let wire = ax25_to_kiss_wire(&packet);
        let kiss = KissFrame::from_bytes(&wire).map_err(to_test_err)?;
        let parsed_packet = Ax25Packet::from_bytes(&kiss.data).map_err(to_test_err)?;
        let data = AprsData::from_bytes(parsed_packet.information()).map_err(to_test_err)?;
        let AprsData::Message(parsed) = data else {
            prop_assert!(false, "expected message variant");
            return Ok(());
        };
        prop_assert_eq!(parsed.addressee().as_str(), addressee.as_str());
        prop_assert_eq!(parsed.text(), text.as_str());
        prop_assert_eq!(parsed.message_id().map(MessageId::as_str), msg_id.as_ref().map(MessageId::as_str));
    }
}

// ---------------------------------------------------------------------------
// Status round-trip
// ---------------------------------------------------------------------------

proptest! {
    #[test]
    fn status_roundtrip(
        source in arb_ax25_address(),
        text in arb_status_text(),
    ) {
        let packet = AprsFrameHeader::new(source, Tocall::th_d75(), DigipeaterPath::empty()).status_packet(&text);
        let wire = ax25_to_kiss_wire(&packet);
        let kiss = KissFrame::from_bytes(&wire).map_err(to_test_err)?;
        let parsed_packet = Ax25Packet::from_bytes(&kiss.data).map_err(to_test_err)?;
        let data = AprsData::from_bytes(parsed_packet.information()).map_err(to_test_err)?;
        let AprsData::Status(parsed) = data else {
            prop_assert!(false, "expected status variant");
            return Ok(());
        };
        // The APRS status wire format is ambiguous: text that begins
        // with a 7-char timestamp or a Maidenhead grid + `/symbol` is,
        // per spec, indistinguishable from a structured status, so the
        // parser reinterprets it. The plain-text round-trip property
        // only holds when no structured prefix was detected.
        //
        // The builder appends the APRS `\r` terminator and the parser
        // strips only *trailing* whitespace (that `\r` plus any right
        // padding); leading whitespace is preserved as content. The
        // comparison is therefore `trim_end`, not `trim`.
        if parsed.timestamp().is_none() && parsed.grid_locator().is_none() {
            prop_assert_eq!(parsed.text(), text.as_str().trim_end());
        }
    }
}

/// Status parsing preserves leading content whitespace while removing its
/// trailing wire terminator and padding.
#[test]
fn status_roundtrip_preserves_leading_whitespace() -> Result<(), Box<dyn std::error::Error>> {
    let source = Ax25Address::new("A", 0)?;
    let text = StatusText::new(" A")?;
    let packet = AprsFrameHeader::new(source, Tocall::th_d75(), DigipeaterPath::empty())
        .status_packet(&text);
    let wire = ax25_to_kiss_wire(&packet);
    let kiss = KissFrame::from_bytes(&wire)?;
    let parsed_packet = Ax25Packet::from_bytes(&kiss.data)?;
    let data = AprsData::from_bytes(parsed_packet.information())?;
    let AprsData::Status(parsed) = data else {
        return Err("expected status variant".into());
    };
    assert_eq!(
        parsed.text(),
        " A",
        "leading whitespace is status content, not strippable padding"
    );
    Ok(())
}
