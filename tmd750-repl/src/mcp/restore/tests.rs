//! Restores planned from two backup reports: the pages and kept settings, the
//! journal description and the endpoint gate.

use std::fs::File;
use std::path::Path;

use kenwood_tmd750::SlotIndex;
use kenwood_tmd750::memory::MenuFieldId;

use super::*;

type TestError = Box<dyn std::error::Error + Send + Sync>;
type TestResult = Result<(), TestError>;

/// A standard backup named `name` whose bytes are the fixture's except the
/// patched ones.
fn backup(directory: &Path, name: &str, patches: &[(u32, u8)]) -> Result<PathBuf, TestError> {
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
            if let Some((_, value)) = patches.iter().find(|(patched, _)| *patched == offset) {
                *byte = Value::from(*value);
            }
        }
    }
    let path = directory.join(name);
    serde_json::to_writer(File::create_new(&path)?, &document)?;
    Ok(path)
}

/// Where `name` is stored in slot 0, or globally, and the slot it reports.
fn located(name: &str) -> Result<(u32, Option<u8>), TestError> {
    let descriptor = &MenuFieldId::from_name(name)
        .ok_or("a registry field")?
        .field()
        .descriptor;
    let slot = (!descriptor.terms.is_empty()).then_some(SlotIndex::new(0)?);
    Ok((
        descriptor.address(slot)?.as_u32(),
        slot.map(SlotIndex::index),
    ))
}

#[test]
fn a_restore_writes_the_changed_pages_and_keeps_lifecycle_settings() -> TestResult {
    let directory = tempfile::tempdir()?;
    let (bluetooth, slot) = located("radio.BluetoothOnOff")?;
    let current = backup(directory.path(), "current.json", &[])?;
    let target = backup(
        directory.path(),
        "target.json",
        &[(0x40, 0x31), (bluetooth, 1)],
    )?;
    let selection = RestoreSelection {
        backup: current.clone(),
        from: Some(target.clone()),
        from_file: None,
    };
    let plan = selection.prepare(&Snapshot::load(&current)?)?;
    let pages: Vec<u32> = plan
        .replacements()
        .iter()
        .map(|replacement| replacement.page().address().as_u32())
        .collect();
    assert_eq!(pages, [0x38]);
    assert_eq!(changed_bytes(&plan), 1);
    assert_eq!(
        description(&plan, &selection.source()?).get("restore"),
        Some(&json!({
            "source": "backup",
            "from": target.display().to_string(),
            "pages": 1,
            "changed_bytes": 1,
            "kept": [{"field": "radio.BluetoothOnOff", "slot": slot}],
        }))
    );
    let unchanged = RestoreSelection {
        backup: current.clone(),
        from: Some(current.clone()),
        from_file: None,
    }
    .prepare(&Snapshot::load(&current)?)?;
    assert!(unchanged.replacements().is_empty());
    Ok(())
}

/// A full `.d750` file for `radio_type` holding the fixture backup's bytes,
/// `0xFF` elsewhere, and the patched bytes.
fn d750(directory: &Path, radio_type: &str, patches: &[(u32, u8)]) -> Result<PathBuf, TestError> {
    let mut image = vec![0xFF_u8; kenwood_tmd750::types::IMAGE_LENGTH];
    let document = crate::mcp::snapshot::tests::fixture();
    let segments = document
        .get("backup")
        .and_then(|backup| backup.get("segments"))
        .and_then(Value::as_array)
        .ok_or("fixture segments")?;
    for segment in segments {
        let address = segment
            .get("address")
            .and_then(Value::as_u64)
            .ok_or("segment address")?;
        let data = segment
            .get("data")
            .and_then(Value::as_array)
            .ok_or("segment data")?;
        for (offset, value) in (usize::try_from(address)?..).zip(data) {
            if let Some(byte) = image.get_mut(offset) {
                *byte = u8::try_from(value.as_u64().ok_or("a byte")?)?;
            }
        }
    }
    for (address, value) in patches {
        if let Some(byte) = image.get_mut(usize::try_from(*address)?) {
            *byte = *value;
        }
    }
    let header = kenwood_tmd750::file::ConfigHeader::for_mcp_d750(
        &kenwood_tmd750::types::RadioType::new(radio_type)?,
    )?;
    let path = directory.join(format!("{radio_type}.d750"));
    std::fs::write(&path, RadioConfig::new(header, image)?.to_bytes())?;
    Ok(path)
}

#[test]
fn a_d750_file_restores_the_bytes_it_holds_for_the_radio_type() -> TestResult {
    let directory = tempfile::tempdir()?;
    let current = backup(directory.path(), "current.json", &[])?;
    let file = d750(directory.path(), "K,2,1", &[(0x40, 0x31)])?;
    let selection = RestoreSelection {
        backup: current.clone(),
        from: None,
        from_file: Some(file),
    };
    let plan = selection.prepare(&Snapshot::load(&current)?)?;
    assert_eq!(changed_bytes(&plan), 1);
    assert_eq!(
        description(&plan, &selection.source()?)
            .get("restore")
            .and_then(|restore| restore.get("source")),
        Some(&json!("d750"))
    );
    let other = RestoreSelection {
        backup: current.clone(),
        from: None,
        from_file: Some(d750(directory.path(), "E,2,1", &[])?),
    }
    .prepare(&Snapshot::load(&current)?);
    assert!(
        matches!(&other, Err(error) if error.to_string().contains("radio-type bytes")),
        "{:?}",
        other.err().map(|error| error.to_string())
    );
    let neither = RestoreSelection {
        backup: current,
        from: None,
        from_file: None,
    };
    assert!(
        matches!(neither.source(), Err(error) if error.to_string().contains("--from-file")),
        "a source is required"
    );
    Ok(())
}

#[test]
fn apply_requires_apply_an_explicit_port_and_one_source() -> TestResult {
    let arguments = |extra: &[&str]| {
        ["mcp", "restore", "apply", "--backup", "now.json"]
            .iter()
            .chain(extra)
            .map(|word| (*word).to_owned())
            .collect::<Vec<_>>()
    };
    assert!(
        crate::mcp::parse(&arguments(&[
            "--from",
            "a.json",
            "--from-file",
            "b.d750",
            "--apply"
        ]))
        .is_err(),
        "--from conflicts with --from-file"
    );
    assert!(
        crate::mcp::parse(&arguments(&["--from", "then.json"])).is_err(),
        "--apply is required"
    );
    let request = crate::mcp::parse(&arguments(&["--from", "then.json", "--apply"]))?;
    let refused = request.validate_endpoint_selection(false);
    assert!(
        matches!(&refused, Err(error) if error.to_string().contains("--port")),
        "{refused:?}"
    );
    request.validate_endpoint_selection(true)?;
    Ok(())
}
