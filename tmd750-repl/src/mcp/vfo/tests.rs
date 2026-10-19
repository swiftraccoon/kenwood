//! VFO memory edits planned from backup reports: tuning, limits, the pages
//! and journal description they produce, the refusals and the endpoint gate.

use std::collections::BTreeMap;
use std::fs::File;
use std::path::Path;

use super::*;

type TestError = Box<dyn std::error::Error + Send + Sync>;
type TestResult = Result<(), TestError>;

/// Band A's current VFO record in a firmware 1.02 backup: 145.190 MHz FM,
/// minus 600 kHz.
const CURRENT_A: [u8; 40] = [
    0x70, 0x6C, 0xA7, 0x08, 0xC0, 0x27, 0x09, 0x00, 0x22, 0x01, 0x02, 0x08, 0x08, 0x00, 0x00, 0x43,
    0x51, 0x43, 0x51, 0x43, 0x51, 0x00, 0x00, 0x44, 0x49, 0x52, 0x45, 0x43, 0x54, 0x00, 0x00, 0x44,
    0x49, 0x52, 0x45, 0x43, 0x54, 0x00, 0x00, 0x00,
];

/// Band B's: 223.020 MHz FM simplex.
const CURRENT_B: [u8; 40] = [
    0xE0, 0x03, 0x4B, 0x0D, 0x00, 0x6A, 0x18, 0x00, 0x88, 0x01, 0x00, 0x08, 0x08, 0x00, 0x00, 0x43,
    0x51, 0x43, 0x51, 0x43, 0x51, 0x00, 0x00, 0x44, 0x49, 0x52, 0x45, 0x43, 0x54, 0x00, 0x00, 0x44,
    0x49, 0x52, 0x45, 0x43, 0x54, 0x00, 0x00, 0x00,
];

/// Each VFO band record's offset from `0x50200` and receive frequency.
const BANDS: [(u32, u32); 10] = [
    (0, 118_000_000),
    (48, 145_190_000),
    (96, 223_000_000),
    (144, 446_000_000),
    (256, 118_000_000),
    (304, 144_000_000),
    (352, 180_750_000),
    (400, 223_020_000),
    (448, 440_000_000),
    (512, 523_000_000),
];

/// The stored limit pairs: lower bound and end, in megahertz.
const LIMITS: [(u32, u32); 10] = [
    (108, 136),
    (136, 174),
    (216, 260),
    (410, 470),
    (108, 136),
    (136, 174),
    (174, 216),
    (216, 410),
    (410, 470),
    (470, 524),
];

fn put(patches: &mut BTreeMap<u32, u8>, address: u32, bytes: &[u8]) {
    for (offset, byte) in (address..).zip(bytes) {
        let _previous = patches.insert(offset, *byte);
    }
}

/// A backup whose slot 0 holds the bench VFO memory.
fn backup(directory: &Path) -> Result<PathBuf, TestError> {
    let mut patches = BTreeMap::new();
    put(&mut patches, 0x5_0100, &CURRENT_A);
    put(&mut patches, 0x5_0128, &CURRENT_B);
    put(&mut patches, 0x5_0154, &[1]);
    put(&mut patches, 0x5_0164, &[7]);
    for (index, (offset, hz)) in BANDS.into_iter().enumerate() {
        let record = match index {
            1 => CURRENT_A,
            7 => CURRENT_B,
            _ => {
                let mut record = CURRENT_A;
                for (destination, source) in record.iter_mut().zip(hz.to_le_bytes()) {
                    *destination = source;
                }
                record
            }
        };
        put(&mut patches, 0x5_0200 + offset, &record);
        put(&mut patches, 0x5_0228 + offset, &[0xFF; 8]);
    }
    for (index, (lower, end)) in (0_u32..).zip(LIMITS) {
        put(
            &mut patches,
            0x5_0488 + 8 * index,
            &(lower * 1_000_000).to_le_bytes(),
        );
        put(
            &mut patches,
            0x5_048C + 8 * index,
            &(end * 1_000_000).to_le_bytes(),
        );
    }
    let mut document = crate::mcp::snapshot::tests::fixture();
    let segments = document
        .get_mut("backup")
        .and_then(|backup| backup.get_mut("segments"))
        .and_then(Value::as_array_mut)
        .ok_or("fixture segments")?;
    for segment in segments {
        let address = segment
            .get("address")
            .and_then(Value::as_u64)
            .ok_or("segment address")?;
        let data = segment
            .get_mut("data")
            .and_then(Value::as_array_mut)
            .ok_or("segment data")?;
        for (offset, byte) in (u32::try_from(address)?..).zip(data.iter_mut()) {
            if let Some(patch) = patches.get(&offset) {
                *byte = Value::from(*patch);
            }
        }
    }
    let path = directory.join("report.json");
    serde_json::to_writer(File::create_new(&path)?, &document)?;
    Ok(path)
}

fn edit(backup: PathBuf, tune: Option<[&str; 2]>, limits: Option<[&str; 3]>) -> EditRequest {
    let owned = |words: &[&str]| words.iter().map(|word| (*word).to_owned()).collect();
    EditRequest {
        backup,
        slot: 0,
        tune: tune.map(|words| owned(&words)),
        limits: limits.map(|words| owned(&words)),
    }
}

fn pages(prepared: &Prepared) -> Vec<u32> {
    prepared
        .replacements
        .iter()
        .map(|replacement| replacement.page().address().as_u32())
        .collect()
}

#[test]
fn a_tune_inside_the_selected_vfo_band_plans_its_two_records() -> TestResult {
    let directory = tempfile::tempdir()?;
    let report = backup(directory.path())?;
    let snapshot = Snapshot::load(&report)?;
    let prepared = edit(report, Some(["b", "223.040"]), None).prepare(&snapshot)?;
    assert_eq!(prepared.change, Change::Tune(Band::B, VfoBand::B200And300));
    assert_eq!(pages(&prepared), [0x5_0100, 0x5_0300]);
    assert_eq!(
        description(&prepared).get("vfo"),
        Some(&json!({
            "slot": 0,
            "change": {"tune": {
                "band": "b",
                "vfo_band": "b200-300",
                "before_hz": 223_020_000,
                "after_hz": 223_040_000,
            }},
        }))
    );
    Ok(())
}

#[test]
fn a_tune_into_another_vfo_band_selects_it() -> TestResult {
    let directory = tempfile::tempdir()?;
    let report = backup(directory.path())?;
    let snapshot = Snapshot::load(&report)?;
    let prepared = edit(report, Some(["a", "446.100"]), None).prepare(&snapshot)?;
    assert_eq!(prepared.change, Change::Tune(Band::A, VfoBand::A430));
    assert_eq!(prepared.after.selected(Band::A), VfoBand::A430);
    assert_eq!(pages(&prepared), [0x5_0100, 0x5_0200]);
    Ok(())
}

#[test]
fn a_limits_edit_writes_the_limits_page() -> TestResult {
    let directory = tempfile::tempdir()?;
    let report = backup(directory.path())?;
    let snapshot = Snapshot::load(&report)?;
    let prepared = edit(report, None, Some(["b200-300", "222", "224"])).prepare(&snapshot)?;
    assert_eq!(prepared.change, Change::Limits(VfoBand::B200And300));
    assert_eq!(pages(&prepared), [0x5_0400]);
    assert_eq!(
        description(&prepared).get("vfo"),
        Some(&json!({
            "slot": 0,
            "change": {"limits": {
                "vfo_band": "b200-300",
                "before_mhz": [216, 409],
                "after_mhz": [222, 224],
            }},
        }))
    );
    Ok(())
}

#[test]
fn an_edit_needs_one_valid_change() -> TestResult {
    let directory = tempfile::tempdir()?;
    let report = backup(directory.path())?;
    let snapshot = Snapshot::load(&report)?;
    let cases = [
        (edit(report.clone(), None, None), "give --tune"),
        (
            edit(report.clone(), Some(["c", "146.520"]), None),
            "band must be a or b",
        ),
        (
            edit(report.clone(), Some(["a", "180.000"]), None),
            "no VFO band of band A",
        ),
        (
            edit(report.clone(), None, Some(["b-xyz", "1", "2"])),
            "VFO band must be one of",
        ),
        (
            edit(report.clone(), None, Some(["a144", "146", "150"])),
            "leave out",
        ),
        (
            edit(report, None, Some(["a144", "136", "low"])),
            "whole number of megahertz",
        ),
    ];
    for (request, expected) in cases {
        let result = request.prepare(&snapshot);
        assert!(
            matches!(&result, Err(error) if error.to_string().contains(expected)),
            "{expected}: {:?}",
            result.err().map(|error| error.to_string())
        );
    }
    Ok(())
}

#[test]
fn every_vfo_band_name_parses_back_to_its_band() -> TestResult {
    for band in VfoBand::ALL {
        assert_eq!(parse_vfo_band(label(band))?, band);
    }
    Ok(())
}

#[test]
fn set_requires_apply_and_an_explicit_port() -> TestResult {
    let arguments = |extra: &[&str]| {
        [
            "mcp",
            "vfo",
            "set",
            "--backup",
            "report.json",
            "--tune",
            "b",
            "223.040",
        ]
        .iter()
        .chain(extra)
        .map(|word| (*word).to_owned())
        .collect::<Vec<_>>()
    };
    assert!(
        crate::mcp::parse(&arguments(&[])).is_err(),
        "--apply is required"
    );
    assert!(
        crate::mcp::parse(&arguments(&["--apply", "--limits", "a144", "136", "173"])).is_err(),
        "--tune conflicts with --limits"
    );
    let request = crate::mcp::parse(&arguments(&["--apply"]))?;
    let refused = request.validate_endpoint_selection(false);
    assert!(
        matches!(&refused, Err(error) if error.to_string().contains("--port")),
        "{refused:?}"
    );
    request.validate_endpoint_selection(true)?;
    Ok(())
}
