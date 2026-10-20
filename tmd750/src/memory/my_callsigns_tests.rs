use super::*;
use crate::types::{IMAGE_LENGTH, SLOT_STRIDE};

type TestResult = Result<(), Box<dyn std::error::Error>>;

/// An erased image with the bytes of `writes` placed at their addresses.
fn image(writes: &[(u32, &[u8])]) -> Result<MemoryImage, Box<dyn std::error::Error>> {
    let mut bytes = vec![0xFF; IMAGE_LENGTH];
    for (address, data) in writes {
        let start = usize::try_from(*address)?;
        bytes
            .get_mut(start..start + data.len())
            .ok_or("write outside the image")?
            .copy_from_slice(data);
    }
    Ok(MemoryImage::from_bytes(bytes)?)
}

/// Six entries' bytes: `first` and `second`, then four unset entries.
fn list_bytes(first: &[u8; 12], second: &[u8; 12]) -> Vec<u8> {
    let mut bytes = Vec::new();
    bytes.extend_from_slice(first);
    bytes.extend_from_slice(second);
    bytes.extend_from_slice(&[0; 48]);
    bytes
}

#[test]
fn decodes_each_entry_and_the_zero_based_selection() -> TestResult {
    let entries = list_bytes(b"KQ4NIT\0\0HOME", b"N0CALL\0\0\0\0\0\0");
    let image = image(&[(0x5_0003, &[1]), (0x5_0004, &entries)])?;
    let list = MyCallsignList::read(&image, SlotIndex::new(0)?)?;
    assert_eq!(
        list.entry(DstarSlot::new(1)?),
        &DstarCallsignEntry::new(
            DstarSlot::new(1)?,
            DstarCallsignText {
                callsign: "KQ4NIT".into(),
                memo: "HOME".into()
            }
        )?
    );
    assert_eq!(
        list.entry(DstarSlot::new(2)?),
        &DstarCallsignEntry::new(
            DstarSlot::new(2)?,
            DstarCallsignText {
                callsign: "N0CALL".into(),
                memo: "".into()
            }
        )?
    );
    for slot in 3..=6 {
        let slot = DstarSlot::new(slot)?;
        assert_eq!(list.entry(slot), &DstarCallsignEntry::empty(slot));
    }
    assert_eq!(list.selected(), DstarSlot::new(2)?);
    assert_eq!(list.entries().len(), 6);
    Ok(())
}

#[test]
fn each_pm_slot_reads_its_own_copy_one_stride_further() -> TestResult {
    let entries = list_bytes(b"W1AW\0\0\0\0\0\0\0\0", &[0; 12]);
    let offset = 3 * SLOT_STRIDE;
    let image = image(&[(0x5_0003 + offset, &[0]), (0x5_0004 + offset, &entries)])?;
    let list = MyCallsignList::read(&image, SlotIndex::new(3)?)?;
    assert_eq!(list.entry(DstarSlot::new(1)?).callsign(), "W1AW");
    assert!(
        MyCallsignList::read(&image, SlotIndex::new(0)?).is_err(),
        "PM Off's erased list is not decoded from PM3's bytes"
    );
    Ok(())
}

#[test]
fn erased_padded_or_unsendable_text_and_an_out_of_range_selection_are_refused() -> TestResult {
    for first in [
        *b"\xFF\xFF\xFF\xFF\xFF\xFF\xFF\xFF\xFF\xFF\xFF\xFF",
        *b"AB\0C\0\0\0\0\0\0\0\0",
        *b"KQ4,NIT\0\0\0\0\0",
        *b"KQ4NIT\0\0H\0M\0",
    ] {
        let entries = list_bytes(&first, &[0; 12]);
        let image = image(&[(0x5_0003, &[0]), (0x5_0004, &entries)])?;
        let result = MyCallsignList::read(&image, SlotIndex::new(0)?);
        assert!(
            matches!(result, Err(ValidationError::InvalidCallsignText { .. })),
            "{first:?}: {result:?}"
        );
    }
    let entries = list_bytes(&[0; 12], &[0; 12]);
    let image = image(&[(0x5_0003, &[6]), (0x5_0004, &entries)])?;
    let result = MyCallsignList::read(&image, SlotIndex::new(0)?);
    assert!(
        matches!(
            result,
            Err(ValidationError::SettingOutOfRange {
                value: 6,
                max: 5,
                ..
            })
        ),
        "{result:?}"
    );
    Ok(())
}

#[test]
fn the_region_spans_the_selection_and_six_entries() -> TestResult {
    assert_eq!(
        MyCallsignList::region(SlotIndex::new(0)?)?,
        Region::new(0x5_0003, 0x5_0004 + 72)?
    );
    assert_eq!(
        MyCallsignList::region(SlotIndex::new(5)?)?,
        Region::new(0x5_0003, 0x5_0004 + 72)?.offset_by(5 * SLOT_STRIDE)?
    );
    Ok(())
}
