use super::*;
use crate::types::IMAGE_LENGTH;

type TestResult = Result<(), Box<dyn std::error::Error>>;

/// A full image of `0x42` whose three tables are all zero, as a radio with no
/// entries stores them.
fn empty_tables() -> Result<Vec<u8>, Box<dyn std::error::Error>> {
    let mut image = vec![0x42; IMAGE_LENGTH];
    image
        .get_mut(0x800..0x8F0)
        .ok_or("the tables inside the image")?
        .fill(0);
    Ok(image)
}

fn put(image: &mut [u8], offset: usize, bytes: &[u8]) -> TestResult {
    image
        .get_mut(offset..offset + bytes.len())
        .ok_or("offset inside the image")?
        .copy_from_slice(bytes);
    Ok(())
}

#[test]
fn the_tables_fill_the_page_at_0x800_with_five_entries_each() {
    let addresses: Vec<u32> = MicEFamily::ALL
        .iter()
        .map(|family| family.address())
        .collect();
    assert_eq!(addresses, [0x800, 0x850, 0x8A0]);
    assert_eq!(entry_address(MicEFamily::KenwoodMobile, 0), 0x850);
    assert_eq!(
        entry_address(MicEFamily::Yaesu, 4) + MIC_E_ENTRY_SIZE,
        0x8F0
    );
    assert_eq!(MicEFamily::KenwoodHandheld.to_string(), "Kenwood handheld");
}

#[test]
fn an_entry_round_trips_through_its_stored_form() -> TestResult {
    let entry = MicETypeCode::new('*', "TM-D750")?;
    let bytes = entry.to_bytes();
    assert_eq!(
        bytes,
        [
            b'*', b'T', b'M', b'-', b'D', b'7', b'5', b'0', 0, 0, 0, 0, 0, 0, 0, 0
        ]
    );
    assert_eq!(MicETypeCode::from_bytes(bytes)?, Some(entry));
    assert_eq!(entry.code(), '*');
    assert_eq!(entry.model(), "TM-D750");
    assert_eq!(entry.to_string(), "* \"TM-D750\"");
    let full = MicETypeCode::new('~', "Fifteen bytes!!")?;
    assert_eq!(MicETypeCode::from_bytes(full.to_bytes())?, Some(full));
    Ok(())
}

#[test]
fn an_all_zero_entry_is_unused_and_other_shapes_are_refused() -> TestResult {
    assert_eq!(MicETypeCode::from_bytes([0; MIC_E_ENTRY_SIZE])?, None);
    let mut nameless_code = [0; MIC_E_ENTRY_SIZE];
    nameless_code[1] = b'A';
    let mut gap = MicETypeCode::new('A', "BC")?.to_bytes();
    gap[5] = b'D';
    let mut control = MicETypeCode::new('A', "BC")?.to_bytes();
    control[2] = 0x07;
    for bytes in [nameless_code, gap, control, [0xFF; MIC_E_ENTRY_SIZE]] {
        assert_eq!(
            MicETypeCode::from_bytes(bytes),
            Err(MicETypeCodeError::Malformed { bytes })
        );
    }
    Ok(())
}

#[test]
fn new_admits_printable_ascii_only() {
    for code in [' ', '\u{7F}', 'é'] {
        assert_eq!(
            MicETypeCode::new(code, "X"),
            Err(MicETypeCodeError::InvalidCode { code })
        );
    }
    for model in ["Sixteen bytes!!!", "Ünï", "tab\there"] {
        assert_eq!(
            MicETypeCode::new('A', model),
            Err(MicETypeCodeError::InvalidModel {
                model: model.to_owned()
            })
        );
    }
    assert!(MicETypeCode::new('A', "").is_ok());
}

#[test]
fn an_edit_rewrites_only_its_entry_in_the_page() -> TestResult {
    let mut bytes = empty_tables()?;
    let stored = MicETypeCode::new('_', "FT-99")?;
    put(&mut bytes, 0x8C0, &stored.to_bytes())?;
    let image = MemoryImage::from_bytes(bytes)?;
    let mut tables = MicETypeCodeTables::read(&image)?;
    assert_eq!(tables.get(MicEFamily::Yaesu, 2), Some(&stored));
    assert_eq!(tables.get(MicEFamily::Yaesu, 1), None);
    assert!(tables.page_replacements(&image)?.is_empty());
    let added = MicETypeCode::new('*', "TM-D750")?;
    tables.set(MicEFamily::KenwoodMobile, 4, Some(added))?;
    let replacements = tables.page_replacements(&image)?;
    let [replacement] = replacements.as_slice() else {
        return Err(format!("expected one page, got {}", replacements.len()).into());
    };
    assert_eq!(replacement.page().address().as_u32(), 0x800);
    let changed: Vec<usize> = replacement
        .expected()
        .iter()
        .zip(replacement.replacement())
        .enumerate()
        .filter(|(_, (before, after))| before != after)
        .map(|(offset, _)| 0x800 + offset)
        .collect();
    assert!(!changed.is_empty());
    assert!(
        changed
            .iter()
            .all(|address| (0x890..0x8A0).contains(address)),
        "{changed:X?}"
    );
    tables.set(MicEFamily::KenwoodMobile, 4, None)?;
    assert!(tables.page_replacements(&image)?.is_empty());
    assert_eq!(
        tables.set(MicEFamily::Yaesu, MIC_E_TABLE_ENTRIES, None),
        Err(MicETypeCodeError::PositionOutOfRange { position: 5 })
    );
    Ok(())
}

#[test]
fn a_malformed_stored_entry_names_its_table_and_position() -> TestResult {
    let mut bytes = empty_tables()?;
    put(&mut bytes, 0x860, &[0xFF; MIC_E_ENTRY_SIZE])?;
    let image = MemoryImage::from_bytes(bytes)?;
    assert!(matches!(
        MicETypeCodeTables::read(&image),
        Err(MicETypeCodeError::Entry {
            family: MicEFamily::KenwoodMobile,
            position: 1,
            ..
        })
    ));
    Ok(())
}
