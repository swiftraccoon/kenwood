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
    AprsData, AprsPosition, AprsWeather, MiceMessage, build_aprs_message_packet,
    build_aprs_mice_with_message_packet, build_aprs_position_compressed_packet,
    build_aprs_position_report_packet, build_aprs_status_packet, build_aprs_weather_packet,
    parse_aprs_data, parse_aprs_position, parse_mice_position,
};
use ax25_codec::{Ax25Address, Ax25Packet, build_ax25, parse_ax25};
use kenwood_thd75::aprs::ax25_to_kiss_wire;
use kiss_tnc::decode_kiss_frame;

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
    (arb_callsign(), arb_ssid()).prop_map(|(c, s)| Ax25Address::new(&c, s))
}

fn arb_digi_path() -> impl Strategy<Value = Vec<Ax25Address>> {
    prop::collection::vec(arb_ax25_address(), 0..=4)
}

fn arb_latitude() -> impl Strategy<Value = f64> {
    -89.9f64..=89.9
}

fn arb_longitude() -> impl Strategy<Value = f64> {
    -179.9f64..=179.9
}

fn arb_printable_text() -> impl Strategy<Value = String> {
    "[ -~]{0,40}".prop_map(String::from)
}

fn arb_message_addressee() -> impl Strategy<Value = String> {
    "[A-Z0-9-]{3,9}".prop_map(String::from)
}

fn arb_message_id() -> impl Strategy<Value = String> {
    "[A-Za-z0-9]{1,5}".prop_map(String::from)
}

fn arb_weather() -> impl Strategy<Value = AprsWeather> {
    (
        prop::option::of(0u16..=360),
        prop::option::of(0u16..=200),
        prop::option::of(0u16..=300),
        prop::option::of(-99i16..=150),
        prop::option::of(0u16..=999),
        prop::option::of(0u16..=999),
        prop::option::of(0u16..=999),
        prop::option::of(1u8..=99),
        prop::option::of(9500u32..=10600),
    )
        .prop_map(|(wd, ws, g, t, r1, r24, rm, h, b)| AprsWeather {
            wind_direction: wd,
            wind_speed: ws,
            wind_gust: g,
            temperature: t,
            rain_1h: r1,
            rain_24h: r24,
            rain_since_midnight: rm,
            humidity: h,
            pressure: b,
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
        let packet = Ax25Packet {
            source: source.clone(),
            destination: dest.clone(),
            digipeaters: digis.clone(),
            control: 0x03,
            protocol: 0xF0,
            info: info.clone(),
        };
        let bytes = build_ax25(&packet);
        let parsed = parse_ax25(&bytes).map_err(to_test_err)?;
        prop_assert_eq!(parsed.source.callsign, source.callsign);
        prop_assert_eq!(parsed.source.ssid, source.ssid);
        prop_assert_eq!(parsed.destination.callsign, dest.callsign);
        prop_assert_eq!(parsed.destination.ssid, dest.ssid);
        prop_assert_eq!(parsed.digipeaters.len(), digis.len());
        prop_assert_eq!(parsed.info, info);
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
        let packet = build_aprs_position_report_packet(
            &source, lat, lon, '/', '>', "", &[],
        );
        let wire = ax25_to_kiss_wire(&packet);
        let kiss = decode_kiss_frame(&wire).map_err(to_test_err)?;
        let parsed_packet = parse_ax25(&kiss.data).map_err(to_test_err)?;
        let parsed: AprsPosition = parse_aprs_position(&parsed_packet.info).map_err(to_test_err)?;
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
        let packet = build_aprs_position_compressed_packet(
            &source, lat, lon, '/', '>', "", &[],
        );
        let wire = ax25_to_kiss_wire(&packet);
        let kiss = decode_kiss_frame(&wire).map_err(to_test_err)?;
        let parsed_packet = parse_ax25(&kiss.data).map_err(to_test_err)?;
        let parsed: AprsPosition = parse_aprs_position(&parsed_packet.info).map_err(to_test_err)?;
        // Compressed is less precise than uncompressed — allow more slop.
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
        let packet = build_aprs_mice_with_message_packet(
            &source, lat, lon, 0, 0, message, '/', '>', "", &[],
        );
        let wire = ax25_to_kiss_wire(&packet);
        let kiss = decode_kiss_frame(&wire).map_err(to_test_err)?;
        let parsed_packet = parse_ax25(&kiss.data).map_err(to_test_err)?;
        let parsed = parse_mice_position(
            &parsed_packet.destination.callsign,
            &parsed_packet.info,
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
    ) {
        let packet = build_aprs_weather_packet(&source, &wx, &[]);
        let wire = ax25_to_kiss_wire(&packet);
        let kiss = decode_kiss_frame(&wire).map_err(to_test_err)?;
        let parsed_packet = parse_ax25(&kiss.data).map_err(to_test_err)?;
        let data = parse_aprs_data(&parsed_packet.info).map_err(to_test_err)?;
        let AprsData::Weather(parsed) = data else {
            prop_assert!(false, "expected weather variant");
            return Ok(());
        };
        prop_assert_eq!(parsed.wind_direction, wx.wind_direction);
        prop_assert_eq!(parsed.wind_speed, wx.wind_speed);
        prop_assert_eq!(parsed.wind_gust, wx.wind_gust);
        prop_assert_eq!(parsed.temperature, wx.temperature);
        prop_assert_eq!(parsed.humidity, wx.humidity);
        prop_assert_eq!(parsed.pressure, wx.pressure);
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
        text in arb_printable_text(),
        msg_id in prop::option::of(arb_message_id()),
    ) {
        // Ensure text has no braces to avoid ambiguity with the
        // `{id` trailer. (Brace in text is a separate test.)
        let text: String = text.chars().filter(|c| *c != '{' && *c != '}').collect();
        let packet = build_aprs_message_packet(
            &source, &addressee, &text, msg_id.as_deref(), &[],
        );
        let wire = ax25_to_kiss_wire(&packet);
        let kiss = decode_kiss_frame(&wire).map_err(to_test_err)?;
        let parsed_packet = parse_ax25(&kiss.data).map_err(to_test_err)?;
        let data = parse_aprs_data(&parsed_packet.info).map_err(to_test_err)?;
        let AprsData::Message(parsed) = data else {
            prop_assert!(false, "expected message variant");
            return Ok(());
        };
        prop_assert_eq!(parsed.addressee.as_str(), addressee.trim());
        // Text length may have been truncated to 67 bytes by the builder.
        let expected_text = if text.len() > 67 {
            let mut end = 67;
            while end > 0 && !text.is_char_boundary(end) {
                end -= 1;
            }
            text[..end].to_owned()
        } else {
            text
        };
        prop_assert_eq!(parsed.text.as_str(), expected_text.as_str());
        prop_assert_eq!(parsed.message_id, msg_id);
    }
}

// ---------------------------------------------------------------------------
// Status round-trip
// ---------------------------------------------------------------------------

proptest! {
    #[test]
    fn status_roundtrip(
        source in arb_ax25_address(),
        text in arb_printable_text(),
    ) {
        let packet = build_aprs_status_packet(&source, &text, &[]);
        let wire = ax25_to_kiss_wire(&packet);
        let kiss = decode_kiss_frame(&wire).map_err(to_test_err)?;
        let parsed_packet = parse_ax25(&kiss.data).map_err(to_test_err)?;
        let data = parse_aprs_data(&parsed_packet.info).map_err(to_test_err)?;
        let AprsData::Status(parsed) = data else {
            prop_assert!(false, "expected status variant");
            return Ok(());
        };
        // The parser trims leading/trailing whitespace to match the
        // APRS convention of right-padded status strings.
        prop_assert_eq!(parsed.text.as_str(), text.trim());
    }
}
