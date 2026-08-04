//! Property-based round-trip tests for the AX.25 / APRS codec.
//!
//! Every parser the library exposes is paired with a builder. This file
//! generates arbitrary well-formed inputs and checks `parse(build(x)) ==
//! x` for each layer.
//!
//! The pure-KISS codec round-trip lives in `kiss-tnc/tests/roundtrip.rs`
//! (extracted in PR 1 of the KISS / AX.25 / APRS split). AX.25 and
//! APRS round-trips stay here until those layers are extracted in
//! PRs 2 and 3.

use proptest::prelude::*;
use proptest::test_runner::TestCaseError;

use aprs::{
    AprsData, AprsPosition, AprsPositionlessWeatherReport, AprsSymbol, AprsWeather,
    AprsWeatherTimestamp, BarometricPressure, CompressedPositionText, Course, Fahrenheit, Humidity,
    Latitude, Longitude, Luminosity, MessageAddressee, MessageId, MessageText, MiceMessage,
    MiceSpeed, MiceStatusText, PositionReportText, StatusText, ThreeDigitWeatherValue,
    WeatherComment, WindDirection, build_aprs_message_packet, build_aprs_mice_with_message_packet,
    build_aprs_position_compressed_packet, build_aprs_position_report_packet,
    build_aprs_status_packet, build_aprs_weather_packet, parse_aprs_data, parse_aprs_position,
    parse_mice_position,
};
use ax25_codec::{
    Ax25Address, Ax25Packet, Ax25Pid, CommandResponse, DigipeaterPath, RouteEntry, build_ax25,
    parse_ax25,
};
use kenwood_thd75::aprs::ax25_to_kiss_wire;
use kiss_tnc::decode_kiss_frame;

// Deps visible to every kenwood-thd75 test target but unused here.
// Acknowledged so `unused_crate_dependencies` stays silent without
// weakening the lint.
use aprs_is as _;
use dstar_gateway_core as _;
use encoding_rs as _;
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

fn arb_message_addressee() -> impl Strategy<Value = MessageAddressee> {
    "[A-Z0-9-]{3,9}".prop_filter_map("valid APRS addressee", |value: String| {
        MessageAddressee::new(&value).ok()
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
            source.clone(),
            dest.clone(),
            digis.clone(),
            CommandResponse::Command,
            false,
            Ax25Pid::NoLayer3,
            info.clone(),
        );
        let bytes = build_ax25(&packet);
        let parsed = parse_ax25(&bytes).map_err(to_test_err)?;
        prop_assert_eq!(parsed.source.callsign.as_str(), source.callsign.as_str());
        prop_assert_eq!(parsed.source.ssid, source.ssid);
        prop_assert_eq!(
            parsed.destination.callsign.as_str(),
            dest.callsign.as_str()
        );
        prop_assert_eq!(parsed.destination.ssid, dest.ssid);
        prop_assert_eq!(parsed.digipeaters.len(), digis.len());
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
        let packet = build_aprs_position_report_packet(
            &source,
            latitude,
            longitude,
            AprsSymbol::CAR,
            &PositionReportText::default(),
            &DigipeaterPath::empty(),
        );
        let wire = ax25_to_kiss_wire(&packet);
        let kiss = decode_kiss_frame(&wire).map_err(to_test_err)?;
        let parsed_packet = parse_ax25(&kiss.data).map_err(to_test_err)?;
        let parsed: AprsPosition = parse_aprs_position(parsed_packet.information()).map_err(to_test_err)?;
        prop_assert!((parsed.latitude - lat).abs() < 0.02,
            "lat roundtrip failed: in {lat}, out {}", parsed.latitude);
        prop_assert!((parsed.longitude - lon).abs() < 0.02,
            "lon roundtrip failed: in {lon}, out {}", parsed.longitude);
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
        let packet = build_aprs_position_compressed_packet(
            &source,
            latitude,
            longitude,
            AprsSymbol::CAR,
            &CompressedPositionText::default(),
            &DigipeaterPath::empty(),
        );
        let wire = ax25_to_kiss_wire(&packet);
        let kiss = decode_kiss_frame(&wire).map_err(to_test_err)?;
        let parsed_packet = parse_ax25(&kiss.data).map_err(to_test_err)?;
        let parsed: AprsPosition = parse_aprs_position(parsed_packet.information()).map_err(to_test_err)?;
        // Compressed is less precise than uncompressed, so allow more slop.
        prop_assert!((parsed.latitude - lat).abs() < 0.1);
        prop_assert!((parsed.longitude - lon).abs() < 0.1);
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
        let packet = build_aprs_mice_with_message_packet(
            &source,
            latitude,
            longitude,
            MiceSpeed::new(0).map_err(to_test_err)?,
            Course::new(0).map_err(to_test_err)?,
            message,
            AprsSymbol::CAR,
            &MiceStatusText::default(),
            &DigipeaterPath::empty(),
        );
        let wire = ax25_to_kiss_wire(&packet);
        let kiss = decode_kiss_frame(&wire).map_err(to_test_err)?;
        let parsed_packet = parse_ax25(&kiss.data).map_err(to_test_err)?;
        let parsed = parse_mice_position(
            &parsed_packet.destination.callsign,
            parsed_packet.information(),
        ).map_err(to_test_err)?;
        prop_assert_eq!(parsed.mice_message, Some(message));
        // Mic-E encodes position to 0.01 minute → ~18 metre precision.
        prop_assert!((parsed.latitude - lat).abs() < 0.02);
        prop_assert!((parsed.longitude - lon).abs() < 0.02);
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
        let packet = build_aprs_weather_packet(&source, &report, &DigipeaterPath::empty());
        let wire = ax25_to_kiss_wire(&packet);
        let kiss = decode_kiss_frame(&wire).map_err(to_test_err)?;
        let parsed_packet = parse_ax25(&kiss.data).map_err(to_test_err)?;
        let data = parse_aprs_data(parsed_packet.information()).map_err(to_test_err)?;
        let AprsData::PositionlessWeather(parsed) = data else {
            prop_assert!(false, "expected positionless weather variant");
            return Ok(());
        };
        prop_assert_eq!(parsed.timestamp, report.timestamp);
        prop_assert_eq!(parsed.comment(), report.comment());
        prop_assert_eq!(&parsed.weather, &wx);
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
        let packet = build_aprs_message_packet(
            &source,
            &addressee,
            &text,
            msg_id.as_ref(),
            &DigipeaterPath::empty(),
        );
        let wire = ax25_to_kiss_wire(&packet);
        let kiss = decode_kiss_frame(&wire).map_err(to_test_err)?;
        let parsed_packet = parse_ax25(&kiss.data).map_err(to_test_err)?;
        let data = parse_aprs_data(parsed_packet.information()).map_err(to_test_err)?;
        let AprsData::Message(parsed) = data else {
            prop_assert!(false, "expected message variant");
            return Ok(());
        };
        prop_assert_eq!(parsed.addressee.as_str(), addressee.as_str());
        prop_assert_eq!(parsed.text.as_str(), text.as_str());
        prop_assert_eq!(parsed.message_id.as_deref(), msg_id.as_ref().map(MessageId::as_str));
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
        let packet = build_aprs_status_packet(&source, &text, &DigipeaterPath::empty());
        let wire = ax25_to_kiss_wire(&packet);
        let kiss = decode_kiss_frame(&wire).map_err(to_test_err)?;
        let parsed_packet = parse_ax25(&kiss.data).map_err(to_test_err)?;
        let data = parse_aprs_data(parsed_packet.information()).map_err(to_test_err)?;
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
        if parsed.timestamp.is_none() && parsed.grid_locator.is_none() {
            prop_assert_eq!(parsed.text.as_str(), text.as_str().trim_end());
        }
    }
}

/// Regression: a status whose text carries leading whitespace must
/// round-trip with that space intact. `parse_aprs_status` strips only
/// trailing whitespace (the `\r` terminator and right padding);
/// leading whitespace is content, as the parser's own grid+comment
/// handling relies on. The `status_roundtrip` proptest previously
/// compared against `text.trim()`, which also stripped the leading
/// space and failed for minimal inputs such as `" A"`.
#[test]
fn status_roundtrip_preserves_leading_whitespace() -> Result<(), Box<dyn std::error::Error>> {
    let source = Ax25Address::new("A", 0)?;
    let text = StatusText::new(" A")?;
    let packet = build_aprs_status_packet(&source, &text, &DigipeaterPath::empty());
    let wire = ax25_to_kiss_wire(&packet);
    let kiss = decode_kiss_frame(&wire)?;
    let parsed_packet = parse_ax25(&kiss.data)?;
    let data = parse_aprs_data(parsed_packet.information())?;
    let AprsData::Status(parsed) = data else {
        return Err("expected status variant".into());
    };
    assert_eq!(
        parsed.text, " A",
        "leading whitespace is status content, not strippable padding"
    );
    Ok(())
}
