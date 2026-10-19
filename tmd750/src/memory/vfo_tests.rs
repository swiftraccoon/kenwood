use std::collections::BTreeSet;

use super::*;
use crate::types::IMAGE_LENGTH;

type TestResult = Result<(), Box<dyn std::error::Error>>;

/// Band A's current VFO record in a firmware 1.02 backup.
const CURRENT_A_BYTES: [u8; 40] = [
    0x70, 0x6C, 0xA7, 0x08, 0xC0, 0x27, 0x09, 0x00, 0x22, 0x01, 0x02, 0x08, 0x08, 0x00, 0x00, 0x43,
    0x51, 0x43, 0x51, 0x43, 0x51, 0x00, 0x00, 0x44, 0x49, 0x52, 0x45, 0x43, 0x54, 0x00, 0x00, 0x44,
    0x49, 0x52, 0x45, 0x43, 0x54, 0x00, 0x00, 0x00,
];

/// Band B's current VFO record in the same backup.
const CURRENT_B_BYTES: [u8; 40] = [
    0xE0, 0x03, 0x4B, 0x0D, 0x00, 0x6A, 0x18, 0x00, 0x88, 0x01, 0x00, 0x08, 0x08, 0x00, 0x00, 0x43,
    0x51, 0x43, 0x51, 0x43, 0x51, 0x00, 0x00, 0x44, 0x49, 0x52, 0x45, 0x43, 0x54, 0x00, 0x00, 0x44,
    0x49, 0x52, 0x45, 0x43, 0x54, 0x00, 0x00, 0x00,
];

/// The `FO` record each band reported while those records were stored.
const FO_A: &str = "0145190000,0000600000,2,2,0,0,0,0,0,0,2,08,08,000,0,CQCQCQ,0,00";
const FO_B: &str = "0223020000,0001600000,8,8,0,0,0,0,0,0,0,08,08,000,0,CQCQCQ,0,00";

/// Offsets of the ten VFO band records from `0x50200` in that backup.
const OFFSETS: [usize; 10] = [0, 48, 96, 144, 256, 304, 352, 400, 448, 512];

/// Receive frequency of each VFO band record in that backup.
const BAND_HZ: [u32; 10] = [
    118_000_000,
    145_190_000,
    223_000_000,
    446_000_000,
    118_000_000,
    144_000_000,
    180_750_000,
    223_020_000,
    440_000_000,
    523_000_000,
];

/// The stored limit pairs, in megahertz: lower bound and end.
const LIMITS_MHZ: [(u32, u32); 10] = [
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

fn put(image: &mut [u8], offset: usize, bytes: &[u8]) -> TestResult {
    image
        .get_mut(offset..offset + bytes.len())
        .ok_or("offset inside the image")?
        .copy_from_slice(bytes);
    Ok(())
}

/// A record with the current band A bytes and `hz` as its receive frequency.
fn record_at(hz: u32) -> [u8; 40] {
    let mut bytes = CURRENT_A_BYTES;
    for (destination, source) in bytes.iter_mut().zip(hz.to_le_bytes()) {
        *destination = source;
    }
    bytes
}

/// A full image of `0x42` holding the bench VFO memory in `slot`.
fn bench_image(slot: usize) -> Result<Vec<u8>, Box<dyn std::error::Error>> {
    let mut image = vec![0x42; IMAGE_LENGTH];
    let base = 8192 * slot;
    put(&mut image, base + 0x5_0100, &CURRENT_A_BYTES)?;
    put(&mut image, base + 0x5_0128, &CURRENT_B_BYTES)?;
    put(&mut image, base + 0x5_0154, &[1])?;
    put(&mut image, base + 0x5_0164, &[7])?;
    for (index, (offset, hz)) in OFFSETS.into_iter().zip(BAND_HZ).enumerate() {
        let record = match index {
            1 => CURRENT_A_BYTES,
            7 => CURRENT_B_BYTES,
            _ => record_at(hz),
        };
        put(&mut image, base + 0x5_0200 + offset, &record)?;
        put(&mut image, base + 0x5_0200 + offset + 40, &[0xFF; 8])?;
    }
    for (index, (lower, end)) in LIMITS_MHZ.into_iter().enumerate() {
        let address = base + 0x5_0488 + 8 * index;
        put(&mut image, address, &(lower * 1_000_000).to_le_bytes())?;
        put(&mut image, address + 4, &(end * 1_000_000).to_le_bytes())?;
    }
    Ok(image)
}

fn slot(index: u8) -> Result<SlotIndex, Box<dyn std::error::Error>> {
    Ok(SlotIndex::new(index)?)
}

#[test]
fn vfo_bands_follow_the_stored_layout() -> TestResult {
    for (index, band) in (0_u8..).zip(VfoBand::ALL) {
        assert_eq!(band.index(), index);
        assert_eq!(VfoBand::from_index(index), Some(band));
        let offset = OFFSETS.get(usize::from(index)).ok_or("an offset")?;
        assert_eq!(band.record_address(slot(0)?) as usize, 0x5_0200 + offset);
        assert_eq!(
            band.limits_address(slot(0)?) as usize,
            0x5_0488 + 8 * usize::from(index)
        );
        assert_eq!(
            band.record_address(slot(1)?),
            band.record_address(slot(0)?) + 8192
        );
        assert_eq!(band.band(), if index < 4 { Band::A } else { Band::B });
    }
    assert_eq!(VfoBand::from_index(10), None);
    assert_eq!(VfoBand::B200And300.to_string(), "B 200 & 300 MHz");
    Ok(())
}

#[test]
fn slot_zero_decodes_to_what_fo_reported() -> TestResult {
    let image = MemoryImage::from_bytes(bench_image(0)?)?;
    let memory = VfoMemory::read(&image, slot(0)?)?;
    assert_eq!(memory.current(Band::A).cat_record()?.to_wire_fields(), FO_A);
    assert_eq!(memory.current(Band::B).cat_record()?.to_wire_fields(), FO_B);
    assert_eq!(memory.selected(Band::A), VfoBand::A144);
    assert_eq!(memory.selected(Band::B), VfoBand::B200And300);
    assert_eq!(memory.band(VfoBand::A144).record(), memory.current(Band::A));
    assert_eq!(
        memory.band(VfoBand::B200And300).record(),
        memory.current(Band::B)
    );
    for ((band, hz), (lower, end)) in VfoBand::ALL.into_iter().zip(BAND_HZ).zip(LIMITS_MHZ) {
        let stored = memory.band(band);
        assert_eq!(stored.band(), band);
        assert_eq!(stored.record().receive_frequency.as_hz(), hz);
        let limits = stored.limits();
        assert_eq!(
            (
                u32::from(limits.lower_mhz()),
                u32::from(limits.upper_mhz()) + 1
            ),
            (lower, end)
        );
    }
    assert!(memory.page_replacements(&image)?.is_empty());
    Ok(())
}

/// Image addresses whose bytes the replacements change.
fn changed(replacements: &[PageReplacement]) -> BTreeSet<usize> {
    replacements
        .iter()
        .flat_map(|replacement| {
            let start = replacement.page().address().as_usize();
            replacement
                .expected()
                .iter()
                .zip(replacement.replacement())
                .enumerate()
                .filter(|(_, (before, after))| before != after)
                .map(move |(offset, _)| start + offset)
        })
        .collect()
}

#[test]
fn tuning_selects_the_vfo_band_and_writes_only_its_records() -> TestResult {
    let image = MemoryImage::from_bytes(bench_image(0)?)?;
    let mut memory = VfoMemory::read(&image, slot(0)?)?;
    let mut record = memory.current(Band::A).clone();
    record.receive_frequency = Frequency::new(446_100_000)?;
    assert_eq!(memory.tune(Band::A, record.clone())?, VfoBand::A430);
    assert_eq!(memory.selected(Band::A), VfoBand::A430);
    assert_eq!(memory.current(Band::A), &record);
    assert_eq!(memory.band(VfoBand::A430).record(), &record);
    let addresses = changed(&memory.page_replacements(&image)?);
    assert!(!addresses.is_empty());
    let allowed = |address: &usize| {
        (0x5_0100..0x5_0128).contains(address)
            || *address == 0x5_0154
            || (0x5_0290..0x5_02B8).contains(address)
    };
    assert!(addresses.iter().all(allowed), "{addresses:X?}");
    let mut beyond = record;
    beyond.receive_frequency = Frequency::new(523_000_000)?;
    assert_eq!(
        memory.tune(Band::A, beyond),
        Err(VfoError::OutsideLimits {
            band: Band::A,
            hz: 523_000_000
        })
    );
    Ok(())
}

#[test]
fn a_band_record_edit_keeps_the_current_record_in_step() -> TestResult {
    let image = MemoryImage::from_bytes(bench_image(0)?)?;
    let mut memory = VfoMemory::read(&image, slot(0)?)?;
    let mut unselected = memory.band(VfoBand::B430).record().clone();
    unselected.receive_frequency = Frequency::new(441_000_000)?;
    memory.set_band_record(VfoBand::B430, unselected)?;
    assert_eq!(
        memory.current(Band::B).receive_frequency.as_hz(),
        223_020_000
    );
    let mut selected = memory.current(Band::B).clone();
    selected.receive_frequency = Frequency::new(223_040_000)?;
    memory.set_band_record(VfoBand::B200And300, selected.clone())?;
    assert_eq!(memory.current(Band::B), &selected);
    let mut outside = selected;
    outside.receive_frequency = Frequency::new(146_520_000)?;
    assert!(matches!(
        memory.set_band_record(VfoBand::B200And300, outside),
        Err(VfoError::OutsideLimits { band: Band::B, .. })
    ));
    Ok(())
}

#[test]
fn limits_stay_inside_their_band_and_around_stored_records() -> TestResult {
    assert!(VfoLimits::new(VfoBand::A144, 136, 173).is_ok());
    for (lower, upper) in [(135, 150), (140, 139), (140, 174)] {
        assert_eq!(
            VfoLimits::new(VfoBand::A144, lower, upper),
            Err(VfoError::InvalidLimits {
                band: VfoBand::A144,
                lower_mhz: lower,
                upper_mhz: upper
            })
        );
    }
    let image = MemoryImage::from_bytes(bench_image(0)?)?;
    let mut memory = VfoMemory::read(&image, slot(0)?)?;
    assert_eq!(
        memory.set_limits(VfoBand::A144, 146, 150),
        Err(VfoError::LimitsExcludeRecord {
            band: VfoBand::A144
        })
    );
    memory.set_limits(VfoBand::A144, 144, 147)?;
    let limits = memory.band(VfoBand::A144).limits();
    assert!(limits.contains(Frequency::new(147_995_000)?));
    assert!(!limits.contains(Frequency::new(148_000_000)?));
    let replacements = memory.page_replacements(&image)?;
    let addresses = changed(&replacements);
    assert!(
        addresses
            .iter()
            .all(|address| (0x5_0490..0x5_0498).contains(address)),
        "{addresses:X?}"
    );
    let [replacement] = replacements.as_slice() else {
        return Err("one changed page".into());
    };
    let pair = replacement
        .replacement()
        .get(0x90..0x98)
        .ok_or("the limit pair")?;
    assert_eq!(pair.get(..4), Some(&144_000_000_u32.to_le_bytes()[..]));
    assert_eq!(pair.get(4..), Some(&148_000_000_u32.to_le_bytes()[..]));
    Ok(())
}

#[test]
fn malformed_selections_and_limits_are_refused() -> TestResult {
    let mut crossed = bench_image(0)?;
    put(&mut crossed, 0x5_0154, &[7])?;
    assert_eq!(
        VfoMemory::read(&MemoryImage::from_bytes(crossed)?, slot(0)?),
        Err(VfoError::Selection {
            band: Band::A,
            index: 7
        })
    );
    for (lower, end) in [
        (108_500_000_u32, 136_000_000_u32),
        (100_000_000, 136_000_000),
    ] {
        let mut odd = bench_image(0)?;
        put(&mut odd, 0x5_0488, &lower.to_le_bytes())?;
        put(&mut odd, 0x5_048C, &end.to_le_bytes())?;
        assert_eq!(
            VfoMemory::read(&MemoryImage::from_bytes(odd)?, slot(0)?),
            Err(VfoError::MalformedLimits {
                band: VfoBand::A118,
                lower,
                end
            })
        );
    }
    Ok(())
}

#[test]
fn another_slot_reads_at_its_stride() -> TestResult {
    let mut bytes = bench_image(2)?;
    put(&mut bytes, 2 * 8192 + 0x5_0100, &record_at(146_520_000))?;
    let image = MemoryImage::from_bytes(bytes)?;
    let memory = VfoMemory::read(&image, slot(2)?)?;
    assert_eq!(memory.slot(), slot(2)?);
    assert_eq!(
        memory.current(Band::A).receive_frequency.as_hz(),
        146_520_000
    );
    assert!(VfoMemory::read(&image, slot(0)?).is_err());
    Ok(())
}
