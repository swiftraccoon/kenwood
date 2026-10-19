//! The offline channel listing, and a backup fixture holding the CALL and
//! weather channels as firmware 1.02 stores them.

use std::collections::BTreeMap;
use std::path::Path;

use super::*;

pub(crate) type TestError = Box<dyn std::error::Error + Send + Sync>;
type TestResult = Result<(), TestError>;

/// Record bytes 0..15 of the six CALL channels, in table order.
const CALL_HEADS: [[u8; 15]; 6] = [
    [
        0xC0, 0xB7, 0xBB, 0x08, 0xC0, 0x27, 0x09, 0x00, 0x22, 0x01, 0x00, 0x08, 0x08, 0x00, 0x00,
    ],
    [
        0x00, 0x44, 0x95, 0x08, 0xC0, 0x27, 0x09, 0x00, 0x22, 0x11, 0x00, 0x08, 0x08, 0x00, 0x00,
    ],
    [
        0xE0, 0x56, 0x52, 0x0D, 0x00, 0x6A, 0x18, 0x00, 0x88, 0x01, 0x00, 0x08, 0x08, 0x00, 0x00,
    ],
    [
        0xC0, 0xB5, 0x4A, 0x0D, 0x00, 0x6A, 0x18, 0x00, 0x88, 0x11, 0x00, 0x08, 0x08, 0x00, 0x00,
    ],
    [
        0x80, 0x6B, 0x95, 0x1A, 0x40, 0x4B, 0x4C, 0x00, 0x99, 0x01, 0x00, 0x08, 0x08, 0x00, 0x00,
    ],
    [
        0x00, 0xDE, 0x39, 0x1A, 0x40, 0x4B, 0x4C, 0x00, 0x99, 0x11, 0x00, 0x08, 0x08, 0x00, 0x00,
    ],
];

/// Record bytes 15..40 of every CALL record.
const CALL_TAIL: [u8; 25] = *b"CQCQCQ\0\0DIRECT\0\0DIRECT\0\0\0";

/// The CALL names, stored from name-table index 1136.
const CALL_NAMES: [&str; 6] = [
    "Call VHF (FM)",
    "Call VHF (DV)",
    "Call 220M (FM)",
    "Call 220M (DV)",
    "Call UHF (FM)",
    "Call UHF (DV)",
];

/// The `WX 1` record: 162.550 MHz FM, bytes 15..39 `0xFF`, byte 39 zero.
const WEATHER_HEAD: [u8; 15] = [
    0xF0, 0x50, 0xB0, 0x09, 0x00, 0x00, 0x00, 0x00, 0x22, 0x01, 0x10, 0x08, 0x08, 0x00, 0x70,
];

const fn record_address(index: u32) -> u32 {
    0x4000 + 256 * (index / 6) + 40 * (index % 6)
}

fn call_record(head: [u8; 15]) -> Vec<u8> {
    head.into_iter().chain(CALL_TAIL).collect()
}

fn weather_record() -> Vec<u8> {
    WEATHER_HEAD
        .into_iter()
        .chain([0xFF; 24])
        .chain([0])
        .collect()
}

fn name(text: &str) -> Vec<u8> {
    let mut bytes = text.as_bytes().to_vec();
    bytes.resize(16, 0);
    bytes
}

fn put(patches: &mut BTreeMap<u32, u8>, address: u32, bytes: &[u8]) {
    for (offset, byte) in (address..).zip(bytes) {
        let _previous = patches.insert(offset, *byte);
    }
}

/// A backup report whose channel tables hold regular channel 5 (a copy of
/// the VHF FM CALL record), the six CALL channels and ten weather channels
/// on `WX 1`'s record; every other memory slot is empty.
pub(crate) fn stored_backup(directory: &Path) -> Result<PathBuf, TestError> {
    crate::mcp::snapshot::tests::patched_backup(directory, &stored_patches()?)
}

/// The image bytes of [`stored_backup`]'s channel tables.
pub(crate) fn stored_patches() -> Result<BTreeMap<u32, u8>, TestError> {
    let mut patches = BTreeMap::new();
    for index in 0..1102 {
        put(&mut patches, 0x2000 + 4 * index, &[0xFF, 0x00, 0x00, 0xFF]);
    }
    put(&mut patches, 0x2000 + 4 * 5, &[0x05, 0x00, 0x00, 0x00]);
    put(&mut patches, record_address(5), &call_record(CALL_HEADS[0]));
    put(&mut patches, 0x1_0000 + 16 * 5, &[0; 16]);
    for ((position, head), text) in (0_u32..).zip(CALL_HEADS).zip(CALL_NAMES) {
        let index = 1132 + position;
        let band = u8::try_from(position / 2 + 1)?;
        put(&mut patches, 0x2000 + 4 * index, &[band, 0xFF, 0x22, 0xFF]);
        put(&mut patches, record_address(index), &call_record(head));
        put(&mut patches, 0x1_0000 + 16 * (1136 + position), &name(text));
    }
    for number in 1..=10_u32 {
        let index = 1101 + number;
        put(&mut patches, 0x2000 + 4 * index, &[0x01, 0x00, 0x20, 0x00]);
        put(&mut patches, record_address(index), &weather_record());
        put(
            &mut patches,
            0x1_0000 + 16 * index,
            &name(&format!("WX {number:>2}")),
        );
    }
    Ok(patches)
}

#[test]
fn the_listing_shows_stored_call_and_weather_channels() -> TestResult {
    let directory = tempfile::tempdir()?;
    let snapshot = Snapshot::load(&stored_backup(directory.path())?)?;
    let listing = lines(covered(&snapshot)?);
    let regular = format!("{}: 146.520000 MHz FM, group 0", PhysicalChannel::new(5)?);
    let expected_head = [regular.as_str(), "Stored memory channels: 1."];
    assert_eq!(
        listing.get(..2),
        Some(&expected_head.map(str::to_owned)[..])
    );
    assert_eq!(
        listing.get(2).map(String::as_str),
        Some("CALL VHF FM: 146.520000 MHz FM \"Call VHF (FM)\"")
    );
    assert_eq!(
        listing.get(7).map(String::as_str),
        Some("CALL UHF DV: 440.000000 MHz DV \"Call UHF (DV)\"")
    );
    assert_eq!(
        listing.get(8).map(String::as_str),
        Some("WX 1: 162.550000 MHz")
    );
    assert_eq!(listing.len(), 2 + 6 + 10);
    Ok(())
}
