use std::collections::BTreeMap;

use super::*;
use crate::RadioConfig;
use crate::error::ValidationError;
use crate::protocol::mcp::regions::menu_regions;
use crate::radio::Identity;
use crate::types::{FirmwareIdentity, RadioModel, RadioType, Region, SlotIndex};

type TestResult = Result<(), Box<dyn std::error::Error>>;

fn identity(firmware: &str) -> Result<Identity, ValidationError> {
    Ok(Identity {
        model: RadioModel::TmD750,
        firmware: FirmwareIdentity::new(firmware)?,
        radio_type: RadioType::new("K,2,1")?,
    })
}

/// Every page of the standard schedule, zero except the patched bytes.
fn standard(
    firmware: &str,
    patches: &[(u32, u8)],
) -> Result<Configuration, Box<dyn std::error::Error>> {
    let pages = menu_regions()
        .into_iter()
        .flat_map(Region::pages)
        .map(|page| {
            let mut data = vec![0; page.len()];
            for (address, value) in patches {
                if let Some(byte) = address
                    .checked_sub(page.address().as_u32())
                    .and_then(|offset| data.get_mut(usize::try_from(offset).ok()?))
                {
                    *byte = *value;
                }
            }
            (page, data)
        });
    Ok(Configuration::from_pages(identity(firmware)?, pages)?)
}

/// The registry field `name` and its slot 0 address, or its global one.
fn located(
    name: &str,
) -> Result<(MenuFieldId, Option<SlotIndex>, u32), Box<dyn std::error::Error>> {
    let id = MenuFieldId::from_name(name).ok_or("a registry field")?;
    let descriptor = &id.field().descriptor;
    let slot = (!descriptor.terms.is_empty()).then_some(SlotIndex::new(0)?);
    Ok((id, slot, descriptor.address(slot)?.as_u32()))
}

/// The bytes the plan's replacements change: address, before, after.
fn changes(plan: &RestorePlan) -> Vec<(u32, u8, u8)> {
    plan.replacements()
        .iter()
        .flat_map(|replacement| {
            (replacement.page().address().as_u32()..)
                .zip(replacement.expected().iter().zip(replacement.replacement()))
                .filter(|(_, (before, after))| before != after)
                .map(|(address, (before, after))| (address, *before, *after))
        })
        .collect()
}

#[test]
fn a_restore_writes_every_page_whose_bytes_differ() -> TestResult {
    let current = standard("1.02", &[(0x40, 0x31)])?;
    let target = standard("1.02", &[(0x41, 0x32), (0x1_0010, b'A'), (0x1_0110, b'B')])?;
    let plan = RestorePlan::new(&current, &target)?;
    let pages: Vec<u32> = plan
        .replacements()
        .iter()
        .map(|replacement| replacement.page().address().as_u32())
        .collect();
    assert_eq!(pages, [0x38, 0x1_0000, 0x1_0100]);
    assert_eq!(
        changes(&plan),
        [
            (0x40, 0x31, 0x00),
            (0x41, 0x00, 0x32),
            (0x1_0010, 0x00, b'A'),
            (0x1_0110, 0x00, b'B')
        ]
    );
    assert_eq!(plan.kept(), []);
    assert_eq!(RestorePlan::new(&current, &current)?.replacements(), []);
    Ok(())
}

#[test]
fn lifecycle_fields_keep_their_current_values() -> TestResult {
    let (bluetooth, bluetooth_slot, bluetooth_address) = located("radio.BluetoothOnOff")?;
    let (gateway, gateway_slot, gateway_address) = located("dv.DvGatewayModeDvGateway")?;
    let (_, _, beep) = located("radio.Beep")?;
    let current = standard(
        "1.02",
        &[(bluetooth_address, 0), (gateway_address, 0), (beep, 1)],
    )?;
    let target = standard(
        "1.02",
        &[(bluetooth_address, 1), (gateway_address, 1), (beep, 0)],
    )?;
    let plan = RestorePlan::new(&current, &target)?;
    assert_eq!(changes(&plan), [(beep, 1, 0)]);
    let kept: Vec<(MenuFieldId, Option<SlotIndex>)> = plan
        .kept()
        .iter()
        .map(|kept| (kept.field(), kept.slot()))
        .collect();
    assert_eq!(kept.len(), 2);
    assert!(kept.contains(&(bluetooth, bluetooth_slot)));
    assert!(kept.contains(&(gateway, gateway_slot)));
    Ok(())
}

#[test]
fn a_shared_byte_keeps_only_the_lifecycle_bits() -> TestResult {
    let (_, _, byte) = located("aprs.Ptt")?;
    assert_eq!(located("aprs.Frequency")?.2, byte);
    let current = standard("1.02", &[(byte, 0b00)])?;
    let target = standard("1.02", &[(byte, 0b11)])?;
    let plan = RestorePlan::new(&current, &target)?;
    assert_eq!(changes(&plan), [(byte, 0b00, 0b01)]);
    assert_eq!(plan.kept().len(), 1);
    Ok(())
}

#[test]
fn every_lifecycle_field_in_every_slot_is_kept() -> TestResult {
    let mut patches = Vec::new();
    let mut partial = BTreeMap::new();
    let mut kept = 0;
    for id in MenuFieldId::ALL {
        let field = id.field();
        if field.write_policy() != MenuWritePolicy::LifecycleRequired {
            continue;
        }
        let slots: Vec<Option<SlotIndex>> = if field.descriptor.terms.is_empty() {
            vec![None]
        } else {
            SlotIndex::all().map(Some).to_vec()
        };
        for slot in slots {
            let start = field.descriptor.address(slot)?.as_u32();
            let length = u32::try_from(field.descriptor.codec.encoded_len())?;
            for address in start..start + length {
                patches.push((address, 0xFF));
            }
            let mask = owned_bits(field.descriptor.codec);
            if mask != 0xFF {
                *partial.entry(start).or_insert(0) |= mask;
            }
            kept += 1;
        }
    }
    let current = standard("1.02", &[])?;
    let target = standard("1.02", &patches)?;
    let plan = RestorePlan::new(&current, &target)?;
    assert_eq!(plan.kept().len(), kept);
    let changed = changes(&plan);
    assert_ne!(changed, Vec::<(u32, u8, u8)>::new());
    for (address, before, after) in changed {
        let mask = partial
            .get(&address)
            .ok_or_else(|| format!("{address:#X} is not a shared lifecycle byte"))?;
        assert_eq!(after & mask, before & mask, "{address:#X}");
    }
    Ok(())
}

/// A full-layout `.d750` file for `radio_type` whose image is zero except
/// the patched bytes.
fn d750(
    radio_type: &str,
    patches: &[(u32, u8)],
) -> Result<RadioConfig, Box<dyn std::error::Error>> {
    let header = crate::file::ConfigHeader::for_mcp_d750(&RadioType::new(radio_type)?)?;
    let mut image = vec![0; crate::types::IMAGE_LENGTH];
    for (address, value) in patches {
        if let Some(byte) = image.get_mut(usize::try_from(*address)?) {
            *byte = *value;
        }
    }
    Ok(RadioConfig::new(header, image)?)
}

#[test]
fn a_d750_file_restores_the_standard_bytes_it_holds() -> TestResult {
    let current = standard("1.02", &[])?;
    let file = d750("K,2,1", &[(0x40, 0x31), (0x0, 0xF2), (0x6_0000, 0x55)])?;
    assert_eq!(file.header().radio_type_bytes(), *b"K21");
    let plan = RestorePlan::from_d750(&current, &file)?;
    assert_eq!(changes(&plan), [(0x40, 0x00, 0x31)]);
    assert_eq!(plan.kept(), []);
    Ok(())
}

#[test]
fn a_d750_file_of_another_radio_type_is_refused() -> TestResult {
    let current = standard("1.02", &[])?;
    let file = d750("E,2,1", &[])?;
    assert!(matches!(
        RestorePlan::from_d750(&current, &file),
        Err(RestoreError::FileRadioType { found, .. }) if found == *b"E21"
    ));
    Ok(())
}

#[test]
fn configurations_of_another_identity_are_refused() -> TestResult {
    let current = standard("1.02", &[])?;
    let target = standard("1.03", &[])?;
    assert!(matches!(
        RestorePlan::new(&current, &target),
        Err(RestoreError::Configuration(
            ConfigurationError::IdentityMismatch { .. }
        ))
    ));
    Ok(())
}
