//! Integration tests for SD card file format parsers.

// Deps visible to every kenwood-thd75 test target but unused here.
// Acknowledged so `unused_crate_dependencies` stays silent without
// weakening the lint.
use aprs as _;
use aprs_is as _;
use ax25_codec as _;
use dstar_gateway_core as _;
use encoding_rs as _;
use kiss_tnc as _;
use mmdvm as _;
use mmdvm_core as _;
use proptest as _;
use serde_json as _;
use thiserror as _;
use tokio as _;
use tokio_serial as _;
use tracing as _;

use kenwood_thd75::sdcard::SdCardError;
use kenwood_thd75::sdcard::callsign_list::{
    CallsignEntry, parse_callsign_list, write_callsign_list,
};
use kenwood_thd75::sdcard::config::{
    ConfigFileModel, HEADER_SIZE, MAX_CHANNELS, RadioConfig, make_header, parse_config,
    write_config,
};
use kenwood_thd75::sdcard::qso_log::{
    QSO_LOG_HEADER, QsoDateTime, QsoDirection, QsoEntry, QsoFastData, QsoFrequency, QsoMode,
    QsoRfPower, parse_qso_log, write_qso_log,
};
use kenwood_thd75::sdcard::repeater_list::{
    REPEATER_CATALOG_HEADER, RepeaterCatalogSelection, RepeaterShift, parse_repeater_catalog,
    write_repeater_catalog,
};
use kenwood_thd75::types::channel::StoredChannel;
use kenwood_thd75::types::frequency::Frequency;
use kenwood_thd75::types::{MemoryChannelBand, MemoryGroup, RegularChannel};
use kenwood_thd75::{memory::MemoryImage, protocol::programming};

type TestResult = Result<(), Box<dyn std::error::Error>>;
type BoxErr = Box<dyn std::error::Error>;

fn synthetic_stored_channel(receive_frequency: Frequency) -> StoredChannel {
    let mut wire = [0_u8; StoredChannel::BYTE_SIZE];
    wire[..4].copy_from_slice(&receive_frequency.to_le_bytes());
    StoredChannel::from_bytes(&wire).unwrap_or_else(|error| {
        unreachable!("fixed all-zero synthetic channel record must decode: {error}")
    })
}

/// Copy `data` into `image` starting at `offset`.
fn write_slice(image: &mut [u8], offset: usize, data: &[u8]) -> Result<(), BoxErr> {
    let end = offset + data.len();
    let img_len = image.len();
    image
        .get_mut(offset..end)
        .ok_or_else(|| format!("write_slice: range {offset}..{end} out of bounds (len={img_len})"))?
        .copy_from_slice(data);
    Ok(())
}

// ---------------------------------------------------------------------------
// .d75 config tests
// ---------------------------------------------------------------------------

/// Channel data file offset (`HEADER_SIZE` + 0x4000).
const CH_DATA_OFFSET: usize = 0x4100;

/// Channel flag file offset (`HEADER_SIZE` + 0x2000).
const CH_FLAGS_OFFSET: usize = 0x2100;

/// Channel name file offset (`HEADER_SIZE` + 0x10000).
const CH_NAME_OFFSET: usize = 0x10100;

/// Builds an exact-size synthetic `.d75` file.
fn make_synthetic_d75() -> Result<Vec<u8>, BoxErr> {
    let mut data = vec![0u8; HEADER_SIZE + programming::TOTAL_SIZE];

    // Write model string at offset 0
    write_slice(&mut data, 0, b"Data For TH-D75A")?;

    // Write version bytes at offset 0x14
    write_slice(&mut data, 0x14, &[0x95, 0xC4, 0x8F, 0x42])?;

    // Every synthetic channel starts empty. A zero marker means a populated
    // VHF slot, so zero-filled flag tables are not an empty configuration.
    let flags = data
        .get_mut(CH_FLAGS_OFFSET..CH_FLAGS_OFFSET + MAX_CHANNELS * 4)
        .ok_or("synthetic channel flag table is out of bounds")?
        .chunks_exact_mut(4);
    for flag in flags {
        *flag
            .first_mut()
            .ok_or("synthetic channel flag record has no marker")? = 0xFF;
    }

    Ok(data)
}

#[test]
fn parse_synthetic_d75_header() -> TestResult {
    let data = make_synthetic_d75()?;
    let config = parse_config(&data)?;

    assert_eq!(config.header().model(), ConfigFileModel::ThD75A);
    assert_eq!(config.header().version_bytes(), [0x95, 0xC4, 0x8F, 0x42]);
    assert_eq!(config.channels().all_slots()?.len(), MAX_CHANNELS);
    Ok(())
}

#[test]
fn parse_d75_rejects_bad_model() -> TestResult {
    let mut data = make_synthetic_d75()?;
    // Overwrite model with something invalid
    write_slice(&mut data, 0, b"Data For TH-D74A")?;
    let err = parse_config(&data)
        .err()
        .ok_or("expected InvalidModelIdentifier but got Ok")?;
    assert_eq!(
        err,
        SdCardError::InvalidModelIdentifier {
            found: *b"Data For TH-D74A"
        }
    );
    Ok(())
}

#[test]
fn parse_d75_rejects_too_small() -> TestResult {
    let data = vec![0u8; 100];
    let err = parse_config(&data)
        .err()
        .ok_or("expected FileTooSmall but got Ok")?;
    assert!(
        matches!(err, SdCardError::FileTooSmall { .. }),
        "expected FileTooSmall, got {err:?}"
    );
    Ok(())
}

#[test]
fn d75_all_channels_unused_in_empty_file() -> TestResult {
    let data = make_synthetic_d75()?;
    let config = parse_config(&data)?;
    for channel in RegularChannel::all() {
        let ch = config.channels().get(channel)?;
        assert!(
            ch.flag().is_empty(),
            "channel {} should be empty",
            ch.number()
        );
        assert_eq!(
            ch.flag().to_wire_bytes(),
            [0xFF, 0x00, 0x00, 0x00],
            "channel {} should retain the synthetic file's exact empty flag",
            ch.number()
        );
    }
    Ok(())
}

#[test]
fn d75_channel_with_frequency_is_used() -> TestResult {
    let mut data = make_synthetic_d75()?;

    // Write 145 MHz into channel 0's RX frequency (at file offset 0x4100)
    write_slice(&mut data, CH_DATA_OFFSET, &145_000_000u32.to_le_bytes())?;
    write_slice(&mut data, CH_FLAGS_OFFSET, &[0x00, 0x00, 0x00, 0xFF])?;

    let config = parse_config(&data)?;
    let ch0 = config.channels().get(RegularChannel::new(0)?)?;
    assert!(ch0.is_programmed());
    assert_eq!(
        ch0.programmed()
            .ok_or("channel 0 should be programmed")?
            .receive_frequency
            .as_hz(),
        145_000_000,
    );
    Ok(())
}

#[test]
fn d75_channel_name_roundtrip() -> TestResult {
    let mut data = make_synthetic_d75()?;

    // Write a name for channel 0 at the name table offset
    write_slice(&mut data, CH_NAME_OFFSET, b"2M RPT")?;

    let config = parse_config(&data)?;
    assert_eq!(
        config
            .channels()
            .get(RegularChannel::new(0)?)?
            .name()
            .as_str(),
        "2M RPT"
    );
    Ok(())
}

#[test]
fn d75_write_roundtrip() -> TestResult {
    // Build a config from scratch
    let header = make_header(ConfigFileModel::ThD75A, [0x95, 0xC4, 0x8F, 0x42]);
    let source = make_synthetic_d75()?;
    let raw_image = source
        .get(HEADER_SIZE..)
        .ok_or("synthetic .d75 body missing")?
        .to_vec();
    let mut memory_image = MemoryImage::from_raw(raw_image)?;

    // Program channel 0 with a real frequency
    let stored_channel = synthetic_stored_channel(Frequency::new(145_000_000));
    let ch = kenwood_thd75::memory::ChannelEntry::new_programmed(
        RegularChannel::new(0)?,
        kenwood_thd75::ChannelDisplayName::new("2M CALL")?,
        stored_channel,
        MemoryChannelBand::Vhf,
        MemoryGroup::new(0)?,
        true,
    )?;
    memory_image.channels_mut().set(&ch)?;

    let config = RadioConfig::new(header, memory_image)?;

    // Write then re-parse
    let bytes = write_config(&config);
    let parsed = parse_config(&bytes)?;

    assert_eq!(parsed.header().model(), ConfigFileModel::ThD75A);
    let ch0 = parsed.channels().get(RegularChannel::new(0)?)?;
    assert!(ch0.is_programmed());
    assert_eq!(ch0.name().as_str(), "2M CALL");
    assert_eq!(
        ch0.programmed()
            .ok_or("channel 0 should be programmed")?
            .receive_frequency
            .as_hz(),
        145_000_000,
    );
    assert_eq!(ch0.flag().scan_lockout(), Some(true));

    // Channel 1 should remain unused
    let ch1 = parsed.channels().get(RegularChannel::new(1)?)?;
    assert!(ch1.flag().is_empty());
    assert_eq!(
        ch1.flag().to_wire_bytes(),
        [0xFF, 0x00, 0x00, 0x00],
        "untouched empty-slot metadata must round-trip byte-for-byte"
    );
    Ok(())
}

// ---------------------------------------------------------------------------
// Repeater list TSV tests
// ---------------------------------------------------------------------------

/// Builds a representative 31-column Kenwood source catalog as UTF-16LE.
fn make_repeater_tsv() -> Vec<u8> {
    let row = "4\tNorth America\t1\tUnited States\t1\tSoutheast\tW4MOE  B\t\
W4MOE  G\tOff\tAsheville\tNC\t145.25\t-\t0.6\tDigital\tOff\tOff\tApprox.\t\
35\t35.71\tN\t82\t33.09\tW\t-05:00\tOn\tOff\tOff\tUSA\tSE\t";
    encode_utf16le_bom(&format!("{REPEATER_CATALOG_HEADER}\r\n{row}\r\n"))
}

fn encode_utf16le_bom(text: &str) -> Vec<u8> {
    let mut out = Vec::with_capacity(2 + text.len() * 2);
    out.push(0xFF);
    out.push(0xFE);
    for unit in text.encode_utf16() {
        let [lo, hi] = unit.to_le_bytes();
        out.push(lo);
        out.push(hi);
    }
    out
}

#[test]
fn parse_repeater_catalog_basic() -> TestResult {
    let data = make_repeater_tsv();
    let catalog = parse_repeater_catalog(&data)?;

    assert_eq!(catalog.len(), 1);
    let entry = catalog.entries().first().ok_or("repeater entry missing")?;
    assert_eq!(entry.group(), "Southeast");
    assert_eq!(entry.name(), "Asheville");
    assert_eq!(entry.callsign().as_str(), "W4MOE  B");
    assert_eq!(entry.gateway().as_str(), "W4MOE  G");
    assert_eq!(entry.frequency().as_hz(), 145_250_000);
    assert_eq!(entry.shift(), RepeaterShift::Negative);
    assert_eq!(entry.offset().as_hz(), 600_000);

    let selected = catalog.select(RepeaterCatalogSelection::new(ConfigFileModel::ThD75A))?;
    assert_eq!(selected.len(), 1);
    Ok(())
}

#[test]
fn repeater_catalog_write_roundtrip() -> TestResult {
    let catalog = parse_repeater_catalog(&make_repeater_tsv())?;
    let bytes = write_repeater_catalog(&catalog);
    let parsed = parse_repeater_catalog(&bytes)?;

    assert_eq!(parsed, catalog);
    assert_eq!(
        parsed.entries().first().ok_or("parsed[0] missing")?.aux_2(),
        "SE"
    );
    Ok(())
}

#[test]
fn repeater_catalog_rejects_malformed_shift_jis() -> TestResult {
    let err = parse_repeater_catalog(&[0x81])
        .err()
        .ok_or("expected unsupported encoding but got Ok")?;
    assert!(
        matches!(err, SdCardError::UnsupportedTextEncoding { .. }),
        "expected UnsupportedTextEncoding, got {err:?}"
    );
    Ok(())
}

// ---------------------------------------------------------------------------
// Callsign list TSV tests
// ---------------------------------------------------------------------------

#[test]
fn callsign_list_parse_basic() -> TestResult {
    let data =
        encode_utf16le_bom("Name\tCallsign\tMemo\r\nAlice\tW4CDR\tFriend\r\nBob\tKE4FOX\tClub\r\n");
    let entries = parse_callsign_list(&data)?;

    assert_eq!(entries.len(), 2);
    assert_eq!(
        entries.first().ok_or("entries[0] missing")?.name().as_str(),
        "Alice"
    );
    assert_eq!(
        entries
            .first()
            .ok_or("entries[0] missing")?
            .callsign()
            .as_str(),
        "W4CDR"
    );
    assert_eq!(
        entries
            .get(1)
            .ok_or("entries[1] missing")?
            .callsign()
            .as_str(),
        "KE4FOX"
    );
    Ok(())
}

#[test]
fn callsign_list_write_roundtrip() -> TestResult {
    let entries = vec![
        CallsignEntry::new("Alice", "W4CDR", "Friend")?,
        CallsignEntry::new("Bob", "KE4FOX", "Club")?,
    ];

    let bytes = write_callsign_list(&entries)?;
    let parsed = parse_callsign_list(&bytes)?;

    assert_eq!(parsed.len(), 2);
    assert_eq!(
        parsed.first().ok_or("parsed[0] missing")?.callsign(),
        entries.first().ok_or("entries[0] missing")?.callsign()
    );
    assert_eq!(
        parsed.get(1).ok_or("parsed[1] missing")?.callsign(),
        entries.get(1).ok_or("entries[1] missing")?.callsign()
    );
    Ok(())
}

// ---------------------------------------------------------------------------
// QSO log `.csv` tests (tab-separated content)
// ---------------------------------------------------------------------------

/// Builds a synthetic QSO log entry line with 24 tab-separated columns.
fn make_qso_line() -> String {
    [
        "TX",               // TX/RX
        "2026/03/28 14:30", // Date
        "145.000.000",      // Frequency
        "DV",               // Mode
        "",                 // My Latitude (spelling intentionally uninterpreted)
        "",                 // My Longitude (spelling intentionally uninterpreted)
        "",                 // My Altitude (spelling intentionally uninterpreted)
        "HIGH",             // RF Power
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
        "1",                // Fast Data
        "",                 // Latitude (spelling intentionally uninterpreted)
        "",                 // Longitude (spelling intentionally uninterpreted)
        "",                 // Altitude (spelling intentionally uninterpreted)
        "270",              // Course
        "0",                // Speed
    ]
    .join("\t")
}

#[test]
fn parse_qso_log_basic() -> TestResult {
    let line = make_qso_line();
    let data = format!("{QSO_LOG_HEADER}\r\n{line}\r\n");
    let entries = parse_qso_log(data.as_bytes())?;

    assert_eq!(entries.len(), 1);
    let entry = entries.first().ok_or("qso entry missing")?;
    assert_eq!(entry.direction(), QsoDirection::Tx);
    assert_eq!(entry.date().as_str(), "2026/03/28 14:30");
    assert_eq!(entry.frequency().as_hz(), 145_000_000);
    assert_eq!(entry.mode(), QsoMode::Dv);
    assert_eq!(entry.rf_power(), QsoRfPower::High);
    assert_eq!(entry.fast_data(), QsoFastData::Enabled);
    assert_eq!(entry.caller(), "W4CDR");
    assert_eq!(entry.called(), "CQCQCQ");
    assert_eq!(entry.rx_rpt1(), "W4MOE  B");
    assert_eq!(entry.message(), "Hello");
    Ok(())
}

#[test]
fn qso_log_write_roundtrip() -> TestResult {
    let entry = QsoEntry::builder(
        QsoDirection::Rx,
        QsoDateTime::new("2026/03/28 15:00")?,
        QsoFrequency::new("439.310.000")?,
        QsoMode::FmN,
        QsoRfPower::Mid,
        QsoFastData::Disabled,
    )
    .s_meter("S5")
    .caller("KE4FOX")
    .called("W4CDR")
    .build()?;

    let bytes = write_qso_log(std::slice::from_ref(&entry));
    let parsed = parse_qso_log(&bytes)?;

    assert_eq!(parsed.len(), 1);
    let parsed0 = parsed.first().ok_or("parsed qso missing")?;
    assert_eq!(parsed0, &entry);
    assert_eq!(parsed0.mode(), QsoMode::FmN);
    assert_eq!(parsed0.rf_power(), QsoRfPower::Mid);
    Ok(())
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
        SdCardError::InvalidModelIdentifier {
            found: *b"Data For TH-D74A",
        },
        SdCardError::MissingBom,
        SdCardError::InvalidUtf16Length { len: 3 },
        SdCardError::Utf16Decode {
            detail: "bad".to_owned(),
        },
        SdCardError::InvalidUtf8 {
            file_type: "test file",
            valid_up_to: 3,
            error_len: Some(1),
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
