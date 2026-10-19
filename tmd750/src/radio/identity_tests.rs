//! The write-target predicate every setter, plan and session applies.

use super::*;
use crate::types::{FirmwareIdentity, RadioModel, RadioType};

type TestResult = Result<(), Box<dyn std::error::Error>>;

fn identity(firmware: &str, radio_type: &str) -> Result<Identity, Box<dyn std::error::Error>> {
    Ok(Identity {
        model: RadioModel::TmD750,
        firmware: FirmwareIdentity::new(firmware)?,
        radio_type: RadioType::new(radio_type)?,
    })
}

#[test]
fn every_listed_write_target_admits_exactly_its_own_identity() -> TestResult {
    assert!(!WRITE_TARGETS.is_empty(), "at least one write target");
    for target in WRITE_TARGETS {
        let admitted = identity(target.firmware(), target.radio_type())?;
        assert!(target.matches(&admitted), "{target} matches itself");
        assert!(admitted.is_write_target(), "{target} is a write target");
    }
    Ok(())
}

#[test]
fn the_observed_firmware_and_type_are_a_write_target() -> TestResult {
    assert!(
        identity("1.02", "K,2,1")?.is_write_target(),
        "TM-D750 / 1.02 / K,2,1 is admitted"
    );
    Ok(())
}

#[test]
fn another_firmware_or_type_is_not_a_write_target() -> TestResult {
    for (firmware, radio_type) in [("1.03", "K,2,1"), ("1.00", "K,2,1"), ("1.02", "K,2,2")] {
        assert!(
            !identity(firmware, radio_type)?.is_write_target(),
            "{firmware} with {radio_type} is refused"
        );
    }
    Ok(())
}

#[test]
fn a_write_target_displays_as_the_identity_tuple() -> TestResult {
    let target = WRITE_TARGETS.first().ok_or("one write target")?;
    assert_eq!(
        target.to_string(),
        format!("TM-D750 / {} / {}", target.firmware(), target.radio_type())
    );
    Ok(())
}
