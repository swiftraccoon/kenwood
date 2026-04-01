//! Integration tests for SD card file format parsers.

use kenwood_thd75::sdcard::SdCardError;
use kenwood_thd75::sdcard::callsign_list::{
    CallsignEntry, parse_callsign_list, write_callsign_list,
};
use kenwood_thd75::sdcard::config::{
    HEADER_SIZE, MAX_CHANNELS, RadioConfig, empty_channel, make_channel, make_header, parse_config,
    write_config,
};
use kenwood_thd75::sdcard::qso_log::{QsoEntry, parse_qso_log, write_qso_log};
use kenwood_thd75::sdcard::repeater_list::{
    RepeaterEntry, parse_repeater_list, write_repeater_list,
};
use kenwood_thd75::types::channel::FlashChannel;
use kenwood_thd75::types::frequency::Frequency;

// ---------------------------------------------------------------------------
// .d75 config tests
// ---------------------------------------------------------------------------

/// Channel data file offset (`HEADER_SIZE` + 0x4000).
const CH_DATA_OFFSET: usize = 0x4100;

/// Channel name file offset (`HEADER_SIZE` + 0x10000).
const CH_NAME_OFFSET: usize = 0x10100;

/// Builds a synthetic `.d75` file large enough to parse.
fn make_synthetic_d75() -> Vec<u8> {
    // Minimum file size: channel name region end
    let min_size = CH_NAME_OFFSET + (1000 * 16);
    let mut data = vec![0u8; min_size];

    // Write model string at offset 0
    let model = b"Data For TH-D75A";
    data[..model.len()].copy_from_slice(model);

    // Write version bytes at offset 0x14
    data[0x14..0x18].copy_from_slice(&[0x95, 0xC4, 0x8F, 0x42]);

    data
}

#[test]
fn parse_synthetic_d75_header() {
    let data = make_synthetic_d75();
    let config = parse_config(&data).unwrap();

    assert_eq!(config.header.model, "Data For TH-D75A");
    assert_eq!(config.header.version_bytes, [0x95, 0xC4, 0x8F, 0x42]);
    assert_eq!(config.channels.len(), MAX_CHANNELS);
}

#[test]
fn parse_d75_rejects_bad_model() {
    let mut data = make_synthetic_d75();
    // Overwrite model with something invalid
    data[..16].copy_from_slice(b"Data For TH-D74A");
    let err = parse_config(&data).unwrap_err();
    assert!(matches!(err, SdCardError::InvalidModelString { .. }));
}

#[test]
fn parse_d75_rejects_too_small() {
    let data = vec![0u8; 100];
    let err = parse_config(&data).unwrap_err();
    assert!(matches!(err, SdCardError::FileTooSmall { .. }));
}

#[test]
fn d75_all_channels_unused_in_empty_file() {
    let data = make_synthetic_d75();
    let config = parse_config(&data).unwrap();
    for ch in &config.channels {
        assert!(!ch.used, "channel {} should be unused", ch.number);
    }
}

#[test]
fn d75_channel_with_frequency_is_used() {
    let mut data = make_synthetic_d75();

    // Write 145 MHz into channel 0's RX frequency (at file offset 0x4100)
    let freq_bytes = 145_000_000u32.to_le_bytes();
    data[CH_DATA_OFFSET..CH_DATA_OFFSET + 4].copy_from_slice(&freq_bytes);

    let config = parse_config(&data).unwrap();
    assert!(config.channels[0].used);
    assert_eq!(config.channels[0].flash.rx_frequency.as_hz(), 145_000_000);
}

#[test]
fn d75_channel_name_roundtrip() {
    let mut data = make_synthetic_d75();

    // Write a name for channel 0 at the name table offset
    data[CH_NAME_OFFSET..CH_NAME_OFFSET + 6].copy_from_slice(b"2M RPT");

    let config = parse_config(&data).unwrap();
    assert_eq!(config.channels[0].name, "2M RPT");
}

#[test]
fn d75_write_roundtrip() {
    // Build a config from scratch
    let header = make_header("Data For TH-D75A", [0x95, 0xC4, 0x8F, 0x42]).unwrap();
    let raw_image_size = CH_NAME_OFFSET + (1000 * 16) - HEADER_SIZE;
    let raw_image = vec![0u8; raw_image_size];

    let mut channels: Vec<_> = (0..1000).map(empty_channel).collect();

    // Program channel 0 with a real frequency
    let flash = FlashChannel {
        rx_frequency: Frequency::new(145_000_000),
        ..FlashChannel::default()
    };
    channels[0] = make_channel(0, "2M CALL", flash);
    channels[0].lockout = true;

    let config = RadioConfig {
        header,
        channels,
        raw_image,
    };

    // Write then re-parse
    let bytes = write_config(&config);
    let parsed = parse_config(&bytes).unwrap();

    assert_eq!(parsed.header.model, "Data For TH-D75A");
    assert!(parsed.channels[0].used);
    assert_eq!(parsed.channels[0].name, "2M CALL");
    assert_eq!(parsed.channels[0].flash.rx_frequency.as_hz(), 145_000_000);
    assert!(parsed.channels[0].lockout);

    // Channel 1 should remain unused
    assert!(!parsed.channels[1].used);
}

// ---------------------------------------------------------------------------
// Repeater list TSV tests
// ---------------------------------------------------------------------------

/// Builds a synthetic repeater list TSV as UTF-16LE bytes.
fn make_repeater_tsv() -> Vec<u8> {
    let text = "Group Name\tName\tSub Name\tRepeater Call Sign\t\
                Gateway Call Sign\tFrequency\tDup\tOffset\r\n\
                Kanto\tTokyo\tTokyo\tJR1YXV B\tJR1YXV G\t439.310000\t-\t5.000000\r\n";
    encode_utf16le_bom(text)
}

fn encode_utf16le_bom(text: &str) -> Vec<u8> {
    let mut out = Vec::with_capacity(2 + text.len() * 2);
    out.push(0xFF);
    out.push(0xFE);
    for unit in text.encode_utf16() {
        let bytes = unit.to_le_bytes();
        out.push(bytes[0]);
        out.push(bytes[1]);
    }
    out
}

#[test]
fn parse_repeater_list_basic() {
    let data = make_repeater_tsv();
    let entries = parse_repeater_list(&data).unwrap();

    assert_eq!(entries.len(), 1);
    assert_eq!(entries[0].group_name, "Kanto");
    assert_eq!(entries[0].name, "Tokyo");
    assert_eq!(entries[0].callsign_rpt1, "JR1YXV B");
    assert_eq!(entries[0].callsign_rpt2, "JR1YXV G");
    assert_eq!(entries[0].frequency, 439_310_000);
    assert_eq!(entries[0].duplex, "-");
    assert_eq!(entries[0].offset, 5_000_000);
}

#[test]
fn repeater_list_write_roundtrip() {
    let entries = vec![RepeaterEntry {
        group_name: "Southeast".to_owned(),
        name: "Asheville".to_owned(),
        sub_name: "NC".to_owned(),
        callsign_rpt1: "W4MOE  B".to_owned(),
        callsign_rpt2: "W4MOE  G".to_owned(),
        frequency: 145_250_000,
        duplex: "-".to_owned(),
        offset: 600_000,
    }];

    let bytes = write_repeater_list(&entries);
    let parsed = parse_repeater_list(&bytes).unwrap();

    assert_eq!(parsed.len(), 1);
    assert_eq!(parsed[0].group_name, "Southeast");
    assert_eq!(parsed[0].name, "Asheville");
    assert_eq!(parsed[0].callsign_rpt1, "W4MOE  B");
    assert_eq!(parsed[0].frequency, 145_250_000);
    assert_eq!(parsed[0].offset, 600_000);
}

#[test]
fn repeater_list_missing_bom() {
    let err = parse_repeater_list(&[0x00, 0x00]).unwrap_err();
    assert!(matches!(err, SdCardError::MissingBom));
}

// ---------------------------------------------------------------------------
// Callsign list TSV tests
// ---------------------------------------------------------------------------

#[test]
fn callsign_list_parse_basic() {
    let data = encode_utf16le_bom("Callsign\r\nW4CDR   \r\nKE4FOX  \r\n");
    let entries = parse_callsign_list(&data).unwrap();

    assert_eq!(entries.len(), 2);
    assert_eq!(entries[0].callsign, "W4CDR   ");
    assert_eq!(entries[1].callsign, "KE4FOX  ");
}

#[test]
fn callsign_list_write_roundtrip() {
    let entries = vec![
        CallsignEntry {
            callsign: "W4CDR   ".to_owned(),
        },
        CallsignEntry {
            callsign: "KE4FOX  ".to_owned(),
        },
    ];

    let bytes = write_callsign_list(&entries);
    let parsed = parse_callsign_list(&bytes).unwrap();

    assert_eq!(parsed.len(), 2);
    assert_eq!(parsed[0].callsign, entries[0].callsign);
    assert_eq!(parsed[1].callsign, entries[1].callsign);
}

// ---------------------------------------------------------------------------
// QSO log TSV tests
// ---------------------------------------------------------------------------

/// Builds a synthetic QSO log entry line with 24 tab-separated columns.
fn make_qso_line() -> String {
    [
        "TX",               // TX/RX
        "2026/03/28 14:30", // Date
        "145.000.000",      // Frequency
        "DV",               // Mode
        "35.5951N",         // My Latitude
        "082.5515W",        // My Longitude
        "648m",             // My Altitude
        "High",             // RF Power
        "S9",               // S Meter
        "W4CDR",            // Caller
        "",                 // Memo
        "CQCQCQ",           // Called
        "W4MOE  B",         // RPT1
        "W4MOE  G",         // RPT2
        "Hello",            // Message
        "",                 // Repeater Control
        "",                 // BK
        "",                 // EMR
        "",                 // Fast Data
        "35.5950N",         // Latitude
        "082.5514W",        // Longitude
        "650m",             // Altitude
        "270",              // Course
        "0",                // Speed
    ]
    .join("\t")
}

#[test]
fn parse_qso_log_basic() {
    let header = "TX/RX\tDate\tFrequency\tMode\t\
        My Latitude\tMy Longitude\tMy Altitude\t\
        RF Power\tS Meter\tCaller\tMemo\tCalled\t\
        RPT1\tRPT2\tMessage\tRepeater Control\t\
        BK\tEMR\tFast Data\t\
        Latitude\tLongitude\tAltitude\tCourse\tSpeed";
    let line = make_qso_line();
    let data = format!("{header}\r\n{line}\r\n");
    let entries = parse_qso_log(data.as_bytes()).unwrap();

    assert_eq!(entries.len(), 1);
    assert_eq!(entries[0].tx_rx, "TX");
    assert_eq!(entries[0].date, "2026/03/28 14:30");
    assert_eq!(entries[0].mode, "DV");
    assert_eq!(entries[0].caller, "W4CDR");
    assert_eq!(entries[0].called, "CQCQCQ");
    assert_eq!(entries[0].rpt1, "W4MOE  B");
    assert_eq!(entries[0].message, "Hello");
}

#[test]
fn qso_log_write_roundtrip() {
    let entry = QsoEntry {
        tx_rx: "RX".to_owned(),
        date: "2026/03/28 15:00".to_owned(),
        frequency: "439.310.000".to_owned(),
        mode: "FM".to_owned(),
        my_latitude: "35.5951N".to_owned(),
        my_longitude: "082.5515W".to_owned(),
        my_altitude: "648m".to_owned(),
        rf_power: "Mid".to_owned(),
        s_meter: "S5".to_owned(),
        caller: "KE4FOX".to_owned(),
        memo: String::new(),
        called: "W4CDR".to_owned(),
        rpt1: String::new(),
        rpt2: String::new(),
        message: String::new(),
        repeater_control: String::new(),
        bk: String::new(),
        emr: String::new(),
        fast_data: String::new(),
        latitude: String::new(),
        longitude: String::new(),
        altitude: String::new(),
        course: String::new(),
        speed: String::new(),
    };

    let bytes = write_qso_log(std::slice::from_ref(&entry));
    let parsed = parse_qso_log(&bytes).unwrap();

    assert_eq!(parsed.len(), 1);
    assert_eq!(parsed[0].tx_rx, entry.tx_rx);
    assert_eq!(parsed[0].date, entry.date);
    assert_eq!(parsed[0].frequency, entry.frequency);
    assert_eq!(parsed[0].mode, entry.mode);
    assert_eq!(parsed[0].caller, entry.caller);
    assert_eq!(parsed[0].called, entry.called);
}

// ---------------------------------------------------------------------------
// Error display tests
// ---------------------------------------------------------------------------

#[test]
fn error_display_coverage() {
    let errs: Vec<SdCardError> = vec![
        SdCardError::FileTooSmall {
            expected: 1000,
            actual: 100,
        },
        SdCardError::InvalidModelString {
            found: "bad".to_owned(),
        },
        SdCardError::MissingBom,
        SdCardError::InvalidUtf16Length { len: 3 },
        SdCardError::Utf16Decode {
            detail: "bad".to_owned(),
        },
        SdCardError::ColumnCount {
            line: 2,
            expected: 8,
            actual: 3,
        },
        SdCardError::InvalidField {
            line: 5,
            column: "Freq".to_owned(),
            detail: "bad".to_owned(),
        },
        SdCardError::ChannelParse {
            index: 42,
            detail: "bad".to_owned(),
        },
    ];

    for err in &errs {
        // Verify Display impl produces non-empty output
        let msg = err.to_string();
        assert!(!msg.is_empty(), "error Display was empty for {err:?}");
    }
}
